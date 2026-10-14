use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::refresh::{RefreshPolicy, RevokeTarget, Rotated};
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

    /// Runs a lock `query` step: validated, parameterised SQL in the tenant
    /// schema under row level security. Params are sent as text and parsed
    /// by PostgreSQL for whatever type the statement needs. Only PostgreSQL
    /// implements this.
    ///
    /// `role` is `public_reader` or `public_writer` (spec §12.8): the statement
    /// runs under `SET LOCAL ROLE`, and in a READ ONLY transaction unless
    /// `writes`.
    async fn raw(
        &self,
        _tenant_id: &str,
        _role: &str,
        _writes: bool,
        _sql: &str,
        _params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        Err(ExecError::Message(
            "query steps need PostgreSQL; the --memory executor does not run them".into(),
        ))
    }

    /// Stores a new refresh family; returns the family id.
    async fn refresh_issue(
        &self,
        tenant_id: &str,
        user_id: &str,
        token_hash: &str,
        policy: RefreshPolicy,
    ) -> Result<String, ExecError>;

    /// Presents a refresh token (by hash) and atomically rotates it to
    /// `new_hash` (see [`super::refresh`]).
    async fn refresh_rotate(
        &self,
        tenant_id: &str,
        token_hash: &str,
        new_hash: &str,
        policy: RefreshPolicy,
    ) -> Result<Rotated, ExecError>;

    async fn refresh_revoke(
        &self,
        tenant_id: &str,
        target: RevokeTarget<'_>,
    ) -> Result<u64, ExecError>;

    /// Family id of a stored refresh token hash (for the per-family limit).
    async fn refresh_family(
        &self,
        tenant_id: &str,
        token_hash: &str,
    ) -> Result<Option<String>, ExecError>;

    /// Platform lookup for `sign-jwt`: `users.roles` of the user whose `id`
    /// is `user_id`; `None` when there is no such user.
    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, ExecError>;
}

/// Database roles a `query` step may run as.
pub const QUERY_ROLES: [&str; 2] = ["public_reader", "public_writer"];

fn roles_from(value: Option<&Value>) -> Result<Vec<String>, ExecError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ExecError::Message("users.roles must be text[]".into()))
            })
            .collect(),
        Some(_) => Err(ExecError::Message("users.roles must be text[]".into())),
    }
}

/// A query parameter sent in text format so PostgreSQL infers its type.
#[derive(Debug)]
struct TextParam(Option<String>);

impl tokio_postgres::types::ToSql for TextParam {
    fn to_sql(
        &self,
        _ty: &tokio_postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match &self.0 {
            Some(text) => {
                out.extend_from_slice(text.as_bytes());
                Ok(tokio_postgres::types::IsNull::No)
            }
            None => Ok(tokio_postgres::types::IsNull::Yes),
        }
    }

    fn accepts(_ty: &tokio_postgres::types::Type) -> bool {
        true
    }

    fn encode_format(&self, _ty: &tokio_postgres::types::Type) -> tokio_postgres::types::Format {
        tokio_postgres::types::Format::Text
    }

    tokio_postgres::types::to_sql_checked!();
}

const REFRESH_TABLE: &str = "_platform_refresh_tokens";

#[derive(Default)]
pub struct MemoryExec {
    tables: Mutex<BTreeMap<String, Vec<Row>>>,
    /// Test stub for `query` steps (the memory executor runs no SQL).
    query_stub: Mutex<Option<String>>,
}

impl MemoryExec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tests only: every `query` step returns one row `{column: <first
    /// param>}` instead of the PostgreSQL-only error (a stand-in lookup).
    pub fn stub_queries(&self, column: &str) {
        if let Ok(mut stub) = self.query_stub.lock() {
            *stub = Some(column.to_string());
        }
    }

    /// Adds a row to a tenant table directly (dev data and tests).
    pub fn seed(&self, tenant_id: &str, table: &str, row: Row) {
        if let Ok(mut tables) = self.tables.lock() {
            tables.entry(key(tenant_id, table)).or_default().push(row);
        }
    }
}

