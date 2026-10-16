//! Allowlist check for lock SQL run by `query` steps, using a real
//! PostgreSQL parser: one SELECT/INSERT/UPDATE/DELETE, unqualified plain
//! table names, no table functions, no data-modifying WITH and only
//! allowlisted functions (so no session/role/setting changes and no
//! dynamic SQL).

use std::ops::ControlFlow;

use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, DataType, Expr, Ident, ObjectName, Query, SelectItem,
    SetExpr, Statement, TableFactor, TimezoneInfo, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

/// Functions lock SQL may call. Anything else is rejected.
pub const ALLOWED_FUNCTIONS: &[&str] = &[
    "abs",
    "array_agg",
    "array_length",
    "avg",
    "btrim",
    "cardinality",
    "ceil",
    "char_length",
    "coalesce",
    "count",
    "date_trunc",
    "floor",
    "gen_random_uuid",
    "greatest",
    "json_agg",
    "json_build_object",
    "jsonb_agg",
    "jsonb_build_object",
    "least",
    "length",
    "lower",
    "max",
    "min",
    "now",
    "nullif",
    "round",
    "sum",
    "to_json",
    "to_jsonb",
    "trim",
    "upper",
];

fn plain_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn table_problem(name: &ObjectName) -> Option<String> {
    if name.0.len() != 1 {
        return Some(format!("table {name} must not be schema-qualified"));
    }
    let ident = &name.0[0];
    let value = ident.value.to_ascii_lowercase();
    if !(plain_name(&value) && (ident.quote_style.is_none() || plain_name(&ident.value))) {
        return Some(format!("table {name} must be a plain lowercase name"));
    }
    if reserved_name(&value) || value == "only" {
        return Some(format!("table {name} is not allowed"));
    }
    None
}

/// Names reserved for platform and catalog objects (`pg_*`, `_platform*`,
/// `vibe_*`, `information_schema`). Lock SQL and migrations both refuse
/// them. Callers pass the ASCII-lowercased name, so the check is
/// case-insensitive. PostgreSQL folds unquoted identifiers to lowercase but
/// keeps quoted ones exactly (`"VIBE_x"` is a different identifier from
/// `vibe_x`); table references are refused when either the folded or the
/// literal spelling starts with a reserved prefix, and quoted names that are
/// not plain lowercase are refused outright.
pub fn reserved_name(value: &str) -> bool {
    value.starts_with("pg_")
        || value.starts_with("_platform")
        || value.starts_with("vibe_")
        || value == "information_schema"
}

/// SQL special-value keywords and session/introspection functions. Some
/// parse as plain identifiers rather than function calls; both forms are
/// refused like a function outside [`ALLOWED_FUNCTIONS`].
pub const SESSION_KEYWORDS: &[&str] = &[
    "current_role",
    "current_user",
    "session_user",
    "user",
    "system_user",
    "current_schema",
    "current_schemas",
    "current_catalog",
    "current_database",
    "current_query",
    "inet_client_addr",
    "inet_client_port",
    "inet_server_addr",
    "inet_server_port",
    "pg_backend_pid",
    "version",
];

fn session_keyword(ident: &Ident) -> bool {
    ident.quote_style.is_none()
        && SESSION_KEYWORDS.contains(&ident.value.to_ascii_lowercase().as_str())
}

/// Problems in the set-expression tree that the visitor does not reach as
/// relations (for example `TABLE name` operands).
fn set_expr_problem(body: &SetExpr) -> Option<String> {
    match body {
        SetExpr::Table(_) => Some("TABLE expressions are not allowed".into()),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_problem(left).or_else(|| set_expr_problem(right))
        }
        _ => None,
    }
}

struct Guard(Option<String>);

impl Guard {
    fn fail(&mut self, problem: String) -> ControlFlow<()> {
        self.0 = Some(problem);
        ControlFlow::Break(())
    }
}

