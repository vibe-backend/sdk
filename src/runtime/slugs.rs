use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::http::Slug;

pub struct SlugLoad {
    pub slugs: HashMap<String, Slug>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
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

/// Load `.vibe/slugs.json`, or `slugs.example.json` when the runtime file is absent.
pub fn load_project_slugs(project: &Path) -> Result<SlugLoad, String> {
    let runtime = project.join(".vibe").join("slugs.json");
    let example = project.join("slugs.example.json");
    let (path, fallback) = if runtime.exists() {
        (runtime, false)
    } else if example.exists() {
        (example, true)
    } else {
        return Ok(SlugLoad {
            slugs: HashMap::new(),
            warnings: Vec::new(),
        });
    };
    let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let raw: HashMap<String, SlugFile> =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut warnings = Vec::new();
    if fallback {
        warnings.push(format!(
            "using {} because .vibe/slugs.json is missing",
            path.display()
        ));
    }
    let mut slugs = HashMap::new();
    for (slug, entry) in raw {
        if !valid_slug(&slug) {
            return Err(format!("invalid slug {slug}"));
        }
        if !valid_tenant(&entry.tenant_id) {
            return Err(format!(
                "tenant id {} must match ^[a-z0-9]{{8,32}}$",
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
    Ok(SlugLoad { slugs, warnings })
}

pub fn add_slug(
    project: &Path,
    slug: &str,
    tenant_id: &str,
    public_write: bool,
) -> Result<(), String> {
    if !valid_slug(slug) {
        return Err(
            "slug must be lowercase letters, digits, and single hyphens, 1-63 characters".into(),
        );
    }
    if !valid_tenant(tenant_id) {
        return Err("tenant id must match ^[a-z0-9]{8,32}$".into());
    }
    let dir = project.join(".vibe");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join("slugs.json");
    let mut raw: HashMap<String, SlugFile> = if path.exists() {
        let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
        serde_json::from_str(&text).map_err(|err| err.to_string())?
    } else {
        HashMap::new()
    };
    raw.insert(
        slug.to_string(),
        SlugFile {
            tenant_id: tenant_id.to_string(),
            public_access: true,
            public_write,
        },
    );
    let text = serde_json::to_string_pretty(&raw).map_err(|err| err.to_string())?;
    fs::write(path, text + "\n").map_err(|err| err.to_string())
}

fn valid_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

pub(crate) fn valid_tenant(tenant: &str) -> bool {
    (8..=32).contains(&tenant.len())
        && tenant
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_shop_slug_resolves_acme() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/shop");
        let loaded = load_project_slugs(&root).unwrap();
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
