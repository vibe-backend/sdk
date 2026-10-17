use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Col {
    pub name: String,
    pub ty: String,
}

#[derive(Clone, Debug)]
pub struct Sort {
    pub column: String,
    pub desc: bool,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Insert {
        table: String,
        values: Vec<(Col, Value)>,
        returning: Vec<Col>,
    },
    Select {
        table: String,
        columns: Vec<Col>,
        id: Option<String>,
        filters: Vec<(Col, Value)>,
        sort: Option<Sort>,
        limit: i64,
        offset: i64,
    },
    Update {
        table: String,
        id: String,
        sets: Vec<(Col, Value)>,
        expected_updated_at: String,
        touched_at: String,
        returning: Vec<Col>,
        owner: Option<(Col, Value)>,
    },
    SoftDelete {
        table: String,
        id: String,
        touched_at: String,
        owner: Option<(Col, Value)>,
    },
}

pub fn render(stmt: &Stmt) -> Result<(String, Vec<Option<String>>), String> {
    match stmt {
        Stmt::Insert {
            table,
            values,
            returning,
        } => render_insert(table, values, returning),
        Stmt::Select {
            table,
            columns,
            id,
            filters,
            sort,
            limit,
            offset,
        } => render_select(
            table,
            columns,
            id.as_deref(),
            filters,
            sort.as_ref(),
            *limit,
            *offset,
        ),
        Stmt::Update {
            table,
            id,
            sets,
            expected_updated_at,
            returning,
            owner,
            ..
        } => render_update(
            table,
            id,
            sets,
            expected_updated_at,
            returning,
            owner.as_ref(),
        ),
        Stmt::SoftDelete {
            table, id, owner, ..
        } => render_soft_delete(table, id, owner.as_ref()),
    }
}

fn render_insert(
    table: &str,
    values: &[(Col, Value)],
    returning: &[Col],
) -> Result<(String, Vec<Option<String>>), String> {
    let table = quote(table)?;
    let mut cols = Vec::new();
    let mut placeholders = Vec::new();
    let mut params = Vec::new();
    for (col, value) in values {
        cols.push(quote(&col.name)?);
        if matches!(col.name.as_str(), "created_at" | "updated_at") && value.is_null() {
            placeholders.push("now()".into());
            continue;
        }
        params.push(json_text(value));
        placeholders.push(format!("${}::text::{}", params.len(), cast(&col.ty)));
    }
    let ret = returning
        .iter()
        .map(|col| Ok(format!("{}::text", quote(&col.name)?)))
        .collect::<Result<Vec<_>, String>>()?
        .join(", ");
    // An empty list (no readable column) still needs one expression.
    let ret = if ret.is_empty() {
        "NULL".to_string()
    } else {
        ret
    };
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({}) RETURNING {ret}",
        cols.join(", "),
        placeholders.join(", ")
    );
    Ok((sql, params))
}

fn render_select(
    table: &str,
    columns: &[Col],
    id: Option<&str>,
    filters: &[(Col, Value)],
    sort: Option<&Sort>,
    limit: i64,
    offset: i64,
) -> Result<(String, Vec<Option<String>>), String> {
    let table = quote(table)?;
    let cols = columns
        .iter()
        .map(|col| {
            Ok(format!(
                "{}::text AS {}",
                quote(&col.name)?,
                quote(&col.name)?
            ))
        })
        .collect::<Result<Vec<_>, String>>()?
        .join(", ");
    // An empty list (no readable column) still needs one expression.
    let cols = if cols.is_empty() {
        "NULL".to_string()
    } else {
        cols
    };
    let mut params = Vec::new();
    let mut where_sql = vec!["\"deleted_at\" IS NULL".to_string()];
    if let Some(id) = id {
        params.push(Some(id.to_string()));
        where_sql.push(format!("\"id\" = ${}::text::uuid", params.len()));
    }
    for (col, value) in filters {
        params.push(json_text(value));
        let n = params.len();
        where_sql.push(format!(
            "(${n}::text IS NULL OR {} = ${n}::text::{})",
            quote(&col.name)?,
            cast(&col.ty)
        ));
    }
    let order = match sort {
        Some(sort) => format!(
            " ORDER BY {} {}",
            quote(&sort.column)?,
            if sort.desc { "DESC" } else { "ASC" }
        ),
        None => String::new(),
    };
    params.push(Some(limit.to_string()));
    let limit_n = params.len();
    params.push(Some(offset.to_string()));
    let offset_n = params.len();
    let sql = format!(
        "SELECT {cols} FROM {table} WHERE {}{order} LIMIT ${limit_n}::text::bigint OFFSET ${offset_n}::text::bigint",
        where_sql.join(" AND ")
    );
    Ok((sql, params))
}

