use crate::lock::{find_section, Kind, LockDoc, Risk, Scope, Section, SecurityItem};

pub fn extract_items(doc: &LockDoc) -> Vec<SecurityItem> {
    let mut items = Vec::new();
    match doc.meta.kind {
        Kind::Provider => push(&mut items, "provider-new", Risk::High),
        Kind::Migration => push(&mut items, "migration-ddl", Risk::High),
        Kind::Model => {
            if crud_writes(find_section(&doc.sections, "CRUD")) {
                push(&mut items, "write-sql", Risk::Medium);
            }
            if let Some(table) = find_section(&doc.sections, "Table") {
                let ownership = table.get("ownership").unwrap_or("");
                if ownership == "tenant-wide" || ownership.starts_with("role(") {
                    push(&mut items, "ownership", Risk::High);
                }
            }
        }
        Kind::Method => extract_method(&doc.sections, &mut items),
        Kind::Pipeline => {}
        Kind::Api if doc.meta.scope == Some(Scope::Feature) => {
            extract_feature(&doc.sections, &mut items)
        }
        Kind::Api => {}
    }
    items.sort_by(|a, b| a.item.cmp(&b.item));
    items.dedup_by(|a, b| a.item == b.item);
    items
}

fn extract_method(sections: &[Section], items: &mut Vec<SecurityItem>) {
    if let Some(sig) = find_section(sections, "Signature") {
        let caps = sig.get("capabilities").unwrap_or("");
        if caps.contains("sign-token") {
            push(items, "sign-token", Risk::High);
        }
        if caps.contains("set-password") {
            push(items, "set-password", Risk::High);
        }
        if caps.contains("confirm-verification") {
            push(items, "confirm-verification", Risk::High);
        }
        if sig.get("writes") == Some("true") {
            push(items, "write-sql", Risk::Medium);
        }
    }
    if let Some(steps) = find_section(sections, "Steps") {
        for block in &steps.blocks {
            let op = block_value(block, "op");
            if op == "call" {
                push(items, "external-call", Risk::High);
            }
            let sql = block_value(block, "sql");
            if block_value(block, "writes") == "true" || sql_writes(&sql) {
                push(items, "write-sql", Risk::Medium);
            }
        }
    }
}

fn extract_feature(sections: &[Section], items: &mut Vec<SecurityItem>) {
    if let Some(access) = find_section(sections, "Access") {
        if access.get("public") == Some("true") {
            push(items, "public", Risk::High);
        }
        if access.get("public-writes") == Some("true") {
            push(items, "public-writes", Risk::High);
        }
        if access.get("password-write") == Some("allow") {
            push(items, "password-write", Risk::High);
        }
        if access.get("verification-write") == Some("allow") {
            push(items, "verification-write", Risk::High);
        }
        if access.get("external-calls") == Some("allow") {
            push(items, "external-calls", Risk::High);
        }
    }
    if auth_removed(find_section(sections, "Pipelines")) {
        push(items, "auth-removed", Risk::High);
    }
    if jwt_customized(find_section(sections, "Pipelines")) {
        push(items, "jwt-config", Risk::High);
    }
    if let Some(steps) = find_section(sections, "Steps") {
        for block in &steps.blocks {
            let target = block_value(block, "target");
            if target_looks_like_write(&target) {
                push(items, "write-sql", Risk::Medium);
            }
        }
    }
}

fn target_looks_like_write(target: &str) -> bool {
    let Some((_, method)) = target.rsplit_once('.') else {
        return false;
    };
    matches!(
        method,
        "create"
            | "update"
            | "delete"
            | "upsert"
            | "insert"
            | "remove"
            | "add"
            | "set"
            | "write"
            | "bump"
            | "apply"
            | "import"
            | "replace"
    )
}

/// When all locks are available, mark write-sql on features that call a method
/// whose Signature or steps write — covers custom names like `bump-stock`.
pub fn enrich_feature_writes(
    doc: &LockDoc,
    docs: &std::collections::HashMap<String, LockDoc>,
    items: &mut Vec<SecurityItem>,
) {
    if doc.meta.kind != Kind::Api || doc.meta.scope != Some(Scope::Feature) {
        return;
    }
    let Some(steps) = find_section(&doc.sections, "Steps") else {
        return;
    };
    for block in &steps.blocks {
        let target = block_value(block, "target");
        if target.is_empty() {
            continue;
        }
        if method_doc_writes(docs, &target) {
            push(items, "write-sql", Risk::Medium);
        }
    }
}

/// True when the named method lock writes (Signature or step SQL / writes: true).
pub fn method_doc_writes(docs: &std::collections::HashMap<String, LockDoc>, target: &str) -> bool {
    let Some((object, method)) = target.split_once('.') else {
        return target_looks_like_write(target);
    };
    let id = format!("{object}/methods/{method}");
    let Some(doc) = docs.get(&id) else {
        return target_looks_like_write(target);
    };
    if let Some(sig) = find_section(&doc.sections, "Signature") {
        // Anything other than an explicit `false` counts as a write, so a
        // malformed value never lowers the risk (validation rejects it).
        if sig
            .bullets
            .iter()
            .any(|b| b.key == "writes" && b.value.trim() != "false")
        {
            return true;
        }
    }
    if let Some(steps) = find_section(&doc.sections, "Steps") {
        for block in &steps.blocks {
            let marked = block
                .bullets
                .iter()
                .any(|b| b.key == "writes" && b.value.trim() != "false");
            if marked || sql_writes(&block_value(block, "sql")) {
                return true;
            }
        }
    }
    false
}

