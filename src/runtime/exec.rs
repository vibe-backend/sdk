use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::grants::{parse_grants, Grants, RoleGrant};
use super::sql::{self, Col, Stmt};

pub type Row = Map<String, Value>;

#[derive(Debug)]
pub enum ExecError {
    NotFound,
    Conflict,
    Precondition,
    Message(String),
}

#[async_trait]
pub trait QueryExec: Send + Sync {
    async fn execute(&self, tenant_id: &str, role: &str, stmt: Stmt)
        -> Result<Vec<Row>, ExecError>;
}

/// In-memory executor for `vibe dev` / `serve --memory` and tests.
///
/// Emulates the production database (spec 2C.4, decision 74): tenant
/// isolation as with RLS + FORCE (a statement without a valid tenant fails,
/// rows are only visible to their tenant, a foreign `tenant_id` is refused),
/// and the role and column grants from each model's `## Policies`.
#[derive(Default)]
pub struct MemoryExec {
    tables: Mutex<BTreeMap<String, Vec<Row>>>,
    /// Grants by table. `None` (only from [`MemoryExec::new`]) skips grant
    /// checks; a table missing from `Some(..)` has no grants at all.
    grants: Option<BTreeMap<String, Grants>>,
}

/// Columns the platform fills itself (defaults / triggers in PostgreSQL), so
/// they need no column grant.
const PLATFORM_COLUMNS: &[&str] = &["id", "tenant_id", "created_at", "updated_at"];

impl MemoryExec {
    /// No grant checks; only for unit tests that have no manifest.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enforces the role and column grants declared by the manifest models.
    pub fn from_manifest(manifest: &crate::manifest::Manifest) -> Result<Self, String> {
        let mut grants = BTreeMap::new();
        for model in &manifest.models {
            let parsed = parse_grants(&model.grants)
                .map_err(|err| format!("model {}: grants: {err}", model.id))?;
            grants.insert(model.table.clone(), parsed);
        }
        Ok(Self {
            tables: Mutex::default(),
            grants: Some(grants),
        })
    }

    fn role_grant(&self, table: &str, role: &str) -> Result<Option<RoleGrant>, ExecError> {
        let Some(grants) = &self.grants else {
            return Ok(None);
        };
        Ok(Some(
            grants
                .get(table)
                .and_then(|by_role| by_role.get(role))
                .cloned()
                .unwrap_or_default(),
        ))
    }
}

fn denied(table: &str) -> ExecError {
    ExecError::Message(format!("permission denied for table {table}"))
}

fn denied_column(column: &str, table: &str) -> ExecError {
    ExecError::Message(format!(
        "permission denied for column {column} of table {table}"
    ))
}

