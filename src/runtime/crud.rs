use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::manifest::{FieldSpec, Manifest, MethodSpec, ModelSpec};

use super::exec::{etag, ExecError, QueryExec, Row};
use super::sql::{Col, Sort, Stmt};

#[derive(Debug)]
pub struct CallResult {
    pub value: Value,
    pub etag: Option<String>,
    pub status: u16,
}

/// Authenticated caller. `subject` is `ctx.user.id` (JWT `sub`, or the API key id).
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub subject: &'a str,
    /// Roles from the verified principal; used only for `ownership: role(..)`.
    pub roles: &'a [String],
}

#[derive(Clone, Copy)]
pub struct CallCtx<'a> {
    /// Pepper, issuer/audience and signing keys for platform ops.
    pub auth: &'a super::http::AuthConfig,
    pub tenant_id: &'a str,
    pub role: &'a str,
    pub caller: Caller<'a>,
    /// Set by the refresh route: the already-stored rotated refresh token
    /// that `sign-jwt issue-refresh` hands back instead of a new family.
    pub refresh_token: Option<&'a str>,
}

pub async fn run_method(
    manifest: &Manifest,
    exec: &dyn QueryExec,
    target: &str,
    input: &Value,
    ctx: CallCtx<'_>,
    depth: u32,
) -> Result<CallResult, MethodFail> {
    if depth > 3 {
        return Err(MethodFail::code("INTERNAL", "call depth exceeded"));
    }
    let method = crate::manifest::method(manifest, target)
        .ok_or_else(|| MethodFail::code("INTERNAL", format!("unknown method {target}")))?;
    if method.crud_action.is_empty() {
        let mut verified = Vec::new();
        let result = run_steps(manifest, exec, method, input, ctx, depth, &mut verified).await?;
        // Decision 67: a confirmed verification ends every session of that
        // user (platform behaviour; the lock does not declare it).
        for user in verified {
            let target = super::refresh::RevokeTarget::User {
                user: &user,
                reason: "email-verified",
            };
            if let Err(err) = exec.refresh_revoke(ctx.tenant_id, target).await {
                tracing::error!(error = ?err, "revoking sessions after verification failed");
                return Err(MethodFail::code("INTERNAL", "session revoke failed"));
            }
        }
        return Ok(result);
    }
    let model = crate::manifest::model(manifest, &method.crud_model)
        .ok_or_else(|| MethodFail::code("INTERNAL", "unknown model"))?;
    match method.crud_action.as_str() {
        "create" => create(exec, model, input, ctx).await,
        "get" => get(exec, model, input, ctx).await,
        "list" => list(exec, model, input, ctx).await,
        "update" => update(exec, model, input, ctx).await,
        "delete" => delete(exec, model, input, ctx).await,
        other => Err(MethodFail::code(
            "INTERNAL",
            format!("unknown crud {other}"),
        )),
    }
}

async fn run_steps(
    manifest: &Manifest,
    exec: &dyn QueryExec,
    method: &MethodSpec,
    input: &Value,
    ctx: CallCtx<'_>,
    depth: u32,
    verified: &mut Vec<String>,
) -> Result<CallResult, MethodFail> {
    let mut vars = Map::new();
    vars.insert("input".into(), input.clone());
    vars.insert("ctx".into(), ctx_var(&ctx));
    let mut last = Value::Null;
    for step in &method.steps {
        match step.op.as_str() {
            "method" => {
                let args = eval_or_input(&step.args, &vars, input)?;
                let result = Box::pin(run_method(
                    manifest,
                    exec,
                    &step.target,
                    &args,
                    ctx,
                    depth + 1,
                ))
                .await?;
                if !step.out.is_empty() && step.out != "(none)" {
                    vars.insert(step.out.clone(), result.value.clone());
                }
                last = result.value;
            }
            "respond" => {
                let value = if step.value.is_empty() {
                    last
                } else {
                    eval_expr(&step.value, &vars)
                        .map_err(|err| MethodFail::code("INTERNAL", err))?
                };
                return Ok(CallResult {
                    value,
                    etag: None,
                    status: step.status,
                });
            }
            "validate" => {
                if when_matches(&step.when_expr, &vars) {
                    return Err(MethodFail::code(step.fail.clone(), step.fail.clone()));
                }
            }
            "query" | "verify-password" | "hash-password" | "random-token" | "hash-token"
            | "sign-jwt" => {
                let value = match step.op.as_str() {
                    "query" => {
                        let row = super::ops::query(exec, method, step, &vars, ctx).await?;
                        if method
                            .capabilities
                            .iter()
                            .any(|c| c == "confirm-verification")
                            && crate::validate::written_columns(&step.sql)
                                .iter()
                                .any(|c| c == "email_verified_at")
                        {
                            if let Some(id) = row.get("id").and_then(Value::as_str) {
                                verified.push(id.to_string());
                            }
                        }
                        row
                    }
                    "verify-password" => super::ops::verify_password(step, &vars).await?,
                    "hash-password" => super::ops::hash_password_op(step, &vars).await?,
                    "random-token" => super::ops::random_token(step, ctx)?,
                    "hash-token" => super::ops::hash_token(step, &vars, ctx)?,
                    _ => super::ops::sign_jwt(manifest, exec, method, step, &vars, ctx).await?,
                };
                if !step.out.is_empty() && step.out != "(none)" {
                    vars.insert(step.out.clone(), value.clone());
                }
                last = value;
            }
            _ => {
                return Err(MethodFail::code(
                    "INTERNAL",
                    format!("method op {} is not executable in this build", step.op),
                ));
            }
        }
    }
    Ok(CallResult {
        value: last,
        etag: None,
        status: 200,
    })
}

