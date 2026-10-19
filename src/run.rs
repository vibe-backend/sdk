use crate::{
    manifest,
    runtime::{router, AppState, AuthConfig, PgExec, QueryExec},
    runtime_env, sign,
};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
/// Explicit deployer pin values; empty environment values count as unset.
#[derive(Clone, Debug, Default)]
pub struct PinArgs {
    pub identity: Option<String>,
    pub issuer: Option<String>,
    pub repository_id: Option<String>,
}
impl PinArgs {
    pub fn resolve(&self, lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let merge =
            |flag: &Option<String>, key: &str, name: &str| -> Result<Option<String>, String> {
                let env = lookup(key).filter(|v| !v.is_empty());
                if let (Some(a), Some(b)) = (flag, &env) {
                    if a != b {
                        return Err(format!("--{name} and {key} disagree; set only one"));
                    }
                }
                Ok(flag.clone().or(env))
            };
        let out = Self {
            identity: merge(&self.identity, "VIBE_EXPECT_IDENTITY", "expect-identity")?,
            issuer: merge(&self.issuer, "VIBE_EXPECT_ISSUER", "expect-issuer")?,
            repository_id: merge(
                &self.repository_id,
                "VIBE_EXPECT_REPOSITORY_ID",
                "expect-repository-id",
            )?,
        };
        crate::sigstore::Pin::parse(
            out.identity.as_deref().unwrap_or(
                "https://github.com/example/project/.github/workflows/release.yml@refs/tags/v*",
            ),
            out.issuer.as_deref().unwrap_or(crate::sigstore::ISSUER),
            out.repository_id.as_deref(),
        )?;
        Ok(out)
    }
    fn any(&self) -> bool {
        self.identity.is_some() || self.issuer.is_some() || self.repository_id.is_some()
    }
}
const RELEASE_TEST_BUILD: &str =
    "release manifest is marked test_build; release manifests can never be test builds";
/// Loads and verifies the manifest. Release manifests in every environment
/// require deployer caller pins and the compiled-in SDK signer (decision 182).
/// The bundle (`VIBE_MANIFEST_BUNDLE`, default
/// `<manifest>.sigstore.json`) is verified offline against the compiled-in
/// trust root for the deployed version `VIBE_RELEASE_VERSION`. The
/// development key in `.vibe/signing.pub` is accepted in development only.
pub fn load_manifest(
    manifest_path: &Path,
    project: &Path,
    run_env: crate::runtime::RunEnv,
    pins: &PinArgs,
) -> Result<manifest::Manifest, String> {
    load_manifest_with_signer(manifest_path, project, run_env, pins, None)
}

