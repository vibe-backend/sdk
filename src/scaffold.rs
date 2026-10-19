//! Minimal, explicit project sources for a public hello world route.

pub fn definition_content(definition: &str) -> String {
    format!(
        "# {definition}\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n- timeout: 10s\n- body: 1MB\n"
    )
}

pub fn version_content(definition: &str, version: &str) -> String {
    format!("# {definition} {version}\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n(none)\n\n## Lifecycle\n\n- deprecated: false\n- sunset: (none)\n")
}
