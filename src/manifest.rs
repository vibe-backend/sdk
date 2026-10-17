use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diag::{Diag, Report};
use crate::lock::{find_section, Kind, LockDoc, Scope};
use crate::security::pipeline_name;
use crate::sign;
use crate::validate::{self, ValidateOptions};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BuildMode {
    Dev,
    Release,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub manifest_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_mode: Option<BuildMode>,
    pub routes: Vec<RouteSpec>,
    pub models: Vec<ModelSpec>,
    pub methods: Vec<MethodSpec>,
    pub migrations: Vec<MigrationSpec>,
    /// Signed record of what the manifest was built from (spec 10.1, 10.10).
    #[serde(default)]
    pub provenance: Provenance,
    pub signature: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub compilers: BTreeMap<String, crate::lock::LlmStamp>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<String, crate::lock::TranslationStamp>,
    /// Source commit (`GITHUB_SHA`, `--commit`, or `git rev-parse HEAD`).
    pub commit: String,
    /// One entry per lock, sorted by id.
    pub locks: Vec<LockProvenance>,
    /// Every security item of every lock, sorted.
    pub security_items: Vec<ItemProvenance>,
    /// Release version without the leading `v` (release builds only). The
    /// Sigstore certificate identity is bound to `refs/tags/v<version>`.
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LockProvenance {
    pub id: String,
    pub lock_sha256: String,
    pub content_hash: String,
    pub source_hash: String,
    pub compiler_hash: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ItemProvenance {
    pub lock: String,
    pub item: String,
    pub risk: String,
}

impl Provenance {
    pub fn high_risk(&self) -> Vec<&ItemProvenance> {
        self.security_items
            .iter()
            .filter(|item| item.risk == "high")
            .collect()
    }
}

/// How a manifest is built.
#[derive(Clone, Debug, Default)]
pub struct BuildInfo {
    /// Release build: left unsigned here and signed by the release workflow
    /// with Sigstore keyless (decision 119). High-risk items are only listed.
    pub release: bool,
    /// Release version (without `v`); required for a release build.
    pub version: String,
    pub commit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RouteSpec {
    pub method: String,
    pub path: String,
    pub feature_id: String,
    pub public: bool,
    pub auth_removed: bool,
    pub auth_mode: String,
    pub roles: Vec<String>,
    pub scopes: Vec<String>,
    /// Claim that holds roles for this route (`roles-claim`); empty means the
    /// runtime default.
    #[serde(default)]
    pub roles_claim: String,
    /// Per-route JWT overrides (spec §4.1 `### auth` bullets `iss`, `aud`,
    /// `jwks-uri`); empty means the runtime default from env.
    #[serde(default)]
    pub jwt_iss: String,
    #[serde(default)]
    pub jwt_aud: String,
    #[serde(default)]
    pub jwks_uri: String,
    pub rates: Vec<RateSpec>,
    pub body_limit: u64,
    pub request: Vec<ReqField>,
    pub transform_in: String,
    pub transform_out: String,
    pub steps: Vec<StepSpec>,
    pub errors: Vec<ErrMap>,
    pub min_response_ms: u64,
    pub public_writes: bool,
    pub challenge_type: String,
    pub challenge_when: String,
    pub deprecated: bool,
    pub sunset: String,
    /// `### token-refresh` pipeline: this route rotates refresh tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<RefreshSpec>,
    /// `### token-revoke` pipeline: revoke refresh tokens before the steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke: Option<RevokeSpec>,
}

/// Platform refresh-token rotation (spec 4A.1).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RefreshSpec {
    /// Method that signs the new access token (needs `sign-token`).
    pub sign_method: String,
    pub refresh_ttl_s: i64,
    pub idle_ttl_s: i64,
    pub reuse_grace_s: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RevokeSpec {
    /// `family` (the presented refresh token's family) or `all` (every
    /// family of the caller).
    pub scope: String,
}

pub const MAX_REFRESH_TTL_S: i64 = 30 * 86_400;
pub const MAX_IDLE_TTL_S: i64 = 7 * 86_400;
/// Spec §4A.1 (decision 61): `reuse-grace` defaults to 10s and is at most 10s.
pub const DEFAULT_REUSE_GRACE_S: i64 = 10;
pub const MAX_REUSE_GRACE_S: i64 = 10;

/// Startup check of a manifest's token-refresh settings (a manifest built by
/// an older compiler may carry values outside the current limits).
pub fn refresh_spec_problem(route: &str, spec: &RefreshSpec) -> Option<String> {
    let checks = [
        ("refresh-ttl", spec.refresh_ttl_s, MAX_REFRESH_TTL_S),
        ("idle-ttl", spec.idle_ttl_s, MAX_IDLE_TTL_S),
        ("reuse-grace", spec.reuse_grace_s, MAX_REUSE_GRACE_S),
    ];
    checks.iter().find_map(|(name, value, max)| {
        (!(1..=*max).contains(value)).then(|| {
            format!("route {route}: token-refresh {name} is {value}s; it must be between 1s and {max}s (spec §4A.1); recompile the lock")
        })
    })
}

fn bullet<'a>(bullets: &'a [crate::lock::Bullet], key: &str) -> Option<&'a str> {
    bullets
        .iter()
        .find(|b| b.key == key)
        .map(|b| b.value.trim())
}

fn secs(raw: Option<&str>, default: i64, max: i64, name: &str) -> Result<i64, String> {
    let Some(raw) = raw else { return Ok(default) };
    let raw = raw.trim();
    let (number, unit) = [
        ("ms", 1u128),
        ("s", 1000),
        ("m", 60_000),
        ("h", 3_600_000),
        ("d", 86_400_000),
    ]
    .into_iter()
    .find_map(|(suffix, unit)| raw.strip_suffix(suffix).map(|n| (n.trim(), unit)))
    .unwrap_or((raw, 1000));
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(format!("token-refresh {name}: bad duration `{raw}`"));
    }
    // Multiply decimal digits before converting to whole seconds.
    let mut digits = Vec::new();
    let mut carry = 0;
    for digit in whole.bytes().chain(fraction.bytes()).rev() {
        carry += u128::from(digit - b'0') * unit;
        digits.push((carry % 10) as u8 + b'0');
        carry /= 10;
    }
    while carry > 0 {
        digits.push((carry % 10) as u8 + b'0');
        carry /= 10;
    }
    while digits.len() <= fraction.len() {
        digits.push(b'0');
    }
    digits.reverse();
    let split = digits.len() - fraction.len();
    let ms = std::str::from_utf8(&digits[..split])
        .ok()
        .and_then(|n| n.parse::<u128>().ok());
    let fractional = digits[split..].iter().any(|b| *b != b'0');
    let cap = max as u128 * 1000;
    let Some(ms) = ms.filter(|ms| *ms >= 1000 && (*ms < cap || (*ms == cap && !fractional))) else {
        return Err(format!(
            "token-refresh {name} must be between 1s and {max}s"
        ));
    };
    let s = (ms / 1000) as i64;
    Ok(s)
}

/// Parses a `### token-refresh` block.
pub fn parse_refresh_block(bullets: &[crate::lock::Bullet]) -> Result<RefreshSpec, String> {
    let sign_method = bullet(bullets, "sign-method").unwrap_or("").to_string();
    if sign_method.is_empty() {
        return Err("token-refresh needs sign-method".into());
    }
    match bullet(bullets, "transport") {
        None | Some("body") => {}
        Some(other) => {
            return Err(format!(
                "token-refresh transport `{other}` is not supported by this runtime (only body)"
            ))
        }
    }
    for b in bullets {
        // `source-lines` is metadata the compiler adds to every block.
        if b.key == "source-lines" {
            continue;
        }
        if ![
            "sign-method",
            "refresh-ttl",
            "idle-ttl",
            "reuse-grace",
            "transport",
        ]
        .contains(&b.key.as_str())
        {
            return Err(format!("unknown token-refresh option `{}`", b.key));
        }
    }
    Ok(RefreshSpec {
        sign_method,
        refresh_ttl_s: secs(
            bullet(bullets, "refresh-ttl"),
            MAX_REFRESH_TTL_S,
            MAX_REFRESH_TTL_S,
            "refresh-ttl",
        )?,
        idle_ttl_s: secs(
            bullet(bullets, "idle-ttl"),
            MAX_IDLE_TTL_S,
            MAX_IDLE_TTL_S,
            "idle-ttl",
        )?,
        reuse_grace_s: secs(
            bullet(bullets, "reuse-grace"),
            DEFAULT_REUSE_GRACE_S,
            MAX_REUSE_GRACE_S,
            "reuse-grace",
        )?,
    })
}

