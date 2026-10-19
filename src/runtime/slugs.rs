use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::http::Slug;

pub struct SlugLoad {
    pub slugs: HashMap<String, Slug>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct SlugFile {
    tenant_id: String,
    #[serde(default = "yes")]
    public_access: bool,
    #[serde(default)]
    public_write: bool,
}

fn yes() -> bool {
    true
}

/// Load `.vibe/slugs.json`. `slugs.example.json` is used only when
/// `allow_example` is set (the in-memory dev executor); otherwise a missing
/// runtime file means no tenants, so public slug routes fail closed (404).
pub fn load_project_slugs(project: &Path, allow_example: bool) -> Result<SlugLoad, String> {
    let runtime = project.join(".vibe").join("slugs.json");
    let example = project.join("slugs.example.json");
    let (path, fallback) = if runtime.exists() {
        (runtime, false)
    } else if allow_example && example.exists() {
        (example, true)
    } else {
        let mut warnings = Vec::new();
        if example.exists() {
            warnings.push(format!(
                ".vibe/slugs.json is missing; ignoring {} outside --memory, so public slug routes return 404",
                example.display()
            ));
        }
        return Ok(SlugLoad {
            slugs: HashMap::new(),
            warnings,
        });
    };
    let mut loaded = load_slug_file(&path)?;
    if fallback {
        loaded.warnings.push(format!(
            "using {} because .vibe/slugs.json is missing",
            path.display()
        ));
    }
    Ok(loaded)
}

/// Parse one slug configuration file using the runtime rules.
pub fn load_slug_file(path: &Path) -> Result<SlugLoad, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let raw: HashMap<String, SlugFile> =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut slugs = HashMap::new();
    for (slug, entry) in raw {
        if !valid_slug(&slug) {
            return Err(format!("{}: invalid slug {slug}", path.display()));
        }
        if !valid_tenant(&entry.tenant_id) {
            return Err(format!(
                "{}: tenant id {} must match ^[a-z0-9]{{8,32}}$",
                path.display(),
                entry.tenant_id
            ));
        }
        slugs.insert(
            slug,
            Slug {
                tenant_id: entry.tenant_id,
                public_access: entry.public_access,
                public_write: entry.public_write,
            },
        );
    }
    Ok(SlugLoad {
        slugs,
        warnings: Vec::new(),
    })
}

pub(crate) fn valid_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

