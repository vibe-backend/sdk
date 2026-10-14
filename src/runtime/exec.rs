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
        let _ = role;
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        let (sql, params) = sql::render(&stmt).map_err(ExecError::Message)?;
        let returning = returning_cols(&stmt);
        let mut client = self.client.lock().await;
        let tx = client
            .transaction()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
        let schema = format!("\"t_{tenant_id}\"");
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