#[async_trait]
impl QueryExec for MemoryExec {
    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
        // Same as current_setting('app.tenant_id', false): no tenant is an
        // error, not an empty result.
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        self.check_grants(tenant_id, role, &stmt)?;
        role_allows(role, &stmt).map_err(ExecError::Message)?;
        let mut tables = self
            .tables
            .lock()
            .map_err(|_| ExecError::Message("lock".into()))?;
        match stmt {
            Stmt::Insert {
                table,
                values,
                returning,
            } => {
                let key = key(tenant_id, &table);
                let rows = tables.entry(key).or_default();
                let mut row = Map::new();
                for (col, value) in &values {
                    row.insert(col.name.clone(), value.clone());
                }
                row.insert("tenant_id".into(), Value::String(tenant_id.into()));
                rows.push(row.clone());
                Ok(vec![project(&row, &returning)])
            }
            Stmt::Select {
                table,
                columns,
                id,
                filters,
                sort,
                limit,
                offset,
            } => {
                let key = key(tenant_id, &table);
                let mut rows: Vec<Row> = tables
                    .get(&key)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|row| row.get("deleted_at").is_none_or(Value::is_null))
                    .filter(|row| {
                        id.as_ref()
                            .is_none_or(|id| row.get("id").and_then(Value::as_str) == Some(id))
                    })
                    .filter(|row| {
                        filters.iter().all(|(col, value)| {
                            value.is_null()
                                || row.get(&col.name) == Some(value)
                                || values_eq(row.get(&col.name), value)
                        })
                    })
                    .collect();
                if let Some(sort) = sort {
                    rows.sort_by(|a, b| {
                        let av = a.get(&sort.column).cloned().unwrap_or(Value::Null);
                        let bv = b.get(&sort.column).cloned().unwrap_or(Value::Null);
                        let order = json_cmp(&av, &bv);
                        if sort.desc {
                            order.reverse()
                        } else {
                            order
                        }
                    });
                }
                let offset = offset.max(0) as usize;
                let limit = limit.max(0) as usize;
                let slice = rows
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(|row| project(&row, &columns));
                Ok(slice.collect())
            }
            Stmt::Update {
                table,
                id,
                sets,
                expected_updated_at,
                touched_at,
                returning,
                owner,
            } => {
                let key = key(tenant_id, &table);
                let rows = tables.get_mut(&key).ok_or(ExecError::NotFound)?;
                let row = rows
                    .iter_mut()
                    .find(|row| {
                        row.get("id").and_then(Value::as_str) == Some(id.as_str())
                            && row.get("deleted_at").is_none_or(Value::is_null)
                            && owner_matches(row, owner.as_ref())
                    })
                    .ok_or(ExecError::NotFound)?;
                if row.get("updated_at").and_then(Value::as_str)
                    != Some(expected_updated_at.as_str())
                {
                    return Err(ExecError::Precondition);
                }
                for (col, value) in sets {
                    row.insert(col.name, value);
                }
                row.insert("updated_at".into(), Value::String(touched_at));
                Ok(vec![project(row, &returning)])
            }
            Stmt::SoftDelete {
                table,
                id,
                touched_at,
                owner,
            } => {
                let key = key(tenant_id, &table);
                let rows = tables.get_mut(&key).ok_or(ExecError::NotFound)?;
                let row = rows
                    .iter_mut()
                    .find(|row| {
                        row.get("id").and_then(Value::as_str) == Some(id.as_str())
                            && row.get("deleted_at").is_none_or(Value::is_null)
                            && owner_matches(row, owner.as_ref())
                    })
                    .ok_or(ExecError::NotFound)?;
                row.insert("deleted_at".into(), Value::String(touched_at.clone()));
                row.insert("updated_at".into(), Value::String(touched_at));
                Ok(vec![row.clone()])
            }
        }
    }
}

impl MemoryExec {
    fn check_grants(&self, tenant_id: &str, role: &str, stmt: &Stmt) -> Result<(), ExecError> {
        let table = match stmt {
            Stmt::Insert { table, .. }
            | Stmt::Select { table, .. }
            | Stmt::Update { table, .. }
            | Stmt::SoftDelete { table, .. } => table,
        };
        // RLS WITH CHECK: a row for another tenant is refused for every role.
        let foreign = |cols: &[(Col, Value)]| {
            cols.iter()
                .any(|(col, value)| col.name == "tenant_id" && value.as_str() != Some(tenant_id))
        };
        match stmt {
            Stmt::Insert { values, .. } if foreign(values) => {
                return Err(ExecError::Message(format!(
                    "new row violates row-level security policy for table {table}"
                )))
            }
            Stmt::Update { sets, .. } if sets.iter().any(|(col, _)| col.name == "tenant_id") => {
                return Err(denied_column("tenant_id", table))
            }
            _ => {}
        }
        let Some(grant) = self.role_grant(table, role)? else {
            return Ok(());
        };
        let platform = |name: &str| PLATFORM_COLUMNS.contains(&name);
        match stmt {
            Stmt::Select { .. } => {
                if !grant.select {
                    return Err(denied(table));
                }
            }
            Stmt::Insert { values, .. } => {
                if grant.insert.is_none() {
                    return Err(denied(table));
                }
                for (col, _) in values {
                    if !platform(&col.name) && !grant.can_insert(&col.name) {
                        return Err(denied_column(&col.name, table));
                    }
                }
            }
            Stmt::Update { sets, .. } => {
                if grant.update.is_none() {
                    return Err(denied(table));
                }
                for (col, _) in sets {
                    if !platform(&col.name) && !grant.can_update(&col.name) {
                        return Err(denied_column(&col.name, table));
                    }
                }
            }
            Stmt::SoftDelete { .. } => {
                if grant.update.is_none() {
                    return Err(denied(table));
                }
                if !grant.can_update("deleted_at") {
                    return Err(denied_column("deleted_at", table));
                }
            }
        }
        Ok(())
    }
}