/// Checks that a `token-refresh` `sign-method` names an existing method
/// with a `sign-jwt` step. A typo here must fail the build, not every
/// refresh call at runtime.
pub fn refresh_sign_method_problem(sign_method: &str, methods: &[MethodSpec]) -> Option<String> {
    match methods.iter().find(|m| m.target == sign_method) {
        None => Some(format!(
            "token-refresh sign-method `{sign_method}` is not a known method"
        )),
        Some(m) if !m.steps.iter().any(|s| s.op == "sign-jwt") => Some(format!(
            "token-refresh sign-method `{sign_method}` has no sign-jwt step"
        )),
        Some(_) => None,
    }
}

/// Parses a `### token-revoke` block.
pub fn parse_revoke_block(bullets: &[crate::lock::Bullet]) -> Result<RevokeSpec, String> {
    let scope = bullet(bullets, "scope").unwrap_or("family");
    if scope != "family" && scope != "all" {
        return Err(format!(
            "token-revoke scope must be family or all (got `{scope}`)"
        ));
    }
    Ok(RevokeSpec {
        scope: scope.to_string(),
    })
}

/// Route-shape rules for the refresh pipelines.
pub fn check_refresh_route(
    refresh: Option<&RefreshSpec>,
    revoke: Option<&RevokeSpec>,
    public: bool,
    auth_removed: bool,
) -> Result<(), String> {
    if refresh.is_some() && revoke.is_some() {
        return Err("a route cannot have both token-refresh and token-revoke".into());
    }
    if refresh.is_some() && !(public && auth_removed) {
        return Err("token-refresh routes must be public with auth removed (the refresh token is the credential)".into());
    }
    if revoke.is_some() && auth_removed {
        return Err("token-revoke needs auth".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RateSpec {
    pub per_minute: u32,
    pub key: String,
}

/// Checks a `rate-limit` key against spec §4.3. Supported here: `ip`,
/// `tenant`, `user` (needs auth), `api-key` (needs api-key auth),
/// `input.{field}`, and the legacy login-guard `account` (= `input.email`).
/// `family` needs the platform `token-refresh` pipeline, which this runtime
/// does not provide, so it is rejected rather than silently keyed by IP.
/// Rate-limit parameters this runtime does not enforce. They are rejected
/// rather than silently ignored.
pub const UNSUPPORTED_RATE_PARAMS: [&str; 3] = ["per-hour", "per-day", "burst"];

pub fn check_rate_params<'a>(keys: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    for key in keys {
        if UNSUPPORTED_RATE_PARAMS.contains(&key) {
            return Err(format!(
                "rate-limit `{key}` is not supported by this runtime (only per-minute); remove it rather than rely on it"
            ));
        }
    }
    Ok(())
}

/// Highest accepted `per-minute` value (bounds per-bucket memory).
pub const MAX_PER_MINUTE: u32 = 10_000;

/// Route-level rate rules: every limit is within [`MAX_PER_MINUTE`], and an
/// `input.*`/`account` limit must be paired with an `ip` limit. Otherwise a
/// client could rotate input values to dodge a per-input limit.
pub fn check_rate_set(rates: &[RateSpec]) -> Result<(), String> {
    for rate in rates {
        if rate.per_minute == 0 || rate.per_minute > MAX_PER_MINUTE {
            return Err(format!(
                "rate-limit per-minute must be between 1 and {MAX_PER_MINUTE} (got {})",
                rate.per_minute
            ));
        }
    }
    let input_keyed = rates
        .iter()
        .any(|r| r.key == "account" || r.key.starts_with("input."));
    if input_keyed && !rates.iter().any(|r| r.key == "ip") {
        return Err(
            "a rate-limit keyed by input.* or account must be paired with an `ip` rate-limit on the same route"
                .into(),
        );
    }
    Ok(())
}

pub fn check_rate_key(key: &str, auth_removed: bool, auth_mode: &str) -> Result<(), String> {
    match key {
        "ip" | "tenant" | "account" => Ok(()),
        "user" if auth_removed => Err("rate-limit key `user` needs auth".into()),
        "user" => Ok(()),
        "api-key" if auth_removed || auth_mode != "api-key" => {
            Err("rate-limit key `api-key` needs `auth: { mode: api-key }`".into())
        }
        "api-key" => Ok(()),
        "family" => Err("rate-limit key `family` is only allowed on token-refresh routes".into()),
        _ => match key.strip_prefix("input.") {
            Some(field)
                if !field.is_empty()
                    && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                Ok(())
            }
            _ => Err(format!("unknown rate-limit key `{key}`")),
        },
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReqField {
    pub location: String,
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ErrMap {
    pub status: u16,
    pub code: String,
    pub from: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StepSpec {
    pub id: String,
    pub op: String,
    pub target: String,
    pub args: String,
    pub value: String,
    pub status: u16,
    pub sql: String,
    pub params: Vec<String>,
    pub out: String,
    pub fail: String,
    pub when_expr: String,
    pub writes: bool,
    pub table: String,
    /// Every bullet of the step, raw (op-specific settings such as `hash`,
    /// `plain`, `bytes`, `claims`, `ttl`, `key-ref`).
    #[serde(default)]
    pub opts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub provider: String,
    pub table: String,
    pub fields: Vec<FieldSpec>,
    #[serde(default)]
    pub constraints: Vec<Vec<String>>,
    pub crud: Vec<String>,
    pub list: ListOpts,
    pub sensitive_output: Vec<String>,
    /// Column filled from `ctx.user.id` and used to scope non-admin reads and writes.
    #[serde(default)]
    pub owner_key: Option<String>,
    /// Explicit relaxation: `tenant-wide` or `role(admin)`.
    #[serde(default)]
    pub ownership: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldSpec {
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub unique: bool,
    pub primary: bool,
    pub system: bool,
    pub sensitive: bool,
    pub nullable: bool,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ListOpts {
    pub page_size: i64,
    pub max_page_size: i64,
    pub sort: Vec<String>,
    pub filter: Vec<String>,
}

impl Default for ListOpts {
    fn default() -> Self {
        Self {
            page_size: 20,
            max_page_size: 100,
            sort: vec!["created_at".into()],
            filter: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MethodSpec {
    pub target: String,
    pub writes: bool,
    pub input: Vec<ReqField>,
    pub errors: Vec<String>,
    pub capabilities: Vec<String>,
    pub steps: Vec<StepSpec>,
    pub crud_model: String,
    pub crud_action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MigrationSpec {
    pub id: String,
    pub provider: String,
    pub seq: String,
    pub statements: Vec<String>,
}

pub fn manifest_from_project(root: &Path, signing_key: &SigningKey) -> Result<Manifest, Report> {
    manifest_from_project_with_mode(root, signing_key, false)
}

/// Build a local manifest with explicit private-endpoint policy.
pub fn manifest_from_project_with_mode(
    root: &Path,
    signing_key: &SigningKey,
    dev: bool,
) -> Result<Manifest, Report> {
    let loaded = validate::load_project(root)?;
    validate::validate_loaded(
        &ValidateOptions {
            root,
            deploy: true,
            dev,
        },
        &loaded,
    )
    .0?;
    manifest_from_loaded(root, &loaded, signing_key)
}

/// Validates (deploy rules, including the recompile check) and builds the
/// manifest from exactly the loaded lock texts; nothing is re-read.
pub fn manifest_from_loaded(
    root: &Path,
    loaded: &validate::LoadedProject,
    signing_key: &SigningKey,
) -> Result<Manifest, Report> {
    build_signed(root, loaded, Some(signing_key), &BuildInfo::default())
}

/// Validates (deploy rules) and builds the manifest from exactly the loaded
/// lock texts; nothing is re-read. Records provenance. A dev build is signed
/// with the development key; a release build takes no key and is left
/// unsigned for the release workflow's Sigstore keyless signature (decisions
/// 92, 119). High-risk items are listed in the provenance, never blocking
/// (decision 90 withdrawn).
pub fn build_signed(
    root: &Path,
    loaded: &validate::LoadedProject,
    signing_key: Option<&SigningKey>,
    info: &BuildInfo,
) -> Result<Manifest, Report> {
    match (info.release, signing_key) {
        (true, Some(_)) => {
            return Err(single(
                "release manifests are signed with Sigstore keyless only, never with a key".into(),
            ))
        }
        (false, None) => return Err(single("a dev manifest needs the development key".into())),
        _ => {}
    }
    if info.release {
        crate::sigstore::check_version(&info.version).map_err(single)?;
        let commit = &info.commit;
        if commit.len() != 40
            || !commit
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(single(format!(
                "release builds need the full 40-character source commit, got `{commit}`"
            )));
        }
    }
    let (result, docs) = validate::validate_loaded(
        &ValidateOptions {
            root,
            deploy: true,
            dev: !info.release,
        },
        loaded,
    );
    result?;
    let record = loaded
        .build_record
        .as_deref()
        .and_then(|raw| serde_json::from_str::<crate::buildrecord::BuildRecord>(raw).ok());
    let mut manifest = build_manifest_with_record(&docs, !info.release, record.as_ref())?;
    manifest.build_mode = Some(if info.release {
        BuildMode::Release
    } else {
        BuildMode::Dev
    });
    let mut locks = Vec::new();
    let mut items = Vec::new();
    for (source, text) in loaded.sources.iter().zip(&loaded.lock_texts) {
        let (Some(text), Some(doc)) = (text, docs.get(&source.id)) else {
            continue;
        };
        locks.push(LockProvenance {
            id: doc.meta.id.clone(),
            lock_sha256: crate::buildrecord::lock_hash(text),
            content_hash: crate::lock::content_hash(doc),
            source_hash: doc.meta.source_hash.clone(),
            compiler_hash: doc.meta.compiler_hash.clone(),
        });
        for item in &doc.meta.security_items {
            items.push(ItemProvenance {
                lock: doc.meta.id.clone(),
                item: item.item.clone(),
                risk: item.risk.as_str().to_string(),
            });
        }
    }
    locks.sort_by(|a, b| a.id.cmp(&b.id));
    items.sort_by(|a, b| (&a.lock, &a.item).cmp(&(&b.lock, &b.item)));
    manifest.provenance = Provenance {
        compilers: record.map(|record| record.compilers).unwrap_or_default(),
        translations: loaded
            .build_record
            .as_deref()
            .and_then(|raw| serde_json::from_str::<crate::buildrecord::BuildRecord>(raw).ok())
            .map(|record| record.translations)
            .unwrap_or_default(),
        commit: info.commit.clone(),
        locks,
        security_items: items,
        version: if info.release {
            info.version.clone()
        } else {
            String::new()
        },
    };
    manifest.signature = String::new();
    let bytes =
        serde_json::to_vec(&manifest).map_err(|err| single(format!("manifest json: {err}")))?;
    if let Some(key) = signing_key {
        manifest.signature = sign::sign_bytes(&bytes, key);
    }
    Ok(manifest)
}

/// Outside development only release manifests run (decisions 75, 79, 91):
/// built with `vibe manifest --release` and verified with Sigstore keyless
/// ([`read_release_manifest`]). High-risk items are reported, not blocking
/// (decision 90 withdrawn).
pub fn check_deployable(manifest: &Manifest, development: bool) -> Result<(), String> {
    if development {
        return Ok(());
    }
    let provenance = &manifest.provenance;
    if manifest.build_mode == Some(BuildMode::Dev) {
        return Err(
            "only release manifests (`vibe manifest --release`) can run outside development".into(),
        );
    }
    if provenance.locks.is_empty() {
        return Err("manifest has no lock provenance".into());
    }
    if provenance.version.is_empty() {
        return Err("release manifest has no version".into());
    }
    Ok(())
}

/// Reads a release manifest and verifies its Sigstore keyless signature
/// (decision 119): the bundle certificate must name exactly
/// `release.yml@refs/tags/v<version>` with the GitHub Actions issuer, the
/// manifest's commit and a GitHub-hosted runner, with Rekor inclusion,
/// verified offline against the compiled-in trust root.
pub fn read_release_manifest(
    path: &Path,
    bundle: &Path,
    version: &str,
) -> Result<Manifest, Report> {
    crate::sigstore::check_version(version).map_err(single)?;
    let bytes = fs::read(path).map_err(|err| single(format!("cannot read manifest: {err}")))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|err| single(format!("manifest json: {err}")))?;
    let provenance = &manifest.provenance;
    if manifest.build_mode == Some(BuildMode::Dev) {
        return Err(single(
            "only release manifests (`vibe manifest --release`) can run outside development".into(),
        ));
    }
    if !manifest.signature.is_empty() {
        return Err(single(
            "release manifests carry no key signature; they are verified with Sigstore only".into(),
        ));
    }
    if provenance.version != version {
        return Err(single(format!(
            "manifest version `{}` is not the deployed version `{version}`",
            provenance.version
        )));
    }
    let expected = crate::sigstore::Expected {
        version: version.to_string(),
        commit: provenance.commit.clone(),
    };
    crate::sigstore::verify_release_manifest(path, bundle, &expected).map_err(single)?;
    Ok(manifest)
}

pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), Report> {
    let json = serde_json::to_vec_pretty(manifest).map_err(|err| single(err.to_string()))?;
    crate::fsutil::write_atomic(path, &json).map_err(|err| single(err.to_string()))
}

pub fn read_manifest(path: &Path, key: &VerifyingKey) -> Result<Manifest, Report> {
    let bytes = fs::read(path).map_err(|err| single(format!("cannot read manifest: {err}")))?;
    let mut manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|err| single(format!("manifest json: {err}")))?;
    let signature = manifest.signature.clone();
    manifest.signature.clear();
    let canonical = serde_json::to_vec(&manifest).map_err(|err| single(err.to_string()))?;
    if !sign::verify_bytes(&canonical, &signature, key) {
        return Err(single("manifest signature is invalid".into()));
    }
    manifest.signature = signature;
    Ok(manifest)
}

pub fn build_manifest(docs: &HashMap<String, LockDoc>) -> Result<Manifest, Report> {
    build_manifest_with_mode(docs, false)
}

pub fn build_manifest_with_mode(
    docs: &HashMap<String, LockDoc>,
    dev: bool,
) -> Result<Manifest, Report> {
    build_manifest_with_record(docs, dev, None)
}

pub fn build_manifest_with_record(
    docs: &HashMap<String, LockDoc>,
    dev: bool,
    record: Option<&crate::buildrecord::BuildRecord>,
) -> Result<Manifest, Report> {
    if !dev
        && record.is_some_and(|record| {
            record
                .compilers
                .values()
                .any(|stamp| stamp.endpoint_private)
        })
    {
        return Err(single(
            "locks compiled against a private endpoint cannot be deployed".into(),
        ));
    }
    let mut report = Report::new();
    let mut models = Vec::new();
    let mut methods = Vec::new();
    let mut routes = Vec::new();
    let mut migrations = Vec::new();
    let mut ids: Vec<&LockDoc> = docs.values().collect();
    ids.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
    for doc in ids {
        match doc.meta.kind {
            Kind::Model => models.push(model_from(doc)),
            Kind::Method => match method_from(doc) {
                Ok(method) => methods.push(method),
                Err(diag) => report.push(diag),
            },
            Kind::Migration => migrations.push(migration_from(doc)),
            Kind::Api if doc.meta.scope == Some(Scope::Feature) => match route_from(doc, docs) {
                Ok(route) => routes.push(route),
                Err(diag) => report.push(diag),
            },
            _ => {}
        }
    }
    for model in &models {
        for action in &model.crud {
            let target = format!("{}.{}", model.id, action);
            if methods.iter().any(|method| method.target == target) {
                continue;
            }
            methods.push(MethodSpec {
                target,
                writes: matches!(action.as_str(), "create" | "update" | "delete"),
                input: Vec::new(),
                errors: CRUD_ERROR_CODES
                    .iter()
                    .map(|code| code.to_string())
                    .collect(),
                capabilities: Vec::new(),
                steps: Vec::new(),
                crud_model: model.id.clone(),
                crud_action: action.clone(),
            });
        }
    }
    for route in &routes {
        if let Some(spec) = &route.refresh {
            if let Some(err) = refresh_sign_method_problem(&spec.sign_method, &methods) {
                report.push(Diag::error(&route.feature_id, err).at_section("Pipelines"));
            }
        }
    }
    if report.has_errors() {
        return Err(report);
    }
    Ok(Manifest {
        provenance: Provenance::default(),
        manifest_version: MANIFEST_VERSION,
        build_mode: dev.then_some(BuildMode::Dev),
        routes,
        models,
        methods,
        migrations,
        signature: String::new(),
    })
}

fn model_from(doc: &LockDoc) -> ModelSpec {
    let provider = find_section(&doc.sections, "Table")
        .and_then(|s| s.get("provider"))
        .unwrap_or("")
        .to_string();
    let table = find_section(&doc.sections, "Table")
        .and_then(|s| s.get("table"))
        .unwrap_or("")
        .to_string();
    let mut fields = Vec::new();
    if let Some(section) = find_section(&doc.sections, "Fields") {
        if let Some(table) = &section.table {
            for row in &table.rows {
                if row.len() < 2 {
                    continue;
                }
                let rules = row.get(2).map(String::as_str).unwrap_or("");
                let stringy = matches!(row[1].as_str(), "string" | "password" | "bytes");
                fields.push(FieldSpec {
                    name: row[0].clone(),
                    ty: row[1].clone(),
                    required: rules.split(',').any(|part| part.trim() == "required"),
                    unique: rules.split(',').any(|part| part.trim() == "unique"),
                    primary: rules.split(',').any(|part| part.trim() == "primary"),
                    system: row.get(3).map(String::as_str) == Some("yes"),
                    sensitive: rules.contains("sensitive"),
                    nullable: rules.contains("nullable"),
                    min: number_rule(rules, "min").filter(|_| !stringy).or_else(|| {
                        if stringy {
                            None
                        } else {
                            number_rule(rules, "min")
                        }
                    }),
                    max: if stringy {
                        None
                    } else {
                        number_rule(rules, "max")
                    },
                    format: text_rule(rules, "format"),
                });
                if stringy {
                    let field = fields.last_mut().expect("just pushed");
                    field.min = number_rule(rules, "min");
                    field.max = number_rule(rules, "max");
                }
            }
        }
    }
    let table_section = find_section(&doc.sections, "Table");
    let owner_key = table_section
        .and_then(|section| section.get("owner-key"))
        .filter(|value| !value.is_empty() && *value != "(none)")
        .map(str::to_string);
    let ownership = table_section
        .and_then(|section| section.get("ownership"))
        .unwrap_or("")
        .to_string();
    let mut crud = Vec::new();
    let mut list = ListOpts::default();
    let mut sensitive_output = Vec::new();
    if let Some(section) = find_section(&doc.sections, "CRUD") {
        if let Some(table) = &section.table {
            for row in &table.rows {
                let Some(action) = row.first() else { continue };
                if action.is_empty() {
                    continue;
                }
                crud.push(action.clone());
                let options = row.get(1).map(String::as_str).unwrap_or("");
                if action == "list" {
                    list.page_size = number_rule(options, "page-size").unwrap_or(20);
                    list.max_page_size = number_rule(options, "max-page-size")
                        .unwrap_or(100)
                        .min(500);
                    list.sort = bracket_list(options, "sort");
                    list.filter = bracket_list(options, "filter");
                    if list.sort.is_empty() {
                        list.sort.push("created_at".into());
                    }
                }
                if action == "get" {
                    sensitive_output = bracket_list(options, "sensitive-output");
                }
            }
        }
    }
    ModelSpec {
        id: doc.meta.id.clone(),
        provider,
        table,
        fields,
        constraints: crate::zh::constraints(doc).unwrap_or_default(),
        crud,
        list,
        sensitive_output,
        owner_key,
        ownership,
    }
}

/// The single value of `key` in `bullets`. A repeated key is an error.
pub(crate) fn single_value<'a>(
    bullets: &'a [crate::lock::Bullet],
    key: &str,
) -> Result<Option<&'a str>, String> {
    let mut found = bullets.iter().filter(|b| b.key == key);
    let first = found.next().map(|b| b.value.as_str());
    if found.next().is_some() {
        return Err(format!("duplicate `{key}`"));
    }
    Ok(first)
}

/// Strict `writes`: absent is `None`; present must be exactly `true` or
/// `false` and appear once.
pub(crate) fn parse_writes(bullets: &[crate::lock::Bullet]) -> Result<Option<bool>, String> {
    match single_value(bullets, "writes")?.map(str::trim) {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(other) => Err(format!(
            "invalid writes `{other}`: expected `true` or `false`"
        )),
    }
}

/// Strict step `status`: absent is `None`; present must be a plain decimal
/// HTTP status (100-599) and appear once.
pub(crate) fn parse_status(bullets: &[crate::lock::Bullet]) -> Result<Option<u16>, String> {
    let Some(raw) = single_value(bullets, "status")? else {
        return Ok(None);
    };
    let raw = raw.trim();
    let valid = !raw.is_empty() && raw.len() == 3 && raw.bytes().all(|b| b.is_ascii_digit());
    match raw.parse::<u16>() {
        Ok(code) if valid && (100..600).contains(&code) => Ok(Some(code)),
        _ => Err(format!(
            "invalid status `{raw}`: expected an HTTP status (100-599)"
        )),
    }
}

fn method_from(doc: &LockDoc) -> Result<MethodSpec, Diag> {
    let target = doc
        .meta
        .id
        .split_once("/methods/")
        .map(|(object, method)| format!("{object}.{method}"))
        .unwrap_or_else(|| doc.meta.id.clone());
    let signature = find_section(&doc.sections, "Signature");
    let writes = match signature {
        Some(sig) => parse_writes(&sig.bullets)
            .map_err(|err| Diag::error(&doc.meta.id, err).at_section("Signature"))?
            .unwrap_or(false),
        None => false,
    };
    let mut errors = Vec::new();
    if let Some(raw) = signature.and_then(|s| s.get("errors")) {
        errors = crate::lock::parse_string_list(raw).map_err(|err| {
            Diag::error(&doc.meta.id, format!("invalid errors: {err}")).at_section("Signature")
        })?;
    }
    let mut capabilities = Vec::new();
    if let Some(raw) = signature.and_then(|s| s.get("capabilities")) {
        capabilities = crate::lock::parse_string_list(raw).map_err(|err| {
            Diag::error(&doc.meta.id, format!("invalid capabilities: {err}"))
                .at_section("Signature")
        })?;
    }
    Ok(MethodSpec {
        target,
        writes,
        input: Vec::new(),
        errors,
        capabilities,
        steps: steps_from(find_section(&doc.sections, "Steps"), &doc.meta.id)?,
        crud_model: String::new(),
        crud_action: String::new(),
    })
}

fn migration_from(doc: &LockDoc) -> MigrationSpec {
    let provider = find_section(&doc.sections, "Target")
        .and_then(|s| s.get("provider"))
        .unwrap_or("")
        .to_string();
    let seq = find_section(&doc.sections, "Target")
        .and_then(|s| s.get("seq"))
        .unwrap_or("")
        .to_string();
    let mut statements = Vec::new();
    if let Some(up) = find_section(&doc.sections, "Up") {
        for block in &up.blocks {
            if let Some(sql) = block.bullets.iter().find(|b| b.key == "sql") {
                statements.push(sql.value.trim_matches('`').trim().to_string());
            }
        }
    }
    MigrationSpec {
        id: doc.meta.id.clone(),
        provider,
        seq,
        statements,
    }
}

fn route_from(doc: &LockDoc, docs: &HashMap<String, LockDoc>) -> Result<RouteSpec, Diag> {
    let route = find_section(&doc.sections, "Route")
        .ok_or_else(|| Diag::error(&doc.meta.id, "missing route"))?;
    let access = find_section(&doc.sections, "Access");
    let public = access.and_then(|s| s.get("public")) == Some("true");
    let mut auth_removed = false;
    let mut auth_mode = "jwt".to_string();
    let mut roles = Vec::new();
    let mut scopes = Vec::new();
    let mut roles_claim = String::new();
    let mut jwt_iss = String::new();
    let mut jwt_aud = String::new();
    let mut jwks_uri = String::new();
    let mut rates = Vec::new();
    let mut refresh = None;
    let mut revoke = None;
    if let Some(pipes) = find_section(&doc.sections, "Pipelines") {
        for block in &pipes.blocks {
            let name = pipeline_name(&block.heading);
            if name == "token-refresh" {
                refresh = Some(
                    parse_refresh_block(&block.bullets)
                        .map_err(|err| Diag::error(&doc.meta.id, err).at_section("Pipelines"))?,
                );
            }
            if name == "token-revoke" {
                revoke = Some(
                    parse_revoke_block(&block.bullets)
                        .map_err(|err| Diag::error(&doc.meta.id, err).at_section("Pipelines"))?,
                );
            }
            if name == "-auth"
                || (name == "auth"
                    && block
                        .bullets
                        .iter()
                        .any(|b| b.key == "removed" && b.value == "true"))
            {
                auth_removed = true;
            }
            if name == "auth" {
                if let Some(mode) = block.bullets.iter().find(|b| b.key == "mode") {
                    auth_mode = mode.value.clone();
                }
                if let Some(raw) = block.bullets.iter().find(|b| b.key == "roles") {
                    // A malformed roles list is a build error, never "no roles"
                    // (which would let any JWT reach role-gated handlers).
                    roles = crate::lock::parse_string_list(&raw.value).map_err(|err| {
                        Diag::error(&doc.meta.id, format!("invalid roles: {err}"))
                            .at_section("Pipelines")
                    })?;
                }
                if let Some(raw) = block.bullets.iter().find(|b| b.key == "scopes") {
                    // A malformed scopes list is a build error, never "no scopes".
                    scopes = crate::lock::parse_string_list(&raw.value).map_err(|err| {
                        Diag::error(&doc.meta.id, format!("invalid scopes: {err}"))
                            .at_section("Pipelines")
                    })?;
                }
                if let Some(raw) = block.bullets.iter().find(|b| b.key == "roles-claim") {
                    roles_claim = raw.value.trim().to_string();
                    if let Some(problem) = roles_claim_problem(&roles_claim) {
                        return Err(Diag::error(&doc.meta.id, problem).at_section("Pipelines"));
                    }
                }
                for (bullet, slot) in [
                    ("iss", &mut jwt_iss),
                    ("aud", &mut jwt_aud),
                    ("jwks-uri", &mut jwks_uri),
                ] {
                    if let Some(raw) = block.bullets.iter().find(|b| b.key == bullet) {
                        *slot = raw.value.trim().to_string();
                    }
                }
            }
            if name == "rate-limit" {
                check_rate_params(block.bullets.iter().map(|b| b.key.as_str()))
                    .map_err(|err| Diag::error(&doc.meta.id, err).at_section("Pipelines"))?;
                let per_minute = block
                    .bullets
                    .iter()
                    .find(|b| b.key == "per-minute")
                    .and_then(|b| b.value.parse().ok())
                    .unwrap_or(60);
                let key = block
                    .bullets
                    .iter()
                    .find(|b| b.key == "key")
                    .map(|b| b.value.clone())
                    .unwrap_or_else(|| "ip".into());
                rates.push(RateSpec { per_minute, key });
            }
            for (key_name, key) in [
                ("ip-per-minute", "ip"),
                ("account-per-minute", "account"),
                ("tenant-per-minute", "tenant"),
            ] {
                if let Some(value) = block.bullets.iter().find(|b| b.key == key_name) {
                    if let Ok(per_minute) = value.value.parse() {
                        rates.push(RateSpec {
                            per_minute,
                            key: key.into(),
                        });
                    }
                }
            }
        }
    }
    check_refresh_route(refresh.as_ref(), revoke.as_ref(), public, auth_removed)
        .map_err(|err| Diag::error(&doc.meta.id, err).at_section("Pipelines"))?;
    if let Some(problem) =
        jwt_override_problem(&jwt_iss, &jwt_aud, &jwks_uri, auth_removed, &auth_mode)
    {
        return Err(Diag::error(&doc.meta.id, problem).at_section("Pipelines"));
    }
    for rate in &rates {
        if rate.key == "family" && refresh.is_some() {
            continue;
        }
        check_rate_key(&rate.key, auth_removed, &auth_mode)
            .map_err(|err| Diag::error(&doc.meta.id, err).at_section("Pipelines"))?;
    }
    check_rate_set(&rates).map_err(|err| Diag::error(&doc.meta.id, err).at_section("Pipelines"))?;
    let body_limit = find_section(&doc.sections, "Limits")
        .and_then(|s| s.get("body"))
        .map(validate::parse_bytes)
        .unwrap_or(1_048_576);
    let transform = find_section(&doc.sections, "Transform");
    let transform_expr = |key: &str, default: &str| -> Result<String, Diag> {
        let expr = transform.and_then(|s| s.get(key)).unwrap_or(default);
        let expr = if expr.trim() == "(none)" {
            default
        } else {
            expr
        };
        crate::runtime::check_expr(expr).map_err(|err| {
            Diag::error(&doc.meta.id, format!("transform {key}: {err}")).at_section("Transform")
        })?;
        Ok(expr.to_string())
    };
    let transform_in = transform_expr("in", "{}")?;
    let transform_out = transform_expr("out", "result")?;
    let (challenge_type, challenge_when) =
        parse_challenge(access.and_then(|s| s.get("challenge")).unwrap_or(""));
    let min_response_ms = access
        .and_then(|s| s.get("min-response"))
        .and_then(crate::llm::parse_duration_ms)
        .unwrap_or(if public { 300 } else { 0 });
    let (deprecated, sunset) = lifecycle(doc, docs);
    Ok(RouteSpec {
        method: route.get("method").unwrap_or("GET").to_string(),
        path: route.get("path").unwrap_or("/").to_string(),
        feature_id: doc.meta.id.clone(),
        public,
        auth_removed,
        auth_mode,
        roles,
        scopes,
        roles_claim,
        jwt_iss,
        jwt_aud,
        jwks_uri,
        rates,
        body_limit,
        request: request_fields(find_section(&doc.sections, "Request")),
        transform_in,
        transform_out,
        steps: steps_from(find_section(&doc.sections, "Steps"), &doc.meta.id)?,
        errors: error_rows(find_section(&doc.sections, "Errors")),
        min_response_ms,
        public_writes: access.and_then(|s| s.get("public-writes")) == Some("true"),
        challenge_type,
        challenge_when,
        deprecated,
        sunset,
        refresh,
        revoke,
    })
}

fn lifecycle(doc: &LockDoc, docs: &HashMap<String, LockDoc>) -> (bool, String) {
    let mut parts = doc.meta.id.split('/');
    let definition = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let Some(ver) = docs.get(&format!("{definition}/{version}/_version")) else {
        return (false, "(none)".into());
    };
    let Some(section) = find_section(&ver.sections, "Lifecycle") else {
        return (false, "(none)".into());
    };
    (
        section.get("deprecated") == Some("true"),
        section.get("sunset").unwrap_or("(none)").to_string(),
    )
}

fn request_fields(section: Option<&crate::lock::Section>) -> Vec<ReqField> {
    let Some(section) = section else {
        return Vec::new();
    };
    let Some(table) = &section.table else {
        return Vec::new();
    };
    table
        .rows
        .iter()
        .filter_map(|row| {
            let raw = row.first()?.as_str();
            let (location, name) = raw.split_once('.')?;
            let rules = row.get(2).map(String::as_str).unwrap_or("");
            Some(ReqField {
                location: location.into(),
                name: name.into(),
                ty: row.get(1).cloned().unwrap_or_else(|| "string".into()),
                required: row.get(3).map(String::as_str) == Some("yes"),
                min: number_rule(rules, "min"),
                max: number_rule(rules, "max"),
                format: text_rule(rules, "format"),
            })
        })
        .collect()
}

fn error_rows(section: Option<&crate::lock::Section>) -> Vec<ErrMap> {
    let Some(section) = section else {
        return Vec::new();
    };
    let Some(table) = &section.table else {
        return Vec::new();
    };
    table
        .rows
        .iter()
        .filter_map(|row| {
            Some(ErrMap {
                status: row.first()?.parse().ok()?,
                code: row.get(1)?.clone(),
                from: row.get(2).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

fn steps_from(section: Option<&crate::lock::Section>, file: &str) -> Result<Vec<StepSpec>, Diag> {
    let Some(section) = section else {
        return Ok(Vec::new());
    };
    let mut steps = Vec::new();
    for block in &section.blocks {
        let get = |key: &str| {
            block
                .bullets
                .iter()
                .find(|b| b.key == key)
                .map(|b| b.value.clone())
                .unwrap_or_default()
        };
        let op = get("op");
        let expression_key = match op.as_str() {
            "method" if !get("args").is_empty() => Some("args"),
            "respond" if !get("value").is_empty() => Some("value"),
            _ => None,
        };
        if let Some(key) = expression_key {
            crate::runtime::check_expr(&get(key)).map_err(|err| {
                Diag::error(file, format!("{key}: {err}"))
                    .at_step(&block.heading)
                    .at_section("Steps")
            })?;
        }
        if let Some(problem) = validate::op_expression_problems(block, &op)
            .into_iter()
            .next()
        {
            return Err(Diag::error(file, problem)
                .at_step(&block.heading)
                .at_section("Steps"));
        }
        let id = block
            .heading
            .split_whitespace()
            .next()
            .unwrap_or("s")
            .to_string();
        let mut params = Vec::new();
        if let Some(raw) = block.bullets.iter().find(|b| b.key == "params") {
            params = crate::lock::parse_string_list(&raw.value).map_err(|err| {
                Diag::error(file, format!("invalid params: {err}")).at_section("Steps")
            })?;
        }
        let step_err = |err: String| {
            Diag::error(file, err)
                .at_step(&block.heading)
                .at_section("Steps")
        };
        let status = parse_status(&block.bullets)
            .map_err(step_err)?
            .unwrap_or(if op == "respond" { 200 } else { 0 });
        let writes = parse_writes(&block.bullets)
            .map_err(step_err)?
            .unwrap_or(false);
        steps.push(StepSpec {
            id,
            op: op.clone(),
            target: get("target"),
            args: get("args"),
            value: get("value"),
            status,
            sql: get("sql").trim_matches('`').trim().to_string(),
            params,
            out: get("out"),
            fail: get("fail"),
            when_expr: get("when"),
            writes,
            table: get("table"),
            opts: block
                .bullets
                .iter()
                .map(|b| (b.key.clone(), b.value.clone()))
                .collect(),
        });
    }
    Ok(steps)
}

pub(crate) fn parse_challenge(raw: &str) -> (String, String) {
    let map = crate::lock::parse_inline_map(raw).unwrap_or_default();
    let ty = map
        .iter()
        .find(|(k, _)| k == "type")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "none".into());
    let when = map
        .iter()
        .find(|(k, _)| k == "when")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "always".into());
    (ty, when)
}

fn number_rule(rules: &str, key: &str) -> Option<i64> {
    for part in rules.split(',') {
        let part = part.trim();
        // Skip other rules; `?` here used to drop every rule after the first.
        let Some(rest) = part.strip_prefix(&format!("{key}:")) else {
            continue;
        };
        if let Ok(value) = rest.trim().parse() {
            return Some(value);
        }
    }
    None
}

fn text_rule(rules: &str, key: &str) -> String {
    for part in rules.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&format!("{key}:")) {
            return rest.trim().to_string();
        }
    }
    String::new()
}

fn bracket_list(options: &str, key: &str) -> Vec<String> {
    let Some(idx) = options.find(&format!("{key}:")) else {
        return Vec::new();
    };
    let rest = &options[idx + key.len() + 1..];
    let rest = rest.trim();
    if let Some(start) = rest.find('[') {
        if let Some(end) = rest[start..].find(']') {
            return crate::lock::parse_string_list(&rest[start..start + end + 1])
                .unwrap_or_default();
        }
    }
    Vec::new()
}

fn single(message: String) -> Report {
    let mut report = Report::new();
    report.push(Diag::error("manifest", message));
    report
}

pub fn model<'a>(manifest: &'a Manifest, id: &str) -> Option<&'a ModelSpec> {
    manifest.models.iter().find(|model| model.id == id)
}

pub fn method<'a>(manifest: &'a Manifest, target: &str) -> Option<&'a MethodSpec> {
    manifest
        .methods
        .iter()
        .find(|method| method.target == target)
}

pub fn json_null() -> Value {
    Value::Null
}

#[cfg(test)]
mod rule_tests {
    use super::number_rule;

    #[test]
    fn number_rules_are_found_in_any_position() {
        assert_eq!(number_rule("min: 1, max: 99", "max"), Some(99));
        assert_eq!(number_rule("min: 1, max: 99", "min"), Some(1));
        assert_eq!(number_rule("format: email, max: 120", "max"), Some(120));
        assert_eq!(number_rule("max: -5", "max"), Some(-5));
        assert_eq!(number_rule("-", "max"), None);
    }
}

/// JWT claims that carry identity or token metadata; a roles claim must never
/// read them, or a token's own `aud`/`scope`/`sub` would become roles.
pub const RESERVED_JWT_CLAIMS: &[&str] = &[
    "iss",
    "sub",
    "aud",
    "exp",
    "nbf",
    "iat",
    "jti",
    "tenant_id",
    "scope",
    "scp",
    "azp",
    "kid",
    "alg",
    "typ",
    "cnf",
    "nonce",
    "auth_time",
    "acr",
    "amr",
    "sid",
];

/// Why `name` cannot be a roles claim, if it cannot. Dotted paths
/// (`realm_access.roles`) are allowed and resolved as nested JSON; reserved
/// claim names (and any path segment that is reserved) are refused.
pub fn roles_claim_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("roles-claim must not be empty".into());
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some(format!("roles-claim `{name}` must not contain whitespace"));
    }
    // URL-style claims (Auth0) are matched as a single key, not a path.
    if name.contains("://") {
        if RESERVED_JWT_CLAIMS.contains(&name.to_ascii_lowercase().as_str()) {
            return Some(format!("roles-claim `{name}` is a reserved JWT claim"));
        }
        return None;
    }
    for segment in name.split('.') {
        if segment.is_empty() {
            return Some(format!("roles-claim `{name}` has an empty path segment"));
        }
        if RESERVED_JWT_CLAIMS.contains(&segment.to_ascii_lowercase().as_str()) {
            return Some(format!(
                "roles-claim `{name}` must not use reserved claim `{segment}`"
            ));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Some(format!(
                "roles-claim `{name}` path segments must be alphanumeric/_/-"
            ));
        }
    }
    None
}