impl Visitor for Guard {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if let Some(problem) = set_expr_problem(&query.body) {
            return self.fail(problem);
        }
        if let SetExpr::Select(select) = &*query.body {
            if select.into.is_some() {
                return self.fail("SELECT ... INTO is not allowed".into());
            }
            for item in &select.projection {
                if let SelectItem::ExprWithAlias { expr, alias } = item {
                    // `typename 'literal'` — sqlparser uses a single-quoted alias.
                    if alias.quote_style == Some('\'') {
                        if let Some(problem) = typed_literal_alias_problem(expr) {
                            return self.fail(problem);
                        }
                    }
                }
            }
        }
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                if !matches!(
                    *cte.query.body,
                    SetExpr::Select(_)
                        | SetExpr::Values(_)
                        | SetExpr::SetOperation { .. }
                        | SetExpr::Query(_)
                ) {
                    return self.fail("data-modifying WITH queries are not allowed".into());
                }
            }
        }
        if matches!(*query.body, SetExpr::Insert(_) | SetExpr::Update(_)) && query.with.is_some() {
            return self.fail("data-modifying WITH queries are not allowed".into());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        match factor {
            TableFactor::Table { name, args, .. } => {
                if args.is_some() {
                    return self.fail(format!("table function {name} is not allowed"));
                }
                match table_problem(name) {
                    Some(problem) => self.fail(problem),
                    None => ControlFlow::Continue(()),
                }
            }
            TableFactor::Derived { .. } | TableFactor::NestedJoin { .. } => {
                ControlFlow::Continue(())
            }
            _ => self.fail("only plain tables and subqueries may appear in FROM".into()),
        }
    }

    fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<()> {
        match table_problem(name) {
            Some(problem) => self.fail(problem),
            None => ControlFlow::Continue(()),
        }
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::Cast {
                kind, data_type, ..
            } => {
                use sqlparser::ast::CastKind;
                // Only PostgreSQL CAST / :: forms; reject TRY_CAST / SAFE_CAST.
                if !matches!(kind, CastKind::Cast | CastKind::DoubleColon) {
                    return self.fail(format!("cast form {expr} is not allowed in lock SQL"));
                }
                if let Some(problem) = cast_type_problem(data_type) {
                    return self.fail(problem);
                }
            }
            Expr::TypedString { data_type, .. } => {
                if let Some(problem) = cast_type_problem(data_type) {
                    return self.fail(problem);
                }
            }
            Expr::Convert { .. } => {
                return self.fail("CONVERT is not allowed in lock SQL".into());
            }
            Expr::Collate { collation, .. } if collation.0.len() != 1 => {
                return self.fail(format!(
                    "collation {collation} must not be schema-qualified"
                ));
            }
            Expr::BinaryOp {
                op: BinaryOperator::PGCustomBinaryOperator(_),
                ..
            } => return self.fail("OPERATOR(...) is not allowed in lock SQL".into()),
            Expr::Identifier(ident) if session_keyword(ident) => {
                return self.fail(format!("function {ident} is not allowed in lock SQL"));
            }
            _ => {}
        }
        if let Expr::Function(func) = expr {
            let ok = func.name.0.len() == 1 && {
                let ident = &func.name.0[0];
                ident.quote_style.is_none()
                    && ALLOWED_FUNCTIONS.contains(&ident.value.to_ascii_lowercase().as_str())
            };
            if !ok {
                return self.fail(format!("function {} is not allowed in lock SQL", func.name));
            }
        }
        ControlFlow::Continue(())
    }
}

/// Cast and typed-literal target types lock SQL may use (builtin scalars and
/// common aliases only). Anything else is rejected.
pub const ALLOWED_CAST_TYPES: &[&str] = &[
    "text",
    "varchar",
    "character varying",
    "char",
    "character",
    "bpchar",
    "int2",
    "smallint",
    "int4",
    "int",
    "integer",
    "int8",
    "bigint",
    "numeric",
    "decimal",
    "float4",
    "real",
    "float8",
    "float",
    "double",
    "double precision",
    "bool",
    "boolean",
    "date",
    "time",
    "timetz",
    "timestamp",
    "timestamptz",
    "interval",
    "uuid",
    "json",
    "jsonb",
    "bytea",
];

/// Fold an identifier the way PostgreSQL does for type names: unquoted names
/// are lowercased; quoted names keep their exact spelling (so `"TEXT"` is not
/// `text`). Backslashes are left as-is — only `U&"..."` would decode them, and
/// sqlparser does not parse that form as an identifier.
fn type_ident_name(ident: &Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    }
}