fn owner_matches(row: &Row, owner: Option<&(Col, Value)>) -> bool {
    let Some((col, value)) = owner else {
        return true;
    };
    row.get(&col.name) == Some(value) || values_eq(row.get(&col.name), value)
}

fn key(tenant: &str, table: &str) -> String {
    format!("{tenant}\u{0}{table}")
}

fn values_eq(left: Option<&Value>, right: &Value) -> bool {
    match (left, right) {
        (Some(Value::String(a)), Value::String(b)) => a == b,
        (Some(Value::Number(a)), Value::Number(b)) => a == b,
        _ => false,
    }
}

fn json_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => {
            a.as_i64().unwrap_or(0).cmp(&b.as_i64().unwrap_or(0))
        }
        _ => a.to_string().cmp(&b.to_string()),
    }
}

fn project(row: &Row, columns: &[Col]) -> Row {
    let mut out = Map::new();
    for col in columns {
        if let Some(value) = row.get(&col.name) {
            out.insert(col.name.clone(), value.clone());
        }
    }
    out
}

/// Privileges of the runtime connection role, as read from the catalog.
/// Everything is evaluated with `pg_has_role(.., 'MEMBER')`, so a role that
/// could `SET ROLE` to a privileged or owning role (even through NOINHERIT
/// membership) counts as having that privilege.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoleStatus {
    pub name: String,
    /// Superuser roles the connection role is (or can become).
    pub superuser_via: Vec<String>,
    /// BYPASSRLS roles the connection role is (or can become).
    pub bypass_rls_via: Vec<String>,
    /// Tenant-schema relations (tables, views, sequences, ...) it can own.
    pub owned_relations: i64,
    /// Tenant-schema functions/procedures it can own.
    pub owned_functions: i64,
    /// Tenant-schema types it can own.
    pub owned_types: i64,
    /// Tenant schemas it can own.
    pub owned_schemas: i64,
    /// Tenant relations the role can read or write that are not tables with
    /// ENABLE + FORCE row level security (views and RLS-less tables would
    /// expose every tenant).
    pub unprotected_relations: Vec<String>,
    /// Policies on tenant tables other than the canonical `tenant_isolation`
    /// (permissive policies are OR-combined, so any extra one widens access),
    /// plus a `tenant_isolation` whose definition was altered.
    pub foreign_policies: Vec<String>,
}

impl RoleStatus {
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.superuser_via.is_empty() {
            out.push(format!(
                "is or can become a superuser ({})",
                self.superuser_via.join(", ")
            ));
        }
        if !self.bypass_rls_via.is_empty() {
            out.push(format!(
                "is or can become a BYPASSRLS role ({})",
                self.bypass_rls_via.join(", ")
            ));
        }
        for (count, what) in [
            (self.owned_relations, "tenant relation(s)"),
            (self.owned_functions, "tenant function(s)"),
            (self.owned_types, "tenant type(s)"),
            (self.owned_schemas, "tenant schema(s)"),
        ] {
            if count > 0 {
                out.push(format!("can own {count} {what}"));
            }
        }
        if !self.unprotected_relations.is_empty() {
            out.push(format!(
                "can access tenant relation(s) without ENABLE + FORCE ROW LEVEL SECURITY ({}); re-run `vibe migrate` or enable RLS on them",
                self.unprotected_relations.join(", ")
            ));
        }
        if !self.foreign_policies.is_empty() {
            out.push(format!(
                "tenant table(s) carry policies other than the canonical tenant_isolation ({}); permissive policies are OR-combined and would let rows leak across tenants; drop them and re-run `vibe migrate`",
                self.foreign_policies.join(", ")
            ));
        }
        out
    }
}