/// Why a route's `iss` / `aud` / `jwks-uri` override cannot be used, if it
/// cannot. Issuers and JWKS URIs must be https (an http issuer would let a
/// network attacker serve keys); overrides only make sense on JWT routes.
pub fn jwt_override_problem(
    iss: &str,
    aud: &str,
    jwks_uri: &str,
    auth_removed: bool,
    auth_mode: &str,
) -> Option<String> {
    if iss.is_empty() && aud.is_empty() && jwks_uri.is_empty() {
        return None;
    }
    if auth_removed || auth_mode != "jwt" {
        return Some("iss / aud / jwks-uri apply only to JWT auth routes".into());
    }
    for (name, value) in [("iss", iss), ("jwks-uri", jwks_uri)] {
        if value.is_empty() {
            continue;
        }
        match url::Url::parse(value) {
            Ok(url) if url.scheme() == "https" && url.host_str().is_some() => {
                if !url.username().is_empty() || url.password().is_some() {
                    return Some(format!("{name} must not carry credentials"));
                }
                if crate::runtime::jwks::non_public_host(url.host_str().unwrap_or("")) {
                    // Locks ship to production: no private/loopback/link-local
                    // issuers (the derived {iss}/.well-known/jwks.json too).
                    return Some(format!(
                        "{name} host must be public (not loopback, private or link-local)"
                    ));
                }
                if name == "jwks-uri" && url.fragment().is_some() {
                    return Some("jwks-uri must not have a fragment".into());
                }
            }
            _ => return Some(format!("{name} `{value}` must be an absolute https URL")),
        }
    }
    if !aud.is_empty()
        && (aud.len() > 256
            || aud
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ',' || c == '['))
    {
        return Some(format!("aud `{aud}` must be a single audience value"));
    }
    if !jwks_uri.is_empty() && iss.is_empty() {
        // Keys from another IdP checked against the default issuer would mix
        // trust domains; pin the issuer that goes with the keys.
        return Some("jwks-uri requires iss on the same route".into());
    }
    None
}

