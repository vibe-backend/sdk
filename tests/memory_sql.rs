use serde_json::json;
use vibe::manifest::ModelSpec;
use vibe::runtime::{memsql::classify, ExecError, MemoryExec, QueryExec};

fn model() -> ModelSpec {
    let mut fields = Vec::new();
    for (name, ty, nullable, unique) in [
        ("id", "uuid", false, true),
        ("tenant_id", "string", false, false),
        ("name", "string", false, true),
        ("other", "string", true, true),
        ("value", "integer", false, false),
        ("created_at", "timestamp", false, false),
        ("updated_at", "timestamp", false, false),
    ] {
        fields.push(json!({"name":name,"ty":ty,"nullable":nullable,"required":!nullable,
            "unique":unique,"primary":name=="id","system":matches!(name,"id"|"tenant_id"|"created_at"|"updated_at"),
            "sensitive":false,"min":null,"max":null,"format":""}));
    }
    serde_json::from_value(json!({"id":"counter","provider":"main","table":"counters","fields":fields,
        "constraints":[],"crud":[],"list":{"page_size":20,"max_page_size":100,"sort":[],"filter":[]},
        "sensitive_output":[],"ownership":""})).unwrap()
}
fn executor() -> MemoryExec {
    let e = MemoryExec::new();
    e.configure_models(&[model()]);
    e
}
async fn raw(
    e: &MemoryExec,
    tenant: &str,
    sql: &str,
    params: &[&str],
) -> Result<Vec<vibe::runtime::Row>, ExecError> {
    e.raw(
        tenant,
        "public_writer",
        true,
        sql,
        params.iter().map(|s| Some((*s).into())).collect(),
    )
    .await
}
const UPSERT: &str = "INSERT INTO counters (name, value) VALUES ($1, 1) ON CONFLICT (name) DO UPDATE SET value = counters.value + EXCLUDED.value RETURNING *";
const GET: &str = "SELECT COALESCE(MAX(value), 0) AS value FROM counters WHERE name = $1";

#[tokio::test]
async fn allowlisted_forms_are_atomic_scoped_and_touch_timestamps() {
    let e = executor();
    assert_eq!(raw(&e, "one", GET, &["a"]).await.unwrap()[0]["value"], 0);
    let first = raw(&e, "one", UPSERT, &["a"]).await.unwrap().remove(0);
    assert_eq!(first["value"], 1);
    assert!(first["id"].is_string());
    assert!(first["created_at"].is_string());
    assert_eq!(first["tenant_id"], "one");
    assert_eq!(raw(&e, "one", UPSERT, &["a"]).await.unwrap()[0]["value"], 2);
    let rows = raw(
        &e,
        "one",
        "UPDATE counters SET value = value + $2 WHERE name = $1 RETURNING *",
        &["a", "3"],
    )
    .await
    .unwrap();
    assert_eq!(rows[0]["value"], 5);
    assert_ne!(rows[0]["updated_at"], first["updated_at"]);
    assert_eq!(
        raw(
            &e,
            "one",
            "UPDATE counters SET value = value + 2 WHERE name = $1 RETURNING value",
            &["a"]
        )
        .await
        .unwrap()[0]["value"],
        7
    );
    assert_eq!(
        raw(
            &e,
            "one",
            "UPDATE counters SET value = 12 WHERE name = $1 RETURNING counters.value",
            &["a"]
        )
        .await
        .unwrap()[0]["value"],
        12
    );
    assert_eq!(raw(&e, "one", GET, &["a"]).await.unwrap()[0]["value"], 12);
    assert_eq!(raw(&e, "two", GET, &["a"]).await.unwrap()[0]["value"], 0);
    assert!(raw(
        &e,
        "two",
        "UPDATE counters SET value = value + 1 WHERE name = $1 RETURNING value",
        &["a"]
    )
    .await
    .unwrap()
    .is_empty());
    assert_eq!(
        raw(
            &e,
            "one",
            "SELECT counters.value FROM counters WHERE name = $1",
            &["a"]
        )
        .await
        .unwrap()[0]["value"],
        12
    );
    assert!(raw(
        &e,
        "one",
        "SELECT * FROM counters WHERE name = $1",
        &["missing"]
    )
    .await
    .unwrap()
    .is_empty());
    assert!(raw(&e,"one","INSERT INTO counters (name, value) VALUES ($1, 99) ON CONFLICT (name) DO NOTHING RETURNING *",&["a"]).await.unwrap().is_empty());
    assert!(raw(
        &e,
        "one",
        "INSERT INTO counters (name, value) VALUES ($1, 99) ON CONFLICT (name) DO NOTHING",
        &["b"]
    )
    .await
    .unwrap()
    .is_empty());
    assert_eq!(raw(&e,"one","INSERT INTO counters (name, value) VALUES ($1, 1) ON CONFLICT (name) DO UPDATE SET value = counters.value + 4 RETURNING value",&["a"]).await.unwrap()[0]["value"],16);
}