fn eval_or_input(
    args: &str,
    vars: &Map<String, Value>,
    input: &Value,
) -> Result<Value, MethodFail> {
    if args.is_empty() || args == "transform.in" || args == "input" {
        if args == "input" {
            return Ok(input.clone());
        }
        if let Some(value) = vars.get("input") {
            return Ok(value.clone());
        }
        return Ok(input.clone());
    }
    eval_expr(args, vars).map_err(|err| {
        tracing::warn!(error = %err, "method args expression failed");
        MethodFail::code("VALIDATION_FAILED", "Invalid request")
    })
}

async fn create(
    exec: &dyn QueryExec,
    model: &ModelSpec,
    input: &Value,
    ctx: CallCtx<'_>,
) -> Result<CallResult, MethodFail> {
    let obj = input.as_object().cloned().unwrap_or_default();
    reject_system(&obj)?;
    if let Some(owner) = &model.owner_key {
        if obj.contains_key(owner) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{owner} cannot be written"),
            ));
        }
    }
    let tenant_id = ctx.tenant_id;
    let role = ctx.role;
    let mut row_values: Vec<(Col, Value)> = Vec::new();
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    for field in &model.fields {
        if field.name == "id" {
            row_values.push((col(field), Value::String(id.clone())));
            continue;
        }
        if field.name == "tenant_id" {
            row_values.push((col(field), Value::String(tenant_id.into())));
            continue;
        }
        if matches!(field.name.as_str(), "created_at" | "updated_at") {
            row_values.push((col(field), Value::String(now.clone())));
            continue;
        }
        if matches!(field.name.as_str(), "deleted_at" | "erased_at") {
            row_values.push((col(field), Value::Null));
            continue;
        }
        if model.owner_key.as_deref() == Some(field.name.as_str()) {
            row_values.push((col(field), owner_value(model, field, ctx.caller)?));
            continue;
        }
        let provided = obj.get(&field.name).cloned();
        if field.required && provided.as_ref().is_none_or(Value::is_null) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{} is required", field.name),
            ));
        }
        let mut value = provided.unwrap_or(Value::Null);
        if field.ty == "password" && value.is_string() {
            value = Value::String(hash_password(value.as_str().unwrap_or(""))?);
        }
        check_field(field, &value)?;
        if field.unique && !value.is_null() {
            let existing = exec
                .execute(
                    tenant_id,
                    role,
                    Stmt::Select {
                        table: model.table.clone(),
                        columns: vec![Col {
                            name: "id".into(),
                            ty: "uuid".into(),
                        }],
                        id: None,
                        filters: vec![(col(field), value.clone())],
                        sort: None,
                        limit: 1,
                        offset: 0,
                    },
                )
                .await
                .map_err(db)?;
            if !existing.is_empty() {
                return Err(MethodFail::code(
                    "CONFLICT",
                    format!("{} already exists", field.name),
                ));
            }
        }
        row_values.push((col(field), value));
    }
    for key in obj.keys() {
        if !model.fields.iter().any(|field| field.name == *key) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("unknown field {key}"),
            ));
        }
    }
    let returning = output_cols(model, false);
    let rows = exec
        .execute(
            tenant_id,
            role,
            Stmt::Insert {
                table: model.table.clone(),
                values: row_values,
                returning: returning.clone(),
            },
        )
        .await
        .map_err(db)?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| MethodFail::code("INTERNAL", "insert returned nothing"))?;
    Ok(finish_row(model, row, false))
}

