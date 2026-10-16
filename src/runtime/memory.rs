use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::exec::{reject_tenant_input, role_error, roles_from, ExecError, QueryExec, Row};
use super::grants::{effective, parse_grants, RoleGrant, RUNTIME_ROLES};
use super::sql::{Col, Stmt};

/// Error prefix for manifests the in-memory executor cannot model
/// faithfully (spec 2C.4). It refuses them instead of guessing.
pub const MEMORY_UNSUPPORTED_SQL: &str = "MEMORY_UNSUPPORTED_SQL";

/// The only row-level security setup the memory executor models: the one
/// the compiler generates for every model.
const STANDARD_RLS: &str = "enable, force";
const STANDARD_POLICY: &str = "tenant_id = current_setting('app.tenant_id', false)";

/// In-memory executor for `vibe dev` / `serve --memory` and tests.
///
/// Emulates the production database (spec 2C.4, decision 74) for the
/// statements the platform generates:
///
/// - tenant isolation as with the standard `tenant_isolation` policy under
///   ENABLE + FORCE RLS: rows are only visible to their tenant, and
///   `tenant_id` is always set by the executor;
/// - the role's effective table and column grants
///   ([`super::grants::effective`]), including the SELECT privilege that
///   PostgreSQL needs on every column in `WHERE` and `RETURNING`;
/// - PostgreSQL's outcome for a write that matches no row (UPDATE:
///   precondition failed, soft delete: not found).
///
/// Not modelled, so refused with [`MEMORY_UNSUPPORTED_SQL`] when a manifest
/// asks for it: any other RLS setting or policy (extra, restrictive or
/// per-command policies, USING vs WITH CHECK differences, DELETE), and grants
/// for roles other than the three runtime roles. Owner/superuser, BYPASSRLS
/// and connection pooling do not exist here; production refuses such
/// connection roles at startup (`check_runtime_role`).
pub struct MemoryExec {
    tables: Mutex<BTreeMap<String, Vec<Row>>>,
    models: BTreeMap<String, MemTable>,
}

struct MemTable {
    columns: BTreeSet<String>,
    grants: BTreeMap<String, RoleGrant>,
}

fn unsupported(model: &str, what: String) -> String {
    format!("{MEMORY_UNSUPPORTED_SQL}: model {model}: {what}; use PostgreSQL (`--db postgres`)")
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl MemoryExec {
    /// Executor without models: every CRUD statement is refused. For tests
    /// that only need `seed` / `user_roles`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            tables: Mutex::default(),
            models: BTreeMap::new(),
        }
    }

    /// Builds the executor for a manifest. Fails closed with
    /// [`MEMORY_UNSUPPORTED_SQL`] for anything it cannot model.
    pub fn from_manifest(manifest: &crate::manifest::Manifest) -> Result<Self, String> {
        let mut models = BTreeMap::new();
        for model in &manifest.models {
            if squash(&model.rls) != STANDARD_RLS || squash(&model.policy) != STANDARD_POLICY {
                return Err(unsupported(
                    &model.id,
                    format!(
                        "only `rls: {STANDARD_RLS}` with `policy: {STANDARD_POLICY}` is modelled (got rls `{}`, policy `{}`; recompile old manifests)",
                        model.rls, model.policy
                    ),
                ));
            }
            let parsed = parse_grants(&model.grants)
                .map_err(|err| format!("model {}: grants: {err}", model.id))?;
            if let Some(role) = parsed.keys().find(|r| !RUNTIME_ROLES.contains(&r.as_str())) {
                return Err(unsupported(
                    &model.id,
                    format!(
                        "grants for role `{role}` (only {} run here)",
                        RUNTIME_ROLES.join(", ")
                    ),
                ));
            }
            let grants = RUNTIME_ROLES
                .iter()
                .map(|role| (role.to_string(), effective(model, &parsed, role)))
                .collect();
            models.insert(
                model.table.clone(),
                MemTable {
                    columns: model.fields.iter().map(|f| f.name.clone()).collect(),
                    grants,
                },
            );
        }
        Ok(Self {
            tables: Mutex::default(),
            models,
        })
    }

    /// Adds a row to a tenant table directly (dev data and tests).
    pub fn seed(&self, tenant_id: &str, table: &str, row: Row) {
        if let Ok(mut tables) = self.tables.lock() {
            tables.entry(key(tenant_id, table)).or_default().push(row);
        }
    }
}

fn denied(table: &str) -> ExecError {
    // PostgreSQL reports a missing column privilege as a table-level denial.
    ExecError::Denied(format!("permission denied for table {table}"))
}

