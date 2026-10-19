//! Deterministic SQL policy for lock statements (deploy gate).
//!
//! Every non-migration statement must parse (PostgreSQL dialect) as exactly
//! one allowlisted statement: a query (`SELECT`, `WITH ...`), `INSERT`, or
//! `UPDATE`. `UPDATE` and `DELETE`, including ones nested in CTEs, need a
//! `WHERE` that passes an allowlist: every OR branch must compare a column
//! with a placeholder or a session setting, and every other conjunct must be
//! a plain column restriction. Everything else is rejected, and anything
//! that does not parse fails closed.

use sqlparser::ast::{BinaryOperator, Expr, Query, SetExpr, Statement, Value};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

pub fn problems(sql: &str) -> Vec<String> {
    let statements = match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
        Ok(statements) => statements,
        Err(err) => {
            return vec![format!(
                "SQL does not parse under the allowlist parser: {err}"
            )]
        }
    };
    if statements.len() != 1 {
        return vec!["SQL must be a single statement".into()];
    }
    let mut out = Vec::new();
    match &statements[0] {
        Statement::Query(_)
        | Statement::Insert(_)
        | Statement::Update { .. }
        | Statement::Delete(_) => check_statement(&statements[0], &mut out),
        other => out.push(format!(
            "SQL statement type is not allowed: {}",
            other.to_string().split_whitespace().next().unwrap_or("?")
        )),
    }
    out
}

fn check_statement(statement: &Statement, out: &mut Vec<String>) {
    match statement {
        Statement::Update { selection, .. } => check_where("UPDATE", selection.as_ref(), out),
        Statement::Delete(delete) => check_where("DELETE", delete.selection.as_ref(), out),
        Statement::Insert(insert) => {
            if let Some(source) = &insert.source {
                check_query(source, out);
            }
        }
        Statement::Query(query) => check_query(query, out),
        _ => out.push("SQL statement type is not allowed".into()),
    }
}

fn check_query(query: &Query, out: &mut Vec<String>) {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            check_query(&cte.query, out);
        }
    }
    check_set(&query.body, out);
}

fn check_set(body: &SetExpr, out: &mut Vec<String>) {
    match body {
        SetExpr::Query(query) => check_query(query, out),
        SetExpr::SetOperation { left, right, .. } => {
            check_set(left, out);
            check_set(right, out);
        }
        SetExpr::Insert(statement) | SetExpr::Update(statement) => check_statement(statement, out),
        SetExpr::Select(_) | SetExpr::Values(_) | SetExpr::Table(_) => {}
    }
}

fn check_where(kind: &str, selection: Option<&Expr>, out: &mut Vec<String>) {
    match selection {
        None => out.push(format!("{kind} needs a WHERE clause")),
        Some(expr) if !scoped(expr) => out.push(format!(
            "{kind} WHERE is always true or not allowlisted (`{expr}`): every OR branch must compare a column with a placeholder or a session setting"
        )),
        Some(_) => {}
    }
}

/// Allowlist: the predicate is split into OR branches; each branch is an AND
/// of conjuncts. Every branch needs an anchor (`column = $n`,
/// `column = ANY($n)`, or `column = current_setting(...)`), and every other
/// conjunct must be a plain column restriction. Anything else is rejected.
fn scoped(expr: &Expr) -> bool {
    let mut branches = Vec::new();
    split(expr, BinaryOperator::Or, &mut branches);
    branches.iter().all(|branch| {
        let mut conjuncts = Vec::new();
        split(branch, BinaryOperator::And, &mut conjuncts);
        conjuncts.iter().any(|c| anchor(c)) && conjuncts.iter().all(|c| anchor(c) || restriction(c))
    })
}

fn split<'a>(expr: &'a Expr, op: BinaryOperator, out: &mut Vec<&'a Expr>) {
    match expr {
        Expr::Nested(inner) => split(inner, op, out),
        Expr::BinaryOp { left, op: o, right } if *o == op => {
            split(left, op.clone(), out);
            split(right, op, out);
        }
        other => out.push(other),
    }
}

fn unnest(expr: &Expr) -> &Expr {
    match expr {
        Expr::Nested(inner) => unnest(inner),
        other => other,
    }
}

fn is_column(expr: &Expr) -> bool {
    match unnest(expr) {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        // Main's refresh-session fixture compares the UUID id as text with
        // the JWT subject. Keep this column anchor; arbitrary casts remain
        // outside the allowlist.
        Expr::Cast {
            expr,
            data_type: sqlparser::ast::DataType::Text,
            ..
        } => is_column(expr),
        _ => false,
    }
}