#[cfg(test)]
mod jwt_override_tests {
    use super::jwt_override_problem;

    #[test]
    fn overrides_are_validated() {
        fn ok(i: &str, a: &str, j: &str) -> bool {
            jwt_override_problem(i, a, j, false, "jwt").is_none()
        }
        assert!(ok("", "", ""));
        assert!(ok("https://idp.example.com/t1", "shop", ""));
        assert!(ok(
            "https://idp.example.com/t1",
            "",
            "https://idp.example.com/t1/keys"
        ));
        assert!(ok("", "other-aud", ""));
        assert!(!ok("http://idp.example.com", "", ""));
        assert!(!ok(
            "https://idp.example.com",
            "",
            "http://idp.example.com/k"
        ));
        assert!(
            !ok("", "", "https://idp.example.com/k"),
            "jwks-uri needs iss"
        );
        assert!(!ok("https://u:p@idp.example.com", "", ""));
        assert!(!ok("idp.example.com", "", ""));
        assert!(!ok("", "a b", ""));
        assert!(!ok("", "a,b", ""));
        assert!(!ok("", "[a,b]", ""));
        assert!(!ok(
            "https://idp.example.com",
            "",
            "https://u:p@idp.example.com/k"
        ));
        for host in [
            "https://127.0.0.1",
            "https://localhost",
            "https://169.254.169.254",
            "https://10.1.2.3",
            "https://[::1]",
        ] {
            assert!(!ok(host, "", ""), "iss {host}");
            assert!(
                !ok("https://idp.example.com", "", &format!("{host}/k")),
                "jwks {host}"
            );
        }
        assert!(jwt_override_problem("https://i.example.com", "", "", true, "jwt").is_some());
        assert!(jwt_override_problem("https://i.example.com", "", "", false, "api-key").is_some());
    }
}

