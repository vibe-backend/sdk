//! Model `## Policies` `grants:` parsing, used by the in-memory executor to
//! emulate PostgreSQL role and column grants (spec 2C.4, decision 74).
//!
//! Syntax: `app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT],
//! public_writer [INSERT(email, name), UPDATE(name)]`. A privilege without a
//! column list covers every column.

use std::collections::{BTreeMap, BTreeSet};

/// Columns covered by a privilege: `None` means every column.
pub type Cols = Option<BTreeSet<String>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoleGrant {
    pub select: bool,
    pub insert: Option<Cols>,
    pub update: Option<Cols>,
}

impl RoleGrant {
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

/// Grants by role name.
pub type Grants = BTreeMap<String, RoleGrant>;

pub fn parse_grants(raw: &str) -> Result<Grants, String> {
    let mut out = Grants::new();
    for entry in split_top(raw.split('#').next().unwrap_or("")) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (role, rest) = entry
            .split_once('[')
            .ok_or_else(|| format!("grant `{entry}` has no [privileges]"))?;
        let role = role.trim();
        let role = role.split_once('(').map_or(role, |(name, _)| name).trim();
        let body = rest
            .trim_end()
            .strip_suffix(']')
            .ok_or_else(|| format!("grant `{entry}` is missing `]`"))?;
        let mut grant = RoleGrant::default();
        for privilege in split_top(body) {
            let privilege = privilege.trim();
            if privilege.is_empty() {
                continue;
            }
            let (name, cols) = match privilege.split_once('(') {
                Some((name, cols)) => {
                    let cols = cols
                        .strip_suffix(')')
                        .ok_or_else(|| format!("privilege `{privilege}` is missing `)`"))?;
                    let set = cols
                        .split(',')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect();
                    (name.trim(), Some(set))
                }
                None => (privilege, None),
            };
            match name.to_ascii_uppercase().as_str() {
                "SELECT" => grant.select = true,
                "INSERT" => grant.insert = Some(cols),
                "UPDATE" => grant.update = Some(cols),
                other => return Err(format!("unsupported privilege `{other}`")),
            }
        }
        out.insert(role.to_string(), grant);
    }
    Ok(out)
}

/// Splits on commas that are not inside `[...]` or `(...)`.
fn split_top(raw: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (idx, ch) in raw.char_indices() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&raw[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&raw[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_table_and_column_grants() {
        let grants = parse_grants(
            "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer [INSERT(email, name), UPDATE(name)], mail_worker(platform) [SELECT]",
        )
        .unwrap();
        let rw = &grants["app_rw"];
        assert!(rw.select && rw.can_insert("anything") && rw.can_update("x"));
        let reader = &grants["public_reader"];
        assert!(reader.select && !reader.can_insert("email") && !reader.can_update("email"));
        let writer = &grants["public_writer"];
        assert!(!writer.select);
        assert!(writer.can_insert("email") && !writer.can_insert("role"));
        assert!(writer.can_update("name") && !writer.can_update("email"));
        assert!(grants["mail_worker"].select);
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