async fn get(
    exec: &dyn QueryExec,
    model: &ModelSpec,
    input: &Value,
    ctx: CallCtx<'_>,
) -> Result<CallResult, MethodFail> {
    let tenant_id = ctx.tenant_id;
    let role = ctx.role;
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return Err(MethodFail::code("VALIDATION_FAILED", "id is required"));
    }
    let rows = exec
        .execute(
            tenant_id,
            role,
            Stmt::Select {
                table: model.table.clone(),
                columns: all_cols(model),
                id: Some(id),
                filters: owner_filters(model, ctx.caller)?,
                sort: None,
                limit: 1,
                offset: 0,
            },
        )
        .await
        .map_err(db)?;
    let Some(row) = rows.into_iter().next() else {
        return Err(MethodFail::code("NOT_FOUND", "not found"));
    };
    Ok(finish_row(model, row, true))
}

async fn list(
    exec: &dyn QueryExec,
    model: &ModelSpec,
    input: &Value,
    ctx: CallCtx<'_>,
) -> Result<CallResult, MethodFail> {
    let tenant_id = ctx.tenant_id;
    let role = ctx.role;
    let page = int_field(input, "page").unwrap_or(1);
    let page_size = int_field(input, "page_size")
        .or_else(|| int_field(input, "page-size"))
        .unwrap_or(model.list.page_size);
    if page < 1 {
        return Err(MethodFail::code("VALIDATION_FAILED", "page starts at 1"));
    }
    if page_size < 1 || page_size > model.list.max_page_size {
        return Err(MethodFail::code(
            "VALIDATION_FAILED",
            "page_size is outside the allowed range",
        ));
    }
    let (sort_col, desc) = parse_sort(
        input.get("sort").and_then(Value::as_str).unwrap_or(""),
        &model.list.sort,
    )?;
    let mut filters = Vec::new();
    if let Some(filter) = input.get("filter").and_then(Value::as_object) {
        for (key, value) in filter {
            if value.is_null() {
                continue;
            }
            if !model.list.filter.iter().any(|name| name == key) {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{key} cannot be filtered"),
                ));
            }
            let field = model
                .fields
                .iter()
                .find(|field| field.name == *key)
                .ok_or_else(|| {
                    MethodFail::code("VALIDATION_FAILED", format!("unknown filter {key}"))
                })?;
            if field.sensitive {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{key} cannot be filtered"),
                ));
            }
            filters.push((col(field), value.clone()));
        }
    }
    filters.extend(owner_filters(model, ctx.caller)?);
    let rows = exec
        .execute(
            tenant_id,
            role,
            Stmt::Select {
                table: model.table.clone(),
                columns: output_cols(model, false),
                id: None,
                filters,
                sort: Some(Sort {
                    column: sort_col,
                    desc,
                }),
                limit: page_size + 1,
                offset: (page - 1) * page_size,
            },
        )
        .await
        .map_err(db)?;
    let has_more = rows.len() as i64 > page_size;
    let items: Vec<Value> = rows
        .into_iter()
        .take(page_size as usize)
        .map(|row| Value::Object(row_output(model, &row, false).0))
        .collect();
    Ok(CallResult {
        value: serde_json::json!({
            "items": items,
            "page": page,
            "page_size": page_size,
            "has_more": has_more,
        }),
        etag: None,
        status: 200,
    })
}

async fn update(
    exec: &dyn QueryExec,
    model: &ModelSpec,
    input: &Value,
    ctx: CallCtx<'_>,
) -> Result<CallResult, MethodFail> {
    let tenant_id = ctx.tenant_id;
    let role = ctx.role;
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let if_match = input
        .get("if_match")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return Err(MethodFail::code("VALIDATION_FAILED", "id is required"));
    }
    if if_match.is_empty() {
        return Err(MethodFail::code(
            "PRECONDITION_REQUIRED",
            "If-Match is required",
        ));
    }
    let current = get(exec, model, &serde_json::json!({"id": id}), ctx).await?;
    let current_etag = current.etag.unwrap_or_default();
    if current_etag != if_match {
        return Err(MethodFail::code(
            "PRECONDITION_FAILED",
            "If-Match does not match",
        ));
    }
    let updated_at = current
        .value
        .get("updated_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let obj = input.as_object().cloned().unwrap_or_default();
    reject_system_allow_id(&obj)?;
    let mut sets = Vec::new();
    for (key, value) in &obj {
        if matches!(key.as_str(), "id" | "if_match") {
            continue;
        }
        let Some(field) = model.fields.iter().find(|field| field.name == *key) else {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("unknown field {key}"),
            ));
        };
        if field.system || model.owner_key.as_deref() == Some(key.as_str()) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{key} is system-owned"),
            ));
        }
        check_field(field, value)?;
        sets.push((col(field), value.clone()));
    }
    if sets.is_empty() {
        return Err(MethodFail::code(
            "VALIDATION_FAILED",
            "update needs at least one field",
        ));
    }
    let touched = chrono::Utc::now().to_rfc3339();
    let rows = exec
        .execute(
            tenant_id,
            role,
            Stmt::Update {
                table: model.table.clone(),
                id,
                sets,
                expected_updated_at: updated_at,
                touched_at: touched,
                returning: all_cols(model),
                owner: owner_eq(model, ctx.caller)?,
            },
        )
        .await
        .map_err(db)?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| MethodFail::code("PRECONDITION_FAILED", "conflict"))?;
    Ok(finish_row(model, row, true))
}

