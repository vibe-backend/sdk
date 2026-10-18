//! Deliberately small, pure memory SQL classifier. Model-dependent checks happen at execution.
use sqlparser::ast::{self as a, Expr, Statement};

#[derive(Debug, Clone)]
pub struct Unsupported(pub String);
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MEMORY_UNSUPPORTED_SQL: the in-memory database supports only UPDATE ... SET c = c + n WHERE <unique> = $n, INSERT ... ON CONFLICT (<unique>) DO UPDATE|DO NOTHING, SELECT ... WHERE <unique> = $n and SELECT COALESCE(MAX(c), n); use --db postgres for: {}", self.0)
    }
}
impl std::error::Error for Unsupported {}

fn unsupported(reason: impl ToString) -> Unsupported {
    Unsupported(reason.to_string())
}
#[derive(Debug, Clone)]
pub enum Input {
    Literal(serde_json::Value),
    Param(usize),
    Excluded(String),
}
#[derive(Debug, Clone)]
pub struct Set {
    pub column: String,
    pub add: bool,
    pub input: Input,
}
#[derive(Debug, Clone)]
pub enum Action {
    Select,
    Max {
        column: String,
        default: i64,
        alias: String,
    },
    Update(Vec<Set>),
    Insert {
        columns: Vec<String>,
        values: Vec<Input>,
        sets: Option<Vec<Set>>,
    },
}
#[derive(Debug, Clone)]
pub struct MemPlan {
    pub table: String,
    pub unique: String,
    pub param: Option<usize>,
    pub returning: Vec<String>,
    pub action: Action,
}
fn ident(i: &a::Ident) -> Result<String, Unsupported> {
    let name = if i.quote_style.is_some() {
        i.value.clone()
    } else {
        i.value.to_lowercase()
    };
    if name == "tenant_id" {
        return Err(unsupported("tenant_id is managed by the runtime"));
    }
    Ok(name)
}
fn object(n: &a::ObjectName) -> Result<String, Unsupported> {
    match n.0.as_slice() {
        [i] => ident(i),
        _ => Err(unsupported("qualified table or assignment target")),
    }
}
fn column(e: &Expr, table: &str) -> Result<String, Unsupported> {
    match e {
        Expr::Identifier(i) => ident(i),
        Expr::CompoundIdentifier(parts) => match parts.as_slice() {
            [t, c] if ident(t)? == table => ident(c),
            _ => Err(unsupported("column qualifier")),
        },
        _ => Err(unsupported(format!("expression {e}"))),
    }
}
fn input(e: &Expr, excluded: bool) -> Result<Input, Unsupported> {
    use serde_json::Value;
    match e {
        Expr::Value(a::Value::Placeholder(p)) => p
            .strip_prefix('$')
            .and_then(|p| p.parse::<usize>().ok())
            .filter(|p| *p > 0)
            .map(Input::Param)
            .ok_or_else(|| unsupported("parameter placeholder")),
        Expr::Value(a::Value::Number(n, false)) => n
            .parse::<serde_json::Number>()
            .map(|n| Input::Literal(n.into()))
            .map_err(|_| unsupported("out-of-range numeric literal")),
        Expr::UnaryOp {
            op: a::UnaryOperator::Minus,
            expr,
        } => match input(expr, false)? {
            Input::Literal(Value::Number(n)) => n
                .as_i64()
                .and_then(i64::checked_neg)
                .map(|n| Input::Literal(n.into()))
                .ok_or_else(|| unsupported("integer literal overflow")),
            _ => Err(unsupported("unary expression")),
        },
        Expr::Value(a::Value::SingleQuotedString(s)) => Ok(Input::Literal(s.clone().into())),
        Expr::Value(a::Value::Boolean(b)) => Ok(Input::Literal((*b).into())),
        Expr::Value(a::Value::Null) => Ok(Input::Literal(Value::Null)),
        Expr::CompoundIdentifier(parts) if excluded => match parts.as_slice() {
            [t, c] if t.value.eq_ignore_ascii_case("excluded") => Ok(Input::Excluded(ident(c)?)),
            _ => Err(unsupported("EXCLUDED expression")),
        },
        _ => Err(unsupported(format!("value expression {e}"))),
    }
}
fn assignments(
    items: &[a::Assignment],
    table: &str,
    upsert: bool,
) -> Result<Vec<Set>, Unsupported> {
    let mut seen = std::collections::BTreeSet::new();
    items
        .iter()
        .map(|item| {
            let a::AssignmentTarget::ColumnName(n) = &item.target else {
                return Err(unsupported("tuple assignment"));
            };
            let c = object(n)?;
            if !seen.insert(c.clone()) {
                return Err(unsupported("duplicate SET column"));
            }
            let (add, value) = match &item.value {
                Expr::BinaryOp {
                    left,
                    op: a::BinaryOperator::Plus,
                    right,
                } if column(left, table)? == c => (true, input(right, upsert)?),
                e if !upsert => (false, input(e, false)?),
                _ => return Err(unsupported(format!("SET expression {}", item.value))),
            };
            if add
                && !matches!(
                    value,
                    Input::Param(_)
                        | Input::Excluded(_)
                        | Input::Literal(serde_json::Value::Number(_))
                )
            {
                return Err(unsupported("non-integer increment"));
            }
            if add && matches!(&value, Input::Literal(serde_json::Value::Number(n)) if n.as_i64().is_none()) {
                return Err(unsupported("non-integer increment literal"));
            }
            if upsert && matches!(value, Input::Param(_)) {
                return Err(unsupported("upsert parameter increment"));
            }
            Ok(Set {
                column: c,
                add,
                input: value,
            })
        })
        .collect()
}
fn projection(items: &[a::SelectItem], table: &str) -> Result<Vec<String>, Unsupported> {
    items
        .iter()
        .map(|i| match i {
            a::SelectItem::Wildcard(options)
                if *options == a::WildcardAdditionalOptions::default() =>
            {
                Ok("*".into())
            }
            a::SelectItem::UnnamedExpr(e) => column(e, table),
            _ => Err(unsupported(format!("projection {i}"))),
        })
        .collect()
}
fn scope(e: Option<&Expr>, table: &str) -> Result<(String, usize), Unsupported> {
    match e {
        Some(Expr::BinaryOp {
            left,
            op: a::BinaryOperator::Eq,
            right,
        }) => match input(right, false)? {
            Input::Param(n) => Ok((column(left, table)?, n)),
            _ => Err(unsupported("WHERE must compare a unique column to $n")),
        },
        _ => Err(unsupported("WHERE shape (expected <unique> = $n)")),
    }
}
fn joined<T: std::fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn function_args<'a>(expr: &'a Expr, name: &str) -> Result<Vec<&'a Expr>, Unsupported> {
    let Expr::Function(f) = expr else {
        return Err(unsupported(format!("expected {name} function")));
    };
    if f.name.0.len() != 1
        || !f.name.to_string().eq_ignore_ascii_case(name)
        || !matches!(f.parameters, a::FunctionArguments::None)
        || f.filter.is_some()
        || f.over.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return Err(unsupported(format!("function/modifier {expr}")));
    }
    let a::FunctionArguments::List(args) = &f.args else {
        return Err(unsupported("function arguments"));
    };
    if args.duplicate_treatment.is_some() || !args.clauses.is_empty() {
        return Err(unsupported("DISTINCT or function argument clause"));
    }
    args.args
        .iter()
        .map(|arg| match arg {
            a::FunctionArg::Unnamed(a::FunctionArgExpr::Expr(e)) => Ok(e),
            _ => Err(unsupported("named or wildcard function argument")),
        })
        .collect()
}