fn render_soft_delete(
    table: &str,
    id: &str,
    owner: Option<&(Col, Value)>,
) -> Result<(String, Vec<Option<String>>), String> {
    let table = quote(table)?;
    let mut params = vec![Some(id.to_string())];
    let owner_sql = owner_predicate(owner, &mut params)?;
    Ok((
        format!(
            "UPDATE {table} SET \"deleted_at\" = now(), \"updated_at\" = now() WHERE \"id\" = $1::text::uuid AND \"deleted_at\" IS NULL{owner_sql} RETURNING \"id\"::text"
        ),
        params,
    ))
}

fn owner_predicate(
    owner: Option<&(Col, Value)>,
    params: &mut Vec<Option<String>>,
) -> Result<String, String> {
    let Some((col, value)) = owner else {
        return Ok(String::new());
    };
    params.push(json_text(value));
    Ok(format!(
        " AND {} = ${}::text::{}",
        quote(&col.name)?,
        params.len(),
        cast(&col.ty)
    ))
}

fn render_update(
    table: &str,
    id: &str,
    sets: &[(Col, Value)],
    expected_updated_at: &str,
    returning: &[Col],
    owner: Option<&(Col, Value)>,
) -> Result<(String, Vec<Option<String>>), String> {
    let table = quote(table)?;
    let mut params = Vec::new();
    let mut assigns = vec!["\"updated_at\" = now()".to_string()];
    for (col, value) in sets {
        params.push(json_text(value));
        assigns.push(format!(
            "{} = ${}::text::{}",
            quote(&col.name)?,
            params.len(),
            cast(&col.ty)
        ));
    }
    params.push(Some(id.to_string()));
    let id_n = params.len();
    params.push(Some(expected_updated_at.to_string()));
    let ts_n = params.len();
    let owner_sql = owner_predicate(owner, &mut params)?;
    let ret = returning
        .iter()
        .map(|col| Ok(format!("{}::text", quote(&col.name)?)))
        .collect::<Result<Vec<_>, String>>()?
        .join(", ");
    // An empty list (no readable column) still needs one expression.
    let ret = if ret.is_empty() {
        "NULL".to_string()
    } else {
        ret
    };
    let sql = format!(
        "UPDATE {table} SET {} WHERE \"id\" = ${id_n}::text::uuid AND \"updated_at\" = ${ts_n}::text::timestamptz AND \"deleted_at\" IS NULL{owner_sql} RETURNING {ret}",
        assigns.join(", ")
    );
    Ok((sql, params))
}

pub fn quote(name: &str) -> Result<String, String> {
    if name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        Ok(format!("\"{name}\""))
    } else {
        Err(format!("unsafe identifier {name}"))
    }
}

fn cast(ty: &str) -> &'static str {
    match ty {
        "uuid" => "uuid",
        "integer" => "integer",
        "number" => "double precision",
        "boolean" => "boolean",
        "timestamp" => "timestamptz",
        "json" => "jsonb",
        _ => "text",
    }
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        other => Some(other.to_string()),
    }
}

/// Schema-level statements for one tenant (decision 97): nobody but the
/// owner may CREATE in `t_<tenant>`, PUBLIC gets nothing, and only that
/// tenant's own roles ([`super::grants::tenant_roles`]) get USAGE. Table
/// grants come from [`super::grants::pg_grant_statements`]; there is no
/// `ALTER DEFAULT PRIVILEGES` (tables a migration creates get no grants).
pub fn tenant_schema_statements(tenant: &str) -> Result<Vec<String>, String> {
    let schema = quote(&format!("t_{tenant}"))?;
    let mut out = vec![format!("REVOKE ALL ON SCHEMA {schema} FROM PUBLIC")];
    for role in super::grants::tenant_roles(tenant)? {
        let role = quote(&role)?;
        out.push(format!("REVOKE CREATE ON SCHEMA {schema} FROM {role}"));
        out.push(format!("GRANT USAGE ON SCHEMA {schema} TO {role}"));
    }
    Ok(out)
}

