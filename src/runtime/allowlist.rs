//! Migration statement allowlist (spec §2B "Owner role 條件", D129).
//!
//! Migrations run as the tenant owner role, which PostgreSQL lets do
//! anything to its own tables. The parser therefore accepts only listed
//! statement kinds and always rejects DELETE, TRUNCATE, GRANT, REVOKE,
//! `ALTER ... OWNER`, DISABLE / NO FORCE ROW LEVEL SECURITY, DROP POLICY,
//! SECURITY DEFINER and any GUC change.
//!
//! It also decides where an index is built: UNIQUE indexes are part of the
//! schema's correctness and are created inside the main migration
//! transaction; only non-unique (purely performance) indexes are built
//! `CONCURRENTLY` after the main transaction commits.

/// Where a migration statement runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Inside the main migration transaction (the SQL to run there).
    InTransaction(String),
    /// A non-unique index built `CONCURRENTLY` after the main transaction
    /// commits, as its own audited step (the SQL to run).
    AfterCommit(String),
}

/// Statement kinds the allowlist accepts, by leading words.
pub const ALLOWED: &[&str] = &[
    "CREATE TABLE",
    "ALTER TABLE",
    "CREATE [UNIQUE] INDEX",
    "COMMENT ON",
    "CREATE [OR REPLACE] FUNCTION",
    "CREATE [OR REPLACE] PROCEDURE",
    "CREATE [OR REPLACE] TRIGGER",
    "CREATE TYPE",
    "CREATE SEQUENCE",
    "INSERT",
    "UPDATE",
];

/// Lower-cased words outside comments, string literals and dollar-quoted
/// bodies. A quoted identifier becomes `"q"`; `.` stays part of a word, so
/// `a.b` and `"a"."b"` are single tokens. A top-level `;` is a `;` token.
pub fn words(sql: &str) -> Vec<String> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    while i < b.len() {
        let c = b[i];
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            flush(&mut cur, &mut out);
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            flush(&mut cur, &mut out);
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == b'\'' {
            flush(&mut cur, &mut out);
            i += 1;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push("'".into());
        } else if c == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if b.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            cur.push_str("\"q\"");
        } else if c == b'$' && cur.is_empty() {
            // Dollar quote `$tag$ ... $tag$` (a `$` inside a word, as in
            // `a$x$`, is part of the identifier).
            let rest = &sql[i + 1..];
            let tag_len = rest
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                .count();
            if rest.as_bytes().get(tag_len) == Some(&b'$')
                && !rest.as_bytes().first().is_some_and(|c| c.is_ascii_digit())
            {
                let tag = &sql[i..i + tag_len + 2];
                let body_start = i + tag_len + 2;
                let end = sql[body_start..]
                    .find(tag)
                    .map_or(b.len(), |p| body_start + p + tag.len());
                out.push("$".into());
                i = end;
            } else {
                cur.push('$');
                i += 1;
            }
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'$' || c >= 0x80 {
            cur.push((c as char).to_ascii_lowercase());
            i += 1;
        } else {
            flush(&mut cur, &mut out);
            if c == b';' {
                out.push(";".into());
            } else if !c.is_ascii_whitespace() {
                out.push((c as char).to_string());
            }
            i += 1;
        }
    }
    flush(&mut cur, &mut out);
    while out.last().map(String::as_str) == Some(";") {
        out.pop();
    }
    out
}

fn has_seq(words: &[String], seq: &[&str]) -> bool {
    words
        .windows(seq.len())
        .any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
}