async fn delete(
    exec: &dyn QueryExec,
    model: &ModelSpec,
    input: &Value,
    ctx: CallCtx<'_>,
) -> Result<CallResult, MethodFail> {
    let tenant_id = ctx.tenant_id;
    let role = ctx.role;
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return Err(MethodFail::code("VALIDATION_FAILED", "id is required"));
    }
    exec.execute(
        tenant_id,
        role,
        Stmt::SoftDelete {
            table: model.table.clone(),
            id,
            touched_at: chrono::Utc::now().to_rfc3339(),
            owner: owner_eq(model, ctx.caller)?,
        },
    )
    .await
    .map_err(db)?;
    Ok(CallResult {
        value: Value::Null,
        etag: None,
        status: 204,
    })
}

/// True when a route's steps reach a method with a `verify-password` step
/// (a login-class endpoint, padded to 500 ms).
pub fn route_verifies_password(manifest: &Manifest, route: &crate::manifest::RouteSpec) -> bool {
    fn method_verifies(manifest: &Manifest, target: &str, depth: u32) -> bool {
        let Some(method) = crate::manifest::method(manifest, target) else {
            return false;
        };
        method.steps.iter().any(|s| {
            s.op == "verify-password"
                || (s.op == "method"
                    && depth < 3
                    && method_verifies(manifest, &s.target, depth + 1))
        })
    }
    route
        .steps
        .iter()
        .any(|s| s.op == "method" && method_verifies(manifest, &s.target, 0))
}

fn finish_row(model: &ModelSpec, row: Row, include_sensitive: bool) -> CallResult {
    let (out, tag) = row_output(model, &row, include_sensitive);
    CallResult {
        value: Value::Object(out),
        etag: tag,
        status: 200,
    }
}

fn row_output(
    model: &ModelSpec,
    row: &Row,
    include_sensitive: bool,
) -> (Map<String, Value>, Option<String>) {
    let id = row.get("id").and_then(Value::as_str).unwrap_or("");
    let updated = row.get("updated_at").and_then(Value::as_str).unwrap_or("");
    let tag = if id.is_empty() {
        None
    } else {
        Some(etag(id, updated))
    };
    let mut out = Map::new();
    for field in &model.fields {
        if field.ty == "password"
            || matches!(
                field.name.as_str(),
                "tenant_id" | "deleted_at" | "erased_at"
            )
        {
            continue;
        }
        if field.sensitive
            && !(include_sensitive
                && model
                    .sensitive_output
                    .iter()
                    .any(|name| name == &field.name))
        {
            continue;
        }
        if let Some(value) = row.get(&field.name) {
            if value.is_null() && matches!(field.name.as_str(), "deleted_at" | "erased_at") {
                continue;
            }
            out.insert(field.name.clone(), value.clone());
        }
    }
    (out, tag)
}

fn all_cols(model: &ModelSpec) -> Vec<Col> {
    model.fields.iter().map(col).collect()
}

fn output_cols(model: &ModelSpec, include_sensitive: bool) -> Vec<Col> {
    model
        .fields
        .iter()
        .filter(|field| field.ty != "password")
        .filter(|field| {
            !matches!(
                field.name.as_str(),
                "tenant_id" | "deleted_at" | "erased_at"
            )
        })
        .filter(|field| !field.sensitive || include_sensitive)
        .map(col)
        .collect()
}

/// Parsed `ownership` relaxation of a model (spec §12.6a, Decision 70).
#[derive(Debug, PartialEq)]
pub enum Ownership<'a> {
    /// No relaxation: non-bypass callers are scoped to `owner-key = ctx.user.id`.
    Owner,
    TenantWide,
    /// Callers holding this role are not owner-scoped.
    Role(&'a str),
}

