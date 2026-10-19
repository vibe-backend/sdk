//! Deterministic project routing settings; never compiled by an LLM.
use std::{fs, path::Path};

#[derive(Clone, Debug, Default)]
pub struct Routes {
    pub prefix: String,
    pub default_slug: Option<String>,
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
            if line == "# project" { continue; }
            if line == "## Routes" { routes = true; continue; }
            if routes && line == "(none)" { continue; }
            let Some((key, value)) = line.strip_prefix("- ").and_then(|l| l.split_once(':')) else {
                return Err(format!("api/_project.md: unknown section/key `{line}`; use ## Routes with prefix and default-slug bullets"));
            };
            let (key, value) = (key.trim(), value.trim());
            if !routes || !seen.insert(key.to_string()) {
                return Err(format!("api/_project.md: misplaced or duplicate key `{key}`; use one bullet under ## Routes"));
            }
            match key {
                "prefix" => settings.prefix = if value == "(none)" { "" } else { value }.into(),
                "default-slug" => if !value.is_empty() && value != "(none)" { settings.default_slug = Some(value.into()); },
                _ => return Err(format!("api/_project.md: unknown key `{key}`; use prefix or default-slug")),
            }
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), String> {
        let prefix = &self.prefix;
        if !prefix.is_empty() && (prefix.len() > 64 || !prefix.starts_with('/') || prefix[1..].split('/').any(|s| {
            s.is_empty() || !s.as_bytes()[0].is_ascii_lowercase() && !s.as_bytes()[0].is_ascii_digit()
                || !s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || matches!(s, "health" | "_internal") && prefix.split('/').nth(1) == Some(s)
        })) {
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
                return Err(format!("api/_project.md: default-slug {default} conflicts with extra slugs {}; remove them or remove default-slug", extra.join(", ")));
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
                    self.check_slugs(loaded.slugs.keys()).map_err(|e| format!("{name}: {e}"))?;
                }
            }
        }
        Ok(())
    }
}

pub fn for_source(source: &crate::discover::SourceFile) -> Routes {
    let mut root = source.abs.clone();
    for _ in Path::new(&source.rel).components() { root.pop(); }
    Routes::load(&root).unwrap_or_default()
}
