//! Deterministic project routing settings; never compiled by an LLM.
use std::{fs, path::Path};

#[derive(Clone, Debug)]
pub struct Routes {
    pub prefix: String,
    pub default_slug: Option<String>,
}

impl Default for Routes {
    fn default() -> Self {
        Self {
            prefix: "/api".to_string(),
            default_slug: Some("dev".to_string()),
        }
    }
}

impl Routes {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("api/_project.md");
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("api/_project.md: {e}")),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut settings = Self::default();
        let mut routes = false;
        let mut seen = std::collections::HashSet::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if line == "# project" {
                continue;
            }
            if line == "## Routes" {
                routes = true;
                continue;
            }
            if routes && line == "(none)" {
                continue;
            }
            let Some((key, value)) = line.strip_prefix("- ").and_then(|l| l.split_once(':')) else {
                return Err(format!("api/_project.md: unknown section/key `{line}`; use ## Routes with prefix and default-slug bullets"));
            };
            let (key, value) = (key.trim(), value.trim());
            if !routes || !seen.insert(key.to_string()) {
                return Err(format!("api/_project.md: misplaced or duplicate key `{key}`; use one bullet under ## Routes"));
            }
            match key {
                "prefix" => settings.prefix = if value == "(none)" { "" } else { value }.into(),
                "default-slug" => {
                    if value == "(none)" {
                        settings.default_slug = None;
                    } else if !value.is_empty() {
                        settings.default_slug = Some(value.into());
                    }
                }
                _ => {
                    return Err(format!(
                        "api/_project.md: unknown key `{key}`; use prefix or default-slug"
                    ))
                }
            }
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), String> {
        let prefix = &self.prefix;
        if !prefix.is_empty()
            && (prefix.len() > 64
                || !prefix.starts_with('/')
                || prefix[1..].split('/').any(|s| {
                    s.is_empty()
                        || !s.as_bytes()[0].is_ascii_lowercase()
                            && !s.as_bytes()[0].is_ascii_digit()
                        || !s
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        || matches!(s, "health" | "_internal")
                            && prefix.split('/').nth(1) == Some(s)
                }))
        {
            return Err("api/_project.md: invalid prefix; use /api or lowercase alphanumeric/hyphen segments, no trailing slash or empty segments, at most 64 bytes; first segment cannot be health or _internal".into());
        }
        if let Some(slug) = &self.default_slug {
            if !crate::runtime::slugs::valid_slug(slug) {
                return Err(format!("api/_project.md: invalid default-slug `{slug}`; use 1–63 lowercase letters, digits or single internal hyphens (same rules as slugs.json)"));
            }
        }
        Ok(())
    }

    pub fn rule(&self) -> &'static str {
        if self.default_slug.is_some() {
            "Project route rule: public routes use /{definition}/{version}/<endpoint> and tenant: default-slug (single tenant; never add {slug})"
        } else {
            "Project route rule: public routes use /{definition}/{version}/{slug}/<endpoint> and tenant: path.slug"
        }
    }

    pub fn check_slugs<'a>(&self, slugs: impl Iterator<Item = &'a String>) -> Result<(), String> {
        if let Some(default) = &self.default_slug {
            let mut extra: Vec<_> = slugs.filter(|s| *s != default).cloned().collect();
            extra.sort();
            if !extra.is_empty() {
                return Err(format!("api/_project.md: default-slug {default} conflicts with extra slugs {}; remove them or set default-slug: (none)", extra.join(", ")));
            }
        }
        Ok(())
    }

    pub fn check_project_slugs(&self, root: &Path) -> Result<(), String> {
        if self.default_slug.is_some() {
            for name in [".vibe/slugs.json", "slugs.example.json"] {
                let path = root.join(name);
                if path.exists() {
                    let loaded = crate::runtime::load_slug_file(&path)?;
                    self.check_slugs(loaded.slugs.keys())
                        .map_err(|e| format!("{name}: {e}"))?;
                }
            }
        }
        Ok(())
    }
}

pub fn for_source(source: &crate::discover::SourceFile) -> Routes {
    let mut root = source.abs.clone();
    for _ in Path::new(&source.rel).components() {
        root.pop();
    }
    Routes::load(&root).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_routes_are_strict_and_optional() {
        for text in ["", "# project", "## Routes\n(none)"] {
            let routes = Routes::parse(text).unwrap();
            assert_eq!(routes.prefix, "/api");
            assert_eq!(routes.default_slug.as_deref(), Some("dev"));
        }
        let opted_out =
            Routes::parse("## Routes\n- prefix: (none)\n- default-slug: (none)").unwrap();
        assert!(opted_out.prefix.is_empty());
        assert!(opted_out.default_slug.is_none());
        let prefix_only = Routes::parse("## Routes\n- prefix: (none)").unwrap();
        assert!(prefix_only.prefix.is_empty());
        assert_eq!(prefix_only.default_slug.as_deref(), Some("dev"));
        let slug_only = Routes::parse("## Routes\n- default-slug: (none)").unwrap();
        assert_eq!(slug_only.prefix, "/api");
        assert!(slug_only.default_slug.is_none());
        let overridden = Routes::parse("## Routes\n- prefix: /v2\n- default-slug: acme").unwrap();
        assert_eq!(overridden.prefix, "/v2");
        assert_eq!(overridden.default_slug.as_deref(), Some("acme"));
        for (text, key) in [
            ("## Unknown", "Unknown"),
            ("## Routes\n- typo: x", "typo"),
            ("- prefix: /api", "prefix"),
            ("## Routes\n- prefix: /api\n- prefix: /other", "prefix"),
        ] {
            assert!(Routes::parse(text).unwrap_err().contains(key));
        }
        assert!(Routes::parse(&format!("## Routes\n- prefix: /{}", "a".repeat(63))).is_ok());
        for slug in ["-acme", "acme-", "a--b", "A", "a/b", "a.b", &"a".repeat(64)] {
            assert!(Routes::parse(&format!("## Routes\n- default-slug: {slug}")).is_err());
        }
    }
}