#[tokio::test]
async fn constraints_bad_params_overflow_and_roles_do_not_mutate() {
    let e = executor();
    raw(&e, "one", UPSERT, &["a"]).await.unwrap();
    for sql in [
        "INSERT INTO counters (name, value) VALUES ('a', 1) ON CONFLICT (other) DO NOTHING RETURNING value",
        "INSERT INTO counters (name, value) VALUES (NULL, 1) ON CONFLICT (name) DO NOTHING RETURNING value",
        "UPDATE counters SET value = NULL WHERE name = $1 RETURNING value",
        "UPDATE counters SET value = value + 2147483647 WHERE name = $1 RETURNING value",
        "UPDATE counters SET value = value + 9223372036854775807 WHERE name = $1 RETURNING value",
        "UPDATE counters SET value = 'bad' WHERE name = $1 RETURNING value",
        "UPDATE counters SET name = 'changed', value = $2 WHERE name = $1 RETURNING value",
        "UPDATE counters SET updated_at = 'bad' WHERE name = $1 RETURNING value",
    ] { assert!(raw(&e,"one",sql,&["a","bad"]).await.is_err(),"{sql}"); }
    for (role, writes) in [
        ("public_reader", true),
        ("public_writer", false),
        ("bogus", true),
        ("app_rw", true),
    ] {
        assert!(e
            .raw("one", role, writes, UPSERT, vec![Some("a".into())])
            .await
            .is_err());
    }
    assert!(raw(
        &e,
        "one",
        "UPDATE counters SET value = value + $2 WHERE name = $1 RETURNING value",
        &["a", "bad"]
    )
    .await
    .is_err());
    assert_eq!(raw(&e, "one", GET, &["a"]).await.unwrap()[0]["value"], 1);
    assert!(raw(
        &e,
        "one",
        "SELECT * FROM counters WHERE name = $1",
        &["changed"]
    )
    .await
    .unwrap()
    .is_empty());
    let bad = raw(&e, "one", "SELECT * FROM counters WHERE value = $1", &["1"])
        .await
        .unwrap_err();
    assert!(matches!(bad,ExecError::Message(s) if s.starts_with("MEMORY_UNSUPPORTED_SQL:")));
}

#[test]
fn classifier_rejects_unsupported_constructs_without_panicking() {
    for sql in [
        "SELECT * FROM counters JOIN other ON counters.id = other.id WHERE counters.name = $1",
        "SELECT * FROM (SELECT * FROM counters) s WHERE name = $1",
        "DELETE FROM counters WHERE name = $1 RETURNING *",
        "SELECT lower(name) FROM counters WHERE name = $1",
        "UPDATE counters SET value = value - 1 WHERE name = $1 RETURNING *",
        "UPDATE counters SET value = value * 2 WHERE name = $1 RETURNING *",
        "UPDATE counters SET (name, value) = ('a', 1) WHERE name = $1 RETURNING *",
        "UPDATE counters SET value = value + 1, value = value + 2 WHERE name = $1 RETURNING *",
        "SELECT * FROM counters WHERE name = $1 ORDER BY value",
        "SELECT * FROM counters WHERE tenant_id = $1",
        "SELECT tenant_id FROM counters WHERE name = $1",
        "SELECT * FROM counters WHERE name = $0",
        "SELECT * FROM counters WHERE name = $9999999999999999999999999999",
        "SELECT * FROM counters WHERE name = $1; SELECT 1",
        "",
        ";",
        "SELECT",
        "💥",
        "'",
        "(((",
        "\0",
        "-- comment",
    ] {
        let err = classify(sql).expect_err(sql);
        assert!(err.to_string().starts_with("MEMORY_UNSUPPORTED_SQL:"));
    }
    for sql in [
        UPSERT,
        GET,
        "select coalesce(max(value), -1) AS value from counters where name = $1",
    ] {
        classify(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}