/// ENABLE + FORCE row level security and the tenant policy for one table of
/// `t_<tenant>`.
pub fn rls_statements(tenant: &str, table: &str) -> Result<Vec<String>, String> {
    rls_statements_for(
        tenant,
        &format!("{}.{}", quote(&format!("t_{tenant}"))?, quote(table)?),
    )
}

/// The `## Policies` `policy:` line the compiler writes into every model
/// lock, and the only policy `vibe migrate` installs (decision 97). `<id>`
/// stands for the tenant id; [`render_policy`] substitutes it, and the
/// `CREATE POLICY` statement is built from exactly that text, so the lock
/// and the database say the same thing.
pub const POLICY_TEMPLATE: &str = "tenant_isolation TO tenant_<id>_rw, tenant_<id>_public_reader, tenant_<id>_public_writer USING (tenant_id = '<id>') WITH CHECK (tenant_id = '<id>')";

/// [`POLICY_TEMPLATE`] for one tenant. `tenant` is `^[a-z0-9_]{8,61}$`, so
/// the literal needs no escaping, and every role name must fit 63 bytes.
pub fn render_policy(tenant: &str) -> Result<String, String> {
    if !super::slugs::valid_tenant(tenant) {
        return Err(super::slugs::TENANT_ID_RULE.into());
    }
    super::grants::tenant_roles(tenant)?;
    Ok(POLICY_TEMPLATE.replace("<id>", tenant))
}

/// What a rendered policy line says, for comparison with `pg_policies`.
#[derive(Debug, PartialEq)]
pub struct PolicyParts {
    pub name: String,
    pub roles: Vec<String>,
    pub using: String,
    pub with_check: String,
}

/// Splits a rendered `<name> TO <roles> USING (<expr>) WITH CHECK (<expr>)`.
pub fn policy_parts(rendered: &str) -> Result<PolicyParts, String> {
    let bad = || format!("unexpected policy line `{rendered}`");
    let (name, rest) = rendered.split_once(" TO ").ok_or_else(bad)?;
    let (roles, rest) = rest.split_once(" USING (").ok_or_else(bad)?;
    let (using, rest) = rest.split_once(") WITH CHECK (").ok_or_else(bad)?;
    let with_check = rest.strip_suffix(')').ok_or_else(bad)?;
    let mut roles: Vec<String> = roles.split(", ").map(str::to_string).collect();
    roles.sort();
    Ok(PolicyParts {
        name: name.to_string(),
        roles,
        using: using.to_string(),
        with_check: with_check.to_string(),
    })
}

/// The policy applies only to the tenant's own roles and compares with the
/// tenant id as a constant (decision 97): `app.tenant_id` never takes part in
/// an access decision. Built from [`render_policy`].
fn rls_statements_for(tenant: &str, qualified: &str) -> Result<Vec<String>, String> {
    let rendered = render_policy(tenant)?;
    let (name, rest) = rendered
        .split_once(' ')
        .ok_or("unexpected policy template")?;
    Ok(vec![
        format!("ALTER TABLE {qualified} ENABLE ROW LEVEL SECURITY"),
        format!("ALTER TABLE {qualified} FORCE ROW LEVEL SECURITY"),
        format!("DROP POLICY IF EXISTS {name} ON {qualified}"),
        format!("CREATE POLICY {name} ON {qualified} {rest}"),
    ])
}

