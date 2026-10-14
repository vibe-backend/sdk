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
            let sql = block_value(block, "sql").to_ascii_lowercase();
            if block_value(block, "writes") == "true"
                || sql.starts_with("insert")
                || sql.starts_with("update")
            {
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
            if target.ends_with(".create")
                || target.ends_with(".update")
                || target.ends_with(".delete")
            {
                push(items, "write-sql", Risk::Medium);
            }
        }
    }
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
                .any(|b| matches!(b.key.as_str(), "iss" | "aud" | "jwks-uri"))
    })
}

pub fn pipeline_name(heading: &str) -> String {
    heading
        .split_whitespace()
        .nth(1)
        .unwrap_or(heading)
        .to_string()
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