fn load_manifest_with_signer(
    manifest_path: &Path,
    project: &Path,
    run_env: crate::runtime::RunEnv,
    pins: &PinArgs,
    signer: Option<&crate::sigstore::Signer>,
) -> Result<manifest::Manifest, String> {
    let mode: manifest::Manifest =
        serde_json::from_slice(&fs::read(manifest_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if mode.build_mode != Some(manifest::BuildMode::Release) && run_env.is_dev() {
        if pins.any() {
            eprintln!("note: --expect-identity/--expect-issuer pins are ignored for development manifests");
        }
        eprintln!("warning: verifying the manifest with the development key in .vibe/signing.pub");
        let artifact_dir = manifest_path.parent().unwrap_or(Path::new("."));
        let key_dir = if project.join(".vibe/signing.pub").exists() {
            project
        } else if artifact_dir.join(".vibe/signing.pub").exists() {
            artifact_dir
        } else {
            project
        };
        let key = sign::load_dev_public(key_dir)?;
        return manifest::verify_manifest(mode, &key).map_err(|err| err.to_string());
    }
    if mode.build_mode == Some(manifest::BuildMode::Dev) {
        return Err(format!(
            "this development manifest (`vibe build`) is refused when VIBE_ENV is {:?}",
            std::env::var("VIBE_ENV").unwrap_or_default()
        ));
    }
    // Claims are diagnostic only: even malformed claims cannot change the result.
    if let (Some(claim), Some(identity), Some(issuer), Ok(version)) = (
        &mode.provenance.signing,
        &pins.identity,
        &pins.issuer,
        std::env::var("VIBE_RELEASE_VERSION"),
    ) {
        if let Ok(pin) =
            crate::sigstore::Pin::parse(identity, issuer, pins.repository_id.as_deref())
        {
            for hint in claim_hints(claim, &pin, &version).unwrap_or_default() {
                eprintln!("{hint}");
            }
        }
    }
    if mode.provenance.test_build {
        return Err(RELEASE_TEST_BUILD.into());
    }
    let mut missing = Vec::new();
    if pins.identity.is_none() {
        missing.push("--expect-identity / VIBE_EXPECT_IDENTITY");
    }
    if pins.issuer.is_none() {
        missing.push("--expect-issuer / VIBE_EXPECT_ISSUER");
    }
    if !missing.is_empty() {
        return Err(format!("release manifest needs a pinned signer: set --expect-identity / VIBE_EXPECT_IDENTITY (e.g. https://github.com/<owner>/<repo>/.github/workflows/release.yml@refs/tags/v*) and --expect-issuer / VIBE_EXPECT_ISSUER (https://token.actions.githubusercontent.com). Matching is exact: the only substitution is v* -> v<VIBE_RELEASE_VERSION>; no globs or regex. Missing: {}.", missing.join(", ")));
    }
    let identity = pins.identity.as_deref().unwrap();
    let issuer = pins.issuer.as_deref().unwrap();
    let pin = crate::sigstore::Pin::parse(identity, issuer, pins.repository_id.as_deref())?;
    let version = std::env::var("VIBE_RELEASE_VERSION").ok().filter(|v| !v.is_empty()).ok_or("VIBE_RELEASE_VERSION is required to verify a release manifest; .vibe/signing.pub is not trusted for release manifests")?;
    let compiled;
    let signer = match signer {
        Some(signer) => signer,
        None => {
            compiled = crate::sigstore::Signer::compiled()?;
            &compiled
        }
    };
    let id = pins
        .repository_id
        .as_ref()
        .map(|id| format!(", repository id {id}"))
        .unwrap_or_default();
    eprintln!(
        "verifying release manifest: identity {identity}, issuer {issuer}{id}, version {version}"
    );
    if manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".vibe/signing.pub")
        .exists()
    {
        eprintln!("warning: .vibe/signing.pub is ignored for release manifests");
    }
    let bundle = std::env::var_os("VIBE_MANIFEST_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut name = manifest_path.as_os_str().to_owned();
            name.push(".sigstore.json");
            PathBuf::from(name)
        });
    manifest::read_release_manifest(manifest_path, &bundle, &version, &pin, signer)
        .map_err(|err| err.to_string())
}

pub fn claim_hints(
    claim: &manifest::SigningClaim,
    pin: &crate::sigstore::Pin,
    version: &str,
) -> Result<Vec<String>, String> {
    let expected = pin.identity_for(version)?;
    let mut hints = Vec::new();
    if claim.source_identity != expected {
        hints.push(format!(
            "note: manifest claims source identity {}; pinned identity is {expected}",
            claim.source_identity
        ));
    }
    if claim.issuer != crate::sigstore::ISSUER {
        hints.push(format!(
            "note: manifest claims issuer {}; pinned issuer is {}",
            claim.issuer,
            crate::sigstore::ISSUER
        ));
    }
    Ok(hints)
}

pub struct VerifyOptions {
    pub artifact: PathBuf,
    pub project: PathBuf,
    pub pins: PinArgs,
    pub allow_test_build: bool,
}
pub struct VerifiedArtifact {
    pub manifest: manifest::Manifest,
    pub path: PathBuf,
    pub identity: Option<String>,
}
/// Verify without serving or connecting to a database. Unset VIBE_ENV is production.
pub fn verify_artifact(opts: &VerifyOptions) -> Result<VerifiedArtifact, String> {
    verify_artifact_with_signer(opts, None)
}
/// Library injection for offline certificate tests; the CLI always uses the compiled signer.
pub fn verify_artifact_with_signer(
    opts: &VerifyOptions,
    signer: Option<&crate::sigstore::Signer>,
) -> Result<VerifiedArtifact, String> {
    load_dotenv(&opts.project);
    let run_env =
        crate::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), false);
    verify_artifact_in_env(opts, run_env, signer)
}
fn verify_artifact_in_env(
    opts: &VerifyOptions,
    run_env: crate::runtime::RunEnv,
    signer: Option<&crate::sigstore::Signer>,
) -> Result<VerifiedArtifact, String> {
    let pins = opts.pins.resolve(|key| std::env::var(key).ok())?;
    check_runtime_llm(run_env)?;
    let path = artifact_paths(&opts.artifact).0;
    fs::metadata(&path).map_err(|err| format!("cannot read manifest {}: {err}", path.display()))?;
    let manifest = load_manifest_with_signer(&path, &opts.project, run_env, &pins, signer)?;
    check_verified_manifest(&manifest, run_env, opts.allow_test_build)?;
    let identity = if manifest.build_mode == Some(manifest::BuildMode::Release) {
        Some(
            crate::sigstore::Pin::parse(
                pins.identity.as_deref().unwrap(),
                pins.issuer.as_deref().unwrap(),
                pins.repository_id.as_deref(),
            )?
            .identity_for(&manifest.provenance.version)?,
        )
    } else {
        None
    };
    Ok(VerifiedArtifact {
        manifest,
        path,
        identity,
    })
}
fn check_verified_manifest(
    manifest: &manifest::Manifest,
    run_env: crate::runtime::RunEnv,
    allow_test_build: bool,
) -> Result<(), String> {
    if manifest.provenance.test_build {
        if manifest.build_mode == Some(manifest::BuildMode::Release) {
            return Err(RELEASE_TEST_BUILD.into());
        }
        if !allow_test_build || !run_env.is_dev() {
            return Err("this manifest is a test build made with LLM_PROVIDER=fake; refusing to run it. Pass --allow-test-build to run it anyway (never in production)".into());
        }
        eprintln!("warning: running a test build made with LLM_PROVIDER=fake");
    }
    check_deployable(manifest, run_env)?;
    for route in &manifest.routes {
        if let Some(problem) = route
            .refresh
            .as_ref()
            .and_then(|spec| crate::manifest::refresh_spec_problem(&route.feature_id, spec))
        {
            return Err(problem);
        }
    }
    Ok(())
}