impl MemPlan {
    /// Check schema-dependent allowlist rules, also used for build/startup warnings.
    pub fn check_models(&self, models: &[crate::manifest::ModelSpec]) -> Result<(), Unsupported> {
        let model = models
            .iter()
            .find(|m| m.table == self.table)
            .ok_or_else(|| unsupported(format!("unconfigured table {}", self.table)))?;
        let f = model
            .fields
            .iter()
            .find(|f| f.name == self.unique)
            .ok_or_else(|| unsupported(format!("unknown column {}", self.unique)))?;
        if self.unique != "id"
            && !f.unique
            && !f.primary
            && !model
                .constraints
                .iter()
                .any(|c| c.len() == 1 && c.first() == Some(&self.unique))
        {
            return Err(unsupported(format!(
                "WHERE/conflict column {} is not single-column unique",
                self.unique
            )));
        }
        let integer_column = |name: &str| -> Result<(), Unsupported> {
            if model
                .fields
                .iter()
                .any(|f| f.name == name && f.ty == "integer")
            {
                Ok(())
            } else {
                Err(unsupported(format!(
                    "integer operation on non-integer or unknown column {name}"
                )))
            }
        };
        let sets = match &self.action {
            Action::Update(sets)
            | Action::Insert {
                sets: Some(sets), ..
            } => sets.as_slice(),
            Action::Max { column, .. } => {
                integer_column(column)?;
                &[]
            }
            _ => &[],
        };
        for set in sets {
            if set.add {
                integer_column(&set.column)?;
            }
            if let Input::Excluded(c) = &set.input {
                integer_column(c)?;
            }
        }

        Ok(())
    }
}