fn is_placeholder(expr: &Expr) -> bool {
    match unnest(expr) {
        Expr::Value(Value::Placeholder(_)) => true,
        Expr::Cast { expr, .. } => is_placeholder(expr),
        _ => false,
    }
}

/// `current_setting('<literal>'[, <bool literal>])`, optionally cast.
fn is_setting(expr: &Expr) -> bool {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
    match unnest(expr) {
        Expr::Cast { expr, .. } => is_setting(expr),
        Expr::Function(f) => {
            if !f.name.to_string().eq_ignore_ascii_case("current_setting") {
                return false;
            }
            let FunctionArguments::List(list) = &f.args else {
                return false;
            };
            !list.args.is_empty()
                && list.args.len() <= 2
                && list.args.iter().enumerate().all(|(i, arg)| match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(v))) => {
                        if i == 0 {
                            matches!(v, Value::SingleQuotedString(_))
                        } else {
                            matches!(v, Value::Boolean(_))
                        }
                    }
                    _ => false,
                })
        }
        _ => false,
    }
}

fn is_literal(expr: &Expr) -> bool {
    match unnest(expr) {
        Expr::Value(Value::Placeholder(_)) | Expr::Value(Value::Boolean(_)) => false,
        Expr::Value(Value::Null) => false,
        Expr::Value(_) => true,
        Expr::Cast { expr, .. } => is_literal(expr),
        _ => false,
    }
}

fn anchor(expr: &Expr) -> bool {
    match unnest(expr) {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            (is_column(left) && (is_placeholder(right) || is_setting(right)))
                || (is_column(right) && (is_placeholder(left) || is_setting(left)))
        }
        Expr::AnyOp {
            left,
            compare_op: BinaryOperator::Eq,
            right,
            ..
        } => is_column(left) && is_placeholder(right),
        _ => false,
    }
}