fn check_runtime_llm(run_env: crate::runtime::RunEnv) -> Result<(), String> {
    if let Some(name) = runtime_env::runtime_llm_env_present() {
        if !run_env.is_dev() {
            return Err(format!("runtime environment must not contain {name}"));
        }
        let names = runtime_env::strip_runtime_llm_env();
        eprintln!(
            "warning: removing compile-time variables from runtime environment: {}",
            names
                .iter()
                .map(|key| key.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

pub fn check_deployable(
    manifest: &manifest::Manifest,
    run_env: crate::runtime::RunEnv,
) -> Result<(), String> {
    let provenance = &manifest.provenance;
    tracing::info!(
        build_mode = ?manifest.build_mode,
        commit = %provenance.commit,
        locks = provenance.locks.len(),
        high_risk = provenance.high_risk().len(),
        "manifest provenance"
    );
    manifest::check_deployable(manifest, run_env.is_dev())
}

#[derive(Clone, Debug)]
pub enum RunSource {
    Project,
    Artifact(PathBuf),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbChoice {
    Memory,
    Postgres,
}
impl DbChoice {
    pub fn from_arg_or_env(arg: Option<&str>) -> Result<Self, String> {
        let value = arg
            .map(str::to_owned)
            .or_else(|| std::env::var("VIBE_DB").ok())
            .unwrap_or_else(|| "memory".into());
        match value.as_str() {
            "memory" => Ok(Self::Memory),
            "postgres" => Ok(Self::Postgres),
            _ => Err("VIBE_DB must be memory or postgres".into()),
        }
    }
}

pub struct RunOptions {
    pub pins: PinArgs,
    pub project: PathBuf,
    pub source: RunSource,
    pub addr: String,
    pub db: DbChoice,
    pub allow_test_build: bool,
    pub allow_destructive_migrations: bool,
}
pub struct Prepared {
    pub app: axum::Router,
    pub addr: SocketAddr,
}
pub fn artifact_paths(path: &Path) -> (PathBuf, PathBuf) {
    let manifest = if path.is_dir() {
        path.join("manifest.json")
    } else {
        path.to_owned()
    };
    let bundle = std::env::var_os("VIBE_MANIFEST_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut name = manifest.as_os_str().to_owned();
            name.push(".sigstore.json");
            PathBuf::from(name)
        });
    (manifest, bundle)
}
/// Validates and prepares a router and listener address without binding a socket.
pub async fn prepare(opts: &RunOptions) -> Result<Prepared, String> {
    #[cfg(not(feature = "sdk"))]
    if matches!(opts.source, RunSource::Project) {
        return Err("project mode needs vibe-sdk (`vibe run`); vibe-runtime runs build artifacts: pass --artifact".into());
    }
    let project = opts.project.as_path();
    let addr = opts.addr.as_str();
    let memory = opts.db == DbChoice::Memory;
    if memory && opts.allow_destructive_migrations {
        eprintln!("warning: --allow-destructive-migrations has no effect with --db memory");
    }
    load_dotenv(project);
    let pins = opts.pins.resolve(|key| std::env::var(key).ok())?;
    let run_env =
        crate::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), memory);
    check_runtime_llm(run_env)?;
    if memory && !run_env.is_dev() {
        return Err(format!(
            "--db memory is a development executor and is refused when VIBE_ENV is {:?}; use PostgreSQL",
            std::env::var("VIBE_ENV").unwrap_or_default()
        ));
    }
    // #199: refuse locks whose challenge is not verified at runtime, before
    // loading the database or the manifest (locks in the project tree).
    let unverified_locks = if matches!(opts.source, RunSource::Project) {
        crate::runtime::challenge::unverified_challenge_locks(project)
    } else {
        Vec::new()
    };
    crate::runtime::challenge::unverified_gate(run_env, &unverified_locks)?;
    // #157: production always uses the PostgreSQL replay store and
    // refuses the memory switch; dev/CI must choose explicitly. Decided
    // before loading anything.
    let replay_choice = crate::runtime::replay::resolve_replay_store(
        run_env,
        crate::runtime::challenge::configured_in_env()?,
        crate::runtime::replay::replay_kind_from_vars(|name| std::env::var(name).ok())?,
        !memory,
    )?;
    let mut auth = auth_from_env(run_env)?;
    let manifest = match &opts.source {
        RunSource::Artifact(path) => {
            verify_artifact_in_env(&VerifyOptions { artifact: path.clone(), project: project.into(), pins: opts.pins.clone(), allow_test_build: opts.allow_test_build }, run_env, None)?.manifest
        },
        #[cfg(not(feature = "sdk"))]
        RunSource::Project => return Err("project mode needs vibe-sdk (`vibe run`); vibe-runtime runs build artifacts: pass --artifact".into()),
        #[cfg(feature = "sdk")]
        RunSource::Project => {
            let validate = || -> Result<_, String> {
                let loaded = crate::validate::load_project(project).map_err(|e| e.to_string())?;
                crate::validate::validate_loaded(
                    &crate::validate::ValidateOptions {
                        root: project,
                        deploy: true,
                        dev: !ci_or_prod(),
                    },
                    &loaded,
                )
                .0
                .map_err(|e| e.to_string())?;
                Ok(loaded)
            };
            let loaded = validate().map_err(|e| format!("{e}\nlocks are missing, stale or from another vibe version: run `vibe build` first"))?;
            let key = sign::load_or_create(project)?;
            let manifest = manifest::build_signed(
                project,
                &loaded,
                Some(&key),
                &manifest::BuildInfo {
                    release: false,
                    version: String::new(),
                    commit: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            if !run_env.is_dev() {
                return Err("development manifests are refused outside development".into());
            }
            manifest::verify_manifest(manifest, &sign::load_dev_public(project)?)
                .map_err(|e| e.to_string())?
        }
    };
    if matches!(opts.source, RunSource::Project) && pins.any() {
        eprintln!(
            "note: --expect-identity/--expect-issuer pins are ignored for development manifests"
        );
    }
    if matches!(opts.source, RunSource::Project) {
        check_verified_manifest(&manifest, run_env, opts.allow_test_build)?;
    }
    if manifest.provenance.implicit_memory_provider {
        eprintln!("warning: this manifest uses the implicit in-memory provider `main`: data is kept in memory only and is lost on restart; add providers/main.md to use PostgreSQL");
        if !memory {
            return Err("the manifest uses the implicit in-memory provider `main` (no providers/); add providers/main.md to use --db postgres".into());
        }
    }
    // `sign-jwt` key-refs name environment variables holding private PEMs.
    auth.signing_keys =
        crate::runtime::load_signing_keys(&manifest, &|name| std::env::var(name).ok(), &auth.kid)?;
    if memory {
        for method in &manifest.methods {
            for step in method.steps.iter().filter(|s| s.op == "query") {
                if let Err(reason) = crate::runtime::memsql::classify(&step.sql)
                    .and_then(|plan| plan.check_models(&manifest.models))
                {
                    eprintln!("warning: {} step {} will fail on --db memory with MEMORY_UNSUPPORTED_SQL; fine on PostgreSQL: {reason}", method.target, step.id);
                }
            }
        }
    }
    // Per-route iss/jwks-uri (spec §4.1): one JWKS cache per URI, https only.
    auth.add_route_jwks(&manifest.routes)?;
    let mut loaded = crate::runtime::load_project_slugs(project, memory)?;
    let tenants = configured_tenants(
        loaded.slugs.values().map(|s| s.tenant_id.clone()),
        std::env::var("VIBE_TENANTS").ok().as_deref(),
    )?;
    let mut pg_pool = None;
    let exec: Arc<dyn QueryExec> = if memory {
        Arc::new(crate::runtime::MemoryExec::new())
    } else {
        let url = std::env::var("VIBE_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                "set VIBE_DATABASE_URL or pass --db memory for the dev executor".to_string()
            })?;
        let pg = PgExec::connect_with(&url, crate::runtime::PgOptions::from_env()?).await?;
        let runtime_role = match std::env::var("VIBE_RUNTIME_DB_ROLE") {
            Ok(role) => role,
            Err(_) => pg
                .query_texts("SELECT current_user::text WHERE $1::text IS NOT NULL", "")
                .await?
                .into_iter()
                .next()
                .ok_or("cannot determine runtime database role")?,
        };
        let owner = match std::env::var("VIBE_MIGRATE_DATABASE_URL") {
            Ok(url) => Some(PgExec::connect(&url).await?),
            Err(_) => None,
        };
        let result = apply_migrations(
            owner.as_ref(),
            &pg,
            &manifest,
            &tenants,
            &runtime_role,
            opts.allow_destructive_migrations,
        )
        .await;
        if let Some(owner) = owner {
            owner.pool().close();
        }
        for line in result? {
            eprintln!("{line}");
        }
        pg.check_runtime_role().await?;
        pg_pool = Some(pg.pool());
        Arc::new(pg)
    };
    let reload_slugs = memory && run_env.is_dev();
    if crate::runtime::apply_dev_default(&mut loaded.slugs, memory, run_env.is_dev()) {
        eprintln!("dev: slug `dev` (tenant devtenant) is available for public routes, e.g. /<def>/<ver>/dev/...");
    }
    if let (Some(pool), true) = (
        &pg_pool,
        manifest.routes.iter().any(|r| r.refresh.is_some()),
    ) {
        let client = pool
            .get()
            .await
            .map_err(|err| format!("refresh token store check: {err}"))?;
        crate::runtime::refresh::check_store(&client, &tenants).await?;
    }
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    if loaded.slugs.is_empty()
        && manifest
            .routes
            .iter()
            .any(|route| route.public && route.path.contains("{slug}"))
    {
        eprintln!(
            "warning: public routes use {{slug}} but no tenants are configured; add public slugs to .vibe/slugs.json (slugs.example.json is used only by `vibe run --db memory`)"
        );
    }
    // #118: refuse `challenge_when: always` in production until a verifier
    // is configured.
    // #157: build the per-tenant store chosen at startup; a missing or unusable
    // table refuses to start.
    let replay_store: Option<Arc<dyn crate::runtime::replay::ReplayStore>> =
        match (replay_choice, pg_pool) {
            (Some(crate::runtime::replay::StoreKind::Postgres), Some(pool)) => {
                // Per-tenant tables; the IP digest is keyed per tenant from
                // the API-key pepper.
                let store = crate::runtime::replay::PgReplayStore::new(pool, &auth.pepper);
                store.check(&tenants).await?;
                Some(Arc::new(store))
            }
            (Some(crate::runtime::replay::StoreKind::Postgres), None) => {
                return Err("the challenge replay store needs PostgreSQL".into())
            }
            _ => None,
        };
    let challenge = crate::runtime::challenge::verifier_from_env_with_store(replay_store)?;
    let always: Vec<String> = manifest
        .routes
        .iter()
        .filter(|r| r.public && r.challenge_type != "none" && r.challenge_when == "always")
        .map(|r| r.feature_id.clone())
        .collect();
    crate::runtime::challenge::production_gate(run_env, challenge.is_some(), &always)?;
    // #199: refuse challenges that are declared but never verified.
    let unverified: Vec<String> = manifest
        .routes
        .iter()
        .filter(|r| {
            crate::runtime::challenge::challenge_unverified(
                r.public,
                &r.challenge_type,
                &r.challenge_when,
            )
        })
        .map(|r| r.feature_id.clone())
        .collect();
    crate::runtime::challenge::unverified_gate(run_env, &unverified)?;
    let addr: SocketAddr = addr.parse().map_err(|err| format!("bad address: {err}"))?;
    let state = Arc::new(AppState {
        manifest,
        exec,
        auth,
        dev_slugs: reload_slugs
            .then(|| crate::runtime::DevSlugs::new(project, loaded.slugs.clone())),
        slugs: loaded.slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        disabled: load_disabled(project),
        min_override_ms: None,
        honor_test_ip: false,
        trusted_proxies: trusted_proxies_from_env()?,
        dev_hints: memory,
        challenge,
    });
    let app = router(state);
    Ok(Prepared { app, addr })
}
/// Prepares the runtime, binds its address, and serves HTTP.
pub async fn run(opts: RunOptions) -> Result<(), String> {
    let Prepared { app, addr } = prepare(&opts).await?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "listening on http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| e.to_string())
}

fn auth_from_env(run_env: crate::runtime::RunEnv) -> Result<AuthConfig, String> {
    let mut auth = AuthConfig::anonymous();
    if let Ok(pem) = std::env::var("VIBE_JWT_PUBLIC_KEY_PEM") {
        auth.public_pem = pem.replace("\\n", "\n");
    }
    let alg_env = std::env::var("VIBE_JWT_ALG").ok();
    auth.alg = crate::runtime::static_jwt_alg(
        (!auth.public_pem.is_empty()).then_some(auth.public_pem.as_str()),
        alg_env.as_deref(),
    )?;
    if let Ok(iss) = std::env::var("VIBE_JWT_ISS") {
        auth.iss = iss;
    }
    if let Ok(aud) = std::env::var("VIBE_JWT_AUD") {
        auth.aud = aud;
    }
    if let Ok(kid) = std::env::var("VIBE_JWT_KID") {
        auth.kid = kid;
    }
    let pepper = std::env::var("VIBE_API_KEY_PEPPER").ok();
    let (pepper, warning) = crate::runtime::resolve_pepper(run_env, pepper.as_deref())?;
    if let Some(warning) = warning {
        eprintln!("warning: {warning}");
    }
    auth.pepper = pepper;
    // API keys are `mbk_live_…` in production and `mbk_test_…` elsewhere.
    auth.env_name = if run_env == crate::runtime::RunEnv::Production {
        "live".into()
    } else {
        "test".into()
    };
    if let Ok(claim) = std::env::var("VIBE_JWT_ROLES_CLAIM") {
        if !claim.trim().is_empty() {
            if let Some(problem) = crate::manifest::roles_claim_problem(claim.trim()) {
                return Err(format!("VIBE_JWT_ROLES_CLAIM: {problem}"));
            }
            auth.roles_claim = claim.trim().to_string();
        }
    }
    let jwks_uri = std::env::var("VIBE_JWT_JWKS_URI")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            // Spec §4.1: default `{iss}/.well-known/jwks.json` when no PEM is pinned.
            (auth.public_pem.is_empty() && std::env::var("VIBE_JWT_ISS").is_ok())
                .then(|| format!("{}/.well-known/jwks.json", auth.iss.trim_end_matches('/')))
        });
    if jwks_uri.is_some() && !auth.public_pem.is_empty() {
        return Err("set either VIBE_JWT_PUBLIC_KEY_PEM or VIBE_JWT_JWKS_URI, not both".into());
    }
    if let Some(uri) = jwks_uri {
        let allow_private = std::env::var("VIBE_JWT_ALLOW_PRIVATE_HOST").as_deref() == Ok("true");
        if allow_private && !run_env.is_dev() {
            return Err("VIBE_JWT_ALLOW_PRIVATE_HOST is only allowed in development".into());
        }
        let source = crate::runtime::jwks::HttpsJwks::with_policy(&uri, allow_private)?;
        auth.jwks = Some(std::sync::Arc::new(
            crate::runtime::jwks::JwksCache::with_alg(std::sync::Arc::new(source), auth.alg),
        ));
    }
    if let Ok(path) = std::env::var("VIBE_API_KEYS_FILE") {
        auth.api_keys_file = Some(std::sync::Arc::new(crate::runtime::ApiKeyFile::open(
            Path::new(&path).to_path_buf(),
        )?));
    }
    Ok(auth)
}

/// `VIBE_TRUSTED_PROXIES`: comma-separated proxy IPs or CIDR blocks whose
/// `X-Forwarded-For` is honoured. Unset = trust no proxy (use the peer).
fn trusted_proxies_from_env() -> Result<Vec<crate::runtime::ProxyNet>, String> {
    let Ok(raw) = std::env::var("VIBE_TRUSTED_PROXIES") else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            crate::runtime::ProxyNet::parse(s).map_err(|err| format!("VIBE_TRUSTED_PROXIES: {err}"))
        })
        .collect()
}

