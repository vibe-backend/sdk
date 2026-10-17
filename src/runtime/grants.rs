//! Model `## Policies` `grants:` parsing, and the effective per-role grants
//! that both executors use (spec 2C.4, decision 74).
//!
//! Syntax: `app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT],
//! public_writer [INSERT(email, name), UPDATE(name)]`, optionally followed by
//! a `# comment` at the top level. A privilege without a column list covers
//! every column. A role listed twice, or a privilege listed twice, accumulates
//! like repeated `GRANT` statements in PostgreSQL.
//!
//! [`effective`] turns the declared grant into the exact column privileges
//! the platform needs for the SQL it generates, and [`pg_grant_statements`]
//! renders that same grant for PostgreSQL, so the in-memory executor and the
//! database check one definition.

use std::collections::{BTreeMap, BTreeSet};

use crate::manifest::ModelSpec;

/// Runtime role classes a statement can run as (spec §7.1/§7.2). In
/// PostgreSQL each class is a separate `LOGIN` role per tenant
/// ([`tenant_role`]); there are no shared server-wide runtime roles
/// (decision 97).
pub const RUNTIME_ROLES: &[&str] = &["app_rw", "public_reader", "public_writer"];

/// PostgreSQL's identifier limit in bytes (NAMEDATALEN - 1).
pub const MAX_IDENTIFIER_BYTES: usize = 63;

/// The PostgreSQL role for one (tenant, role class) pair (decision 97):
/// `tenant_<id>_rw`, `tenant_<id>_public_reader`, `tenant_<id>_public_writer`,
/// and the tenant's migration role `tenant_<id>_owner` (class `owner`).
/// Runtime roles are `LOGIN NOINHERIT`, have privileges only on `t_<id>`,
/// own nothing, and are not members of (nor have as member) any other role.
/// A name over 63 bytes is an error: PostgreSQL would silently truncate it,
/// which could make two tenants share a role.
pub fn tenant_role(tenant: &str, class: &str) -> Result<String, String> {
    if !super::slugs::valid_tenant(tenant) {
        return Err(super::slugs::TENANT_ID_RULE.into());
    }
    let suffix = match class {
        "app_rw" => "rw",
        "public_reader" => "public_reader",
        "public_writer" => "public_writer",
        "owner" => "owner",
        other => return Err(format!("unknown executor role {other}")),
    };
    let name = format!("tenant_{tenant}_{suffix}");
    if name.len() > MAX_IDENTIFIER_BYTES {
        return Err(format!(
            "role name {name} is {} bytes, over PostgreSQL's {MAX_IDENTIFIER_BYTES}-byte limit; refusing (names are never truncated)",
            name.len()
        ));
    }
    Ok(name)
}

/// The tenant's migration role, `tenant_<id>_owner`: owns `t_<id>`.
pub fn tenant_owner_role(tenant: &str) -> Result<String, String> {
    tenant_role(tenant, "owner")
}

/// Whether `name` is a tenant owner role (`tenant_<id>_owner`).
pub fn is_owner_role_name(name: &str) -> bool {
    name.strip_prefix("tenant_")
        .and_then(|rest| rest.strip_suffix("_owner"))
        .is_some_and(|tenant| tenant_owner_role(tenant).ok().as_deref() == Some(name))
}

/// The three [`tenant_role`]s of one tenant, in [`RUNTIME_ROLES`] order.
pub fn tenant_roles(tenant: &str) -> Result<Vec<String>, String> {
    RUNTIME_ROLES
        .iter()
        .map(|c| tenant_role(tenant, c))
        .collect()
}

/// Columns covered by a privilege: `None` means every column.
pub type Cols = Option<BTreeSet<String>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoleGrant {
    pub select: Option<Cols>,
    pub insert: Option<Cols>,
    pub update: Option<Cols>,
}

impl RoleGrant {
    pub fn can_select(&self, column: &str) -> bool {
        covers(&self.select, column)
    }
    pub fn can_insert(&self, column: &str) -> bool {
        covers(&self.insert, column)
    }
    pub fn can_update(&self, column: &str) -> bool {
        covers(&self.update, column)
    }
}

