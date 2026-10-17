//! Minimal, explicit project sources for a public hello world route.
use std::{fs, io::Write, path::Path};

pub fn definition_content(definition: &str) -> String {
    format!("# {definition}\n\n## Providers\n\n- allowlist: [main]\n- default: main\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n- timeout: 10s\n- body: 1MB\n")
}

pub fn version_content(definition: &str, version: &str) -> String {
    format!("# {definition} {version}\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n(none)\n\n## Lifecycle\n\n- deprecated: false\n- sunset: (none)\n")
}

pub fn create(project: &Path, path: &str) -> Result<Vec<String>, String> {
    let parts: Vec<_> = path.split('/').collect();
    let [definition, version, feature] = parts.as_slice() else {
        return Err("path must be <definition>/<version>/<feature>".into());
    };
    crate::discover::check_kebab(definition)?;
    crate::discover::check_version(version)?;
    crate::discover::check_kebab(feature)?;
    let feature_path = format!("api/{definition}/{version}/{feature}/{feature}.md");
    if project.join(&feature_path).exists() {
        return Err(format!(
            "feature already exists: {feature_path}; no files written"
        ));
    }
    let files = [
        (feature_path, format!("---\npublic: true\nwrites: false\nauth: none\n---\n\n# {feature}\n\nGET `/{definition}/{version}/{{slug}}/{feature}`. Public, no login, no database access.\n\n- Always return JSON `{{\"message\":\"hello world\"}}` with status 200.\n")),
        (format!("api/{definition}/_definition.md"), definition_content(definition)),
        (format!("api/{definition}/{version}/_version.md"), version_content(definition, version)),
        ("providers/main.md".into(), "# main\n\n## Connection\n\n- type: postgres\n- secret: PG_MAIN\n\n## Isolation\n\n- mode: schema\n- rls: forced\n\n## Access\n\n- shared: false\n- allowed-endpoints: (none)\n\n## Limits\n\n- pool: 5\n- statement-timeout: 5s\n\n## Migrations\n\n- path: migrations/main\n".into()),
    ];
    let mut messages = Vec::new();
    for (rel, content) in files {
        let target = project.join(&rel);
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
        {
            Ok(mut file) => {
                file.write_all(content.as_bytes())
                    .map_err(|e| e.to_string())?;
                messages.push(format!("wrote {rel}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                messages.push(format!("kept {rel}"))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    messages.push(format!("Next: vibe compile; vibe manifest --dev; vibe serve --memory; curl http://127.0.0.1:8080/{definition}/{version}/dev/{feature}"));
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scaffold_matches_required_metadata() {
        let root = std::env::temp_dir().join(format!("vibe-new-{}", uuid::Uuid::new_v4()));
        create(&root, "hello/v1/hello").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("providers/main.md")).unwrap(),
            include_str!("../tests/fixtures/zh-hant-shop/providers/main.md")
        );
        assert_eq!(
            version_content("shop", "v1"),
            include_str!("../tests/fixtures/zh-hant-shop/api/shop/v1/_version.md")
        );
        assert_eq!(
            fs::read_to_string(root.join("api/hello/_definition.md")).unwrap(),
            definition_content("hello")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_paths_write_nothing() {
        let root = std::env::temp_dir().join(format!("vibe-new-{}", uuid::Uuid::new_v4()));
        for path in [
            "Hello/v1/x",
            "hello/1/x",
            "hello/v0/x",
            "../v1/x",
            "hello/v1/a--b",
            "hello/v1",
        ] {
            assert!(create(&root, path).is_err());
        }
        assert!(!root.exists());
    }
}