fn restriction(expr: &Expr) -> bool {
    use BinaryOperator::*;
    let operand = |e: &Expr| is_placeholder(e) || is_setting(e) || is_literal(e);
    match unnest(expr) {
        Expr::BinaryOp {
            left,
            op: Eq | NotEq | Lt | LtEq | Gt | GtEq,
            right,
        } => (is_column(left) && operand(right)) || (is_column(right) && operand(left)),
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => is_column(inner),
        Expr::InList { expr, list, .. } => is_column(expr) && list.iter().all(operand),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::problems;

    fn rejected(sql: &str) -> String {
        let found = problems(sql);
        assert!(!found.is_empty(), "accepted: {sql}");
        found.join("; ")
    }

    #[test]
    fn accepts_scoped_statements() {
        for sql in [
            "SELECT id, name FROM products WHERE id = $1",
            "INSERT INTO products (sku, name) VALUES ($1, $2) RETURNING id",
            "UPDATE products SET name = $1 WHERE id = $2",
            "UPDATE users SET last_device = $1 WHERE id::text = $2",
            "UPDATE users SET a = $1 WHERE id = $2 AND tenant_id = current_setting('app.tenant_id', false)",
            "INSERT INTO users (email) VALUES ($1) ON CONFLICT (tenant_id, email) DO UPDATE SET email = EXCLUDED.email WHERE users.deleted_at IS NULL",
            "WITH x AS (INSERT INTO t (a) VALUES ($1) RETURNING id) SELECT id FROM x",
        ] {
            assert!(problems(sql).is_empty(), "{sql}: {:?}", problems(sql));
        }
    }

    #[test]
    fn rejects_tautological_where() {
        for sql in [
            "UPDATE products SET price_cents = 0 WHERE true",
            "UPDATE products SET price_cents = 0 WHERE true OR id = $1",
            "UPDATE products SET price_cents = 0 WHERE id = $1 OR true",
            "UPDATE products SET price_cents = 0 WHERE 1 = 1",
            "UPDATE products SET price_cents = 0 WHERE (id = $1) OR (1 = 1)",
            "UPDATE products SET price_cents = 0 WHERE id = id",
            "UPDATE products SET price_cents = 0 WHERE $1 = $1",
            "UPDATE products SET price_cents = 0 WHERE NOT false",
            "UPDATE products SET price_cents = 0 WHERE 'a' <> 'b'",
            "DELETE FROM products WHERE 1 = 1",
            "UPDATE products SET price_cents = 0 WHERE NOT (1 = 0)",
            "UPDATE products SET price_cents = 0 WHERE 'a' LIKE 'a'",
            "UPDATE products SET price_cents = 0 WHERE 1 IN (1, 2)",
            "UPDATE products SET price_cents = 0 WHERE 2 BETWEEN 1 AND 3",
            "UPDATE products SET price_cents = 0 WHERE 't'::boolean",
            "UPDATE products SET price_cents = 0 WHERE COALESCE(true, id = $1)",
            "UPDATE products SET price_cents = 0 WHERE CASE WHEN true THEN true ELSE false END",
            "UPDATE products SET price_cents = 0 WHERE EXISTS (SELECT 1)",
            "UPDATE products SET price_cents = 0 WHERE (SELECT true)",
            "UPDATE products SET price_cents = 0 WHERE id IS NOT NULL",
            "UPDATE products SET price_cents = 0 WHERE id = id + 0",
            "UPDATE products SET price_cents = 0 WHERE x > 0 OR x <= 0",
            "UPDATE products SET price_cents = 0 WHERE lower('a') = 'a'",
            "UPDATE products SET price_cents = 0 WHERE id = $1 OR id IS NOT NULL",
            "UPDATE products SET price_cents = 0 WHERE id = $1 OR lower('a') = 'a'",
            "DELETE FROM products WHERE EXISTS (SELECT 1)",
        ] {
            assert!(rejected(sql).contains("always true"), "{sql}");
        }
    }

    #[test]
    fn rejects_tautologies_inside_cte_updates() {
        for cond in [
            "NOT (1 = 0)",
            "'a' LIKE 'a'",
            "1 IN (1)",
            "2 BETWEEN 1 AND 3",
            "'t'::boolean",
            "COALESCE(true, false)",
            "CASE WHEN true THEN true END",
            "EXISTS (SELECT 1)",
            "(SELECT true)",
            "id IS NOT NULL",
            "id = id + 0",
            "x > 0 OR x <= 0",
            "lower('a') = 'a'",
        ] {
            let sql = format!(
                "WITH x AS (UPDATE products SET price_cents = 0 WHERE {cond} RETURNING id) SELECT id FROM x"
            );
            assert!(rejected(&sql).contains("always true"), "{sql}");
        }
    }

    #[test]
    fn accepts_anchored_restrictions() {
        for sql in [
            "UPDATE carts SET qty = $1 WHERE id = $2::uuid AND deleted_at IS NULL AND version = $3",
            "UPDATE orders SET status = 'paid' WHERE id = $1 AND status IN ('pending', 'unpaid')",
            "UPDATE t SET a = $1 WHERE id = ANY($2) OR owner_id = current_setting('app.user_id', false)::uuid",
        ] {
            assert!(problems(sql).is_empty(), "{sql}: {:?}", problems(sql));
        }
    }

    #[test]
    fn rejects_missing_where_and_nested_writes() {
        assert!(rejected("UPDATE products SET price_cents = 0").contains("needs a WHERE"));
        assert!(rejected("DELETE FROM products").contains("needs a WHERE"));
        assert!(
            rejected("WITH x AS (SELECT 1) UPDATE products SET price_cents = 0")
                .contains("needs a WHERE")
        );
        assert!(rejected(
            "WITH x AS (UPDATE products SET price_cents = 0 WHERE true RETURNING id) SELECT id FROM x"
        )
        .contains("always true"));
    }

    #[test]
    fn rejects_other_statements_and_garbage() {
        assert!(rejected("DROP TABLE products").contains("not allowed"));
        assert!(rejected("GRANT SELECT ON products TO public").contains("not allowed"));
        assert!(rejected("SELECT 1; SELECT 2").contains("single statement"));
        assert!(rejected("UPDATE products SET WHERE").contains("does not parse"));
    }
}

#[cfg(feature = "sdk")]
fn model_table_name(name: &sqlparser::ast::ObjectName) -> String {
    name.0
        .iter()
        .map(|id| id.value.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Check SQL names against normalized model locks. CRUD read permission is
/// represented by either `get` or `list`; writes use create/update/delete.
#[cfg(feature = "sdk")]
pub(crate) fn model_problems(
    sql: &str,
    models: &[crate::manifest::ModelSpec],
    step: &str,
) -> Vec<String> {
    use sqlparser::ast::{ObjectName, Visit, Visitor};
    use std::ops::ControlFlow;
    struct Names {
        tables: Vec<String>,
        columns: Vec<Vec<String>>,
        aliases: std::collections::HashMap<String, String>,
        derived: Vec<String>,
        outputs: Vec<String>,
        projection_columns: Vec<String>,
    }
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if let Some(with) = &query.with {
                self.derived.extend(
                    with.cte_tables
                        .iter()
                        .map(|cte| cte.alias.name.value.clone()),
                );
            }
            if let SetExpr::Select(select) = &*query.body {
                for from in &select.from {
                    for join in &from.joins {
                        use sqlparser::ast::{JoinConstraint, JoinOperator};
                        let constraint = match &join.join_operator {
                            JoinOperator::Inner(c)
                            | JoinOperator::LeftOuter(c)
                            | JoinOperator::RightOuter(c)
                            | JoinOperator::FullOuter(c)
                            | JoinOperator::LeftSemi(c)
                            | JoinOperator::RightSemi(c)
                            | JoinOperator::LeftAnti(c)
                            | JoinOperator::RightAnti(c) => Some(c),
                            _ => None,
                        };
                        if let Some(JoinConstraint::Using(columns)) = constraint {
                            self.columns
                                .extend(columns.iter().map(|id| vec![id.value.clone()]));
                        }
                    }
                }
                for item in &select.projection {
                    if let sqlparser::ast::SelectItem::QualifiedWildcard(table, _) = item {
                        self.columns.push(vec![model_table_name(table), "*".into()]);
                    }
                    if let sqlparser::ast::SelectItem::ExprWithAlias { alias, expr } = item {
                        self.outputs.push(alias.value.clone());
                        struct Columns(Vec<String>);
                        impl Visitor for Columns {
                            type Break = ();
                            fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
                                if let Expr::Identifier(id) = expr {
                                    self.0.push(id.value.clone());
                                }
                                ControlFlow::Continue(())
                            }
                        }
                        let mut columns = Columns(Vec::new());
                        let _ = expr.visit(&mut columns);
                        self.projection_columns.extend(columns.0);
                    }
                }
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(
            &mut self,
            factor: &sqlparser::ast::TableFactor,
        ) -> ControlFlow<()> {
            if let sqlparser::ast::TableFactor::Table {
                name,
                alias: Some(alias),
                ..
            } = factor
            {
                self.aliases
                    .insert(alias.name.value.clone(), model_table_name(name));
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<()> {
            self.tables.push(model_table_name(name));
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            match expr {
                Expr::Identifier(id) => self.columns.push(vec![id.value.clone()]),
                Expr::CompoundIdentifier(ids) => self
                    .columns
                    .push(ids.iter().map(|id| id.value.clone()).collect()),
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    let statement = match crate::sqlguard::parse_one(sql) {
        Ok(statement) => statement,
        Err(error) => return vec![error],
    };
    let mut names = Names {
        tables: Vec::new(),
        columns: Vec::new(),
        aliases: Default::default(),
        derived: Vec::new(),
        outputs: Vec::new(),
        projection_columns: Vec::new(),
    };
    let _ = statement.visit(&mut names);
    let actions: &[&str] = match &statement {
        Statement::Query(_) => &["get", "list"],
        Statement::Insert(insert) => {
            let target = model_table_name(&insert.table_name);
            names.columns.extend(
                insert
                    .columns
                    .iter()
                    .map(|id| vec![target.clone(), id.value.clone()]),
            );
            if let Some(sqlparser::ast::OnInsert::OnConflict(conflict)) = &insert.on {
                if let Some(sqlparser::ast::ConflictTarget::Columns(columns)) =
                    &conflict.conflict_target
                {
                    names.columns.extend(
                        columns
                            .iter()
                            .map(|id| vec![target.clone(), id.value.clone()]),
                    );
                }
                if let sqlparser::ast::OnConflictAction::DoUpdate(update) = &conflict.action {
                    for assignment in &update.assignments {
                        match &assignment.target {
                            sqlparser::ast::AssignmentTarget::ColumnName(name) => names
                                .columns
                                .push(name.0.iter().map(|id| id.value.clone()).collect()),
                            sqlparser::ast::AssignmentTarget::Tuple(columns) => names
                                .columns
                                .extend(columns.iter().map(|name| {
                                    name.0.iter().map(|id| id.value.clone()).collect()
                                })),
                        }
                    }
                }
            }
            &["create"]
        }
        Statement::Update { assignments, .. } => {
            for assignment in assignments {
                use sqlparser::ast::AssignmentTarget;
                match &assignment.target {
                    AssignmentTarget::ColumnName(name) => names
                        .columns
                        .push(name.0.iter().map(|id| id.value.clone()).collect()),
                    AssignmentTarget::Tuple(names_) => names.columns.extend(
                        names_
                            .iter()
                            .map(|name| name.0.iter().map(|id| id.value.clone()).collect()),
                    ),
                }
            }
            &["update"]
        }
        Statement::Delete(_) => &["delete"],
        _ => return vec![],
    };
    let write_target = match &statement {
        Statement::Insert(insert) => Some(model_table_name(&insert.table_name)),
        Statement::Update { table, .. } => match &table.relation {
            sqlparser::ast::TableFactor::Table { name, .. } => Some(model_table_name(name)),
            _ => None,
        },
        _ => None,
    };
    let mut known: Vec<_> = models.iter().map(|m| m.table.as_str()).collect();
    known.sort_unstable();
    let mut problems = Vec::new();
    for table in &names.tables {
        if names.derived.contains(table) {
            continue;
        }
        let table_actions = if write_target.as_ref().is_some_and(|target| target != table) {
            &["get", "list"][..]
        } else {
            actions
        };
        match models.iter().find(|m| &m.table == table) {
            None => problems.push(format!(
                "unknown table {table} in step {step}; known tables: {}",
                known.join(", ")
            )),
            Some(model)
                if !table_actions
                    .iter()
                    .any(|action| model.crud.iter().any(|a| a == action)) =>
            {
                problems.push(format!(
                    "action {} is not allowed by model {} in step {step}",
                    table_actions.join("/"),
                    model.id
                ))
            }
            _ => {}
        }
    }
    for column in &names.columns {
        let Some(name) = column.last() else { continue };
        let qualifier = column.iter().rev().nth(1).map(String::as_str);
        if qualifier.is_some_and(|q| names.derived.iter().any(|d| d == q)) {
            continue;
        }
        let table = qualifier.map(|q| names.aliases.get(q).map(String::as_str).unwrap_or(q));
        let candidates: Vec<_> = models
            .iter()
            .filter(|model| {
                names.tables.contains(&model.table)
                    && table.is_none_or(|t| t.eq_ignore_ascii_case("excluded") || t == model.table)
            })
            .collect();
        let known_column = name == "*"
            || candidates
                .iter()
                .any(|model| model.fields.iter().any(|f| &f.name == name));
        let output_alias = qualifier.is_none()
            && names.outputs.contains(name)
            && !names.projection_columns.contains(name);
        if name == "tenant_id"
            || (candidates.is_empty() && qualifier.is_some())
            || (!candidates.is_empty() && !known_column && !output_alias)
        {
            let table = table
                .unwrap_or_else(|| candidates.first().map(|m| m.table.as_str()).unwrap_or("?"));
            problems.push(format!("unknown column {table}.{name} in step {step}"));
        }
    }
    problems.sort();
    problems.dedup();
    problems
}

#[cfg(all(test, feature = "sdk"))]
mod model_tests {
    use super::model_problems;
    fn model() -> crate::manifest::ModelSpec {
        crate::manifest::model_from(&crate::lock::LockDoc {
            meta: crate::lock::Meta::pending(crate::lock::Kind::Model, None, "counter", "counter.md"),
            sections: crate::lock::parse_sections("## Table\n- table: counters\n## Fields\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| id | uuid | primary | yes |\n| value | integer | required | no |\n## CRUD\n| method | options |\n| --- | --- |\n| get | - |\n| update | - |\n").unwrap(),
        })
    }
    #[test]
    fn model_names_columns_and_permissions() {
        let m = model();
        for (sql, expected) in [
            (
                "SELECT value FROM invented WHERE id = $1",
                "unknown table invented in step s1; known tables: counters",
            ),
            (
                "SELECT missing FROM counters WHERE id = $1",
                "counters.missing",
            ),
            (
                "SELECT tenant_id FROM counters WHERE id = $1",
                "counters.tenant_id",
            ),
            (
                "INSERT INTO counters (value) VALUES ($1)",
                "action create is not allowed by model counter",
            ),
            (
                "UPDATE counters SET missing = $1 WHERE id = $2",
                "counters.missing",
            ),
        ] {
            let errors = model_problems(sql, std::slice::from_ref(&m), "s1");
            assert!(
                errors.iter().any(|e| e.contains(expected)),
                "{sql}: {errors:?}"
            );
        }
        for sql in [
            "SELECT c.value FROM counters c WHERE c.id = $1",
            "UPDATE counters SET value = value + 1 WHERE id = $1 RETURNING value",
        ] {
            assert!(
                model_problems(sql, std::slice::from_ref(&m), "s1").is_empty(),
                "{sql}"
            );
        }
    }
}
