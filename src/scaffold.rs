//! Minimal, explicit project sources for a public hello world route.

pub fn definition_content(definition: &str) -> String {
    format!("# {definition}\n\n## Providers\n\n- allowlist: [main]\n- default: main\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n- timeout: 10s\n- body: 1MB\n")
}

pub fn version_content(definition: &str, version: &str) -> String {
    format!("# {definition} {version}\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n(none)\n\n## Lifecycle\n\n- deprecated: false\n- sunset: (none)\n")
}

pub fn provider_content() -> String {
    "# main\n\n## Connection\n\n- type: postgres\n- secret: PG_MAIN\n\n## Isolation\n\n- mode: schema\n- rls: forced\n\n## Access\n\n- shared: false\n- allowed-endpoints: (none)\n\n## Limits\n\n- pool: 5\n- statement-timeout: 5s\n\n## Migrations\n\n- path: migrations/main\n".into()
}