/// Returns the allowlist key for a (possibly schema-qualified) type name, or
/// `None` when the schema is not exactly `pg_catalog` under PostgreSQL folding
/// rules.
fn normalize_type_name_parts(idents: &[Ident]) -> Option<String> {
    match idents {
        [base] => Some(type_ident_name(base)),
        [schema, base] if type_ident_name(schema) == "pg_catalog" => Some(type_ident_name(base)),
        _ => None,
    }
}

fn normalized_data_type_name(data_type: &DataType) -> Option<String> {
    let mut ty = data_type;
    while let DataType::Array(
        ArrayElemTypeDef::AngleBracket(inner)
        | ArrayElemTypeDef::SquareBracket(inner, _)
        | ArrayElemTypeDef::Parenthesis(inner),
    ) = ty
    {
        ty = inner;
    }
    match ty {
        DataType::Text | DataType::String(_) => Some("text".into()),
        DataType::Varchar(_) | DataType::CharacterVarying(_) | DataType::CharVarying(_) => {
            Some("varchar".into())
        }
        DataType::Char(_) | DataType::Character(_) => Some("char".into()),
        DataType::Int2(_) | DataType::SmallInt(_) => Some("int2".into()),
        DataType::Int4(_) | DataType::Int(_) | DataType::Integer(_) => Some("int4".into()),
        DataType::Int8(_) | DataType::BigInt(_) => Some("int8".into()),
        DataType::Numeric(_) | DataType::Decimal(_) | DataType::Dec(_) => Some("numeric".into()),
        DataType::Float4 | DataType::Real => Some("float4".into()),
        DataType::Float8 | DataType::Double | DataType::DoublePrecision | DataType::Float(_) => {
            Some("float8".into())
        }
        DataType::Bool | DataType::Boolean => Some("bool".into()),
        DataType::Date => Some("date".into()),
        DataType::Time(_, tz) => match tz {
            TimezoneInfo::WithTimeZone | TimezoneInfo::Tz => Some("timetz".into()),
            _ => Some("time".into()),
        },
        DataType::Timestamp(_, tz) => match tz {
            TimezoneInfo::WithTimeZone | TimezoneInfo::Tz => Some("timestamptz".into()),
            _ => Some("timestamp".into()),
        },
        DataType::Interval => Some("interval".into()),
        DataType::Uuid => Some("uuid".into()),
        DataType::JSON => Some("json".into()),
        DataType::JSONB => Some("jsonb".into()),
        DataType::Bytea => Some("bytea".into()),
        DataType::Custom(name, _) => normalize_type_name_parts(&name.0),
        // Explicitly not allowlisted (regclass and other builtins).
        _ => None,
    }
}

/// Rejects casts whose target is not in [`ALLOWED_CAST_TYPES`].
pub fn cast_type_problem(data_type: &DataType) -> Option<String> {
    match normalized_data_type_name(data_type) {
        Some(name) if ALLOWED_CAST_TYPES.contains(&name.as_str()) => None,
        _ => Some(format!("cast to {data_type} is not allowed in lock SQL")),
    }
}

/// Schema-qualified typed literals (`pg_catalog.regclass 'x'`) and some
/// unqualified ones (`oid 'x'`) are parsed as `ExprWithAlias` with a
/// single-quoted alias rather than `TypedString`. Treat them like casts.
fn typed_literal_alias_problem(expr: &Expr) -> Option<String> {
    let name = match expr {
        Expr::Identifier(ident) => normalize_type_name_parts(std::slice::from_ref(ident)),
        Expr::CompoundIdentifier(idents) => normalize_type_name_parts(idents),
        // e.g. U&"..." misparsed as BitwiseAnd — not an allowlisted type form
        _ => None,
    };
    match name {
        Some(n) if ALLOWED_CAST_TYPES.contains(&n.as_str()) => None,
        _ => Some(format!("cast to {expr} is not allowed in lock SQL")),
    }
}

/// What a `query` step's SQL does, from its parse tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlInfo {
    /// INSERT, UPDATE or DELETE.
    pub writes: bool,
    /// UPDATE or DELETE (needs a row scope).
    pub targets_rows: bool,
}