#[async_trait]
impl QueryExec for MemoryExec {
    async fn raw(
        &self,
        _tenant_id: &str,
        _role: &str,
        _writes: bool,
        _sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        let stub = self.query_stub.lock().ok().and_then(|s| s.clone());
        let Some(column) = stub else {
            return Err(ExecError::Message(
                "query steps need PostgreSQL; the --memory executor does not run them".into(),
            ));
        };
        let mut row = Row::new();
        row.insert(column, params.into_iter().next().flatten().into());
        Ok(vec![row])
    }

    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, ExecError> {
        let tables = self
            .tables
            .lock()
            .map_err(|_| ExecError::Message("memory store poisoned".into()))?;
        let Some(row) = tables.get(&key(tenant_id, "users")).and_then(|rows| {
            rows.iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(user_id))
        }) else {
            return Ok(None);
        };
        roles_from(row.get("roles")).map(Some)
    }

    async fn refresh_family(
        &self,
        tenant_id: &str,
        token_hash: &str,
    ) -> Result<Option<String>, ExecError> {
        let tables = self
            .tables
            .lock()
            .map_err(|_| ExecError::Message("memory store poisoned".into()))?;
        Ok(tables.get(&key(tenant_id, REFRESH_TABLE)).and_then(|rows| {
            rows.iter()
                .find(|r| {
                    r.get("token_hash").and_then(Value::as_str) == Some(token_hash)
                        && r.get("revoked_at").is_none_or(Value::is_null)
                })
                .and_then(|r| r.get("family_id").and_then(Value::as_str))
                .map(str::to_string)
        }))
    }

    async fn refresh_issue(
        &self,
        tenant_id: &str,
        user_id: &str,
        token_hash: &str,
        policy: RefreshPolicy,
    ) -> Result<String, ExecError> {
        let mut tables = self
            .tables
            .lock()
            .map_err(|_| ExecError::Message("memory store poisoned".into()))?;
        let rows = tables.entry(key(tenant_id, REFRESH_TABLE)).or_default();
        Ok(super::refresh::memory_issue(
            rows,
            tenant_id,
            user_id,
            token_hash,
            policy,
            chrono::Utc::now().timestamp(),
        ))
    }

    async fn refresh_rotate(
        &self,
        tenant_id: &str,
        token_hash: &str,
        new_hash: &str,
        policy: RefreshPolicy,
    ) -> Result<Rotated, ExecError> {
        let mut tables = self
            .tables
            .lock()
            .map_err(|_| ExecError::Message("memory store poisoned".into()))?;
        let rows = tables.entry(key(tenant_id, REFRESH_TABLE)).or_default();
        Ok(super::refresh::memory_rotate(
            rows,
            tenant_id,
            token_hash,
            new_hash,
            policy,
            chrono::Utc::now().timestamp(),
        ))
    }

    async fn refresh_revoke(
        &self,
        tenant_id: &str,
        target: RevokeTarget<'_>,
    ) -> Result<u64, ExecError> {
        let mut tables = self
            .tables
            .lock()
            .map_err(|_| ExecError::Message("memory store poisoned".into()))?;
        let rows = tables.entry(key(tenant_id, REFRESH_TABLE)).or_default();
        Ok(super::refresh::memory_revoke(
            rows,
            tenant_id,
            &target,
            chrono::Utc::now().timestamp(),
        ))
    }

    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
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
        let exec = Self {
            client: tokio::sync::Mutex::new(client),
        };
        exec.check_role_separation().await?;
        Ok(exec)
    }

    /// The app query roles must never act as the runtime role: none of
    /// public_reader, public_writer or app_rw may be a member of it,
    /// directly or through other roles (that would hand them the runtime's
    /// platform-table access).
    pub async fn check_role_separation(&self) -> Result<(), String> {
        self.check_role_separation_for(None).await
    }

    /// As `check_role_separation`, for a named role (`vibe migrate` checks
    /// the runtime role it grants to); `None` means the connected role.
    pub async fn check_role_separation_for(&self, role: Option<&str>) -> Result<(), String> {
        let client = self.client.lock().await;
        let rows = client
            .query(
                "SELECT r.rolname::text FROM pg_roles r \
                 WHERE r.rolname = ANY(ARRAY['public_reader','public_writer','app_rw']) \
                 AND EXISTS (SELECT 1 FROM pg_roles t WHERE t.rolname = coalesce($1::text, current_user::text) \
                             AND pg_has_role(r.oid, t.oid, 'MEMBER'))",
                &[&role],
            )
            .await
            .map_err(|err| err.to_string())?;
        if let Some(row) = rows.first() {
            let member: String = row.get(0);
            return Err(format!(
                "database role {member} is a member of the runtime role; revoke that membership (it would let app queries reach platform tables)"
            ));
        }
        Ok(())
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

    async fn refresh_family(
        &self,
        tenant_id: &str,
        token_hash: &str,
    ) -> Result<Option<String>, ExecError> {
        let rows = self
            .run_tx(
                tenant_id,
                None,
                false,
                "SELECT family_id::text AS family_id FROM _platform_refresh_tokens WHERE token_hash = $1 AND revoked_at IS NULL",
                vec![Some(token_hash.to_string())],
            )
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("family_id").and_then(Value::as_str))
            .map(str::to_string))
    }

    async fn refresh_issue(
        &self,
        tenant_id: &str,
        user_id: &str,
        token_hash: &str,
        policy: RefreshPolicy,
    ) -> Result<String, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        let family = uuid::Uuid::new_v4().to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let mut client = self.client.lock().await;
        let tx = client.transaction().await.map_err(map_pg)?;
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(map_pg)?;
        // Per-user lock shared with logout-all, so a new family is either
        // committed before logout-all lists the user's families or created
        // after it finishes.
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtext('vibe-refresh-user:' || $1))",
            &[&user_id],
        )
        .await
        .map_err(map_pg)?;
        let family_ttl = policy.family_ttl as f64;
        let idle_ttl = policy.idle_ttl as f64;
        tx.execute(
            "INSERT INTO _platform_refresh_tokens (id, tenant_id, family_id, user_id, token_hash, created_at, family_expires_at, idle_expires_at) \
             VALUES ($1::text::uuid, $2, $3::text::uuid, $4, $5, now(), now() + make_interval(secs => $6), now() + make_interval(secs => $7))",
            &[&id, &tenant_id, &family, &user_id, &token_hash, &family_ttl, &idle_ttl],
        )
        .await
        .map_err(map_pg)?;
        tx.commit().await.map_err(map_pg)?;
        Ok(family)
    }

    async fn refresh_rotate(
        &self,
        tenant_id: &str,
        token_hash: &str,
        new_hash: &str,
        policy: RefreshPolicy,
    ) -> Result<Rotated, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        let mut client = self.client.lock().await;
        let tx = client.transaction().await.map_err(map_pg)?;
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(map_pg)?;
        // Family lock: every rotation and revocation of one family takes
        // this transaction-scoped advisory lock first, so a family revoke
        // cannot miss a child inserted by a concurrent rotation (also with
        // a connection pool or several instances).
        let family_of = tx
            .query_opt(
                "SELECT family_id::text FROM _platform_refresh_tokens WHERE token_hash = $1",
                &[&token_hash],
            )
            .await
            .map_err(map_pg)?;
        let Some(family_of) = family_of else {
            return Ok(Rotated::Invalid);
        };
        let family_of: String = family_of.get(0);
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtext('vibe-refresh:' || $1))",
            &[&family_of],
        )
        .await
        .map_err(map_pg)?;
        // Row lock: concurrent presentations of one token serialise here.
        let row = tx
            .query_opt(
                "SELECT id::text, family_id::text, user_id, revoke_reason, revoked_at IS NOT NULL, \
                 (now() >= family_expires_at OR now() >= idle_expires_at), \
                 used_at IS NOT NULL, \
                 (used_at IS NOT NULL AND now() - used_at < make_interval(secs => $2::double precision) AND grace_reissued_at IS NULL \
                  AND EXISTS (SELECT 1 FROM _platform_refresh_tokens c WHERE c.id = t.replaced_by AND c.used_at IS NULL AND c.revoked_at IS NULL)), \
                 replaced_by::text, \
                 floor(extract(epoch FROM family_expires_at - now()))::bigint \
                 FROM _platform_refresh_tokens t WHERE token_hash = $1 FOR UPDATE",
                &[&token_hash, &(policy.grace as f64)],
            )
            .await
            .map_err(map_pg)?;
        let Some(row) = row else {
            return Ok(Rotated::Invalid);
        };
        let id: String = row.get(0);
        let family: String = row.get(1);
        let user: String = row.get(2);
        let reason: Option<String> = row.get(3);
        let revoked: bool = row.get(4);
        let expired: bool = row.get(5);
        let used: bool = row.get(6);
        let in_grace: bool = row.get(7);
        let old_child: Option<String> = row.get(8);
        let family_left: i64 = row.get(9);
        let revoke_family =
            "UPDATE _platform_refresh_tokens SET revoked_at = now(), revoke_reason = 'reuse' \
                             WHERE family_id = $1::text::uuid AND revoked_at IS NULL";
        if revoked {
            if reason.as_deref() == Some("grace-reissue") {
                tx.execute(revoke_family, &[&family])
                    .await
                    .map_err(map_pg)?;
                tx.commit().await.map_err(map_pg)?;
                return Ok(Rotated::Reuse { family_id: family });
            }
            return Ok(Rotated::Invalid);
        }
        // Spent before expired: a late replay still revokes the family. A
        // grace reissue is allowed only while the earlier child is unused.
        if used && !in_grace {
            tx.execute(revoke_family, &[&family])
                .await
                .map_err(map_pg)?;
            tx.commit().await.map_err(map_pg)?;
            return Ok(Rotated::Reuse { family_id: family });
        }
        if expired {
            return Ok(Rotated::Invalid);
        }
        let child = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO _platform_refresh_tokens (id, tenant_id, family_id, user_id, token_hash, parent_id, created_at, family_expires_at, idle_expires_at) \
             SELECT $1::text::uuid, tenant_id, family_id, user_id, $2, id, now(), family_expires_at, now() + make_interval(secs => $3::double precision) \
             FROM _platform_refresh_tokens WHERE id = $4::text::uuid",
            &[&child, &new_hash, &(policy.idle_ttl as f64), &id],
        )
        .await
        .map_err(map_pg)?;
        if used {
            if let Some(old) = old_child {
                tx.execute(
                    "UPDATE _platform_refresh_tokens SET revoked_at = now(), revoke_reason = 'grace-reissue' \
                     WHERE id = $1::text::uuid AND revoked_at IS NULL",
                    &[&old],
                )
                .await
                .map_err(map_pg)?;
            }
            tx.execute(
                "UPDATE _platform_refresh_tokens SET replaced_by = $1::text::uuid, grace_reissued_at = now() WHERE id = $2::text::uuid",
                &[&child, &id],
            )
            .await
            .map_err(map_pg)?;
        } else {
            tx.execute(
                "UPDATE _platform_refresh_tokens SET replaced_by = $1::text::uuid, used_at = now() WHERE id = $2::text::uuid",
                &[&child, &id],
            )
            .await
            .map_err(map_pg)?;
        }
        tx.commit().await.map_err(map_pg)?;
        Ok(Rotated::Ok {
            user_id: user,
            family_id: family,
            expires_in: policy.idle_ttl.min(family_left),
        })
    }

    async fn refresh_revoke(
        &self,
        tenant_id: &str,
        target: RevokeTarget<'_>,
    ) -> Result<u64, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        let mut client = self.client.lock().await;
        let tx = client.transaction().await.map_err(map_pg)?;
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(map_pg)?;
        // Take the same per-family lock as rotation for every affected family
        // (sorted, so concurrent revokes cannot deadlock), then revoke in a
        // new statement: under READ COMMITTED it sees children committed by
        // a rotation that held the lock before us.
        let families = match target {
            RevokeTarget::FamilyOf { hash, user } => {
                tx.query(
                    "SELECT DISTINCT family_id::text FROM _platform_refresh_tokens \
                     WHERE token_hash = $1 AND user_id = $2 ORDER BY 1",
                    &[&hash, &user],
                )
                .await
            }
            RevokeTarget::User(user) => {
                // Same per-user lock as refresh_issue: a login running at the
                // same time is either listed here or starts after this commit.
                tx.execute(
                    "SELECT pg_advisory_xact_lock(hashtext('vibe-refresh-user:' || $1))",
                    &[&user],
                )
                .await
                .map_err(map_pg)?;
                tx.query(
                    "SELECT DISTINCT family_id::text FROM _platform_refresh_tokens \
                     WHERE user_id = $1 AND revoked_at IS NULL ORDER BY 1",
                    &[&user],
                )
                .await
            }
        }
        .map_err(map_pg)?;
        let families: Vec<String> = families.iter().map(|r| r.get(0)).collect();
        for family in &families {
            tx.execute(
                "SELECT pg_advisory_xact_lock(hashtext('vibe-refresh:' || $1))",
                &[family],
            )
            .await
            .map_err(map_pg)?;
        }
        let n = tx
            .execute(
                "UPDATE _platform_refresh_tokens SET revoked_at = now(), revoke_reason = 'logout' \
                 WHERE revoked_at IS NULL AND family_id::text = ANY($1)",
                &[&families],
            )
            .await
            .map_err(map_pg)?;
        tx.commit().await.map_err(map_pg)?;
        Ok(n)
    }

    async fn raw(
        &self,
        tenant_id: &str,
        role: &str,
        writes: bool,
        sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        if !QUERY_ROLES.contains(&role) {
            return Err(ExecError::Message(format!("query cannot run as {role}")));
        }
        self.run_tx(tenant_id, Some(role), writes, sql, params)
            .await
    }

    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, ExecError> {
        let rows = self
            .run_tx(
                tenant_id,
                None,
                false,
                "SELECT roles FROM users WHERE id::text = $1",
                vec![Some(user_id.to_string())],
            )
            .await?;
        match rows.first() {
            None => Ok(None),
            Some(row) => roles_from(row.get("roles")).map(Some),
        }
    }
}