pub fn parse_ownership(raw: &str) -> Result<Ownership<'_>, String> {
    let raw = raw.trim();
    match raw {
        "" | "(none)" | "owner" => Ok(Ownership::Owner),
        "tenant-wide" => Ok(Ownership::TenantWide),
        _ => {
            let role = raw
                .strip_prefix("role(")
                .and_then(|rest| rest.strip_suffix(')'))
                .map(str::trim)
                .filter(|role| {
                    !role.is_empty()
                        && role.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
                        })
                })
                .ok_or_else(|| format!("unknown ownership {raw:?}"))?;
            Ok(Ownership::Role(role))
        }
    }
}

/// Owner filter for reads, updates and deletes. Fails closed: an unknown
/// `ownership` value or an `owner-key` that does not resolve to a column is an
/// internal error, never "no filter".
fn owner_eq(model: &ModelSpec, caller: Caller<'_>) -> Result<Option<(Col, Value)>, MethodFail> {
    let Some(key) = model.owner_key.as_deref() else {
        return Ok(None);
    };
    let ownership = parse_ownership(&model.ownership)
        .map_err(|err| MethodFail::code("INTERNAL", format!("model {}: {err}", model.id)))?;
    match ownership {
        Ownership::TenantWide => return Ok(None),
        Ownership::Role(role) if caller.roles.iter().any(|have| have == role) => return Ok(None),
        _ => {}
    }
    let column = match model.fields.iter().find(|field| field.name == key) {
        Some(field) => col(field),
        // Only the `users` table may use its own primary key (spec §12.6a).
        None if key == "id" && model.table == "users" => Col {
            name: "id".into(),
            ty: "uuid".into(),
        },
        None => {
            return Err(MethodFail::code(
                "INTERNAL",
                format!("model {}: owner-key {key} is not a field", model.id),
            ))
        }
    };
    // An empty subject matches no rows, so anonymous callers see nothing.
    Ok(Some((column, Value::String(caller.subject.to_string()))))
}

fn owner_filters(model: &ModelSpec, caller: Caller<'_>) -> Result<Vec<(Col, Value)>, MethodFail> {
    Ok(owner_eq(model, caller)?.into_iter().collect())
}

fn owner_value(
    model: &ModelSpec,
    field: &FieldSpec,
    caller: Caller<'_>,
) -> Result<Value, MethodFail> {
    let tenant_wide = parse_ownership(&model.ownership)
        .map_err(|err| MethodFail::code("INTERNAL", format!("model {}: {err}", model.id)))?
        == Ownership::TenantWide;
    if tenant_wide && caller.subject.is_empty() {
        return Ok(Value::Null);
    }
    if caller.subject.is_empty() {
        return Err(MethodFail::code(
            "VALIDATION_FAILED",
            format!("{} is set from the signed-in user", field.name),
        ));
    }
    let value = Value::String(caller.subject.to_string());
    check_field(field, &value)?;
    Ok(value)
}

fn col(field: &FieldSpec) -> Col {
    Col {
        name: field.name.clone(),
        ty: field.ty.clone(),
    }
}

fn reject_system(obj: &Map<String, Value>) -> Result<(), MethodFail> {
    for key in [
        "id",
        "tenant_id",
        "created_at",
        "updated_at",
        "deleted_at",
        "erased_at",
        "created_by",
    ] {
        if obj.contains_key(key) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{key} cannot be written"),
            ));
        }
    }
    Ok(())
}

fn reject_system_allow_id(obj: &Map<String, Value>) -> Result<(), MethodFail> {
    for key in [
        "tenant_id",
        "created_at",
        "updated_at",
        "deleted_at",
        "erased_at",
        "created_by",
    ] {
        if obj.contains_key(key) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{key} cannot be written"),
            ));
        }
    }
    Ok(())
}

fn check_field(field: &FieldSpec, value: &Value) -> Result<(), MethodFail> {
    if value.is_null() {
        if field.required && !field.nullable {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{} is required", field.name),
            ));
        }
        return Ok(());
    }
    match field.ty.as_str() {
        "string" | "password" => {
            let text = value.as_str().ok_or_else(|| {
                MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a string", field.name),
                )
            })?;
            if let Some(max) = field.max {
                if text.chars().count() as i64 > max {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is too long", field.name),
                    ));
                }
            }
            if let Some(min) = field.min {
                if (text.chars().count() as i64) < min {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is too short", field.name),
                    ));
                }
            }
            if field.format == "email" && !text.contains('@') {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be an email", field.name),
                ));
            }
        }
        "integer" => {
            let number = value.as_i64().ok_or_else(|| {
                MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be an integer", field.name),
                )
            })?;
            if let Some(min) = field.min {
                if number < min {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is below the minimum", field.name),
                    ));
                }
            }
            if let Some(max) = field.max {
                if number > max {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is above the maximum", field.name),
                    ));
                }
            }
        }
        "uuid" => {
            let text = value.as_str().unwrap_or("");
            if uuid::Uuid::parse_str(text).is_err() {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a uuid", field.name),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_sort(raw: &str, allowed: &[String]) -> Result<(String, bool), MethodFail> {
    let desc = raw.starts_with('-');
    let name = raw.trim_start_matches('-');
    let name = if name.is_empty() {
        allowed.first().map(String::as_str).unwrap_or("created_at")
    } else {
        name
    };
    if !allowed.iter().any(|item| item == name) {
        return Err(MethodFail::code(
            "VALIDATION_FAILED",
            "sort field is not allowed",
        ));
    }
    Ok((name.to_string(), desc))
}

fn int_field(input: &Value, key: &str) -> Option<i64> {
    match input.get(key)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        Value::Null => None,
        _ => None,
    }
}