/// Runs the lock SQL expression checks (allowlisted functions and cast
/// types, plain unqualified relation names, no table functions, no
/// data-modifying WITH) over any parsed node. Used for migration
/// expressions too.
pub fn node_problem<V: Visit>(node: &V) -> Option<String> {
    let mut guard = Guard(None);
    let _ = node.visit(&mut guard);
    guard.0
}

/// Checks `sql` against the allowlist and describes it.
pub fn analyze(sql: &str) -> Result<SqlInfo, String> {
    if let Some(problem) = query_problem(sql) {
        return Err(problem);
    }
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).map_err(|e| e.to_string())?;
    let statement = &statements[0];
    Ok(SqlInfo {
        writes: !matches!(statement, Statement::Query(_)),
        targets_rows: matches!(statement, Statement::Update { .. } | Statement::Delete(_)),
    })
}

/// Whether a single statement yields rows (a query, or a write with
/// RETURNING), from its parse tree. Unparseable SQL yields no rows.
pub fn returns_rows(sql: &str) -> bool {
    let Ok(statements) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return false;
    };
    match statements.as_slice() {
        [Statement::Query(_)] => true,
        [Statement::Insert(insert)] => insert.returning.is_some(),
        [Statement::Update { returning, .. }] => returning.is_some(),
        [Statement::Delete(delete)] => delete.returning.is_some(),
        _ => false,
    }
}

/// Why `sql` may not run as a `query` step, or None when it may.
pub fn query_problem(sql: &str) -> Option<String> {
    let statements = match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
        Ok(s) => s,
        Err(err) => return Some(format!("SQL does not parse: {err}")),
    };
    let [statement] = statements.as_slice() else {
        return Some("exactly one SQL statement is allowed".into());
    };
    if !matches!(
        statement,
        Statement::Query(_)
            | Statement::Insert(_)
            | Statement::Update { .. }
            | Statement::Delete(_)
    ) {
        return Some("only SELECT, INSERT, UPDATE and DELETE are allowed".into());
    }
    let mut guard = Guard(None);
    let _ = statement.visit(&mut guard);
    guard.0
}

/// The text actually sent for a step whose rows are read: the step SQL in a
/// CTE so every column comes back as JSON.
pub fn wrap_for_rows(body: &str) -> String {
    format!("WITH q AS ({body}\n) SELECT row_to_json(q)::text FROM q")
}

/// Checks the final wrapped text from [`wrap_for_rows`]: it must be exactly
/// one CTE named `q` with the fixed outer SELECT, and the CTE body must pass
/// [`query_problem`].
pub fn wrapped_problem(wrapped: &str) -> Option<String> {
    let statements = match Parser::parse_sql(&PostgreSqlDialect {}, wrapped) {
        Ok(s) => s,
        Err(err) => return Some(format!("SQL does not parse: {err}")),
    };
    let [Statement::Query(query)] = statements.as_slice() else {
        return Some("wrapped SQL has an unexpected shape".into());
    };
    let expected_outer = Parser::parse_sql(
        &PostgreSqlDialect {},
        "WITH q AS (SELECT 1) SELECT row_to_json(q)::text FROM q",
    )
    .ok()
    .and_then(|mut s| match s.pop() {
        Some(Statement::Query(q)) => Some(q),
        _ => None,
    })?;
    let Some(with) = &query.with else {
        return Some("wrapped SQL has an unexpected shape".into());
    };
    let shape_ok = !with.recursive
        && with.cte_tables.len() == 1
        && with.cte_tables[0].alias.name.value == "q"
        && with.cte_tables[0].alias.columns.is_empty()
        && with.cte_tables[0].from.is_none()
        && query.body == expected_outer.body
        && query.order_by == expected_outer.order_by
        && query.limit == expected_outer.limit
        && query.offset == expected_outer.offset
        && query.fetch == expected_outer.fetch
        && query.locks.is_empty();
    if !shape_ok {
        return Some("wrapped SQL has an unexpected shape".into());
    }
    query_problem(&with.cte_tables[0].query.to_string())
}

