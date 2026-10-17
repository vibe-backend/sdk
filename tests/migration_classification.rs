use vibe::runtime::migrate::destructive_reason;

#[test]
fn existing_fixture_and_documentation_migrations_are_non_destructive() {
    for (path, text) in [
        (
            "shop migration",
            include_str!("fixtures/shop/migrations/main/0001-products-name.md"),
        ),
        (
            "refresh-session migration",
            include_str!("fixtures/refresh-session/migrations/main/0001-users-last-device.md"),
        ),
        ("spec-v1.7", include_str!("../docs/spec-v1.7.md")),
        ("spec-v1.8", include_str!("../docs/spec-v1.8.md")),
    ] {
        let mut in_up = false;
        let mut count = 0;
        for line in text.lines() {
            if line.starts_with("## ") {
                in_up = line.trim() == "## Up";
            }
            if !in_up {
                continue;
            }
            if let Some(sql) = line.trim().strip_prefix("- sql: ") {
                let sql = if let Some(quoted) = sql.strip_prefix('`') {
                    quoted.split_once('`').expect("closing SQL backtick").0
                } else {
                    sql
                };
                assert_eq!(destructive_reason(sql), None, "{path}: {sql}");
                count += 1;
            }
        }
        assert!(count > 0, "{path}: audit found no migration SQL");
    }
}