#[cfg(test)]
mod roles_claim_tests {
    use super::roles_claim_problem;

    #[test]
    fn reserved_and_dotted_claims_are_refused() {
        for bad in [
            "aud",
            "tenant_id",
            "scope",
            "ISS",
            "sub",
            "exp",
            "roles.aud",
            "",
        ] {
            assert!(roles_claim_problem(bad).is_some(), "{bad}");
        }
        for ok in [
            "roles",
            "groups",
            "https://example.com/roles",
            "realm_access.roles",
        ] {
            assert!(roles_claim_problem(ok).is_none(), "{ok}");
        }
    }
}

#[cfg(test)]
mod roles_list_tests {
    #[test]
    fn malformed_roles_list_is_a_build_error() {
        let err = crate::lock::parse_string_list("staff").unwrap_err();
        assert!(err.contains("expected a list"), "{err}");
        // Same path route_from uses: error, never empty vec.
        assert!(crate::lock::parse_string_list("staff")
            .map_err(|e| format!("invalid roles: {e}"))
            .is_err());
        assert_eq!(
            crate::lock::parse_string_list("[staff]").unwrap(),
            vec!["staff"]
        );
    }
}

#[cfg(test)]
mod malformed_list_tests {
    use super::{method_from, route_from, steps_from, MethodSpec};
    use crate::diag::Level;
    use crate::lock::{parse_sections, Kind, LockDoc, Meta, Scope};
    use std::collections::HashMap;