#[cfg(test)]
mod tests {
    use super::{analyze, query_problem, wrap_for_rows, wrapped_problem, SqlInfo};

    #[test]
    fn returns_rows_reads_returning_from_the_parse_tree() {
        use super::returns_rows;
        assert!(returns_rows("select 1"));
        assert!(returns_rows("insert into t (a) values ($1) returning id"));
        assert!(!returns_rows("insert into t (a) values ('returning')"));
        assert!(returns_rows(
            "update t set a = 1 where id = $1 returning id"
        ));
        assert!(!returns_rows("delete from t where id = $1"));
    }

    #[test]
    fn analyze_reads_the_statement_kind() {
        let read = SqlInfo {
            writes: false,
            targets_rows: false,
        };
        assert_eq!(analyze("select 1 as x"), Ok(read));
        assert_eq!(
            analyze("insert into t (updated_at) values (now())"),
            Ok(SqlInfo {
                writes: true,
                targets_rows: false
            })
        );
        assert_eq!(
            analyze("update t set v = 1 where id = $1"),
            Ok(SqlInfo {
                writes: true,
                targets_rows: true
            })
        );
        assert_eq!(
            analyze("with x as (select 1 as a) select a from x"),
            Ok(read)
        );
    }

    #[test]
    fn ordinary_lock_sql_passes() {
        for sql in [
            "select id, email from users where email = $1",
            "insert into users (email, updated_at) values ($1, now()) returning id",
            "update users set name = $2 where id = $1::uuid",
            "delete from sessions where user_id = $1",
            "with x as (select id from users) select count(*) from x",
            "select u.id from users u join orders o on o.user_id = u.id where o.id = $1",
        ] {
            assert_eq!(query_problem(sql), None, "{sql}");
        }
    }

    #[test]
    fn disallowed_sql_is_rejected() {
        for sql in [
            "select forbidden_fn(1)",
            "select * from forbidden_fn(1)",
            "select id from users where forbidden_fn(1) is not null",
            "select pg_catalog.lower('x')",
            "set some_setting = 1",
            "reset some_setting",
            "select 1; select 2",
            "select id from other_schema.users",
            "select * from _platform_example",
            "with d as (update users set name = 'x' returning id) select * from d",
            "with d as (delete from users returning id) select * from d",
            "do $$ begin end $$",
            "copy users to stdout",
            "select * into tmp_copy from users",
            "select name from users where name = 'a' collate other_schema.\"C\"",
            "select 1 operator(other_schema.+) 1",
            "select * from vibe_challenge_replay",
            "select count(*) from \"vibe_challenge_replay\"",
            "insert into vibe_challenge_replay (tenant_id) values ('x')",
            "update vibe_challenge_replay set state = 'released' where true",
            "delete from vibe_challenge_replay where true",
            "select id from users where id in (select key from vibe_challenge_replay)",
            "with r as (select * from vibe_challenge_replay) select * from r",
            "select * from vibe_migrate_audit",
            "select * from pg_class",
            "select * from information_schema",
            "update only users set name = 'x' where id = $1",
        ] {
            assert!(query_problem(sql).is_some(), "accepted: {sql}");
        }
    }

    #[test]
    fn quoted_type_idents_keep_exact_spelling() {
        let text = sqlparser::ast::Ident::new("TEXT");
        assert_eq!(super::type_ident_name(&text), "text");
        let quoted = sqlparser::ast::Ident::with_quote('"', "TEXT");
        assert_eq!(super::type_ident_name(&quoted), "TEXT");
        assert_eq!(
            super::normalize_type_name_parts(&[sqlparser::ast::Ident::with_quote('"', "TEXT")]),
            Some("TEXT".into())
        );
        assert_eq!(
            super::normalize_type_name_parts(&[sqlparser::ast::Ident::with_quote(
                '"',
                "\\0074ext"
            )]),
            Some("\\0074ext".into())
        );
        assert_eq!(
            super::normalize_type_name_parts(&[
                sqlparser::ast::Ident::with_quote('"', "PG_CATALOG"),
                sqlparser::ast::Ident::with_quote('"', "text"),
            ]),
            None
        );
        assert_eq!(
            super::normalize_type_name_parts(&[
                sqlparser::ast::Ident::new("PG_CATALOG"),
                sqlparser::ast::Ident::new("text"),
            ]),
            Some("text".into())
        );
    }