#[async_trait]
impl QueryExec for MemoryExec {
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

    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
        // Both executors refuse an invalid tenant before running anything.
        // (In PostgreSQL itself a pooled connection without the setting reads
        // `''` and sees no rows; the executor never gets that far.)
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        super::exec::role_allows(role, &stmt).map_err(role_error)?;
        reject_tenant_input(&stmt)?;
        self.check_grants(role, &stmt)?;
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
                // Like PostgreSQL's `UPDATE .. WHERE id AND updated_at ..`:
                // no matching row (missing, other tenant, other owner or a
                // stale version) is a failed precondition.
                let key = key(tenant_id, &table);
                let row = tables
                    .get_mut(&key)
                    .and_then(|rows| {
                        rows.iter_mut().find(|row| {
                            row.get("id").and_then(Value::as_str) == Some(id.as_str())
                                && row.get("deleted_at").is_none_or(Value::is_null)
                                && owner_matches(row, owner.as_ref())
                                && row.get("updated_at").and_then(Value::as_str)
                                    == Some(expected_updated_at.as_str())
                        })
                    })
                    .ok_or(ExecError::Precondition)?;
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
                let mut out = Map::new();
                out.insert("id".into(), Value::String(id));
                Ok(vec![out])
            }
        }
    }
}

impl MemoryExec {
    /// The privilege checks PostgreSQL makes for the SQL that
    /// [`super::sql::render`] produces for `stmt`.
    fn check_grants(&self, role: &str, stmt: &Stmt) -> Result<(), ExecError> {
        let table = match stmt {
            Stmt::Insert { table, .. }
            | Stmt::Select { table, .. }
            | Stmt::Update { table, .. }
            | Stmt::SoftDelete { table, .. } => table,
        };
        let Some(model) = self.models.get(table) else {
            return Err(ExecError::Message(format!(
                "relation \"{table}\" does not exist"
            )));
        };
        let grant = model.grants.get(role).cloned().unwrap_or_default();
        let mut select: Vec<&str> = Vec::new();
        let mut insert: Vec<&str> = Vec::new();
        let mut update: Vec<&str> = Vec::new();
        let owner_col = |owner: &Option<(Col, Value)>| owner.as_ref().map(|(c, _)| c.name.clone());
        let owner_name;
        match stmt {
            Stmt::Select {
                columns,
                id,
                filters,
                sort,
                ..
            } => {
                if grant.select.is_none() {
                    return Err(denied(table));
                }
                select.extend(columns.iter().map(|c| c.name.as_str()));
                select.push("deleted_at");
                if id.is_some() {
                    select.push("id");
                }
                select.extend(filters.iter().map(|(c, _)| c.name.as_str()));
                if let Some(sort) = sort {
                    select.push(&sort.column);
                }
            }
            Stmt::Insert {
                values, returning, ..
            } => {
                if grant.insert.is_none() {
                    return Err(denied(table));
                }
                insert.push("tenant_id");
                insert.extend(values.iter().map(|(c, _)| c.name.as_str()));
                select.extend(returning.iter().map(|c| c.name.as_str()));
            }
            Stmt::Update {
                sets,
                returning,
                owner,
                ..
            } => {
                if grant.update.is_none() {
                    return Err(denied(table));
                }
                update.push("updated_at");
                update.extend(sets.iter().map(|(c, _)| c.name.as_str()));
                select.extend(["id", "updated_at", "deleted_at"]);
                select.extend(returning.iter().map(|c| c.name.as_str()));
                owner_name = owner_col(owner);
                select.extend(owner_name.as_deref());
            }
            Stmt::SoftDelete { owner, .. } => {
                if grant.update.is_none() {
                    return Err(denied(table));
                }
                update.extend(["deleted_at", "updated_at"]);
                select.extend(["id", "deleted_at"]);
                owner_name = owner_col(owner);
                select.extend(owner_name.as_deref());
            }
        }
        for column in select.iter().chain(&insert).chain(&update) {
            if !model.columns.contains(*column) {
                return Err(ExecError::Message(format!(
                    "column \"{column}\" of relation \"{table}\" does not exist"
                )));
            }
        }
        if !select.iter().all(|c| grant.can_select(c))
            || !insert.iter().all(|c| grant.can_insert(c))
            || !update.iter().all(|c| grant.can_update(c))
        {
            return Err(denied(table));
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

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::manifest::{FieldSpec, ListOpts, Manifest, ModelSpec};
    use serde_json::json;

    fn col(name: &str) -> Col {
        Col {
            name: name.into(),
            ty: "string".into(),
        }
    }

    fn field(name: &str) -> FieldSpec {
        FieldSpec {
            name: name.into(),
            ty: "string".into(),
            required: false,
            unique: false,
            primary: name == "id",
            system: false,
            sensitive: false,
            nullable: true,
            min: None,
            max: None,
            format: String::new(),
        }
    }

    fn model(grants: &str) -> ModelSpec {
        ModelSpec {
            id: "user".into(),
            provider: "main".into(),
            table: "users".into(),
            fields: [
                "id",
                "tenant_id",
                "email",
                "name",
                "role",
                "created_at",
                "updated_at",
                "deleted_at",
            ]
            .into_iter()
            .map(field)
            .collect(),
            crud: vec![],
            list: ListOpts::default(),
            sensitive_output: vec![],
            owner_key: None,
            ownership: String::new(),
            grants: grants.into(),
            rls: STANDARD_RLS.into(),
            policy: STANDARD_POLICY.into(),
        }
    }

    fn manifest(model: ModelSpec) -> Manifest {
        Manifest {
            models: vec![model],
            manifest_version: 1,
            routes: vec![],
            methods: vec![],
            migrations: vec![],
            signature: String::new(),
        }
    }

    fn exec(grants: &str) -> MemoryExec {
        MemoryExec::from_manifest(&manifest(model(grants))).unwrap()
    }

    fn insert(cols: &[(&str, &str)], returning: &[&str]) -> Stmt {
        Stmt::Insert {
            table: "users".into(),
            values: cols.iter().map(|(c, v)| (col(c), json!(v))).collect(),
            returning: returning.iter().map(|c| col(c)).collect(),
        }
    }

    fn select(columns: &[&str]) -> Stmt {
        Stmt::Select {
            table: "users".into(),
            columns: columns.iter().map(|c| col(c)).collect(),
            id: None,
            filters: vec![],
            sort: None,
            limit: 100,
            offset: 0,
        }
    }

    fn update(id: &str, column: &str, expected: &str) -> Stmt {
        Stmt::Update {
            table: "users".into(),
            id: id.into(),
            sets: vec![(col(column), json!("x"))],
            expected_updated_at: expected.into(),
            touched_at: "t1".into(),
            returning: vec![col("id")],
            owner: None,
        }
    }

    fn denied_msg(err: ExecError) -> String {
        match err {
            ExecError::Denied(message) => message,
            other => panic!("unexpected {other:?}"),
        }
    }

    const GRANTS: &str = "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT(email)], public_writer [INSERT(email), UPDATE(name)]";
    const T: &str = "tenant0001";

    #[tokio::test]
    async fn tenants_only_see_their_own_rows() {
        let exec = exec(GRANTS);
        exec.execute(
            T,
            "app_rw",
            insert(
                &[("id", "1"), ("email", "a@x"), ("updated_at", "t0")],
                &["id"],
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            exec.execute(T, "app_rw", select(&["id"]))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(exec
            .execute("tenant0002", "app_rw", select(&["id"]))
            .await
            .unwrap()
            .is_empty());
        // Like PostgreSQL: an UPDATE that matches no row is a failed
        // precondition, whatever the reason.
        assert!(matches!(
            exec.execute("tenant0002", "app_rw", update("1", "email", "t0"))
                .await,
            Err(ExecError::Precondition)
        ));
        assert!(matches!(
            exec.execute(T, "app_rw", update("1", "email", "stale"))
                .await,
            Err(ExecError::Precondition)
        ));
        exec.execute(T, "app_rw", update("1", "email", "t0"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn invalid_tenant_is_refused_before_running() {
        let exec = exec(GRANTS);
        assert!(matches!(
            exec.execute("", "app_rw", select(&["id"])).await,
            Err(ExecError::Message(m)) if m.contains("tenant")
        ));
    }

    #[tokio::test]
    async fn tenant_id_input_is_always_refused() {
        let exec = exec(GRANTS);
        // Even the caller's own tenant id.
        let err = denied_msg(
            exec.execute(
                T,
                "app_rw",
                insert(&[("id", "1"), ("tenant_id", T)], &["id"]),
            )
            .await
            .unwrap_err(),
        );
        assert!(err.contains("row-level security"), "{err}");
        let err = denied_msg(
            exec.execute(T, "app_rw", update("1", "tenant_id", ""))
                .await
                .unwrap_err(),
        );
        assert_eq!(err, "permission denied for table users");
    }

    #[tokio::test]
    async fn public_reader_is_limited_to_granted_columns() {
        let exec = exec(GRANTS);
        exec.execute(T, "public_reader", select(&["email"]))
            .await
            .unwrap();
        for stmt in [
            select(&["email", "role"]),
            insert(&[("email", "a@x")], &["id"]),
            update("1", "email", ""),
        ] {
            denied_msg(exec.execute(T, "public_reader", stmt).await.unwrap_err());
        }
        let mut sorted = select(&["email"]);
        if let Stmt::Select { sort, .. } = &mut sorted {
            *sort = Some(super::super::sql::Sort {
                column: "role".into(),
                desc: false,
            });
        }
        assert!(matches!(
            exec.execute(T, "public_reader", sorted).await,
            Err(ExecError::Denied(_))
        ));
    }

    #[tokio::test]
    async fn public_writer_needs_select_for_returning_and_where() {
        let exec = exec(GRANTS);
        // Platform columns are part of the effective INSERT grant, and
        // RETURNING id is readable.
        exec.execute(
            T,
            "public_writer",
            insert(
                &[
                    ("id", "1"),
                    ("email", "a@x"),
                    ("created_at", "t0"),
                    ("updated_at", "t0"),
                ],
                &["id"],
            ),
        )
        .await
        .unwrap();
        // RETURNING a column without SELECT fails, as in PostgreSQL.
        assert!(matches!(
            exec.execute(
                T,
                "public_writer",
                insert(&[("id", "2"), ("email", "a@x")], &["id", "email"])
            )
            .await,
            Err(ExecError::Denied(_))
        ));
        // A column outside the grant.
        assert!(matches!(
            exec.execute(
                T,
                "public_writer",
                insert(&[("id", "3"), ("role", "admin")], &["id"])
            )
            .await,
            Err(ExecError::Denied(_))
        ));
        assert!(matches!(
            exec.execute(T, "public_writer", select(&["email"])).await,
            Err(ExecError::Denied(_))
        ));
        // UPDATE(name) works (WHERE columns are readable), email does not.
        exec.execute(T, "public_writer", update("1", "name", "t0"))
            .await
            .unwrap();
        assert!(matches!(
            exec.execute(T, "public_writer", update("1", "email", "t1"))
                .await,
            Err(ExecError::Denied(_))
        ));
        // Soft delete needs UPDATE(deleted_at).
        assert!(matches!(
            exec.execute(
                T,
                "public_writer",
                Stmt::SoftDelete {
                    table: "users".into(),
                    id: "1".into(),
                    touched_at: "t2".into(),
                    owner: None,
                },
            )
            .await,
            Err(ExecError::Denied(_))
        ));
    }

    #[tokio::test]
    async fn unknown_columns_and_tables_are_errors() {
        let exec = exec(GRANTS);
        assert!(matches!(
            exec.execute(T, "app_rw", select(&["nope"])).await,
            Err(ExecError::Message(m)) if m.contains("does not exist")
        ));
        let mut other = select(&["id"]);
        if let Stmt::Select { table, .. } = &mut other {
            *table = "other".into();
        }
        assert!(matches!(
            exec.execute(T, "app_rw", other).await,
            Err(ExecError::Message(m)) if m.contains("does not exist")
        ));
        assert!(MemoryExec::new()
            .execute(T, "app_rw", select(&["id"]))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn empty_grants_deny_everything() {
        let exec = exec("public_writer []");
        for role in ["app_rw", "public_writer", "public_reader"] {
            assert!(matches!(
                exec.execute(T, role, select(&["id"])).await,
                Err(ExecError::Denied(_))
            ));
        }
    }

    #[test]
    fn unsupported_policies_and_roles_fail_closed() {
        let mut m = model(GRANTS);
        m.policy = "true".into();
        let err = MemoryExec::from_manifest(&manifest(m)).err().unwrap();
        assert!(err.starts_with(MEMORY_UNSUPPORTED_SQL), "{err}");
        let mut m = model(GRANTS);
        m.rls = String::new();
        assert!(MemoryExec::from_manifest(&manifest(m)).is_err());
        let err = MemoryExec::from_manifest(&manifest(model("mail_worker [SELECT]")))
            .err()
            .unwrap();
        assert!(err.starts_with(MEMORY_UNSUPPORTED_SQL), "{err}");
        assert!(MemoryExec::from_manifest(&manifest(model("app_rw [SELECT"))).is_err());
    }
}