    fn method_doc(body: &str) -> LockDoc {
        LockDoc {
            meta: Meta::pending(
                Kind::Method,
                None,
                "shop/methods/bad",
                "shop/methods/bad.md",
            ),
            sections: parse_sections(body).expect("sections"),
        }
    }

    #[test]
    fn malformed_signature_errors_is_a_build_error() {
        let doc = method_doc("## Signature\n\n- errors: NOT_FOUND\n- capabilities: []\n");
        let err = method_from(&doc).unwrap_err();
        assert_eq!(err.level, Level::Error);
        assert!(err.message.contains("invalid errors"), "{err:?}");
    }

    #[test]
    fn malformed_signature_capabilities_is_a_build_error() {
        let doc = method_doc("## Signature\n\n- errors: []\n- capabilities: sign-jwt\n");
        let err = method_from(&doc).unwrap_err();
        assert!(err.message.contains("invalid capabilities"), "{err:?}");
    }

    #[test]
    fn malformed_step_params_is_a_build_error() {
        let doc = method_doc(
            "## Signature\n\n- errors: []\n\n## Steps\n\n### s1\n\n- op: respond\n- params: a, b\n- status: 200\n",
        );
        let err = method_from(&doc).unwrap_err();
        assert!(err.message.contains("invalid params"), "{err:?}");
    }