    #[test]
    fn identifier_type_casts_are_rejected() {
        for sql in [
            "select 'x'::regclass",
            "select cast('x' as regproc)",
            "select regclass 'x'",
            "select 'x'::oid",
            "select 'x'::name",
            "select 'x'::\"regclass\"",
            "select 'x'::\"REGCLASS\"",
            "select 'x'::pg_catalog.regclass",
            "select 'x'::pg_catalog.\"regclass\"",
            "select 'x'::\"pg_catalog\".\"regtype\"",
            "select 'x'::\"oid\"",
            "select cast('x' as \"regproc\")",
            "select 'x'::regclass[]",
            "select 'x'::\"regclass\"[]",
            // Unicode-escaped identifiers and pg_catalog prefix
            "select 'x'::U&\"\\0072egclass\"",
            "select 'x'::U&\"pg_catalog\".U&\"\\0072egclass\"",
            // Internal array type names
            "select 'x'::_regclass",
            "select 'x'::\"_regclass\"",
            "select 'x'::_oid",
            "select 'x'::_regproc",
            // Schema-qualified typed literals
            "select pg_catalog.regclass 'x'",
            "select pg_catalog.\"regclass\" 'x'",
            // Unqualified names outside the allowlist
            "select 'x'::dom_rc",
            "select oid 'x'",
            "select 'x'::xid8",
            "select 'x'::xid",
            "select 'x'::cid",
            "select 'x'::tid",
            "select 'x'::public.text",
            // Quoted names keep exact spelling (not folded like unquoted)
            "select 'x'::\"TEXT\"",
            "select \"TEXT\" 'x'",
            "select 'x'::\"PG_CATALOG\".\"text\"",
            "select 'x'::\"pg_catalog\".\"TEXT\"",
            // Backslash is literal in quoted idents, not a unicode escape
            "select 'x'::\"\\0074ext\"",
            // Non-PostgreSQL cast forms
            "select convert('x', text)",
            "select try_cast('x' as text)",
            "select safe_cast('x' as text)",
        ] {
            assert!(query_problem(sql).is_some(), "accepted: {sql}");
        }
    }

    #[test]
    fn ordinary_casts_pass() {
        for sql in [
            "select $1::uuid",
            "select '1'::int",
            "select '1'::int2",
            "select '1'::int4",
            "select '1'::int8",
            "select '1'::smallint",
            "select '1'::bigint",
            "select '1'::integer",
            "select 'x'::text",
            "select 'x'::\"text\"",
            "select 'x'::varchar",
            "select 'x'::char",
            "select 'x'::bpchar",
            "select '1'::numeric",
            "select '1'::decimal",
            "select '1'::float4",
            "select '1'::float8",
            "select '1'::real",
            "select '1'::double precision",
            "select 'true'::bool",
            "select 'true'::boolean",
            "select '2020-01-01'::date",
            "select '12:00'::time",
            "select '2020-01-01'::timestamp",
            "select '2020-01-01'::timestamptz",
            "select '1 day'::interval",
            "select '{}'::json",
            "select '{}'::jsonb",
            "select '\\x'::bytea",
            "select 'x'::pg_catalog.text",
            "select 'x'::pg_catalog.\"text\"",
            "select text 'x'",
            "select pg_catalog.text 'x'",
            "select \"text\" 'hello'",
            "select uuid '00000000-0000-0000-0000-000000000001'",
            "select 'x'::text[]",
        ] {
            assert_eq!(query_problem(sql), None, "{sql}");
        }
    }

    #[test]
    fn session_keywords_are_refused_like_functions() {
        for sql in [
            "select current_role",
            "select CURRENT_USER",
            "select session_user",
            "select user",
            "select current_schema",
            "select current_catalog",
            "select current_schema()",
            "select current_database()",
            "select system_user",
            "select id from users where name = current_user",
            "update users set name = current_role where id = $1",
            "insert into users (name) values (session_user)",
        ] {
            let problem = query_problem(sql);
            assert!(
                problem
                    .as_deref()
                    .is_some_and(|p| p.contains("not allowed")),
                "accepted: {sql}: {problem:?}"
            );
        }
        // Quoted or qualified column names are ordinary columns.
        assert_eq!(query_problem("select u.\"user\" from users u"), None);
        assert_eq!(query_problem("select \"current_user\" from users"), None);
    }