fn db(err: ExecError) -> MethodFail {
    match err {
        ExecError::NotFound => MethodFail::code("NOT_FOUND", "not found"),
        ExecError::Conflict => MethodFail::code("CONFLICT", "conflict"),
        ExecError::Precondition => MethodFail::code("PRECONDITION_FAILED", "precondition failed"),
        ExecError::Message(message) => MethodFail::code("INTERNAL", message),
    }
}

/// Platform argon2id parameters; verify accepts only hashes made with them.
pub const ARGON2_M_COST: u32 = 65_536;
pub const ARGON2_T_COST: u32 = 3;
pub const ARGON2_P_COST: u32 = 1;

pub fn hash_password(plain: &str) -> Result<String, MethodFail> {
    let len = plain.chars().count();
    if !(12..=256).contains(&len)
        || matches!(plain, "password1234" | "123456789012" | "qwertyuiopas")
    {
        return Err(MethodFail::code(
            "WEAK_PASSWORD",
            "password does not meet the platform baseline",
        ));
    }
    let params = argon2::Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, Some(32))
        .map_err(|err| MethodFail::code("INTERNAL", err.to_string()))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let hash = argon2::password_hash::PasswordHasher::hash_password(&argon, plain.as_bytes())
        .map_err(|err| MethodFail::code("INTERNAL", err.to_string()))?;
    Ok(hash.to_string())
}

#[derive(Debug)]
pub struct MethodFail {
    pub code: String,
    pub message: String,
}

impl MethodFail {
    pub fn code(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub fn eval_expr(input: &str, vars: &Map<String, Value>) -> Result<Value, String> {
    let (expr, rest) = parse_expr(input.trim())?;
    if !rest.trim().is_empty() {
        return Err(format!("trailing input in expression {input}"));
    }
    Ok(eval_parsed(&expr, vars))
}

enum Expr {
    Path(String),
    Object(Vec<(String, Expr)>),
    /// A quoted string literal: `'register'` or `"..."` (no escapes).
    Lit(String),
}

fn parse_expr(input: &str) -> Result<(Expr, &str), String> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix('{') {
        let mut rest = rest.trim();
        let mut fields = Vec::new();
        if rest.starts_with('}') {
            let Some(stripped) = rest.strip_prefix('}') else {
                return Err("expected }".into());
            };
            return Ok((Expr::Object(fields), stripped));
        }
        loop {
            let (key, next) = take_ident(rest)?;
            let next = next.trim().strip_prefix(':').ok_or("expected :")?.trim();
            let (value, next) = parse_expr(next)?;
            fields.push((key, value));
            let next = next.trim();
            if let Some(next) = next.strip_prefix(',') {
                rest = next.trim();
                continue;
            }
            if let Some(next) = next.strip_prefix('}') {
                return Ok((Expr::Object(fields), next));
            }
            return Err("expected , or }".into());
        }
    }
    for quote in ['\'', '"'] {
        if let Some(body) = input.strip_prefix(quote) {
            let end = body
                .find(quote)
                .ok_or_else(|| format!("unterminated string in {input}"))?;
            return Ok((Expr::Lit(body[..end].to_string()), &body[end + 1..]));
        }
    }
    let (path, rest) = take_path(input)?;
    Ok((Expr::Path(path), rest))
}

fn take_ident(input: &str) -> Result<(String, &str), String> {
    let mut end = 0;
    for (idx, ch) in input.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return Err("expected a name".into());
    }
    Ok((input[..end].to_string(), &input[end..]))
}

fn take_path(input: &str) -> Result<(String, &str), String> {
    let (first, mut rest) = take_ident(input)?;
    let mut path = first;
    while let Some(next) = rest.strip_prefix('.') {
        let (seg, next_rest) = take_ident(next)?;
        path.push('.');
        path.push_str(&seg);
        rest = next_rest;
    }
    Ok((path, rest))
}