/// Catalog query behind [`PgExec::check_runtime_role`].
pub const ROLE_STATUS_SQL: &str = "\
SELECT current_user::text, \
 COALESCE((SELECT array_agg(m.rolname::text ORDER BY m.rolname) FROM pg_roles m \
   WHERE m.rolsuper AND pg_has_role(current_user, m.oid, 'MEMBER')), '{}'), \
 COALESCE((SELECT array_agg(m.rolname::text ORDER BY m.rolname) FROM pg_roles m \
   WHERE m.rolbypassrls AND pg_has_role(current_user, m.oid, 'MEMBER')), '{}'), \
 (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE n.nspname LIKE 't\\_%' AND pg_has_role(current_user, c.relowner, 'MEMBER')), \
 (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
   WHERE n.nspname LIKE 't\\_%' AND pg_has_role(current_user, p.proowner, 'MEMBER')), \
 (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
   WHERE n.nspname LIKE 't\\_%' AND t.typrelid = 0 \
   AND pg_has_role(current_user, t.typowner, 'MEMBER')), \
 (SELECT count(*) FROM pg_namespace n WHERE n.nspname LIKE 't\\_%' \
   AND pg_has_role(current_user, n.nspowner, 'MEMBER')), \
 COALESCE((SELECT array_agg(format('%I.%I', n.nspname, c.relname) ORDER BY 1) \
   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE n.nspname LIKE 't\\_%' AND c.relkind IN ('r','p','v','m','f') \
   AND has_table_privilege(current_user, c.oid, 'SELECT,INSERT,UPDATE,DELETE') \
   AND NOT (c.relkind IN ('r','p') AND c.relrowsecurity AND c.relforcerowsecurity)), '{}'), \
 COALESCE((SELECT array_agg(format('%I.%I:%I', n.nspname, c.relname, p.polname) ORDER BY 1) \
   FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
   JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE n.nspname LIKE 't\\_%' \
   AND has_table_privilege(current_user, c.oid, 'SELECT,INSERT,UPDATE,DELETE') AND NOT ( \
     p.polname = 'tenant_isolation' AND p.polpermissive AND p.polcmd = '*' \
     AND p.polroles = '{0}'::oid[] \
     AND pg_get_expr(p.polqual, p.polrelid) = '(tenant_id = current_setting(''app.tenant_id''::text, false))' \
     AND pg_get_expr(p.polwithcheck, p.polrelid) = '(tenant_id = current_setting(''app.tenant_id''::text, false))')), '{}')";

/// Logical caller classes passed as `role` to [`QueryExec::execute`], and the
/// statements each may run. Enforced by every executor (spec §7.1/§7.2:
/// public readers only read; nobody deletes).
pub fn role_allows(role: &str, stmt: &Stmt) -> Result<(), String> {
    let allowed = match role {
        "public_reader" => matches!(stmt, Stmt::Select { .. }),
        "public_writer" | "app_rw" => matches!(
            stmt,
            Stmt::Select { .. }
                | Stmt::Insert { .. }
                | Stmt::Update { .. }
                | Stmt::SoftDelete { .. }
        ),
        other => return Err(format!("unknown executor role {other}")),
    };
    if allowed {
        Ok(())
    } else {
        Err(format!("role {role} may not run this statement"))
    }
}

pub struct PgExec {
    client: tokio::sync::Mutex<tokio_postgres::Client>,
}