    #[test]
    fn table_operands_are_checked() {
        for sql in [
            "select id from users union table _platform_refresh_tokens",
            "select id from users except table _platform_refresh_tokens",
            "select id from users union all table users",
            "insert into users (id) table _platform_refresh_tokens",
            "table _platform_refresh_tokens",
        ] {
            assert!(query_problem(sql).is_some(), "accepted: {sql}");
        }
    }

    #[test]
    fn wrapped_text_is_checked() {
        for sql in [
            "select id from users where email = $1",
            "insert into users (email) values ($1) returning id",
        ] {
            assert_eq!(wrapped_problem(&wrap_for_rows(sql)), None, "{sql}");
        }
        for sql in [
            "select 1 as x) , d as (delete from users returning id) select row_to_json(d)::text from d --",
            "select id from users) select 1 from q union select 1 from (select 1",
            "select * from _platform_refresh_tokens",
        ] {
            assert!(wrapped_problem(&wrap_for_rows(sql)).is_some(), "accepted: {sql}");
        }
    }

    /// Every spelling of a reserved `vibe_*` table reference is refused by
    /// static validation (`analyze`) and by the runtime checks
    /// (`query_problem`, `wrapped_problem`).
    #[test]
    fn reserved_name_rejected_in_all_forms() {
        for sql in [
            "select * from VIBE_challenge_replay",
            "select * from Vibe_challenge_replay",
            "select * from vIbE_ChAlLeNgE_rEpLaY",
            "select * from \"vibe_challenge_replay\"",
            "select * from \"VIBE_challenge_replay\"",
            "select * from \"Vibe_Challenge_Replay\"",
            "select * from t_x.vibe_challenge_replay",
            "select * from \"t_x\".\"vibe_challenge_replay\"",
            "select * from T_X.VIBE_CHALLENGE_REPLAY",
            "select * from public.vibe_challenge_replay",
            "with r as (select * from VIBE_challenge_replay) select * from r",
            "with vibe_x as (select 1 as a) select * from vibe_x",
            "select * from (select * from \"vibe_challenge_replay\") s",
            "select id from users where exists (select 1 from Vibe_challenge_replay)",
            "select u.id from users u join VIBE_challenge_replay r on r.key = u.id",
            "select u.id from users u left join t_x.vibe_challenge_replay r on true",
            "select id from users union select key from \"VIBE_challenge_replay\"",
            "insert into VIBE_challenge_replay (tenant_id) values ('x')",
            "insert into \"vibe_challenge_replay\" (tenant_id) values ('x')",
            "insert into t_x.vibe_challenge_replay (tenant_id) values ('x')",
            "insert into users (id) select key from Vibe_challenge_replay",
            "update VIBE_challenge_replay set state = 'released' where true",
            "update \"t_x\".\"vibe_challenge_replay\" set state = 'x' where true",
            "update users set name = 'x' from vibe_challenge_replay r where r.key = users.id",
            "delete from Vibe_challenge_replay where true",
            "delete from \"VIBE_challenge_replay\" where true",
            "delete from users using VIBE_challenge_replay r where r.key = users.id",
        ] {
            assert!(
                query_problem(sql).is_some(),
                "query_problem accepted: {sql}"
            );
            assert!(analyze(sql).is_err(), "analyze accepted: {sql}");
            assert!(
                wrapped_problem(&wrap_for_rows(sql)).is_some(),
                "wrapped_problem accepted: {sql}"
            );
        }
    }

    #[test]
    fn reserved_name_check_is_case_insensitive() {
        for name in ["vibe_x", "VIBE_x", "Vibe_X", "PG_class", "_PLATFORM_x"] {
            assert!(super::reserved_name(&name.to_ascii_lowercase()), "{name}");
        }
        assert!(!super::reserved_name("vibes"));
    }
}