fn eval_parsed(expr: &Expr, vars: &Map<String, Value>) -> Value {
    match expr {
        Expr::Path(path) => lookup(path, vars),
        Expr::Lit(text) => Value::String(text.clone()),
        Expr::Object(fields) => {
            let mut obj = Map::new();
            for (key, value) in fields {
                let resolved = eval_parsed(value, vars);
                if !resolved.is_null() {
                    obj.insert(key.clone(), resolved);
                }
            }
            Value::Object(obj)
        }
    }
}

fn lookup(path: &str, vars: &Map<String, Value>) -> Value {
    let mut parts = path.split('.');
    let Some(root) = parts.next() else {
        return Value::Null;
    };
    let mut current = if root == "req" {
        vars.get("req").cloned().unwrap_or(Value::Null)
    } else {
        vars.get(root).cloned().unwrap_or(Value::Null)
    };
    for part in parts {
        current = match current {
            Value::Object(map) => map.get(part).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
    }
    current
}

/// A parsed `when` condition. Forms: `X is empty`, `X is not empty`,
/// `X == v`, `X != v` with `v` one of true, false, null. Surrounding
/// backticks (lock code spans) are stripped.
enum When {
    Empty(String, bool),
    Eq(String, Value, bool),
}

fn parse_when(expr: &str) -> Result<When, String> {
    let expr = expr.trim().trim_matches('`').trim();
    let path_ok = |p: &str| {
        !p.is_empty()
            && p.split('.').all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
    };
    if let Some(name) = expr.strip_suffix(" is not empty") {
        let name = name.trim();
        if path_ok(name) {
            return Ok(When::Empty(name.to_string(), false));
        }
    } else if let Some(name) = expr.strip_suffix(" is empty") {
        let name = name.trim();
        if path_ok(name) {
            return Ok(When::Empty(name.to_string(), true));
        }
    }
    for (op, equal) in [("!=", false), ("==", true)] {
        if let Some((left, right)) = expr.split_once(op) {
            let left = left.trim();
            let literal = match right.trim() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "null" => Value::Null,
                _ => break,
            };
            if path_ok(left) {
                return Ok(When::Eq(left.to_string(), literal, equal));
            }
        }
    }
    Err(format!("unsupported when condition `{expr}`"))
}

/// Why a `when` condition cannot run, if it cannot (validate uses this).
pub fn when_problem(expr: &str) -> Option<String> {
    parse_when(expr).err()
}

/// Checks a `when` condition the way the runtime parses it.
pub fn check_when(expr: &str) -> Result<(), String> {
    parse_when(expr).map(|_| ())
}

/// Checks that an expression parses the way the runtime will parse it.
pub fn check_expr(input: &str) -> Result<(), String> {
    let (_, rest) = parse_expr(input.trim())?;
    if !rest.trim().is_empty() {
        return Err(format!("trailing input in expression {input}"));
    }
    Ok(())
}

/// Evaluates a `when`. An unparseable condition is never "false": it fails
/// closed (the step's `fail` fires), and `vibe validate` rejects it first.
pub(crate) fn when_matches(expr: &str, vars: &Map<String, Value>) -> bool {
    let parsed = match parse_when(expr) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::error!(error = %err, "invalid when condition; failing closed");
            return true;
        }
    };
    match parsed {
        When::Empty(name, want) => {
            let value = lookup(&name, vars);
            let empty = value.is_null()
                || value.as_array().is_some_and(Vec::is_empty)
                || value.as_object().is_some_and(|m| m.is_empty())
                || value.as_str().is_some_and(str::is_empty);
            empty == want
        }
        When::Eq(name, literal, equal) => (lookup(&name, vars) == literal) == equal,
    }
}

/// `ctx.user` for step expressions: the caller's `sub`, `roles` and `tenant`
/// (`sub` is null on unauthenticated routes).
pub fn ctx_var(ctx: &CallCtx<'_>) -> Value {
    let sub = if ctx.caller.subject.is_empty() {
        Value::Null
    } else {
        Value::String(ctx.caller.subject.to_string())
    };
    serde_json::json!({
        "user": {"sub": sub, "roles": ctx.caller.roles, "tenant": ctx.tenant_id}
    })
}