    #[test]
    fn writes_must_be_exactly_true_or_false() {
        for bad in ["TRUE", "yes", "1", "", "true, false"] {
            let doc = method_doc(&format!("## Signature\n\n- writes: {bad}\n"));
            let err = method_from(&doc).unwrap_err();
            assert!(err.message.contains("invalid writes"), "{bad:?}: {err:?}");
        }
        let doc = method_doc("## Signature\n\n- writes: false\n- writes: true\n");
        assert!(method_from(&doc)
            .unwrap_err()
            .message
            .contains("duplicate `writes`"));
        let doc = method_doc(
            "## Signature\n\n- writes: false\n\n## Steps\n\n### s1\n\n- op: query\n- writes: True\n",
        );
        assert!(method_from(&doc)
            .unwrap_err()
            .message
            .contains("invalid writes"));
        let doc = method_doc("## Steps\n\n### s1\n\n- op: query\n- writes: true\n- writes: true\n");
        assert!(method_from(&doc)
            .unwrap_err()
            .message
            .contains("duplicate `writes`"));
    }

    #[test]
    fn status_must_be_an_http_status() {
        for bad in ["abc", "", "99", "600", "+200", "200.0", "0200", "2 00"] {
            let doc = method_doc(&format!(
                "## Steps\n\n### s1\n\n- op: respond\n- status: {bad}\n"
            ));
            let err = method_from(&doc).unwrap_err();
            assert!(err.message.contains("invalid status"), "{bad:?}: {err:?}");
        }
        let doc = method_doc("## Steps\n\n### s1\n\n- op: respond\n- status: 201\n- status: 200\n");
        assert!(method_from(&doc)
            .unwrap_err()
            .message
            .contains("duplicate `status`"));
        let doc = method_doc("## Steps\n\n### s1\n\n- op: respond\n");
        assert_eq!(method_from(&doc).unwrap().steps[0].status, 200);
    }

    #[test]
    fn valid_method_round_trip() {
        let doc = method_doc(
            "## Signature\n\n- writes: true\n- errors: [NOT_FOUND, TAKEN]\n- capabilities: [sign-token]\n\n## Steps\n\n### s1 query\n\n- op: query\n- sql: `INSERT INTO t (a, b) VALUES ($1, $2)`\n- params: [input.a, input.b]\n- writes: true\n\n### s2 respond\n\n- op: respond\n- status: 201\n",
        );
        let method = method_from(&doc).unwrap();
        assert!(method.writes);
        assert_eq!(method.errors, vec!["NOT_FOUND", "TAKEN"]);
        assert_eq!(method.capabilities, vec!["sign-token"]);
        assert_eq!(method.steps[0].params, vec!["input.a", "input.b"]);
        assert!(method.steps[0].writes);
        assert_eq!(method.steps[1].status, 201);
        let json = serde_json::to_string(&method).unwrap();
        let back: MethodSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn malformed_params_in_an_api_route_is_a_build_error() {
        let doc = LockDoc {
            meta: Meta::pending(Kind::Api, Some(Scope::Feature), "shop/api/x", "shop/api/x.md"),
            sections: parse_sections(
                "## Route\n\n- method: GET\n- path: /x\n\n## Steps\n\n### s1\n\n- op: respond\n- params: a, b\n",
            )
            .unwrap(),
        };
        let err = route_from(&doc, &HashMap::new()).unwrap_err();
        assert!(err.message.contains("invalid params"), "{err:?}");
    }

    #[test]
    fn absent_lists_stay_empty() {
        let doc = LockDoc {
            meta: Meta::pending(Kind::Method, None, "shop/methods/ok", "shop/methods/ok.md"),
            sections: parse_sections(
                "## Signature\n\n- writes: false\n\n## Steps\n\n### s1\n\n- op: respond\n- status: 200\n",
            )
            .unwrap(),
        };
        let method = method_from(&doc).unwrap();
        assert!(method.errors.is_empty());
        assert!(method.capabilities.is_empty());
        assert!(method.steps[0].params.is_empty());
        assert!(steps_from(None, "x").unwrap().is_empty());
    }
}

#[cfg(test)]
mod rate_key_tests {
    use super::check_rate_key;