/// Classify without executing or needing a model registry. Unique/table/type checks are deferred.
pub fn classify(sql: &str) -> Result<MemPlan, Unsupported> {
    let statement = crate::sqlguard::parse_one(sql).map_err(unsupported)?;
    let (plan, canonical) = match &statement {
        Statement::Update {
            table,
            assignments: sets,
            selection,
            returning,
            ..
        } => {
            let a::TableFactor::Table { name, .. } = &table.relation else {
                return Err(unsupported("UPDATE relation"));
            };
            let t = object(name)?;
            let (unique, param) = scope(selection.as_ref(), &t)?;
            let ret = returning
                .as_ref()
                .ok_or_else(|| unsupported("UPDATE without RETURNING"))?;
            let plan = MemPlan {
                table: t.clone(),
                unique,
                param: Some(param),
                returning: projection(ret, &t)?,
                action: Action::Update(assignments(sets, &t, false)?),
            };
            (
                plan,
                format!(
                    "UPDATE {name} SET {} WHERE {} RETURNING {}",
                    joined(sets),
                    selection.as_ref().ok_or_else(|| unsupported("WHERE"))?,
                    joined(ret)
                ),
            )
        }
        Statement::Insert(i) => {
            let t = object(&i.table_name)?;
            let source = i
                .source
                .as_ref()
                .ok_or_else(|| unsupported("INSERT source"))?;
            let a::SetExpr::Values(values) = source.body.as_ref() else {
                return Err(unsupported("INSERT SELECT"));
            };
            let [row] = values.rows.as_slice() else {
                return Err(unsupported("multiple VALUES rows"));
            };
            if row.len() != i.columns.len() {
                return Err(unsupported("INSERT column/value count"));
            }
            let Some(a::OnInsert::OnConflict(conflict)) = &i.on else {
                return Err(unsupported("INSERT without ON CONFLICT"));
            };
            let Some(a::ConflictTarget::Columns(cols)) = &conflict.conflict_target else {
                return Err(unsupported("conflict target"));
            };
            let [unique] = cols.as_slice() else {
                return Err(unsupported("composite conflict target"));
            };
            let sets = match &conflict.action {
                a::OnConflictAction::DoNothing => None,
                a::OnConflictAction::DoUpdate(u) if u.selection.is_none() => {
                    Some(assignments(&u.assignments, &t, true)?)
                }
                _ => return Err(unsupported("ON CONFLICT WHERE")),
            };
            if sets.is_some() && i.returning.is_none() {
                return Err(unsupported("upsert without RETURNING"));
            }
            let ret = i.returning.as_deref().unwrap_or_default();
            let plan = MemPlan {
                table: t.clone(),
                unique: ident(unique)?,
                param: None,
                returning: projection(ret, &t)?,
                action: Action::Insert {
                    columns: i.columns.iter().map(ident).collect::<Result<_, _>>()?,
                    values: row
                        .iter()
                        .map(|e| input(e, false))
                        .collect::<Result<_, _>>()?,
                    sets,
                },
            };
            (
                plan,
                format!(
                    "INSERT INTO {} ({}) VALUES ({}) {}{}",
                    i.table_name,
                    joined(&i.columns),
                    joined(row),
                    conflict,
                    if ret.is_empty() {
                        String::new()
                    } else {
                        format!(" RETURNING {}", joined(ret))
                    }
                ),
            )
        }
        Statement::Query(q) => {
            let a::SetExpr::Select(s) = q.body.as_ref() else {
                return Err(unsupported("subquery or set operation"));
            };
            let [from] = s.from.as_slice() else {
                return Err(unsupported("multiple FROM tables"));
            };
            let a::TableFactor::Table { name, .. } = &from.relation else {
                return Err(unsupported("subquery FROM"));
            };
            let t = object(name)?;
            let (unique, param) = scope(s.selection.as_ref(), &t)?;
            let mut action = Action::Select;
            let returning = if let [a::SelectItem::ExprWithAlias { expr, alias }] =
                s.projection.as_slice()
            {
                let args = function_args(expr, "coalesce")?;
                let [max, default] = args.as_slice() else {
                    return Err(unsupported("COALESCE arguments"));
                };
                let max_args = function_args(max, "max")?;
                let [c] = max_args.as_slice() else {
                    return Err(unsupported("MAX arguments"));
                };
                let Input::Literal(serde_json::Value::Number(n)) = input(default, false)? else {
                    return Err(unsupported("COALESCE integer default"));
                };
                action = Action::Max {
                    column: column(c, &t)?,
                    default: n
                        .as_i64()
                        .ok_or_else(|| unsupported("COALESCE integer default"))?,
                    alias: ident(alias)?,
                };
                Vec::new()
            } else {
                projection(&s.projection, &t)?
            };
            let canonical = format!(
                "SELECT {} FROM {name} WHERE {}",
                joined(&s.projection),
                s.selection.as_ref().ok_or_else(|| unsupported("WHERE"))?
            );
            (
                MemPlan {
                    table: t,
                    unique,
                    param: Some(param),
                    returning,
                    action,
                },
                canonical,
            )
        }
        _ => return Err(unsupported(format!("statement {statement}"))),
    };
    // Only the explicitly reconstructed grammar is accepted. This rejects joins, CTEs,
    // aliases, limits, ordering, locks and dialect extensions without silently ignoring them.
    if crate::sqlguard::parse_one(&canonical).map_err(unsupported)? != statement {
        return Err(unsupported(
            "JOIN, alias, subquery, clause or statement modifier outside the allowlist",
        ));
    }
    Ok(plan)
}