pub fn req_var(
    body: Value,
    query: BTreeMap<String, Value>,
    path: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
) -> Value {
    let query = Value::Object(query.into_iter().collect());
    let path = Value::Object(
        path.into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    );
    let header = Value::Object(
        headers
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    );
    serde_json::json!({"body": body, "query": query, "path": path, "header": header})
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::manifest::{FieldSpec, ListOpts, ModelSpec};

    fn model(owner_key: Option<&str>, ownership: &str) -> ModelSpec {
        ModelSpec {
            id: "cart-item".into(),
            provider: "main".into(),
            table: "cart_items".into(),
            fields: vec![FieldSpec {
                name: "user_id".into(),
                ty: "string".into(),
                required: true,
                unique: false,
                primary: false,
                system: true,
                sensitive: false,
                nullable: false,
                min: None,
                max: None,
                format: String::new(),
            }],
            crud: vec![],
            list: ListOpts::default(),
            sensitive_output: vec![],
            owner_key: owner_key.map(str::to_string),
            ownership: ownership.into(),
        }
    }

    fn caller<'a>(subject: &'a str, roles: &'a [String]) -> Caller<'a> {
        Caller { subject, roles }
    }

    #[test]
    fn parses_ownership_values() {
        assert_eq!(parse_ownership(""), Ok(Ownership::Owner));
        assert_eq!(parse_ownership("tenant-wide"), Ok(Ownership::TenantWide));
        assert_eq!(
            parse_ownership("role(support)"),
            Ok(Ownership::Role("support"))
        );
        for bad in ["role()", "role(a b)", "admin", "everyone"] {
            assert!(parse_ownership(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn admin_is_not_hardcoded() {
        let admin = vec!["admin".to_string()];
        let filter = owner_eq(&model(Some("user_id"), ""), caller("u1", &admin)).unwrap();
        assert!(
            filter.is_some(),
            "admin must not bypass without ownership: role(admin)"
        );
    }

    #[test]
    fn role_ownership_bypasses_only_that_role() {
        let support = vec!["support".to_string()];
        let admin = vec!["admin".to_string()];
        let m = model(Some("user_id"), "role(support)");
        assert!(owner_eq(&m, caller("u1", &support)).unwrap().is_none());
        assert!(owner_eq(&m, caller("u1", &admin)).unwrap().is_some());
    }

    #[test]
    fn unresolved_owner_key_fails_closed() {
        let err = owner_eq(&model(Some("missing"), ""), caller("u1", &[])).unwrap_err();
        assert_eq!(err.code, "INTERNAL");
        let err = owner_eq(&model(Some("user_id"), "bogus"), caller("u1", &[])).unwrap_err();
        assert_eq!(err.code, "INTERNAL");
    }

    #[test]
    fn id_owner_key_is_only_for_users() {
        let err = owner_eq(&model(Some("id"), ""), caller("u1", &[])).unwrap_err();
        assert_eq!(err.code, "INTERNAL", "cart_items may not use owner-key: id");
        let mut users = model(Some("id"), "");
        users.table = "users".into();
        let (column, value) = owner_eq(&users, caller("u1", &[])).unwrap().unwrap();
        assert_eq!(column.name, "id");
        assert_eq!(value, Value::String("u1".into()));
    }
}

#[cfg(test)]
mod expr_check_tests {
    use super::{check_expr, check_when, when_matches};
    use serde_json::Map;

    #[test]
    fn expressions_and_conditions_are_checked_like_the_runtime() {
        assert!(check_expr("{sku: req.body.sku, qty: req.body.qty}").is_ok());
        assert!(check_expr("result").is_ok());
        assert!(check_expr("{sku: }").is_err());
        assert!(check_expr("a b").is_err());
        assert!(check_when("found is empty").is_ok());
        assert!(check_when("user.verified != true").is_ok());
        assert!(check_when("qty > 5").is_err());
        // Unknown conditions fail closed at runtime.
        assert!(when_matches("qty > 5", &Map::new()));
    }
}

#[cfg(test)]
mod password_hash_tests {
    use super::hash_password;
    use argon2::password_hash::{phc::PasswordHash, PasswordVerifier};
    use argon2::Argon2;

    fn verify(plain: &str, phc: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(phc) else {
            return false;
        };
        Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok()
    }

    /// PHC string produced by argon2 0.5 with the same params (m=65536,t=3,p=1)
    /// and fixed salt `AAAAAAAAAAAAAAAAAAAAAA` for password `correcthorse1`.
    const ARGON2_0_5_PHC: &str = "$argon2id$v=19$m=65536,t=3,p=1$AAAAAAAAAAAAAAAAAAAAAA$ItWDGBLmXI4K3WBpKL15+erEWf0pZ4NTFTyawNObDt8";

    #[test]
    fn argon2_0_5_phc_still_verifies() {
        assert!(verify("correcthorse1", ARGON2_0_5_PHC));
        assert!(!verify("wrong-password1", ARGON2_0_5_PHC));
    }

    #[test]
    fn fresh_hash_round_trips() {
        let hash = hash_password("correcthorse1").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify("correcthorse1", &hash));
        assert!(!verify("correcthorse2", &hash));
    }
}