pub fn bootstrap_statements(
    tenant: &str,
    table: &str,
    fields: &[Col],
    uniques: &[String],
) -> Result<Vec<String>, String> {
    let schema = quote(&format!("t_{tenant}"))?;
    let table_name = quote(table)?;
    let qualified = format!("{schema}.{table_name}");
    let mut columns = Vec::new();
    for field in fields {
        let sql_ty = match field.ty.as_str() {
            "uuid" => "uuid",
            "integer" => "integer",
            "number" => "double precision",
            "boolean" => "boolean",
            "timestamp" => "timestamptz",
            "json" => "jsonb",
            "string[]" => "text[]",
            _ => "text",
        };
        let null = if field.name == "id"
            || field.name == "tenant_id"
            || field.name == "created_at"
            || field.name == "updated_at"
        {
            " NOT NULL"
        } else {
            ""
        };
        columns.push(format!("{} {sql_ty}{null}", quote(&field.name)?));
    }
    let mut out = vec![
        format!("CREATE SCHEMA IF NOT EXISTS {schema}"),
        format!(
            "CREATE TABLE IF NOT EXISTS {qualified} ({})",
            columns.join(", ")
        ),
    ];
    out.extend(rls_statements_for(tenant, &qualified)?);
    for unique in uniques {
        let index = quote(&format!("{table}_{unique}_active"))?;
        out.push(format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {index} ON {qualified} ({}, {}) WHERE \"deleted_at\" IS NULL",
            quote("tenant_id")?,
            quote(unique)?
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_schema_grants_only_the_tenants_own_roles() {
        let stmts = tenant_schema_statements("acme0001").unwrap().join("\n");
        assert!(stmts.contains("REVOKE ALL ON SCHEMA \"t_acme0001\" FROM PUBLIC"));
        for role in ["rw", "public_reader", "public_writer"] {
            assert!(stmts.contains(&format!(
                "GRANT USAGE ON SCHEMA \"t_acme0001\" TO \"tenant_acme0001_{role}\""
            )));
        }
        assert!(!stmts.contains("app_rw") && !stmts.contains("DEFAULT PRIVILEGES"));
        assert!(!stmts.contains("DELETE") && !stmts.contains("TRUNCATE"));
        assert!(tenant_schema_statements("bad\"x").is_err());
    }

    #[test]
    fn policy_names_the_tenant_roles_and_a_constant() {
        let sql = rls_statements("acme0001", "notes").unwrap().join("\n");
        assert!(sql.contains(
            "TO tenant_acme0001_rw, tenant_acme0001_public_reader, tenant_acme0001_public_writer"
        ));
        assert!(sql.contains("USING (tenant_id = 'acme0001') WITH CHECK (tenant_id = 'acme0001')"));
        let parts = policy_parts(&render_policy("acme0001").unwrap()).unwrap();
        assert_eq!(parts.name, "tenant_isolation");
        assert_eq!(parts.roles.len(), 3);
        assert_eq!(parts.using, "tenant_id = 'acme0001'");
        assert!(
            render_policy(&"a".repeat(43)).is_err(),
            "role names over 63 bytes are refused"
        );
        assert!(!sql.contains("current_setting"));
        assert!(rls_statements("x'; --", "notes").is_err());
    }
    use serde_json::json;

    #[test]
    fn list_uses_offset_and_hides_deleted_rows() {
        let (sql, _) = render(&Stmt::Select {
            table: "products".into(),
            columns: vec![Col {
                name: "id".into(),
                ty: "uuid".into(),
            }],
            id: None,
            filters: vec![(
                Col {
                    name: "name".into(),
                    ty: "string".into(),
                },
                json!("mug"),
            )],
            sort: Some(Sort {
                column: "created_at".into(),
                desc: false,
            }),
            limit: 21,
            offset: 20,
        })
        .unwrap();
        let lower = sql.to_ascii_lowercase();
        assert!(lower.contains("offset"));
        assert!(lower.contains("limit"));
        assert!(lower.contains("deleted_at\" is null") || lower.contains("\"deleted_at\" is null"));
        assert!(!lower.trim_start().starts_with("delete"));
    }

    #[test]
    fn soft_delete_is_an_update() {
        let (sql, _) = render(&Stmt::SoftDelete {
            table: "products".into(),
            id: "11111111-1111-1111-1111-111111111111".into(),
            touched_at: "2026-10-14T00:00:00Z".into(),
            owner: Some((
                Col {
                    name: "user_id".into(),
                    ty: "string".into(),
                },
                json!("user-1"),
            )),
        })
        .unwrap();
        let lower = sql.to_ascii_lowercase();
        assert!(lower.starts_with("update"));
        assert!(lower.contains("set \"deleted_at\""));
        assert!(lower.contains("\"user_id\""));
        assert!(!lower.contains("delete from"));
    }

    #[test]
    fn bootstrap_enables_rls_and_skips_delete_grants() {
        let sql = bootstrap_statements(
            "tenant0001",
            "products",
            &[
                Col {
                    name: "id".into(),
                    ty: "uuid".into(),
                },
                Col {
                    name: "tenant_id".into(),
                    ty: "string".into(),
                },
            ],
            &[],
        )
        .unwrap()
        .join("\n")
        .to_ascii_lowercase();
        assert!(sql.contains("enable row level security"));
        assert!(sql.contains("force row level security"));
        assert!(sql.contains("tenant_id = 'tenant0001'"));
        assert!(!sql.contains("current_setting"));
        assert!(!sql.contains("grant delete"));
    }
}
