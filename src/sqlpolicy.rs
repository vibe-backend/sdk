//! Deterministic SQL policy for lock statements (deploy gate).
//!
//! Every non-migration statement must parse (PostgreSQL dialect) as exactly
//! one allowlisted statement: a query (`SELECT`, `WITH ...`), `INSERT`, or
//! `UPDATE`. `UPDATE` and `DELETE`, including ones nested in CTEs, need a
//! `WHERE` that is not trivially true (`where true`, `1 = 1`, `x = x`,
//! `... or true`, constant comparisons). Anything that does not parse fails
//! closed.

use sqlparser::ast::{BinaryOperator, Expr, Query, SetExpr, Statement, UnaryOperator, Value};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

pub fn problems(sql: &str) -> Vec<String> {
    let statements = match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
        Ok(statements) => statements,
        Err(err) => return vec![format!("SQL does not parse under the allowlist parser: {err}")],
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
        Some(expr) if always_true(expr) => {
            out.push(format!("{kind} WHERE is always true (`{expr}`)"))
        }
        Some(_) => {}
    }
}

fn is_const(expr: &Expr) -> bool {
    match expr {
        Expr::Value(Value::Placeholder(_)) => false,
        Expr::Value(_) => true,
        Expr::Nested(inner) => is_const(inner),
        Expr::UnaryOp { expr, .. } => is_const(expr),
        Expr::Cast { expr, .. } => is_const(expr),
        _ => false,
    }
}

fn always_true(expr: &Expr) -> bool {
    use BinaryOperator::*;
    match expr {
        Expr::Value(Value::Boolean(true)) => true,
        Expr::Nested(inner) | Expr::IsTrue(inner) | Expr::IsNotFalse(inner) => always_true(inner),
        Expr::IsFalse(inner) | Expr::IsNotTrue(inner) => always_false(inner),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => always_false(expr),
        Expr::BinaryOp { left, op: Or, right } => always_true(left) || always_true(right),
        Expr::BinaryOp {
            left,
            op: And,
            right,
        } => always_true(left) && always_true(right),
        Expr::BinaryOp { left, op, right } => {
            // Fail closed: comparing constants, or a value with itself.
            (is_const(left) && is_const(right))
                || (left == right && matches!(op, Eq | GtEq | LtEq))
        }
        Expr::IsNotNull(inner) => is_const(inner),
        _ => false,
    }
}

fn always_false(expr: &Expr) -> bool {
    use BinaryOperator::*;
    match expr {
        Expr::Value(Value::Boolean(false)) | Expr::Value(Value::Null) => true,
        Expr::Nested(inner) => always_false(inner),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => always_true(expr),
        Expr::BinaryOp { left, op: And, right } => always_false(left) || always_false(right),
        Expr::BinaryOp { left, op: Or, right } => always_false(left) && always_false(right),
        Expr::BinaryOp { left, op, right } => left == right && matches!(op, NotEq | Gt | Lt),
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
        ] {
            assert!(rejected(sql).contains("always true"), "{sql}");
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