fn load_disabled(project: &Path) -> std::collections::HashSet<String> {
    let path = project.join(".vibe").join("disabled.txt");
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split('\t').next())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn load_dotenv(project: &Path) {
    let path = project.join(".env");
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let current = runtime_env::env_map_from_process();
    for (key, value) in runtime_env::merge_dotenv(&text, &current) {
        if current.get(&key) != Some(&value) {
            std::env::set_var(key, value);
        }
    }
}

#[cfg(feature = "sdk")]
fn ci_or_prod() -> bool {
    std::env::var("CI").ok().as_deref() == Some("true")
        || std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
        || !crate::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), true)
            .is_dev()
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "sdk"))]
    #[tokio::test]
    async fn project_mode_requires_sdk() {
        let options = super::RunOptions {
            pins: Default::default(),
            project: ".".into(),
            source: super::RunSource::Project,
            addr: "127.0.0.1:0".into(),
            db: super::DbChoice::Memory,
            allow_destructive_migrations: false,
            allow_test_build: false,
        };
        let error = super::prepare(&options).await.err().unwrap();
        assert_eq!(error, "project mode needs vibe-sdk (`vibe run`); vibe-runtime runs build artifacts: pass --artifact");
    }

    #[test]
    fn artifact_file_and_directory_paths() {
        let root = std::env::temp_dir().join(format!("vibe-artifact-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("manifest.json");
        assert_eq!(super::artifact_paths(&root).0, file);
        assert_eq!(super::artifact_paths(&file).0, file);
        if std::env::var_os("VIBE_MANIFEST_BUNDLE").is_none() {
            assert_eq!(
                super::artifact_paths(&file).1,
                root.join("manifest.json.sigstore.json")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Sorted union of slug tenants and the explicitly configured environment list.
pub fn configured_tenants(
    tenants: impl IntoIterator<Item = String>,
    env: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut result: Vec<String> = tenants.into_iter().collect();
    if let Some(value) = env {
        for tenant in value.split(',').map(str::trim) {
            if !(8..=32).contains(&tenant.len())
                || !tenant
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            {
                return Err("VIBE_TENANTS entries must match ^[a-z0-9]{8,32}$".into());
            }
            result.push(tenant.into());
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

/// Plan every tenant before applying any tenant DDL. The serving pool is never
/// connected with owner credentials; without an owner this is read-only.
pub async fn apply_migrations(
    owner: Option<&PgExec>,
    serving: &PgExec,
    manifest: &manifest::Manifest,
    tenants: &[String],
    runtime_role: &str,
    allow_destructive: bool,
) -> Result<Vec<String>, String> {
    use crate::runtime::migrate::{destructive_reason, ensure_tracking, pending_migrations};
    if tenants.is_empty() {
        return Ok(vec!["no tenants configured (.vibe/slugs.json or VIBE_TENANTS); no tenant migrations to apply".into()]);
    }
    let reader = owner.unwrap_or(serving);
    let mut plans = Vec::new();
    for tenant in tenants {
        configured_tenants(std::iter::empty(), Some(tenant))?;
        plans.push((tenant, pending_migrations(reader, manifest, tenant).await?));
    }
    let mut log = Vec::new();
    for (index, migration) in manifest.migrations.iter().enumerate() {
        let affected: Vec<_> = plans
            .iter()
            .filter(|(_, p)| p.contains(&index))
            .map(|(t, _)| t.as_str())
            .collect();
        if affected.is_empty() || owner.is_none() {
            continue;
        }
        if let Some(reason) = migration
            .statements
            .iter()
            .find_map(|s| destructive_reason(s))
        {
            if !allow_destructive {
                return Err(format!("refusing to apply destructive migration {} to tenant(s) {}: {reason}. Back up the database, then start once with --allow-destructive-migrations (vibe run or vibe-runtime run) and restart without it. Non-destructive alternative: add a new column/table instead of dropping or changing one.", migration.id, affected.join(", ")));
            }
            for tenant in affected {
                log.push(format!(
                    "warning: applying destructive migration {} to tenant {tenant}: {reason}",
                    migration.id
                ));
            }
        }
    }
    let Some(owner) = owner else {
        let count: usize = plans.iter().map(|(_, p)| p.len()).sum();
        let exists = serving.query_texts("SELECT coalesce(to_regclass('vibe_platform.applied_migrations')::text, '') WHERE $1::text IS NOT NULL", "").await?;
        if count > 0 || exists.first().is_none_or(String::is_empty) {
            let affected = plans
                .iter()
                .filter(|(_, p)| !p.is_empty())
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>();
            let ids = plans
                .iter()
                .flat_map(|(_, p)| p.iter().map(|i| manifest.migrations[*i].id.as_str()))
                .collect::<std::collections::BTreeSet<_>>();
            return Err(format!("{count} pending migration(s) for tenant(s) {} ({}); set VIBE_MIGRATE_DATABASE_URL (the table-owner role) so vibe run can apply them", if affected.is_empty() { tenants.join(", ") } else { affected.join(", ") }, ids.into_iter().collect::<Vec<_>>().join(", ")));
        }
        return Ok(vec![format!(
            "up to date: {} migrations",
            manifest.migrations.len()
        )]);
    };
    ensure_tracking(owner, runtime_role).await?;
    for (tenant, _) in plans {
        log.extend(crate::runtime::migrate_tenant(owner, manifest, tenant, runtime_role).await?);
        if manifest.routes.iter().any(|r| r.refresh.is_some()) {
            let schema = format!("t_{tenant}");
            owner
                .batch(&crate::runtime::refresh::table_statements(&schema)?)
                .await?;
            owner
                .batch(&crate::runtime::refresh::runtime_grant_statements(
                    &schema,
                    runtime_role,
                )?)
                .await?;
            owner
                .batch(&crate::runtime::refresh::cleanup_statements(&schema)?)
                .await?;
            log.push(format!(
                "bootstrapped {schema}._platform_refresh_tokens (finished families cleaned up)"
            ));
        }
    }
    owner.check_role_separation_for(Some(runtime_role)).await?;
    Ok(log)
}

#[cfg(test)]
mod tenant_tests {
    use super::*;
    #[test]
    fn environment_tenants_are_validated_sorted_and_merged() {
        assert_eq!(
            configured_tenants(["bbbbbbbb".into()], Some(" cccccccc,aaaaaaaa,bbbbbbbb ")).unwrap(),
            vec!["aaaaaaaa", "bbbbbbbb", "cccccccc"]
        );
        for value in [
            "",
            "short",
            "AAAAAAAA",
            "aaaaaaaa,",
            "a_bbbbbb",
            "aaaaaaaa;drop",
        ] {
            assert!(configured_tenants(Vec::new(), Some(value))
                .unwrap_err()
                .contains("VIBE_TENANTS"));
        }
        assert!(configured_tenants(Vec::new(), None).unwrap().is_empty());
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;
    #[test]
    fn resolves_flags_and_environment_without_precedence() {
        for (index, key, name, value) in [
            (
                0,
                "VIBE_EXPECT_IDENTITY",
                "expect-identity",
                "https://github.com/acme/shop/.github/workflows/release.yml@refs/tags/v*",
            ),
            (
                1,
                "VIBE_EXPECT_ISSUER",
                "expect-issuer",
                crate::sigstore::ISSUER,
            ),
            (
                2,
                "VIBE_EXPECT_REPOSITORY_ID",
                "expect-repository-id",
                "123",
            ),
        ] {
            let mut flags = PinArgs::default();
            match index {
                0 => flags.identity = Some(value.into()),
                1 => flags.issuer = Some(value.into()),
                _ => flags.repository_id = Some(value.into()),
            }
            assert!(flags.resolve(|_| None).is_ok());
            assert!(PinArgs::default()
                .resolve(|k| (k == key).then(|| value.into()))
                .is_ok());
            assert!(flags.resolve(|k| (k == key).then(|| value.into())).is_ok());
            assert_eq!(
                flags
                    .resolve(|k| (k == key).then(|| "different".into()))
                    .unwrap_err(),
                format!("--{name} and {key} disagree; set only one")
            );
        }
        assert!(PinArgs::default()
            .resolve(|_| Some(String::new()))
            .unwrap()
            .identity
            .is_none());
        assert!(PinArgs {
            identity: Some("bad".into()),
            ..Default::default()
        }
        .resolve(|_| None)
        .unwrap_err()
        .contains("VIBE_EXPECT_IDENTITY is invalid"));
    }
}

#[cfg(test)]
mod claim_hint_tests {
    use super::*;
    #[test]
    fn only_identity_and_issuer_mismatches_produce_hints() {
        let pin = crate::sigstore::Pin::parse(
            "https://github.com/acme/shop/.github/workflows/release.yml@refs/tags/v*",
            crate::sigstore::ISSUER,
            None,
        )
        .unwrap();
        let mut claim = manifest::SigningClaim {
            issuer: crate::sigstore::ISSUER.into(),
            source_identity: pin.identity_for("1.8.0").unwrap(),
            source_repository: "untrusted".into(),
            signer_workflow: "untrusted".into(),
        };
        assert!(claim_hints(&claim, &pin, "1.8.0").unwrap().is_empty());
        claim.source_identity = "other".into();
        assert_eq!(
            claim_hints(&claim, &pin, "1.8.0").unwrap(),
            vec![format!(
                "note: manifest claims source identity other; pinned identity is {}",
                pin.identity_for("1.8.0").unwrap()
            )]
        );
        claim.issuer = "other".into();
        let hints = claim_hints(&claim, &pin, "1.8.0").unwrap();
        assert_eq!(hints.len(), 2);
        assert_eq!(
            hints[1],
            format!(
                "note: manifest claims issuer other; pinned issuer is {}",
                crate::sigstore::ISSUER
            )
        );
        claim.source_identity = pin.identity_for("1.8.0").unwrap();
        assert_eq!(claim_hints(&claim, &pin, "1.8.0").unwrap().len(), 1);
    }
}