use super::exec::{ExecError, Row};
use crate::manifest::ModelSpec;
use serde_json::Value;
fn error(message: impl Into<String>) -> ExecError {
    ExecError::Message(message.into())
}
fn field<'a>(
    model: &'a ModelSpec,
    name: &str,
) -> Result<&'a crate::manifest::FieldSpec, ExecError> {
    if name == "tenant_id" {
        return Err(error("tenant_id is managed by the runtime"));
    }
    model
        .fields
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| error(format!("unknown column {name}")))
}
fn coerce(value: Value, ty: &str) -> Result<Value, ExecError> {
    if value.is_null() {
        return Ok(value);
    }
    match ty {
        "integer" => {
            let n = value
                .as_i64()
                .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
                .ok_or_else(|| error("invalid integer parameter"))?;
            i32::try_from(n).map_err(|_| error("integer overflow"))?;
            Ok(n.into())
        }
        "number" => value
            .as_f64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| error("invalid number parameter")),
        "boolean" => match value {
            Value::Bool(_) => Ok(value),
            Value::String(s) => match s.as_str() {
                "true" | "t" => Ok(true.into()),
                "false" | "f" => Ok(false.into()),
                _ => Err(error("invalid boolean parameter")),
            },
            _ => Err(error("invalid boolean parameter")),
        },
        "json" => value
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .ok_or_else(|| error("invalid json parameter")),
        "timestamp" => value
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.to_rfc3339().into())
            .ok_or_else(|| error("invalid timestamp parameter")),
        "uuid" => value
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(|id| id.to_string().into())
            .ok_or_else(|| error("invalid uuid parameter")),
        _ => {
            if value.is_string() {
                Ok(value)
            } else {
                Err(error(format!("invalid {ty} parameter")))
            }
        }
    }
}
fn bound(input: &Input, params: &[Option<String>], excluded: &Row) -> Result<Value, ExecError> {
    match input {
        Input::Literal(v) => Ok(v.clone()),
        Input::Param(n) => params
            .get(n.saturating_sub(1))
            .map(|v| v.clone().map(Value::String).unwrap_or(Value::Null))
            .ok_or_else(|| error(format!("missing parameter ${n}"))),
        Input::Excluded(c) => excluded
            .get(c)
            .cloned()
            .ok_or_else(|| error(format!("unknown EXCLUDED column {c}"))),
    }
}
fn set_values(
    model: &ModelSpec,
    row: &Row,
    sets: &[Set],
    params: &[Option<String>],
    excluded: &Row,
) -> Result<Row, ExecError> {
    let mut result = row.clone();
    for set in sets {
        let f = field(model, &set.column)?;
        if f.system {
            return Err(error(format!("{} cannot be written", f.name)));
        }
        let value = coerce(bound(&set.input, params, excluded)?, &f.ty)?;
        let value = if set.add {
            if f.ty != "integer" {
                return Err(error("addition requires integer columns"));
            }
            if value.is_null() || row.get(&f.name).is_none_or(Value::is_null) {
                Value::Null
            } else {
                let a = row
                    .get(&f.name)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| error("invalid stored integer"))?;
                let b = value
                    .as_i64()
                    .ok_or_else(|| error("invalid integer increment"))?;
                let n = a.checked_add(b).ok_or_else(|| error("integer overflow"))?;
                coerce(n.into(), "integer")?
            }
        } else {
            value
        };
        result.insert(f.name.clone(), value);
    }
    result.insert("updated_at".into(), chrono::Utc::now().to_rfc3339().into());
    Ok(result)
}
fn not_null(model: &ModelSpec, row: &Row) -> Result<(), ExecError> {
    for f in &model.fields {
        if (f.required || !f.nullable) && row.get(&f.name).is_none_or(Value::is_null) {
            return Err(error(format!("NOT NULL constraint violated: {}", f.name)));
        }
    }
    Ok(())
}
fn project(row: &Row, columns: &[String], model: &ModelSpec) -> Result<Row, ExecError> {
    let mut result = Row::new();
    for c in columns {
        if c == "*" {
            for f in &model.fields {
                result.insert(
                    f.name.clone(),
                    row.get(&f.name).cloned().unwrap_or(Value::Null),
                );
            }
        } else {
            field(model, c)?;
            result.insert(c.clone(), row.get(c).cloned().unwrap_or(Value::Null));
        }
    }
    Ok(result)
}
/// Evaluate against a private candidate copy. The caller commits only after uniqueness checks.
pub(super) fn evaluate(
    plan: &MemPlan,
    model: &ModelSpec,
    rows: &mut Vec<Row>,
    tenant: &str,
    params: &[Option<String>],
) -> Result<Vec<Row>, ExecError> {
    let unique_field = field(model, &plan.unique)?;
    plan.check_models(std::slice::from_ref(model))
        .map_err(|e| error(e.to_string()))?;
    let sets = match &plan.action {
        Action::Update(sets) => sets.as_slice(),
        Action::Insert {
            sets: Some(sets), ..
        } => sets.as_slice(),
        _ => &[],
    };
    for set in sets {
        let f = field(model, &set.column)?;
        if f.system {
            return Err(error(format!("{} cannot be written", f.name)));
        }
        if set.add && f.ty != "integer" {
            return Err(error("addition requires integer columns"));
        }
        if let Input::Excluded(c) = &set.input {
            if field(model, c)?.ty != "integer" {
                return Err(error("EXCLUDED addition requires an integer column"));
            }
        }
    }
    // Validate projection even if the lookup finds no row.
    project(&Row::new(), &plan.returning, model)?;
    let index = if let Some(param) = plan.param {
        let value = coerce(
            bound(&Input::Param(param), params, &Row::new())?,
            &unique_field.ty,
        )?;
        rows.iter()
            .position(|r| !value.is_null() && r.get(&plan.unique) == Some(&value))
    } else {
        None
    };
    match &plan.action {
        Action::Select => index
            .and_then(|i| rows.get(i))
            .map(|r| project(r, &plan.returning, model).map(|r| vec![r]))
            .unwrap_or(Ok(vec![])),
        Action::Max {
            column,
            default,
            alias,
        } => {
            if field(model, column)?.ty != "integer" {
                return Err(error("MAX requires an integer column"));
            }
            coerce((*default).into(), "integer")?;
            let value = index
                .and_then(|i| rows.get(i))
                .and_then(|r| r.get(column))
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| (*default).into());
            Ok(vec![Row::from_iter([(alias.clone(), value)])])
        }
        Action::Update(sets) => {
            // Validate SET columns/types even for an empty result.
            for set in sets {
                let f = field(model, &set.column)?;
                if f.system {
                    return Err(error("system column cannot be written"));
                }
                if set.add && f.ty != "integer" {
                    return Err(error("addition requires integer columns"));
                }
                coerce(bound(&set.input, params, &Row::new())?, &f.ty)?;
            }
            let Some(row) = index.and_then(|i| rows.get_mut(i)) else {
                return Ok(vec![]);
            };
            let candidate = set_values(model, row, sets, params, &Row::new())?;
            not_null(model, &candidate)?;
            let result = project(&candidate, &plan.returning, model)?;
            *row = candidate;
            Ok(vec![result])
        }
        Action::Insert {
            columns,
            values,
            sets,
        } => {
            let mut row = Row::new();
            let now = chrono::Utc::now().to_rfc3339();
            for f in &model.fields {
                let value = match f.name.as_str() {
                    "id" => uuid::Uuid::new_v4().to_string().into(),
                    "tenant_id" => tenant.into(),
                    "created_at" | "updated_at" => now.clone().into(),
                    _ => Value::Null,
                };
                row.insert(f.name.clone(), value);
            }
            let mut seen = std::collections::BTreeSet::new();
            for (c, value) in columns.iter().zip(values) {
                let f = field(model, c)?;
                if f.system || !seen.insert(c) {
                    return Err(error(format!(
                        "column {c} cannot be written or is duplicated"
                    )));
                }
                row.insert(
                    c.clone(),
                    coerce(bound(value, params, &Row::new())?, &f.ty)?,
                );
            }
            not_null(model, &row)?;
            let conflict = rows.iter().position(|r| {
                row.get(&plan.unique).is_some_and(|v| !v.is_null())
                    && r.get(&plan.unique) == row.get(&plan.unique)
            });
            if let Some(existing) = conflict.and_then(|i| rows.get_mut(i)) {
                let Some(sets) = sets else {
                    return Ok(vec![]);
                };
                let candidate = set_values(model, existing, sets, params, &row)?;
                not_null(model, &candidate)?;
                let result = project(&candidate, &plan.returning, model)?;
                *existing = candidate;
                Ok(vec![result])
            } else {
                let result = project(&row, &plan.returning, model)?;
                rows.push(row);
                Ok(if plan.returning.is_empty() {
                    vec![]
                } else {
                    vec![result]
                })
            }
        }
    }
}