pub fn crud_writes(section: Option<&Section>) -> bool {
    let Some(section) = section else {
        return false;
    };
    let Some(table) = &section.table else {
        return false;
    };
    table.rows.iter().any(|row| {
        matches!(
            row.first().map(String::as_str),
            Some("create") | Some("update") | Some("delete")
        )
    })
}

pub fn auth_removed(section: Option<&Section>) -> bool {
    let Some(section) = section else {
        return false;
    };
    section.blocks.iter().any(|block| {
        let name = pipeline_name(&block.heading);
        name == "-auth"
            || name.starts_with('-') && name.trim_start_matches('-') == "auth"
            || (name == "auth"
                && block
                    .bullets
                    .iter()
                    .any(|b| b.key == "removed" && b.value == "true"))
    })
}

fn jwt_customized(section: Option<&Section>) -> bool {
    let Some(section) = section else {
        return false;
    };
    section.blocks.iter().any(|block| {
        pipeline_name(&block.heading) == "auth"
            && block
                .bullets
                .iter()
                .any(|b| matches!(b.key.as_str(), "iss" | "aud" | "jwks-uri" | "roles-claim"))
    })
}

pub fn pipeline_name(heading: &str) -> String {
    heading
        .split_whitespace()
        .nth(1)
        .unwrap_or(heading)
        .to_string()
}

/// Whether SQL writes anywhere in the statement, not just at its start
/// (leading whitespace, comments, `WITH … INSERT`, `(…)` all count).
/// Comments and string literals are ignored.
/// Strip SQL comments, ordinary strings, and dollar-quoted strings so detectors
/// see only real statement text. Dollar-quote bodies are kept when the opener
/// is `DO` (so writes inside `DO $$ … $$` still count).
pub fn strip_sql_noise(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        // Line comment
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        // Block comment
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            out.push(' ');
            continue;
        }
        // Ordinary string
        if bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(' ');
            continue;
        }
        // Dollar-quote: $tag$…$tag$ or $$…$$
        if bytes[i] == b'$' {
            if let Some((tag_end, tag)) = dollar_tag(bytes, i) {
                let close = format!("${tag}$");
                let start_body = tag_end;
                if let Some(rel) = sql[start_body..].find(&close) {
                    let body = &sql[start_body..start_body + rel];
                    // Keep DO bodies so INSERT inside DO $$ … $$ still counts.
                    let before = out.trim_end().to_ascii_lowercase();
                    if before.ends_with("do")
                        || before.ends_with("do ")
                        || before.split_whitespace().last() == Some("do")
                    {
                        out.push(' ');
                        out.push_str(&strip_sql_noise(body));
                        out.push(' ');
                    } else {
                        out.push(' ');
                    }
                    i = start_body + rel + close.len();
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn dollar_tag(bytes: &[u8], start: usize) -> Option<(usize, String)> {
    if bytes.get(start) != Some(&b'$') {
        return None;
    }
    let mut j = start + 1;
    while j < bytes.len() {
        let c = bytes[j];
        if c == b'$' {
            let tag = std::str::from_utf8(&bytes[start + 1..j]).ok()?.to_string();
            if tag
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                return Some((j + 1, tag));
            }
            return None;
        }
        if !(c.is_ascii_alphanumeric() || c == b'_') {
            return None;
        }
        j += 1;
    }
    None
}

/// Whether SQL writes anywhere in the statement, not just at its start
/// (leading whitespace, comments, `WITH … INSERT`, `(…)`, `SELECT INTO`
/// all count). Comments and string literals are ignored.
pub fn sql_writes(sql: &str) -> bool {
    static WRITE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(insert|update|merge|upsert|delete|truncate)\b|\bselect\b[^;]*\binto\b",
        )
        .expect("regex")
    });
    WRITE.is_match(&strip_sql_noise(sql))
}

fn block_value(block: &crate::lock::Block, key: &str) -> String {
    block
        .bullets
        .iter()
        .find(|b| b.key == key)
        .map(|b| b.value.trim_matches('`').to_string())
        .unwrap_or_default()
}

fn push(items: &mut Vec<SecurityItem>, item: &str, risk: Risk) {
    if items.iter().any(|existing| existing.item == item) {
        return;
    }
    items.push(SecurityItem {
        item: item.into(),
        risk,
        reason: String::new(),
        approved_by: Vec::new(),
    });
}

pub fn same_items(expected: &[SecurityItem], actual: &[SecurityItem]) -> bool {
    let mut a: Vec<_> = expected
        .iter()
        .map(|item| (item.item.as_str(), item.risk))
        .collect();
    let mut b: Vec<_> = actual
        .iter()
        .map(|item| (item.item.as_str(), item.risk))
        .collect();
    a.sort();
    b.sort();
    a == b
}

#[cfg(test)]
mod sql_write_tests {
    use super::sql_writes;

    #[test]
    fn writes_are_found_anywhere() {
        for sql in [
            "UPDATE products SET price_cents = $1",
            "  update products set x = 1",
            "/* bump */ UPDATE products SET x = 1",
            "WITH p AS (SELECT id FROM products) UPDATE products SET x = 1",
            "(INSERT INTO t VALUES (1))",
            "INSERT INTO t SELECT 1 ON CONFLICT DO NOTHING",
            "SELECT * INTO tmp FROM products",
            "/* x */ WITH p AS (SELECT 1) INSERT INTO t SELECT * FROM p",
            "DO $$ BEGIN INSERT INTO t VALUES (1); END $$",
        ] {
            assert!(sql_writes(sql), "{sql}");
        }
        for sql in [
            "SELECT * FROM products WHERE note = 'update me'",
            "SELECT 1 -- insert later",
            "SELECT updated_at FROM products",
            "SELECT $$ update $$",
            "SELECT $body$ delete $body$",
        ] {
            assert!(!sql_writes(sql), "{sql}");
        }
    }
}