impl PgExec {
    pub async fn connect(url: &str) -> Result<Self, String> {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .map_err(pg_error)?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::error!(%err, "postgres connection ended");
            }
        });
        Ok(Self {
            client: tokio::sync::Mutex::new(client),
        })
    }

    /// Refuses a connection role that can bypass row-level security: a
    /// superuser, a role with BYPASSRLS, or a role that owns (directly or via
    /// membership) any tenant schema or table. Spec §7.1: the connection role
    /// must not own the tables and must not have BYPASSRLS.
    pub async fn check_runtime_role(&self) -> Result<(), String> {
        let client = self.client.lock().await;
        let row = client
            .query_one(ROLE_STATUS_SQL, &[])
            .await
            .map_err(pg_error)?;
        drop(client);
        let status = RoleStatus {
            name: row.get(0),
            superuser_via: row.get(1),
            bypass_rls_via: row.get(2),
            owned_relations: row.get(3),
            owned_functions: row.get(4),
            owned_types: row.get(5),
            owned_schemas: row.get(6),
            unprotected_relations: row.get(7),
            foreign_policies: row.get(8),
        };
        let problems = status.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "refusing to serve: database role `{}` {}; connect with a non-owner role without BYPASSRLS (use VIBE_MIGRATE_DATABASE_URL for `vibe migrate`)",
                status.name,
                problems.join(", ")
            ))
        }
    }

    /// Runs exactly one SQL command over the extended protocol; the server
    /// rejects text that contains more than one.
    pub async fn execute_one(&self, statement: &str) -> Result<(), String> {
        let client = self.client.lock().await;
        client.execute(statement, &[]).await.map_err(pg_error)?;
        Ok(())
    }

    /// Runs a query returning one text column.
    pub async fn query_texts(&self, sql: &str, param: &str) -> Result<Vec<String>, String> {
        let client = self.client.lock().await;
        let rows = client.query(sql, &[&param]).await.map_err(pg_error)?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    pub async fn batch(&self, statements: &[String]) -> Result<(), String> {
        let client = self.client.lock().await;
        for statement in statements {
            client.batch_execute(statement).await.map_err(pg_error)?;
        }
        Ok(())
    }
}

#[async_trait]
impl QueryExec for PgExec {
    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        // `role` is the logical caller class. It limits which statements may
        // run; the database additionally enforces the non-owner connection
        // role (checked by `check_runtime_role`) and its migrate-time grants.
        role_allows(role, &stmt).map_err(ExecError::Message)?;
        let (sql, params) = sql::render(&stmt).map_err(ExecError::Message)?;
        let returning = returning_cols(&stmt);
        let mut client = self.client.lock().await;
        let tx = client
            .transaction()
            .await
            .map_err(|err| ExecError::Message(pg_error(err)))?;
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(|err| ExecError::Message(pg_error(err)))?;
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as _).collect();
        let rows = tx.query(&sql, &refs).await.map_err(map_pg)?;
        tx.commit()
            .await
            .map_err(|err| ExecError::Message(pg_error(err)))?;
        if rows.is_empty() {
            return match stmt {
                Stmt::SoftDelete { .. } => Err(ExecError::NotFound),
                Stmt::Update { .. } => Err(ExecError::Precondition),
                _ => Ok(Vec::new()),
            };
        }
        let mut out = Vec::new();
        for row in rows {
            let mut mapped = Map::new();
            for (idx, col) in returning.iter().enumerate() {
                let text: Option<String> = row
                    .try_get(idx)
                    .map_err(|err| ExecError::Message(pg_error(err)))?;
                mapped.insert(col.name.clone(), parse_cell(text, &col.ty));
            }
            out.push(mapped);
        }
        Ok(out)
    }
}

fn returning_cols(stmt: &Stmt) -> Vec<Col> {
    match stmt {
        Stmt::Insert { returning, .. } | Stmt::Update { returning, .. } => returning.clone(),
        Stmt::Select { columns, .. } => columns.clone(),
        Stmt::SoftDelete { .. } => vec![Col {
            name: "id".into(),
            ty: "uuid".into(),
        }],
    }
}

fn parse_cell(text: Option<String>, ty: &str) -> Value {
    let Some(text) = text else {
        return Value::Null;
    };
    match ty {
        "integer" => text
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or(Value::String(text)),
        "number" => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(text)),
        "boolean" => Value::Bool(text == "true" || text == "t"),
        _ => Value::String(text),
    }
}