fn covers(privilege: &Option<Cols>, column: &str) -> bool {
    match privilege {
        None => false,
        Some(None) => true,
        Some(Some(cols)) => cols.contains(column),
    }
}

/// Adds `cols` to a privilege, the way a second `GRANT` adds to the first.
fn add(privilege: &mut Option<Cols>, cols: Cols) {
    *privilege = match (privilege.take(), cols) {
        (Some(None), _) | (_, None) => Some(None),
        (None, Some(new)) => Some(Some(new)),
        (Some(Some(mut old)), Some(new)) => {
            old.extend(new);
            Some(Some(old))
        }
    };
}

/// Grants by role name.
pub type Grants = BTreeMap<String, RoleGrant>;

fn ident(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn parse_grants(raw: &str) -> Result<Grants, String> {
    let body = strip_comment(raw)?;
    let mut out = Grants::new();
    for entry in split_top(body)? {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (role, rest) = entry
            .split_once('[')
            .ok_or_else(|| format!("grant `{entry}` has no [privileges]"))?;
        let role = role.trim();
        // `mail_worker(platform)`: the qualifier names who uses the role.
        let role = role.split_once('(').map_or(role, |(name, _)| name).trim();
        if !ident(role) {
            return Err(format!("invalid role name `{role}`"));
        }
        let body = rest
            .trim_end()
            .strip_suffix(']')
            .ok_or_else(|| format!("grant `{entry}` is missing `]`"))?;
        let grant = out.entry(role.to_string()).or_default();
        for privilege in split_top(body)? {
            let privilege = privilege.trim();
            if privilege.is_empty() {
                continue;
            }
            let (name, cols) = match privilege.split_once('(') {
                Some((name, cols)) => {
                    let cols = cols
                        .strip_suffix(')')
                        .ok_or_else(|| format!("privilege `{privilege}` is missing `)`"))?;
                    let mut set = BTreeSet::new();
                    for col in cols.split(',') {
                        let col = col.trim();
                        if !ident(col) {
                            return Err(format!("invalid column `{col}` in `{privilege}`"));
                        }
                        set.insert(col.to_string());
                    }
                    (name.trim(), Some(set))
                }
                None => (privilege, None),
            };
            match name.to_ascii_uppercase().as_str() {
                "SELECT" => add(&mut grant.select, cols),
                "INSERT" => add(&mut grant.insert, cols),
                "UPDATE" => add(&mut grant.update, cols),
                other => return Err(format!("unsupported privilege `{other}`")),
            }
        }
    }
    Ok(out)
}

/// Strips a trailing `# comment`. A `#` is only a comment at the top level,
/// after a complete grant list; inside `[...]` or `(...)` it is an error.
fn strip_comment(raw: &str) -> Result<&str, String> {
    let mut depth = 0i32;
    for (idx, ch) in raw.char_indices() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            '#' if depth == 0 => return Ok(&raw[..idx]),
            '#' => return Err("`#` inside a grant; comments go after the grant list".into()),
            _ => {}
        }
    }
    Ok(raw)
}

