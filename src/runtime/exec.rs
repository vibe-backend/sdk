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
    async fn raw(
        &self,
        _tenant_id: &str,
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

/// Statements whose rows can be read: SELECT/WITH, or a write with RETURNING.
fn returns_rows(sql: &str) -> bool {
    // Drop quoted literals so a string containing "returning" cannot flip it.
    let mut clean = String::new();
    let mut quoted = false;
    for c in sql.chars() {
        if c == '\'' {
            quoted = !quoted;
            clean.push(' ');
        } else if !quoted {
            clean.push(c.to_ascii_lowercase());
        }
    }
    let head = clean.trim_start();
    head.starts_with("select")
        || head.starts_with("with")
        || clean
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|w| w == "returning")
}

const REFRESH_TABLE: &str = "_platform_refresh_tokens";

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
                .find(|r| r.get("token_hash").and_then(Value::as_str) == Some(token_hash))
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

    async fn refresh_family(
        &self,
        tenant_id: &str,
        token_hash: &str,
    ) -> Result<Option<String>, ExecError> {
        let rows = self
            .raw(
                tenant_id,
                "SELECT family_id::text AS family_id FROM _platform_refresh_tokens WHERE token_hash = $1",
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
        let family = uuid::Uuid::new_v4().to_string();
        let id = uuid::Uuid::new_v4().to_string();
        self.raw(
            tenant_id,
            "INSERT INTO _platform_refresh_tokens (id, tenant_id, family_id, user_id, token_hash, created_at, family_expires_at, idle_expires_at) \
             VALUES ($1::text::uuid, $2, $3::text::uuid, $4, $5, now(), now() + make_interval(secs => $6::double precision), now() + make_interval(secs => $7::double precision))",
            vec![
                Some(id),
                Some(tenant_id.to_string()),
                Some(family.clone()),
                Some(user_id.to_string()),
                Some(token_hash.to_string()),
                Some(policy.family_ttl.to_string()),
                Some(policy.idle_ttl.to_string()),
            ],
        )
        .await?;
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
        // Row lock: concurrent presentations of one token serialise here.
        let row = tx
            .query_opt(
                "SELECT id::text, family_id::text, user_id, revoke_reason, revoked_at IS NOT NULL, \
                 (now() >= family_expires_at OR now() >= idle_expires_at), \
                 used_at IS NOT NULL, \
                 (used_at IS NOT NULL AND now() - used_at <= make_interval(secs => $2::double precision) AND grace_reissued_at IS NULL), \
                 replaced_by::text \
                 FROM _platform_refresh_tokens WHERE token_hash = $1 FOR UPDATE",
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
        let revoke_family = "UPDATE _platform_refresh_tokens SET revoked_at = now(), revoke_reason = 'reuse' \
                             WHERE family_id = $1::text::uuid AND revoked_at IS NULL";
        if revoked {
            if reason.as_deref() == Some("grace-reissue") {
                tx.execute(revoke_family, &[&family]).await.map_err(map_pg)?;
                tx.commit().await.map_err(map_pg)?;
                return Ok(Rotated::Reuse { family_id: family });
            }
            return Ok(Rotated::Invalid);
        }
        if expired {
            return Ok(Rotated::Invalid);
        }
        if used && !in_grace {
            tx.execute(revoke_family, &[&family]).await.map_err(map_pg)?;
            tx.commit().await.map_err(map_pg)?;
            return Ok(Rotated::Reuse { family_id: family });
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
        })
    }

    async fn refresh_revoke(
        &self,
        tenant_id: &str,
        target: RevokeTarget<'_>,
    ) -> Result<u64, ExecError> {
        let (sql, params) = match target {
            RevokeTarget::FamilyOf { hash, user } => (
                "UPDATE _platform_refresh_tokens SET revoked_at = now(), revoke_reason = 'logout' \
                 WHERE revoked_at IS NULL AND family_id = (SELECT family_id FROM _platform_refresh_tokens WHERE token_hash = $1 AND user_id = $2) RETURNING id",
                vec![Some(hash.to_string()), Some(user.to_string())],
            ),
            RevokeTarget::User(user) => (
                "UPDATE _platform_refresh_tokens SET revoked_at = now(), revoke_reason = 'logout' \
                 WHERE revoked_at IS NULL AND user_id = $1 RETURNING id",
                vec![Some(user.to_string())],
            ),
        };
        let rows = self.raw(tenant_id, sql, params).await?;
        Ok(rows.len() as u64)
    }

    async fn raw(
        &self,
        tenant_id: &str,
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
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[&tenant_id, &schema],
        )
        .await
        .map_err(|err| ExecError::Message(err.to_string()))?;
        let params: Vec<TextParam> = params.into_iter().map(TextParam).collect();
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as _).collect();
        let body = sql.trim().trim_end_matches(';');
        let rows = if returns_rows(body) {
            // Every column comes back as JSON, whatever its type.
            let wrapped = format!("WITH q AS ({body}) SELECT row_to_json(q)::text FROM q");
            tx.query(&wrapped, &refs).await.map_err(map_pg)?
        } else {
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
