use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value};

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

#[derive(Default)]
pub struct MemoryExec {
    tables: Mutex<BTreeMap<String, Vec<Row>>>,
}

impl MemoryExec {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl QueryExec for MemoryExec {
    async fn execute(
        &self,
        tenant_id: &str,
        _role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
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

/// Privileges of the runtime connection role, as read from `pg_roles`.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleStatus {
    pub name: String,
    pub superuser: bool,
    pub bypass_rls: bool,
    pub owned_tables: i64,
    pub owned_schemas: i64,
}

impl RoleStatus {
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.superuser {
            out.push("is a superuser".to_string());
        }
        if self.bypass_rls {
            out.push("has BYPASSRLS".to_string());
        }
        if self.owned_tables > 0 {
            out.push(format!("owns {} tenant table(s)", self.owned_tables));
        }
        if self.owned_schemas > 0 {
            out.push(format!("owns {} tenant schema(s)", self.owned_schemas));
        }
        out
    }
}

pub struct PgExec {
    client: tokio::sync::Mutex<tokio_postgres::Client>,
}

impl PgExec {
    pub async fn connect(url: &str) -> Result<Self, String> {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .map_err(|err| err.to_string())?;
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
            .query_one(
                "SELECT r.rolname::text, r.rolsuper, r.rolbypassrls, \
                 (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                   WHERE n.nspname LIKE 't\\_%' AND c.relkind IN ('r', 'p') \
                   AND pg_has_role(r.oid, c.relowner, 'USAGE')), \
                 (SELECT count(*) FROM pg_namespace n WHERE n.nspname LIKE 't\\_%' \
                   AND pg_has_role(r.oid, n.nspowner, 'USAGE')) \
                 FROM pg_roles r WHERE r.rolname = current_user",
                &[],
            )
            .await
            .map_err(|err| err.to_string())?;
        let status = RoleStatus {
            name: row.get(0),
            superuser: row.get(1),
            bypass_rls: row.get(2),
            owned_tables: row.get(3),
            owned_schemas: row.get(4),
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

    pub async fn batch(&self, statements: &[String]) -> Result<(), String> {
        let client = self.client.lock().await;
        for statement in statements {
            client
                .batch_execute(statement)
                .await
                .map_err(|err| err.to_string())?;
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
        // Postgres enforcement comes from the connection role itself (checked
        // at startup by `check_runtime_role`) plus the grants `vibe migrate`
        // issues to VIBE_RUNTIME_DB_ROLE; `role` is the logical caller class.
        let _ = role;
        let (sql, params) = sql::render(&stmt).map_err(ExecError::Message)?;
        let returning = returning_cols(&stmt);
        let mut client = self.client.lock().await;
        let tx = client
            .transaction()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
        let schema = format!("t_{tenant_id}");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(|err| ExecError::Message(err.to_string()))?;
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as _).collect();
        let rows = tx.query(&sql, &refs).await.map_err(map_pg)?;
        tx.commit()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
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
                    .map_err(|err| ExecError::Message(err.to_string()))?;
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
        ExecError::Message(err.to_string())
    }
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
    use super::RoleStatus;

    fn status() -> RoleStatus {
        RoleStatus {
            name: "app".into(),
            superuser: false,
            bypass_rls: false,
            owned_tables: 0,
            owned_schemas: 0,
        }
    }

    #[test]
    fn safe_role_has_no_problems() {
        assert!(status().problems().is_empty());
    }

    #[test]
    fn unsafe_roles_are_reported() {
        let mut s = status();
        s.superuser = true;
        s.bypass_rls = true;
        s.owned_tables = 2;
        s.owned_schemas = 1;
        let p = s.problems();
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(p.iter().any(|x| x.contains("superuser")));
        assert!(p.iter().any(|x| x.contains("BYPASSRLS")));
        assert!(p.iter().any(|x| x.contains("table")));
        assert!(p.iter().any(|x| x.contains("schema")));
    }
}