/// Splits on commas that are not inside `[...]` or `(...)`, and reports
/// unbalanced or mismatched brackets.
fn split_top(raw: &str) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    let mut stack = Vec::new();
    let mut start = 0;
    for (idx, ch) in raw.char_indices() {
        match ch {
            '[' | '(' => stack.push(ch),
            ']' | ')' => {
                let open = if ch == ']' { '[' } else { '(' };
                if stack.pop() != Some(open) {
                    return Err(format!("unbalanced `{ch}` in grants"));
                }
            }
            ',' if stack.is_empty() => {
                parts.push(&raw[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    if let Some(open) = stack.last() {
        return Err(format!("unclosed `{open}` in grants"));
    }
    parts.push(&raw[start..]);
    Ok(parts)
}

/// Columns the platform writes on INSERT itself (`tenant_id` is injected by
/// the executor; the others come from the CRUD code).
const INSERT_PLATFORM: &[&str] = &["id", "tenant_id", "created_at", "updated_at"];
/// Columns no UPDATE may ever set, even with a table-wide UPDATE grant.
const NEVER_UPDATED: &[&str] = &["id", "tenant_id", "created_at"];
/// Columns the platform SQL reads in `WHERE` / `RETURNING` for writes (row
/// lookup, soft-delete filter, optimistic lock, ETag).
const WRITE_READS: &[&str] = &["id", "updated_at", "deleted_at"];

/// The grant a role actually gets on a model's table, in both executors:
///
/// - the declared privileges, with a table-wide UPDATE narrowed to every
///   column except `id`, `tenant_id` and `created_at`;
/// - with INSERT: also INSERT on the columns the platform fills (`id`,
///   `tenant_id`, `created_at`, `updated_at`, the owner key);
/// - with UPDATE: also UPDATE on `updated_at` (every UPDATE sets it);
/// - with INSERT or UPDATE: also SELECT on the columns the generated SQL
///   reads (`id`, `updated_at`, `deleted_at`, the owner key). PostgreSQL
///   needs SELECT on every column used in `WHERE` or `RETURNING`;
/// - with SELECT: also SELECT on `id` and `deleted_at` (every read filters
///   on them).
///
/// Roles that are not declared get nothing.
pub fn effective(model: &ModelSpec, grants: &Grants, role: &str) -> RoleGrant {
    let Some(declared) = grants.get(role) else {
        return RoleGrant::default();
    };
    let has = |name: &str| model.fields.iter().any(|f| f.name == name);
    let set = |names: &[&str]| -> Cols {
        Some(
            names
                .iter()
                .filter(|n| has(n))
                .map(|n| n.to_string())
                .collect(),
        )
    };
    let owner: Vec<&str> = model.owner_key.as_deref().into_iter().collect();
    let mut grant = declared.clone();
    if let Some(None) = grant.update {
        let cols: Vec<&str> = model
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .filter(|n| !NEVER_UPDATED.contains(n))
            .collect();
        grant.update = Some(set(&cols));
    }
    if let Some(Some(cols)) = &mut grant.update {
        cols.retain(|c| !NEVER_UPDATED.contains(&c.as_str()));
    }
    if grant.insert.is_some() {
        add(&mut grant.insert, set(INSERT_PLATFORM));
        add(&mut grant.insert, set(&owner));
    }
    if grant.update.is_some() {
        add(&mut grant.update, set(&["updated_at"]));
    }
    if grant.select.is_some() {
        // Every generated SELECT filters `deleted_at IS NULL`, and get-by-id
        // filters `id`.
        add(&mut grant.select, set(&["id", "deleted_at"]));
    }
    if grant.insert.is_some() || grant.update.is_some() {
        add(&mut grant.select, set(WRITE_READS));
        add(&mut grant.select, set(&owner));
    }
    grant
}

/// `REVOKE` + `GRANT` statements that give each of the tenant's own roles
/// ([`tenant_role`]) exactly its [`effective`] grant on one table of the
/// tenant's schema `t_<tenant>`. Nothing is granted to any other role.
pub fn pg_grant_statements(
    tenant: &str,
    model: &ModelSpec,
    grants: &Grants,
) -> Result<Vec<String>, String> {
    use super::sql::quote;
    let schema = format!("t_{tenant}");
    let qualified = format!("{}.{}", quote(&schema)?, quote(&model.table)?);
    let mut out = vec![format!("REVOKE ALL ON {qualified} FROM PUBLIC")];
    for role in RUNTIME_ROLES {
        let r = quote(&tenant_role(tenant, role)?)?;
        out.push(format!("GRANT USAGE ON SCHEMA {} TO {r}", quote(&schema)?));
        out.push(format!("REVOKE ALL ON {qualified} FROM {r}"));
        let grant = effective(model, grants, role);
        for (privilege, cols) in [
            ("SELECT", &grant.select),
            ("INSERT", &grant.insert),
            ("UPDATE", &grant.update),
        ] {
            match cols {
                None => {}
                Some(None) => out.push(format!("GRANT {privilege} ON {qualified} TO {r}")),
                Some(Some(cols)) if cols.is_empty() => {}
                Some(Some(cols)) => {
                    let list = cols
                        .iter()
                        .map(|c| quote(c))
                        .collect::<Result<Vec<_>, _>>()?
                        .join(", ");
                    out.push(format!("GRANT {privilege} ({list}) ON {qualified} TO {r}"));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_role_names_are_checked_not_truncated() {
        assert_eq!(
            tenant_role("acme0001", "app_rw").unwrap(),
            "tenant_acme0001_rw"
        );
        assert_eq!(
            tenant_owner_role("acme_0001").unwrap(),
            "tenant_acme_0001_owner"
        );
        // 42-byte id: tenant_ + 42 + _public_writer = 63 bytes, accepted.
        let id = "a".repeat(42);
        assert_eq!(tenant_role(&id, "public_writer").unwrap().len(), 63);
        // One byte more is refused for that class, never truncated.
        let id = "a".repeat(43);
        let err = tenant_role(&id, "public_writer").unwrap_err();
        assert!(
            err.contains("63-byte") && err.contains("never truncated"),
            "{err}"
        );
        assert!(tenant_role(&id, "app_rw").is_ok());
        assert!(tenant_role(&"a".repeat(61), "app_rw").is_err());
        assert!(tenant_role("Acme0001", "app_rw").is_err());
        assert!(tenant_role("acme-0001", "app_rw").is_err());
        assert!(tenant_role("acme0001", "bogus").is_err());
    }

    #[test]
    fn parses_table_and_column_grants() {
        let grants = parse_grants(
            "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT(id, name)], public_writer [INSERT(email, name), UPDATE(name)], mail_worker(platform) [SELECT]",
        )
        .unwrap();
        let rw = &grants["app_rw"];
        assert!(rw.can_select("x") && rw.can_insert("anything") && rw.can_update("x"));
        let reader = &grants["public_reader"];
        assert!(reader.can_select("name") && !reader.can_select("email"));
        assert!(!reader.can_insert("email") && !reader.can_update("email"));
        let writer = &grants["public_writer"];
        assert!(!writer.can_select("email"));
        assert!(writer.can_insert("email") && !writer.can_insert("role"));
        assert!(writer.can_update("name") && !writer.can_update("email"));
        assert!(grants["mail_worker"].can_select("x"));
    }

    #[test]
    fn duplicates_accumulate() {
        let g = parse_grants("w [INSERT(a)], w [INSERT(b), SELECT(a)], w [SELECT(c)]").unwrap();
        let w = &g["w"];
        assert!(w.can_insert("a") && w.can_insert("b"));
        assert!(w.can_select("a") && w.can_select("c") && !w.can_select("b"));
        let g = parse_grants("w [SELECT(a), SELECT]").unwrap();
        assert!(g["w"].can_select("anything"));
    }

    #[test]
    fn comments_and_unbalanced_brackets() {
        let g = parse_grants("app_rw [SELECT]   # model-level union").unwrap();
        assert!(g["app_rw"].can_select("x"));
        for bad in [
            "app_rw [SELECT # x]",
            "app_rw [INSERT(a # b)]",
            "app_rw [INSERT(a]",
            "app_rw [INSERT(a))]",
            "app_rw [SELECT",
            "app_rw SELECT]",
            "app_rw [SELECT]]",
            "app_rw [INSERT(a, )]",
            "App [SELECT]",
        ] {
            assert!(parse_grants(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_privileges_grant_nothing() {
        let grants = parse_grants("public_writer []").unwrap();
        assert_eq!(grants["public_writer"], RoleGrant::default());
    }

    #[test]
    fn rejects_unknown_privileges() {
        assert!(parse_grants("app_rw [DELETE]").is_err());
        assert!(parse_grants("app_rw SELECT").is_err());
    }
}