impl PgExec {
    /// One transaction in the tenant schema; `role` switches to a query role
    /// with `SET LOCAL ROLE`; without `writes` it is READ ONLY.
    async fn run_tx(
        &self,
        tenant_id: &str,
        role: Option<&str>,
        writes: bool,
        sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        let mut client = self.client.lock().await;
        let tx = client
            .transaction()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
        if !writes {
            tx.batch_execute("SET TRANSACTION READ ONLY")
                .await
                .map_err(|err| ExecError::Message(err.to_string()))?;
        }
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(|err| ExecError::Message(err.to_string()))?;
        if let Some(role) = role {
            // `role` is one of QUERY_ROLES (checked by the caller).
            tx.batch_execute(&format!("SET LOCAL ROLE {role}"))
                .await
                .map_err(|err| ExecError::Message(format!("SET ROLE {role}: {err}")))?;
        }
        let params: Vec<TextParam> = params.into_iter().map(TextParam).collect();
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as _).collect();
        let body = sql.trim().trim_end_matches(';');
        let rows = if crate::sqlguard::returns_rows(body) {
            // Every column comes back as JSON, whatever its type.
            let wrapped = crate::sqlguard::wrap_for_rows(body);
            if role.is_some() {
                // Query steps: check the text that is actually executed.
                if let Some(problem) = crate::sqlguard::wrapped_problem(&wrapped) {
                    return Err(ExecError::Message(problem));
                }
            }
            tx.query(&wrapped, &refs).await.map_err(map_pg)?
        } else {
            if role.is_some() {
                if let Some(problem) = crate::sqlguard::query_problem(body) {
                    return Err(ExecError::Message(problem));
                }
            }
            tx.execute(body, &refs).await.map_err(map_pg)?;
            Vec::new()
        };
        tx.commit()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
        rows.iter()
            .map(|row| {
                let text: String = row.get(0);
                match serde_json::from_str(&text) {
                    Ok(Value::Object(map)) => Ok(map),
                    _ => Err(ExecError::Message("query row is not an object".into())),
                }
            })
            .collect()
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