fn lead(words: &[String], n: usize) -> String {
    words
        .iter()
        .take(n)
        .map(|w| w.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn refuse(what: &str) -> String {
    format!(
        "{what} is not allowed in migrations (migration allowlist: {})",
        ALLOWED.join(", ")
    )
}

/// Checks one migration statement against the allowlist and decides where
/// it runs. Errors never contain more than the statement's leading words.
pub fn classify(sql: &str) -> Result<Placement, String> {
    let w = words(sql);
    if w.is_empty() {
        return Err("empty statement".into());
    }
    if w.iter().any(|t| t == ";") {
        return Err(refuse("more than one command in a statement"));
    }
    // Always rejected, wherever they appear.
    for (seq, what) in [
        (&["security", "definer"][..], "SECURITY DEFINER"),
        (&["owner", "to"][..], "ALTER ... OWNER"),
        (
            &["disable", "row", "level", "security"][..],
            "DISABLE ROW LEVEL SECURITY",
        ),
        (
            &["no", "force", "row", "level", "security"][..],
            "NO FORCE ROW LEVEL SECURITY",
        ),
        (&["drop", "policy"][..], "DROP POLICY"),
    ] {
        if has_seq(&w, seq) {
            return Err(refuse(what));
        }
    }
    if w.iter()
        .any(|t| t == "set_config" || t.ends_with(".set_config"))
    {
        return Err(refuse("a GUC change (set_config)"));
    }
    let first = w[0].as_str();
    match first {
        "delete" | "truncate" | "grant" | "revoke" | "drop" | "set" | "reset" => {
            let what = match first {
                "set" | "reset" => format!("a GUC change ({})", first.to_uppercase()),
                "drop" => "DROP (only through the deprecation flow)".to_string(),
                _ => first.to_uppercase(),
            };
            return Err(refuse(&what));
        }
        "insert" | "update" | "comment"
            if first != "comment" || w.get(1).map(String::as_str) == Some("on") =>
        {
            return Ok(Placement::InTransaction(sql.to_string()));
        }
        "alter" if w.get(1).map(String::as_str) == Some("table") => {
            if w.iter().skip(2).any(|t| t == "drop") {
                return Err(refuse(
                    "ALTER TABLE ... DROP (only through the deprecation flow)",
                ));
            }
            if w.iter().skip(2).any(|t| t == "set" || t == "reset")
                && !has_seq(&w, &["set", "not", "null"])
                && !has_seq(&w, &["set", "default"])
                && !has_seq(&w, &["set", "data", "type"])
            {
                return Err(refuse("ALTER TABLE ... SET/RESET of table options"));
            }
            return Ok(Placement::InTransaction(sql.to_string()));
        }
        "create" => {}
        _ => return Err(refuse(&format!("`{}`", lead(&w, 2)))),
    }
    let mut rest: Vec<&str> = w[1..].iter().map(String::as_str).collect();
    if rest.starts_with(&["or", "replace"]) {
        rest.drain(..2);
        if !matches!(rest.first(), Some(&"function" | &"procedure" | &"trigger")) {
            return Err(refuse(&format!("`{}`", lead(&w, 4))));
        }
    }
    match rest.first().copied() {
        Some("table") | Some("type") | Some("sequence") | Some("trigger") => {
            Ok(Placement::InTransaction(sql.to_string()))
        }
        Some("function") | Some("procedure") => {
            // `SET param` on a function changes a GUC whenever it runs.
            if rest.contains(&"set") {
                return Err(refuse("a GUC change (SET on a function)"));
            }
            Ok(Placement::InTransaction(sql.to_string()))
        }
        Some("unique") if rest.get(1) == Some(&"index") => {
            Ok(Placement::InTransaction(strip_concurrently(sql)))
        }
        Some("index") => {
            let on = rest.iter().position(|t| *t == "on");
            let mut table = on.and_then(|p| rest.get(p + 1)).copied();
            if table == Some("only") {
                table = on.and_then(|p| rest.get(p + 2)).copied();
            }
            match table {
                Some(t) if t.contains('.') => Err(refuse(
                    "a schema-qualified index target (indexes are built in the tenant schema)",
                )),
                Some(_) => Ok(Placement::AfterCommit(ensure_concurrently(sql))),
                None => Err(refuse("CREATE INDEX without ON <table>")),
            }
        }
        _ => Err(refuse(&format!("`{}`", lead(&w, 3)))),
    }
}

/// Byte offset just after the `INDEX` keyword of a CREATE [UNIQUE] INDEX.
fn after_index_keyword(sql: &str) -> Option<usize> {
    let lower = sql.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find("index") {
        let start = from + p;
        let end = start + 5;
        let before_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let after_ok = lower
            .as_bytes()
            .get(end)
            .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
        if before_ok && after_ok {
            return Some(end);
        }
        from = end;
    }
    None
}

/// Removes a `CONCURRENTLY` right after `INDEX` (it cannot run in a
/// transaction).
pub fn strip_concurrently(sql: &str) -> String {
    let Some(end) = after_index_keyword(sql) else {
        return sql.to_string();
    };
    let tail = &sql[end..];
    let trimmed = tail.trim_start();
    if trimmed.len() >= 12 && trimmed[..12].eq_ignore_ascii_case("concurrently") {
        format!("{}{}", &sql[..end], &trimmed[12..])
    } else {
        sql.to_string()
    }
}

/// Makes sure a non-unique `CREATE INDEX` says `CONCURRENTLY`.
pub fn ensure_concurrently(sql: &str) -> String {
    let stripped = strip_concurrently(sql);
    match after_index_keyword(&stripped) {
        Some(end) => format!("{} CONCURRENTLY{}", &stripped[..end], &stripped[end..]),
        None => stripped,
    }
}

/// `pg_get_indexdef` output rebuilt for the next migrate: UNIQUE inside the
/// main transaction, otherwise `CONCURRENTLY` after commit. The definition
/// comes from the catalog of the tenant's own schema, so it may be
/// schema-qualified.
pub fn rebuild_placement(indexdef: &str) -> Placement {
    let unique = words(indexdef).get(1).map(String::as_str) == Some("unique");
    if unique {
        Placement::InTransaction(strip_concurrently(indexdef))
    } else {
        Placement::AfterCommit(ensure_concurrently(indexdef))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_statements_are_accepted() {
        for sql in [
            "CREATE TABLE audit (tenant_id text, note text)",
            "create table if not exists t (tenant_id text)",
            "ALTER TABLE products ADD COLUMN ends_at timestamptz",
            "ALTER TABLE products ALTER COLUMN name SET NOT NULL",
            "ALTER TABLE products ALTER COLUMN name SET DEFAULT ''",
            "ALTER TABLE products ADD CONSTRAINT c CHECK (price >= 0)",
            "INSERT INTO t VALUES ('commit; rollback')",
            "UPDATE t SET note = 'x'",
            "COMMENT ON TABLE t IS 'begin; end; grant'",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1; $$",
            "CREATE OR REPLACE FUNCTION g() RETURNS int LANGUAGE plpgsql AS $body$ BEGIN DELETE FROM x; RETURN 1; END $body$",
            "CREATE TYPE mood AS ENUM ('a', 'b')",
            "CREATE SEQUENCE s",
            "CREATE TABLE zq (tenant_id text, a$x$ int)",
            "-- note\nCREATE TABLE c (tenant_id text);",
        ] {
            assert!(
                matches!(classify(sql), Ok(Placement::InTransaction(_))),
                "{sql}: {:?}",
                classify(sql)
            );
        }
    }

    #[test]
    fn always_rejected_statements_are_refused() {
        for (sql, what) in [
            ("DELETE FROM t", "DELETE"),
            ("delete from t where true", "DELETE"),
            ("TRUNCATE t", "TRUNCATE"),
            ("GRANT SELECT ON t TO PUBLIC", "GRANT"),
            ("REVOKE SELECT ON t FROM PUBLIC", "REVOKE"),
            ("ALTER TABLE t OWNER TO postgres", "OWNER"),
            ("ALTER TABLE t DISABLE ROW LEVEL SECURITY", "DISABLE ROW LEVEL SECURITY"),
            ("ALTER TABLE t NO FORCE ROW LEVEL SECURITY", "NO FORCE ROW LEVEL SECURITY"),
            ("DROP POLICY tenant_isolation ON t", "DROP POLICY"),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE sql SECURITY DEFINER AS 'SELECT 1'", "SECURITY DEFINER"),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE sql SET search_path = public AS 'SELECT 1'", "GUC"),
            ("SET search_path TO x", "GUC"),
            ("RESET search_path", "GUC"),
            ("UPDATE t SET note = set_config('app.tenant_id', 'x', true)", "GUC"),
            ("ALTER TABLE t SET (fillfactor = 50)", "SET/RESET"),
            ("DROP TABLE t", "DROP"),
            ("ALTER TABLE t DROP COLUMN x", "DROP"),
            ("CREATE VIEW v AS SELECT 1", "CREATE VIEW"),
            ("CREATE POLICY open ON t USING (true)", "CREATE POLICY"),
            ("ALTER POLICY tenant_isolation ON t USING (true)", "ALTER POLICY"),
            ("SELECT pg_sleep(1)", "SELECT"),
            ("CALL p()", "CALL"),
            ("DO $$ BEGIN END $$", "DO"),
            ("ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO PUBLIC", "ALTER DEFAULT"),
            ("CREATE TABLE a (tenant_id text); COMMIT", "more than one command"),
        ] {
            let err = classify(sql).expect_err(sql);
            assert!(err.contains(what), "{sql}: {err}");
            assert!(err.contains("not allowed in migrations"), "{sql}: {err}");
        }
    }

    #[test]
    fn keywords_inside_literals_and_bodies_do_not_count() {
        for sql in [
            "INSERT INTO t VALUES ('DELETE FROM x; SECURITY DEFINER; OWNER TO y')",
            "COMMENT ON COLUMN t.\"drop policy\" IS 'no force row level security'",
            "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $f$ BEGIN TRUNCATE x; END $f$",
        ] {
            assert!(classify(sql).is_ok(), "{sql}: {:?}", classify(sql));
        }
    }

    #[test]
    fn unique_indexes_run_in_the_transaction_others_after_commit() {
        assert_eq!(
            classify("CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS u ON t (a)"),
            Ok(Placement::InTransaction(
                "CREATE UNIQUE INDEX IF NOT EXISTS u ON t (a)".into()
            ))
        );
        assert_eq!(
            classify("CREATE UNIQUE INDEX u ON t (a)"),
            Ok(Placement::InTransaction(
                "CREATE UNIQUE INDEX u ON t (a)".into()
            ))
        );
        assert_eq!(
            classify("CREATE INDEX CONCURRENTLY IF NOT EXISTS p ON products (name)"),
            Ok(Placement::AfterCommit(
                "CREATE INDEX CONCURRENTLY IF NOT EXISTS p ON products (name)".into()
            ))
        );
        assert_eq!(
            classify("create index p on products (name)"),
            Ok(Placement::AfterCommit(
                "create index CONCURRENTLY p on products (name)".into()
            ))
        );
        assert!(classify("CREATE INDEX CONCURRENTLY p ON t_other.notes (x)").is_err());
        assert!(classify("CREATE INDEX ON \"t_other\".\"notes\" (x)").is_err());
    }

    #[test]
    fn invalid_index_rebuild_keeps_the_placement_rule() {
        assert_eq!(
            rebuild_placement("CREATE UNIQUE INDEX u ON t_a.t USING btree (a)"),
            Placement::InTransaction("CREATE UNIQUE INDEX u ON t_a.t USING btree (a)".into())
        );
        assert_eq!(
            rebuild_placement("CREATE INDEX p ON t_a.t USING btree (a)"),
            Placement::AfterCommit("CREATE INDEX CONCURRENTLY p ON t_a.t USING btree (a)".into())
        );
    }
}