    #[test]
    fn accepts_spec_keys_and_rejects_others() {
        for ok in ["ip", "tenant", "user", "input.email", "account"] {
            assert!(check_rate_key(ok, false, "jwt").is_ok(), "{ok}");
        }
        assert!(check_rate_key("api-key", false, "api-key").is_ok());
        assert!(check_rate_key("api-key", false, "jwt").is_err());
        assert!(check_rate_key("user", true, "jwt").is_err());
        let family = check_rate_key("family", false, "jwt").unwrap_err();
        assert!(
            family.contains("only allowed on token-refresh routes"),
            "{family}"
        );
        for bad in ["per-hour", "per-day", "burst"] {
            assert!(super::check_rate_params(["key", bad]).is_err(), "{bad}");
        }
        assert!(super::check_rate_params(["key", "per-minute"]).is_ok());
        let r = |key: &str, per_minute| super::RateSpec {
            per_minute,
            key: key.into(),
        };
        assert!(super::check_rate_set(&[r("input.email", 5)]).is_err());
        assert!(super::check_rate_set(&[r("account", 5)]).is_err());
        assert!(super::check_rate_set(&[r("input.email", 5), r("ip", 10)]).is_ok());
        assert!(super::check_rate_set(&[r("ip", 0)]).is_err());
        assert!(super::check_rate_set(&[r("ip", 10_001)]).is_err());
        assert!(super::check_rate_set(&[r("tenant", 10_000)]).is_ok());
        for bad in ["family", "req.ip", "input.", "input.a-b", "everyone"] {
            assert!(check_rate_key(bad, false, "jwt").is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn token_refresh_accepts_compiler_source_lines() {
        let bullets = vec![
            crate::lock::Bullet {
                key: "sign-method".into(),
                value: "user.sign-token".into(),
            },
            crate::lock::Bullet {
                key: "source-lines".into(),
                value: "12-18".into(),
            },
        ];
        let spec = parse_refresh_block(&bullets).expect("source-lines is metadata");
        assert_eq!(spec.sign_method, "user.sign-token");
    }

    #[test]
    fn token_refresh_sign_method_typo_is_a_build_error() {
        let method = |steps: Vec<StepSpec>| MethodSpec {
            target: "user.sign-token".into(),
            writes: false,
            input: Vec::new(),
            errors: Vec::new(),
            capabilities: vec!["sign-token".into()],
            steps,
            crud_model: String::new(),
            crud_action: String::new(),
        };
        let sign: StepSpec = serde_json::from_value(serde_json::json!({
            "id": "s", "op": "sign-jwt", "target": "", "args": "", "value": "", "status": 0,
            "sql": "", "params": [], "out": "", "fail": "", "when_expr": "", "writes": false,
            "table": "", "opts": {}
        }))
        .unwrap();
        let methods = vec![method(vec![sign])];
        assert_eq!(
            refresh_sign_method_problem("user.sign-token", &methods),
            None
        );
        assert!(refresh_sign_method_problem("user.sign-tokn", &methods)
            .unwrap()
            .contains("not a known method"));
        assert!(
            refresh_sign_method_problem("user.sign-token", &[method(Vec::new())])
                .unwrap()
                .contains("no sign-jwt step")
        );
    }
}

#[cfg(test)]
mod constant_response_tests {
    use super::*;
    use crate::runtime::{router, AppState, AuthConfig, MemoryExec, Slug};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[test]
    fn lowercase_body_limit_builds_in_bytes() {
        let doc = LockDoc {
            meta: crate::lock::Meta::pending(
                Kind::Api, Some(Scope::Feature), "hello", "hello.md",
            ),
            sections: crate::lock::parse_sections(
                "## Route\n- method: GET\n- path: /hello\n## Access\n- public: true\n## Pipelines\n### -auth\n## Limits\n- body: 64kb\n",
            ).unwrap(),
        };
        assert_eq!(
            route_from(&doc, &HashMap::new()).unwrap().body_limit,
            65_536
        );
    }

    #[test]
    fn malformed_transform_bullets_cannot_build() {
        for key in ["in", "out"] {
            for expr in ["", "{a: }"] {
                let doc = LockDoc {
                    meta: crate::lock::Meta::pending(Kind::Api, Some(Scope::Feature), "hello", "hello.md"),
                    sections: crate::lock::parse_sections(&format!("## Route\n- method: GET\n- path: /hello\n## Access\n- public: true\n## Pipelines\n### -auth\n## Transform\n- {key}: {expr}\n")).unwrap(),
                };
                let err = route_from(&doc, &HashMap::new()).unwrap_err();
                assert!(
                    err.message.contains(&format!("transform {key}:")),
                    "{err:?}"
                );
                if expr.is_empty() {
                    assert!(err.message.contains("empty expression"));
                }
            }
        }
    }

    #[tokio::test]
    async fn public_constant_response_without_transform() {
        for transform in [
            "",
            "## Transform
",
            "## Transform

(none)
",
            "## Transform\n- in: (none)\n- out: (none)\n",
        ] {
            for (value, expected, status) in [
                (
                    r#"{"message": "hello world"}"#,
                    r#"{"message":"hello world"}"#,
                    StatusCode::OK,
                ),
                ("", "null", StatusCode::OK),
                ("transform.out", "null", StatusCode::OK),
                ("input", "{}", StatusCode::OK),
                ("{message: }", "", StatusCode::INTERNAL_SERVER_ERROR),
            ] {
                let body = format!(
                    "## Route
- method: GET
- path: /hello/v1/{{slug}}/greet

## Access
- public: true

## Pipelines
### -auth

{transform}
## Steps
### reply
- op: respond
- value: {value}
- status: 200
"
                );
                let doc = LockDoc {
                    meta: crate::lock::Meta::pending(
                        Kind::Api,
                        Some(Scope::Feature),
                        "api/hello/v1/greet/greet",
                        "hello.md",
                    ),
                    sections: crate::lock::parse_sections(&body).unwrap(),
                };
                let mut build_doc = doc.clone();
                if status == StatusCode::INTERNAL_SERVER_ERROR {
                    assert!(route_from(&doc, &HashMap::new()).is_err());
                    build_doc
                        .sections
                        .iter_mut()
                        .find(|s| s.name == "Steps")
                        .unwrap()
                        .blocks[0]
                        .bullets
                        .iter_mut()
                        .find(|b| b.key == "value")
                        .unwrap()
                        .value = "transform.out".into();
                }
                let mut route = route_from(&build_doc, &HashMap::new()).unwrap();
                if status == StatusCode::INTERNAL_SERVER_ERROR {
                    route.transform_out = "{message: }".into();
                }
                assert_eq!(route.transform_in, "{}");
                let manifest = Manifest {
                    provenance: Default::default(),
                    build_mode: None,
                    manifest_version: MANIFEST_VERSION,
                    routes: vec![route],
                    models: vec![],
                    methods: vec![],
                    migrations: vec![],
                    signature: String::new(),
                };
                let state = Arc::new(AppState {
                    manifest,
                    exec: Arc::new(MemoryExec::new()),
                    auth: AuthConfig::anonymous(),
                    dev_slugs: None,
                    slugs: HashMap::from([(
                        "testslug".into(),
                        Slug {
                            tenant_id: "tenant0001".into(),
                            public_access: true,
                            public_write: false,
                        },
                    )]),
                    rates: Mutex::new(Default::default()),
                    quotas: Mutex::new(Default::default()),
                    disabled: Default::default(),
                    min_override_ms: Some(0),
                    honor_test_ip: true,
                    trusted_proxies: vec![],
                    dev_hints: true,
                    challenge: None,
                });
                let response = router(state)
                    .oneshot(
                        Request::builder()
                            .uri("/hello/v1/testslug/greet")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), status, "{transform:?} {value}");
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                if status == StatusCode::OK {
                    assert_eq!(body.as_ref(), expected.as_bytes());
                } else {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body.to_string().contains("INTERNAL"));
                }
            }
        }
    }
}

/// Errors declared by the generated model CRUD methods.
pub const CRUD_ERROR_CODES: &[&str] = &["NOT_FOUND", "UNIQUE_VIOLATION", "PRECONDITION_FAILED"];
