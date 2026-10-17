use crate::{
    manifest::{self, read_manifest},
    runtime::{router, AppState, AuthConfig, PgExec, QueryExec},
    runtime_env, sign,
};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
/// Loads and verifies the manifest. Outside development only a release
/// manifest with a Sigstore keyless signature from the release workflow is
/// trusted (decision 119): the bundle (`VIBE_MANIFEST_BUNDLE`, default
/// `<manifest>.sigstore.json`) is verified offline against the compiled-in
/// trust root for the deployed version `VIBE_RELEASE_VERSION`. The
/// development key in `.vibe/signing.pub` is accepted in development only.
pub fn load_manifest(
    manifest_path: &Path,
    project: &Path,
    run_env: crate::runtime::RunEnv,
) -> Result<manifest::Manifest, String> {
    if run_env.is_dev() {
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
        return read_manifest(manifest_path, &key).map_err(|err| err.to_string());
    }
    let mode: manifest::Manifest =
        serde_json::from_slice(&fs::read(manifest_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if mode.build_mode == Some(manifest::BuildMode::Dev) {
        return Err(format!(
            "this development manifest (`vibe build`) is refused when VIBE_ENV is {:?}",
            std::env::var("VIBE_ENV").unwrap_or_default()
        ));
    }
    let version = std::env::var("VIBE_RELEASE_VERSION").map_err(|_| {
        "VIBE_RELEASE_VERSION (the deployed version, e.g. 1.8.0) is required to verify the release manifest's Sigstore identity; the project's .vibe/signing.pub is not trusted outside development".to_string()
    })?;
    let bundle = std::env::var_os("VIBE_MANIFEST_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut name = manifest_path.as_os_str().to_owned();
            name.push(".sigstore.json");
            PathBuf::from(name)
        });
    manifest::read_release_manifest(manifest_path, &bundle, &version).map_err(|err| err.to_string())
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
    pub project: PathBuf,
    pub source: RunSource,
    pub addr: String,
    pub db: DbChoice,
    pub allow_test_build: bool,
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
    load_dotenv(project);
    let run_env =
        crate::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), memory);
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
            let manifest_path = artifact_paths(path).0;
            fs::metadata(&manifest_path).map_err(|err| format!("cannot read manifest {}: {err}", manifest_path.display()))?;
            load_manifest(&manifest_path, project, run_env)?
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
    if manifest.provenance.test_build {
        if !opts.allow_test_build || !run_env.is_dev() {
            return Err("this manifest is a test build made with LLM_PROVIDER=fake; refusing to run it. Pass --allow-test-build to run it anyway (never in production)".into());
        }
        eprintln!("warning: running a test build made with LLM_PROVIDER=fake");
    }
    check_deployable(&manifest, run_env)?;
    for route in &manifest.routes {
        if let Some(problem) = route
            .refresh
            .as_ref()
            .and_then(|spec| crate::manifest::refresh_spec_problem(&route.feature_id, spec))
        {
            return Err(problem);
        }
    }
    // `sign-jwt` key-refs name environment variables holding private PEMs.
    auth.signing_keys =
        crate::runtime::load_signing_keys(&manifest, &|name| std::env::var(name).ok(), &auth.kid)?;
    if memory {
        let with_query: Vec<&str> = manifest
            .methods
            .iter()
            .filter(|m| m.steps.iter().any(|s| s.op == "query"))
            .map(|m| m.target.as_str())
            .collect();
        if !with_query.is_empty() {
            eprintln!(
                "warning: --db memory does not run `query` steps; these methods will fail until you use PostgreSQL: {}",
                with_query.join(", ")
            );
        }
    }
    // Per-route iss/jwks-uri (spec §4.1): one JWKS cache per URI, https only.
    auth.add_route_jwks(&manifest.routes)?;
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
        pg.check_runtime_role().await?;
        pg_pool = Some(pg.pool());
        Arc::new(pg)
    };
    let mut loaded = crate::runtime::load_project_slugs(project, memory)?;
    let reload_slugs = memory && run_env.is_dev();
    if crate::runtime::apply_dev_default(&mut loaded.slugs, memory, run_env.is_dev()) {
        eprintln!("dev: slug `dev` (tenant devtenant) is available for public routes, e.g. /<def>/<ver>/dev/...");
    }
    if let (Some(pool), true) = (
        &pg_pool,
        manifest.routes.iter().any(|r| r.refresh.is_some()),
    ) {
        let mut tenants: Vec<String> = loaded.slugs.values().map(|s| s.tenant_id.clone()).collect();
        tenants.sort();
        tenants.dedup();
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
                let mut tenants: Vec<String> =
                    loaded.slugs.values().map(|s| s.tenant_id.clone()).collect();
                tenants.sort();
                tenants.dedup();
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
            project: ".".into(),
            source: super::RunSource::Project,
            addr: "127.0.0.1:0".into(),
            db: super::DbChoice::Memory,
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