fn map_pg(err: tokio_postgres::Error) -> ExecError {
    let code = err.code().map(|code| code.code());
    if code == Some("23505") {
        ExecError::Conflict
    } else {
        ExecError::Message(pg_error(err))
    }
}

/// A Postgres error with the server's message, detail, hint and SQLSTATE
/// (tokio-postgres's `Display` only says "db error"). For logs and the CLI;
/// HTTP responses never include it.
pub fn pg_error(err: tokio_postgres::Error) -> String {
    let Some(db) = err.as_db_error() else {
        return err.to_string();
    };
    let mut out = format!("{}: {} ({})", db.severity(), db.message(), db.code().code());
    if let Some(detail) = db.detail() {
        out.push_str(&format!("; detail: {detail}"));
    }
    if let Some(hint) = db.hint() {
        out.push_str(&format!("; hint: {hint}"));
    }
    if let Some(schema) = db.schema() {
        out.push_str(&format!("; schema: {schema}"));
    }
    if let Some(table) = db.table() {
        out.push_str(&format!("; table: {table}"));
    }
    out
}

pub fn etag(id: &str, updated_at: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(b"|");
    hasher.update(updated_at.as_bytes());
    format!("\"{}\"", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::manifest::{ListOpts, Manifest, ModelSpec};
    use serde_json::json;

    fn col(name: &str) -> Col {
        Col {
            name: name.into(),
            ty: "string".into(),
        }
    }

    fn exec(grants: &str) -> MemoryExec {
        let manifest = Manifest {
            models: vec![ModelSpec {
                id: "user".into(),
                provider: "main".into(),
                table: "users".into(),
                fields: vec![],
                crud: vec![],
                list: ListOpts::default(),
                sensitive_output: vec![],
                owner_key: None,
                ownership: String::new(),
                grants: grants.into(),
            }],
            manifest_version: 1,
            routes: vec![],
            methods: vec![],
            migrations: vec![],
            signature: String::new(),
        };
        MemoryExec::from_manifest(&manifest).unwrap()
    }

    fn insert(tenant: &str, cols: &[(&str, &str)]) -> Stmt {
        let mut values: Vec<(Col, Value)> = cols.iter().map(|(c, v)| (col(c), json!(v))).collect();
        values.push((col("tenant_id"), json!(tenant)));
        Stmt::Insert {
            table: "users".into(),
            values,
            returning: vec![col("id"), col("email")],
        }
    }

    fn select() -> Stmt {
        Stmt::Select {
            table: "users".into(),
            columns: vec![col("id"), col("email")],
            id: None,
            filters: vec![],
            sort: None,
            limit: 100,
            offset: 0,
        }
    }

    fn update(id: &str, column: &str) -> Stmt {
        Stmt::Update {
            table: "users".into(),
            id: id.into(),
            sets: vec![(col(column), json!("x"))],
            expected_updated_at: String::new(),
            touched_at: "t1".into(),
            returning: vec![col("id")],
            owner: None,
        }
    }

    fn message(err: ExecError) -> String {
        match err {
            ExecError::Message(message) => message,
            other => panic!("unexpected {other:?}"),
        }
    }

    const GRANTS: &str = "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer [INSERT(email), UPDATE(name)]";

    #[tokio::test]
    async fn tenants_only_see_their_own_rows() {
        let exec = exec(GRANTS);
        let a = "tenant0001";
        let b = "tenant0002";
        exec.execute(a, "app_rw", insert(a, &[("id", "1"), ("email", "a@x")]))
            .await
            .unwrap();
        assert_eq!(exec.execute(a, "app_rw", select()).await.unwrap().len(), 1);
        assert!(exec
            .execute(b, "app_rw", select())
            .await
            .unwrap()
            .is_empty());
        // Another tenant cannot update the row either.
        assert!(matches!(
            exec.execute(b, "app_rw", update("1", "email")).await,
            Err(ExecError::NotFound)
        ));
    }

    #[tokio::test]
    async fn missing_tenant_is_an_error_not_an_empty_result() {
        let exec = exec(GRANTS);
        let err = message(exec.execute("", "app_rw", select()).await.unwrap_err());
        assert!(err.contains("tenant"), "{err}");
    }

    #[tokio::test]
    async fn a_row_for_another_tenant_is_refused() {
        let exec = exec(GRANTS);
        let err = message(
            exec.execute("tenant0001", "app_rw", insert("tenant0002", &[("id", "1")]))
                .await
                .unwrap_err(),
        );
        assert!(err.contains("row-level security"), "{err}");
        let err = message(
            exec.execute("tenant0001", "app_rw", update("1", "tenant_id"))
                .await
                .unwrap_err(),
        );
        assert!(
            err.contains("permission denied for column tenant_id"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn public_reader_can_only_select() {
        let exec = exec(GRANTS);
        let t = "tenant0001";
        exec.execute(t, "public_reader", select()).await.unwrap();
        let err = message(
            exec.execute(t, "public_reader", insert(t, &[("email", "a@x")]))
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for table users");
        let err = message(
            exec.execute(t, "public_reader", update("1", "email"))
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for table users");
    }

    #[tokio::test]
    async fn public_writer_is_limited_to_granted_columns() {
        let exec = exec(GRANTS);
        let t = "tenant0001";
        exec.execute(
            t,
            "public_writer",
            insert(t, &[("id", "1"), ("email", "a@x")]),
        )
        .await
        .unwrap();
        let err = message(
            exec.execute(
                t,
                "public_writer",
                insert(t, &[("id", "2"), ("role", "admin")]),
            )
            .await
            .unwrap_err(),
        );
        assert_eq!(err, "permission denied for column role of table users");
        let err = message(
            exec.execute(t, "public_writer", select())
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for table users");
        let err = message(
            exec.execute(t, "public_writer", update("1", "email"))
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for column email of table users");
        let err = message(
            exec.execute(
                t,
                "public_writer",
                Stmt::SoftDelete {
                    table: "users".into(),
                    id: "1".into(),
                    touched_at: "t1".into(),
                    owner: None,
                },
            )
            .await
            .unwrap_err(),
        );
        assert_eq!(
            err,
            "permission denied for column deleted_at of table users"
        );
    }

    #[tokio::test]
    async fn empty_grants_deny_everything_and_unknown_roles_get_nothing() {
        let exec = exec("public_writer []");
        let err = message(
            exec.execute("tenant0001", "app_rw", select())
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for table users");
        let err = message(
            exec.execute("tenant0001", "public_writer", select())
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for table users");
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;

    #[test]
    fn safe_role_has_no_problems() {
        assert!(RoleStatus::default().problems().is_empty());
    }

    #[test]
    fn unsafe_roles_are_reported() {
        let s = RoleStatus {
            name: "app".into(),
            superuser_via: vec!["postgres".into()],
            bypass_rls_via: vec!["etl".into()],
            owned_relations: 2,
            owned_functions: 1,
            owned_types: 1,
            owned_schemas: 1,
            unprotected_relations: vec!["t_x.later".into()],
            foreign_policies: vec!["t_x.notes:open".into()],
        };
        let p = s.problems();
        assert_eq!(p.len(), 8, "{p:?}");
        assert!(p[6].contains("t_x.later"));
        assert!(p[7].contains("t_x.notes:open") && p[7].contains("OR-combined"));
        assert!(p[0].contains("postgres"));
        assert!(p[1].contains("etl"));
    }

    fn select() -> Stmt {
        Stmt::Select {
            table: "t".into(),
            columns: vec![],
            id: None,
            filters: vec![],
            sort: None,
            limit: 1,
            offset: 0,
        }
    }

    #[test]
    fn executor_role_is_enforced() {
        assert!(role_allows("public_reader", &select()).is_ok());
        assert!(role_allows("app_rw", &select()).is_ok());
        assert!(role_allows("bogus", &select()).is_err());
        let insert = Stmt::Insert {
            table: "t".into(),
            values: vec![],
            returning: vec![],
        };
        assert!(role_allows("public_reader", &insert).is_err());
        assert!(role_allows("public_writer", &insert).is_ok());
    }
}