/// Tenant ids must match `^[a-z0-9]{8,32}$` wherever they come from (slug
/// file, JWT claim, API-key record) before they reach SQL session settings.
pub fn valid_tenant(tenant: &str) -> bool {
    (8..=32).contains(&tenant.len())
        && tenant
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_ids_are_strictly_validated() {
        assert!(valid_tenant("tenant0001"));
        for bad in [
            "zzzzzzzz, t_tenant0001",
            "TENANT0001",
            "short",
            "tenant0001\"",
            "",
            "a b c d e f",
        ] {
            assert!(!valid_tenant(bad), "{bad:?}");
        }
    }

    #[test]
    fn empty_or_broken_runtime_slugs_fail() {
        for (name, text) in [
            ("empty", ""),
            ("broken", "{not json"),
            ("bad-tenant", r#"{"acme": {"tenant_id": "X"}}"#),
        ] {
            let root =
                std::env::temp_dir().join(format!("vibe-slugs-{name}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(root.join(".vibe")).unwrap();
            fs::write(root.join(".vibe/slugs.json"), text).unwrap();
            assert!(load_project_slugs(&root, false).is_err(), "{name}");
            assert!(load_project_slugs(&root, true).is_err(), "{name} (memory)");
        }
    }

    #[test]
    fn example_is_ignored_unless_allowed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop");
        let loaded = load_project_slugs(&root, false).unwrap();
        assert!(loaded.slugs.is_empty());
        assert!(loaded
            .warnings
            .iter()
            .any(|warning| warning.contains("return 404")));
    }

    #[test]
    fn example_shop_slug_resolves_acme() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop");
        let loaded = load_project_slugs(&root, true).unwrap();
        let acme = loaded.slugs.get("acme").expect("acme slug");
        assert_eq!(acme.tenant_id, "acme0001");
        assert!(acme.public_access);
        assert!(!acme.public_write);
        assert!(loaded
            .warnings
            .iter()
            .any(|warning| warning.contains("slugs.example.json")));
    }
}

/// Apply the convenience slug only for the development memory executor.
pub fn apply_dev_default(slugs: &mut HashMap<String, Slug>, memory: bool, dev: bool) -> bool {
    apply_dev_default_named(slugs, memory, dev, "dev")
}

pub(crate) fn apply_dev_default_named(
    slugs: &mut HashMap<String, Slug>,
    memory: bool,
    dev: bool,
    name: &str,
) -> bool {
    if !memory || !dev || slugs.contains_key(name) {
        return false;
    }
    slugs.insert(
        name.into(),
        Slug {
            tenant_id: "devtenant".into(),
            public_access: true,
            public_write: false,
        },
    );
    true
}

/// Request-driven reload, enabled only by the development memory server.
pub struct DevSlugs {
    project: std::path::PathBuf,
    default_slug: Option<String>,
    state: std::sync::Mutex<DevSlugState>,
}

struct DevSlugState {
    stamps: [Option<std::time::SystemTime>; 2],
    checked: std::time::Instant,
    slugs: HashMap<String, Slug>,
}

impl DevSlugs {
    pub fn new(project: &Path, slugs: HashMap<String, Slug>) -> Self {
        Self::with_default(project, slugs, None)
    }

    pub fn with_default(
        project: &Path,
        slugs: HashMap<String, Slug>,
        default_slug: Option<String>,
    ) -> Self {
        Self {
            default_slug,
            project: project.into(),
            state: std::sync::Mutex::new(DevSlugState {
                stamps: Self::stamps(project),
                checked: std::time::Instant::now(),
                slugs,
            }),
        }
    }

    fn stamps(project: &Path) -> [Option<std::time::SystemTime>; 2] {
        [".vibe/slugs.json", "slugs.example.json"].map(|p| {
            fs::metadata(project.join(p))
                .and_then(|m| m.modified())
                .ok()
        })
    }

    pub fn lookup(&self, slug: &str) -> Option<Slug> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.slugs.contains_key(slug)
            || state.checked.elapsed() >= std::time::Duration::from_secs(1)
        {
            state.checked = std::time::Instant::now();
            let stamps = Self::stamps(&self.project);
            if stamps != state.stamps {
                state.stamps = stamps;
                match load_project_slugs(&self.project, true) {
                    Ok(mut loaded) => {
                        let settings = crate::project::Routes {
                            default_slug: self.default_slug.clone(),
                            ..Default::default()
                        };
                        if let Err(error) = settings
                            .check_project_slugs(&self.project)
                            .and_then(|()| settings.check_slugs(loaded.slugs.keys()))
                        {
                            state.slugs.clear();
                            eprintln!("warning: cannot reload slugs: {error}");
                            return None;
                        }
                        apply_dev_default_named(
                            &mut loaded.slugs,
                            true,
                            true,
                            self.default_slug.as_deref().unwrap_or("dev"),
                        );
                        state.slugs = loaded.slugs;
                        for warning in loaded.warnings {
                            eprintln!("warning: {warning}");
                        }
                        let path = if self.project.join(".vibe/slugs.json").exists() {
                            ".vibe/slugs.json"
                        } else {
                            "slugs.example.json"
                        };
                        eprintln!("reloaded slugs from {}", self.project.join(path).display());
                    }
                    Err(error) => {
                        if self.default_slug.is_some() {
                            state.slugs.clear();
                        }
                        eprintln!("warning: cannot reload slugs: {error}");
                    }
                }
            }
        }
        state.slugs.get(slug).cloned()
    }
}

#[cfg(test)]
mod dev_tests {
    use super::*;
    #[test]
    fn default_requires_memory_and_development_and_preserves_explicit_dev() {
        for (memory, dev) in [(false, false), (false, true), (true, false)] {
            let mut slugs = HashMap::new();
            assert!(!apply_dev_default(&mut slugs, memory, dev));
            assert!(slugs.is_empty());
        }
        let mut slugs = HashMap::new();
        slugs.insert(
            "other".into(),
            Slug {
                tenant_id: "other".into(),
                public_access: true,
                public_write: false,
            },
        );
        assert!(apply_dev_default(&mut slugs, true, true));
        assert!(valid_tenant(&slugs["dev"].tenant_id));
        assert!(!slugs["dev"].public_write);
        slugs.get_mut("dev").unwrap().tenant_id = "explicit".into();
        assert!(!apply_dev_default(&mut slugs, true, true));
        assert_eq!(slugs["dev"].tenant_id, "explicit");
    }
}
