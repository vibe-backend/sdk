//! Migration statement allowlist (spec §2B.3 "Owner role 條件", D98, D129).
//!
//! Migrations run as the tenant owner role. Each statement is parsed with a
//! PostgreSQL parser (`sqlparser`, `PostgreSqlDialect`) and classified from
//! its parse tree; only the statement shapes listed in §2B.3 are accepted
//! and everything else is refused, including SQL that does not parse and
//! text holding more than one statement.
//!
//! Accepted (see [`ALLOWED`]):
//! - `CREATE TABLE` with plain columns and table constraints;
//! - `ALTER TABLE ... ADD COLUMN`, `ADD <constraint>`, and
//!   `ALTER COLUMN ... SET NOT NULL | DROP NOT NULL | SET DEFAULT | DROP DEFAULT`;
//! - `CREATE [UNIQUE] INDEX` (named; placement decided from the parsed
//!   `unique` flag);
//! - `DROP INDEX` (no CASCADE);
//! - a backfill `UPDATE` with a WHERE clause on one table.
//!
//! Names must be unqualified; expressions may only call allowlisted
//! functions and cast to allowlisted types ([`crate::sqlguard`]). The SQL
//! that runs is re-printed from the checked parse tree.
//!
//! UNIQUE indexes are created inside the main migration transaction; only
//! non-unique (purely performance) indexes are built `CONCURRENTLY` after
//! the main transaction commits.

use sqlparser::ast::{
    helpers::stmt_create_table::CreateTableBuilder, AlterColumnOperation, AlterTableOperation,
    ColumnDef, ColumnOption, CreateIndex, HiveFormat, Ident, ObjectName, ObjectType, Statement,
    TableConstraint, TableFactor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use sqlparser::ast::{AssignmentTarget, Expr, FunctionArguments, UnaryOperator, Value};

use crate::sqlguard::{cast_type_problem, node_problem, reserved_name};

/// Where a migration statement runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Inside the main migration transaction (the SQL to run there).
    InTransaction(String),
    /// A non-unique index built `CONCURRENTLY` after the main transaction
    /// commits, as its own recorded step.
    AfterCommit(ConcurrentIndex),
}

/// A non-unique index built by the platform's concurrent index step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrentIndex {
    /// Index name (unquoted, as stored in `pg_class.relname`).
    pub name: String,
    /// `CREATE INDEX CONCURRENTLY ...` to run.
    pub sql: String,
}

/// Statement kinds the allowlist accepts.
pub const ALLOWED: &[&str] = &[
    "CREATE TABLE",
    "ALTER TABLE ... ADD COLUMN / ADD <constraint>",
    "ALTER TABLE ... ALTER COLUMN SET NOT NULL / DROP NOT NULL / SET DEFAULT / DROP DEFAULT",
    "CREATE [UNIQUE] INDEX <name>",
    "DROP INDEX",
    "UPDATE ... WHERE (backfill)",
];

/// Index access methods a migration may name in `USING`.
const INDEX_METHODS: &[&str] = &["btree", "hash", "gin", "gist", "brin"];

fn refuse(what: &str) -> String {
    format!(
        "{what} is not allowed in migrations (migration allowlist: {})",
        ALLOWED.join(", ")
    )
}

/// A plain lowercase identifier (`[a-z_][a-z0-9_]*`). Quoted identifiers
/// are accepted only when they spell such a name.
fn plain(ident: &Ident) -> Option<String> {
    let value = if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    };
    let mut chars = value.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && value.len() <= 63;
    ok.then_some(value)
}

/// An unqualified name of an object in the tenant schema, not a platform or
/// catalog object.
fn local_name(name: &ObjectName, what: &str) -> Result<String, String> {
    let [ident] = name.0.as_slice() else {
        return Err(refuse(&format!("a schema-qualified {what} name ({name})")));
    };
    let Some(value) = plain(ident) else {
        return Err(refuse(&format!(
            "{what} name {name} (names must be plain lowercase identifiers)"
        )));
    };
    if reserved_name(&value) || value == "only" {
        return Err(refuse(&format!("{what} name {name} (reserved prefix)")));
    }
    Ok(value)
}

fn expr_ok<V: sqlparser::ast::Visit>(node: &V) -> Result<(), String> {
    match node_problem(node) {
        Some(problem) => Err(refuse(&format!("this expression ({problem})"))),
        None => Ok(()),
    }
}

/// DEFAULT forms a migration may use: a literal (optionally negated or
/// cast to an allowlisted type), `now()`, `CURRENT_TIMESTAMP`, or
/// `gen_random_uuid()`.
fn default_ok(expr: &Expr) -> Result<(), String> {
    fn literal(expr: &Expr) -> bool {
        match expr {
            Expr::Value(
                Value::Number(..) | Value::SingleQuotedString(_) | Value::Boolean(_) | Value::Null,
            ) => true,
            Expr::UnaryOp {
                op: UnaryOperator::Minus | UnaryOperator::Plus,
                expr,
            } => matches!(**expr, Expr::Value(Value::Number(..))),
            Expr::Nested(inner) => literal(inner),
            Expr::Cast {
                expr, data_type, ..
            } => literal(expr) && cast_type_problem(data_type).is_none(),
            _ => false,
        }
    }
    let ok = literal(expr)
        || match expr {
            Expr::Function(func) => {
                let [ident] = func.name.0.as_slice() else {
                    return Err(refuse(&format!("DEFAULT {expr}")));
                };
                let name = (ident.quote_style.is_none()).then(|| ident.value.to_ascii_lowercase());
                let no_args = match &func.args {
                    FunctionArguments::None => true,
                    FunctionArguments::List(list) => {
                        list.args.is_empty()
                            && list.clauses.is_empty()
                            && list.duplicate_treatment.is_none()
                    }
                    FunctionArguments::Subquery(_) => false,
                };
                let plain_call = func.filter.is_none()
                    && func.over.is_none()
                    && func.within_group.is_empty()
                    && matches!(func.parameters, FunctionArguments::None)
                    && func.null_treatment.is_none();
                plain_call
                    && no_args
                    && match (name.as_deref(), &func.args) {
                        (Some("now" | "gen_random_uuid"), FunctionArguments::List(_)) => true,
                        (Some("current_timestamp"), FunctionArguments::None) => true,
                        _ => false,
                    }
            }
            _ => false,
        };
    if ok {
        Ok(())
    } else {
        Err(refuse(&format!(
            "DEFAULT {expr} (defaults may be literals, now(), CURRENT_TIMESTAMP or gen_random_uuid())"
        )))
    }
}

/// The SQL printed from `statement`, checked to parse back to the identical
/// tree; anything else is refused (fail closed).
fn printed(statement: &Statement) -> Result<String, String> {
    let sql = statement.to_string();
    match Parser::parse_sql(&PostgreSqlDialect {}, &sql).as_deref() {
        Ok([again]) if again == statement => Ok(sql),
        _ => Err(refuse(
            "a statement whose printed form does not parse back the same",
        )),
    }
}

fn column_ok(column: &ColumnDef) -> Result<(), String> {
    if plain(&column.name).is_none() {
        return Err(refuse(&format!(
            "column name {} (names must be plain lowercase identifiers)",
            column.name
        )));
    }
    if let Some(problem) = cast_type_problem(&column.data_type) {
        return Err(refuse(&format!(
            "column {} type ({})",
            column.name,
            problem.replace("cast to ", "")
        )));
    }
    if column.collation.is_some() {
        return Err(refuse("COLLATE on a column"));
    }
    for option in &column.options {
        match &option.option {
            ColumnOption::Null | ColumnOption::NotNull => {}
            ColumnOption::Default(expr) => default_ok(expr)?,
            ColumnOption::Check(expr) => expr_ok(expr)?,
            ColumnOption::Unique { .. } => {}
            ColumnOption::ForeignKey { foreign_table, .. } => {
                local_name(foreign_table, "referenced table")?;
            }
            other => return Err(refuse(&format!("column option `{other}`"))),
        }
    }
    Ok(())
}

fn constraint_ok(constraint: &TableConstraint) -> Result<(), String> {
    match constraint {
        TableConstraint::Unique {
            index_name: None,
            index_type: None,
            index_options,
            ..
        }
        | TableConstraint::PrimaryKey {
            index_name: None,
            index_type: None,
            index_options,
            ..
        } if index_options.is_empty() => Ok(()),
        TableConstraint::ForeignKey { foreign_table, .. } => {
            local_name(foreign_table, "referenced table").map(|_| ())
        }
        TableConstraint::Check { expr, .. } => expr_ok(expr),
        other => Err(refuse(&format!("table constraint `{other}`"))),
    }
}

fn index_ok(index: &CreateIndex) -> Result<(), String> {
    local_name(&index.table_name, "index table")?;
    if let Some(name) = &index.name {
        local_name(name, "index")?;
    }
    if let Some(method) = &index.using {
        let ok = plain(method).is_some_and(|m| INDEX_METHODS.contains(&m.as_str()));
        if !ok {
            return Err(refuse(&format!("index method {method}")));
        }
    }
    if !index.with.is_empty() {
        return Err(refuse("index storage parameters (WITH ...)"));
    }
    for column in &index.include {
        if plain(column).is_none() {
            return Err(refuse(&format!("INCLUDE column {column}")));
        }
    }
    expr_ok(&index.columns)?;
    if let Some(predicate) = &index.predicate {
        expr_ok(predicate)?;
    }
    Ok(())
}

/// Parses exactly one statement; anything else (a parse error, no
/// statement, more than one) is refused.
fn parse_one(sql: &str) -> Result<Statement, String> {
    let mut statements = Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|err| refuse(&format!("SQL that does not parse ({err})")))?;
    match statements.len() {
        1 => Ok(statements.remove(0)),
        0 => Err("empty statement".into()),
        _ => Err(refuse("more than one command in a statement")),
    }
}

/// Places a parsed, checked index: UNIQUE inside the transaction without
/// `CONCURRENTLY`; otherwise `CONCURRENTLY` after commit (named).
fn place_index(mut index: CreateIndex) -> Result<Placement, String> {
    if index.unique {
        index.concurrently = false;
        return Ok(Placement::InTransaction(printed(&Statement::CreateIndex(
            index,
        ))?));
    }
    let Some(name) = index.name.as_ref().and_then(|n| n.0.last()).and_then(plain) else {
        return Err(refuse(
            "a non-unique CREATE INDEX without a name (the concurrent index step records each index by name)",
        ));
    };
    index.concurrently = true;
    Ok(Placement::AfterCommit(ConcurrentIndex {
        name,
        sql: printed(&Statement::CreateIndex(index))?,
    }))
}

/// Checks one migration statement against the allowlist and decides where
/// it runs. The returned SQL is printed from the checked parse tree.
pub fn classify(sql: &str) -> Result<Placement, String> {
    let statement = parse_one(sql)?;
    // Every relation the statement names, anywhere (subqueries included).
    let mut relation_problem = None;
    let _ =
        sqlparser::ast::visit_relations(&statement, |name| match local_name(name, "relation") {
            Ok(_) => std::ops::ControlFlow::Continue(()),
            Err(err) => {
                relation_problem = Some(err);
                std::ops::ControlFlow::Break(())
            }
        });
    if let Some(err) = relation_problem {
        return Err(err);
    }
    match statement {
        Statement::CreateTable(table) => {
            local_name(&table.name, "table")?;
            for column in &table.columns {
                column_ok(column)?;
            }
            for constraint in &table.constraints {
                constraint_ok(constraint)?;
            }
            // Any clause beyond name, columns, constraints and IF NOT
            // EXISTS (TEMP, UNLOGGED, AS SELECT, LIKE, WITH, PARTITION BY,
            // ON COMMIT, ...) makes the tree differ from the plain form.
            let plain_form = CreateTableBuilder::new(table.name.clone())
                .if_not_exists(table.if_not_exists)
                .columns(table.columns.clone())
                .constraints(table.constraints.clone())
                .hive_formats(
                    table
                        .hive_formats
                        .as_ref()
                        .filter(|h| **h == HiveFormat::default())
                        .map(|_| HiveFormat::default()),
                )
                .build();
            let statement = Statement::CreateTable(table);
            if statement != plain_form {
                return Err(refuse(
                    "a CREATE TABLE clause other than columns and constraints",
                ));
            }
            Ok(Placement::InTransaction(printed(&statement)?))
        }
        Statement::AlterTable {
            ref name,
            only,
            ref operations,
            ref location,
            ref on_cluster,
            ..
        } => {
            local_name(name, "table")?;
            if only || location.is_some() || on_cluster.is_some() || operations.is_empty() {
                return Err(refuse("this ALTER TABLE form"));
            }
            for op in operations {
                match op {
                    AlterTableOperation::AddColumn {
                        column_def,
                        column_position: None,
                        ..
                    } => column_ok(column_def)?,
                    AlterTableOperation::AddConstraint(constraint) => constraint_ok(constraint)?,
                    AlterTableOperation::AlterColumn { column_name, op } => {
                        if plain(column_name).is_none() {
                            return Err(refuse(&format!("column name {column_name}")));
                        }
                        match op {
                            AlterColumnOperation::DropNotNull
                            | AlterColumnOperation::SetDefault { .. }
                                if plain(column_name).as_deref() == Some("tenant_id") =>
                            {
                                return Err(refuse(&format!("ALTER COLUMN tenant_id {op}")));
                            }
                            AlterColumnOperation::SetNotNull
                            | AlterColumnOperation::DropNotNull
                            | AlterColumnOperation::DropDefault => {}
                            AlterColumnOperation::SetDefault { value } => default_ok(value)?,
                            other => {
                                return Err(refuse(&format!("ALTER COLUMN ... {other}")));
                            }
                        }
                    }
                    other => return Err(refuse(&format!("ALTER TABLE ... {other}"))),
                }
            }
            Ok(Placement::InTransaction(printed(&statement)?))
        }
        Statement::CreateIndex(index) => {
            index_ok(&index)?;
            place_index(index)
        }
        Statement::Drop {
            object_type: ObjectType::Index,
            ref names,
            cascade: false,
            purge: false,
            temporary: false,
            ..
        } => {
            for name in names {
                local_name(name, "index")?;
            }
            Ok(Placement::InTransaction(printed(&statement)?))
        }
        Statement::Drop {
            object_type: ObjectType::Index,
            ..
        } => Err(refuse("DROP INDEX ... CASCADE")),
        Statement::Update {
            ref table,
            ref assignments,
            ref from,
            ref selection,
            ref returning,
        } => {
            let TableFactor::Table {
                name,
                alias: None,
                args: None,
                with_hints,
                version: None,
                with_ordinality: false,
                partitions,
            } = &table.relation
            else {
                return Err(refuse(
                    "UPDATE of anything but one plain table without an alias",
                ));
            };
            local_name(name, "table")?;
            if !with_hints.is_empty() || !partitions.is_empty() {
                return Err(refuse("UPDATE of anything but one plain table"));
            }
            if !table.joins.is_empty() || from.is_some() {
                return Err(refuse("UPDATE ... FROM or joins"));
            }
            if returning.is_some() {
                return Err(refuse("UPDATE ... RETURNING"));
            }
            if selection.is_none() {
                return Err(refuse("UPDATE without WHERE"));
            }
            for assignment in assignments {
                // One unqualified column per assignment, compared after
                // identifier folding.
                let AssignmentTarget::ColumnName(target) = &assignment.target else {
                    return Err(refuse(&format!(
                        "UPDATE assignment target {} (one unqualified column per assignment)",
                        assignment.target
                    )));
                };
                let [column] = target.0.as_slice() else {
                    return Err(refuse(&format!(
                        "UPDATE assignment target {target} (one unqualified column per assignment)"
                    )));
                };
                match plain(column) {
                    None => {
                        return Err(refuse(&format!(
                            "UPDATE assignment target {target} (names must be plain lowercase identifiers)"
                        )))
                    }
                    Some(name) if name == "tenant_id" => {
                        return Err(refuse("UPDATE of tenant_id"))
                    }
                    Some(_) => {}
                }
            }
            expr_ok(&statement)?;
            Ok(Placement::InTransaction(printed(&statement)?))
        }
        other => Err(refuse(&statement_kind(&other))),
    }
}

/// A short name for a refused statement (its leading keywords only).
fn statement_kind(statement: &Statement) -> String {
    let text = statement.to_string();
    let mut words: Vec<String> = text
        .split_whitespace()
        .take(3)
        .map(|w| {
            w.trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_uppercase()
        })
        .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphabetic()))
        .collect();
    if words.first().map(String::as_str) == Some("CREATE") && words.len() > 2 && words[1] != "OR" {
        words.truncate(2);
    }
    match statement {
        Statement::Delete(_) => "DELETE".into(),
        Statement::Truncate { .. } => "TRUNCATE".into(),
        Statement::Grant { .. } => "GRANT".into(),
        Statement::Revoke { .. } => "REVOKE".into(),
        Statement::SetVariable { .. }
        | Statement::SetRole { .. }
        | Statement::SetTimeZone { .. }
        | Statement::SetNames { .. }
        | Statement::SetNamesDefault { .. }
        | Statement::SetTransaction { .. } => format!("a GUC change ({})", words.join(" ")),
        Statement::Drop { .. } => {
            format!("{} (only through the deprecation flow)", words.join(" "))
        }
        Statement::CreateFunction { .. } => "CREATE FUNCTION".into(),
        Statement::CreateProcedure { .. } => "CREATE PROCEDURE".into(),
        Statement::CreateTrigger { .. } => "CREATE TRIGGER".into(),
        _ => format!("`{}`", words.join(" ")),
    }
}

/// `pg_get_indexdef` output rebuilt for the next migrate, placed from the
/// parsed index: UNIQUE inside the main transaction, otherwise
/// `CONCURRENTLY` after commit. The definition comes from the catalog of
/// the tenant's own schema, so it may be schema-qualified.
pub fn rebuild_placement(indexdef: &str) -> Result<Placement, String> {
    match Parser::parse_sql(&PostgreSqlDialect {}, indexdef)
        .map_err(|err| format!("index definition does not parse: {err}"))?
        .as_slice()
    {
        [Statement::CreateIndex(index)] => {
            let mut index = index.clone();
            expr_ok(&index.columns)?;
            if let Some(predicate) = &index.predicate {
                expr_ok(predicate)?;
            }
            if index.name.as_ref().is_none_or(|n| n.0.len() != 1) {
                return Err("index definition has no plain name".into());
            }
            if index.unique {
                index.concurrently = false;
                Ok(Placement::InTransaction(
                    Statement::CreateIndex(index).to_string(),
                ))
            } else {
                let name = index
                    .name
                    .as_ref()
                    .and_then(|n| n.0.last())
                    .map(|i| i.value.clone())
                    .ok_or("index definition has no name")?;
                index.concurrently = true;
                Ok(Placement::AfterCommit(ConcurrentIndex {
                    name,
                    sql: Statement::CreateIndex(index).to_string(),
                }))
            }
        }
        _ => Err("not a CREATE INDEX definition".into()),
    }
}

/// `(unique, concurrently)` of a CREATE INDEX, from its parse tree; `None`
/// when `sql` is not a single CREATE INDEX.
pub fn index_kind(sql: &str) -> Option<(bool, bool)> {
    match Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .ok()?
        .as_slice()
    {
        [Statement::CreateIndex(index)] => Some((index.unique, index.concurrently)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_tx(sql: &str) -> String {
        match classify(sql) {
            Ok(Placement::InTransaction(s)) => s,
            other => panic!("{sql}: {other:?}"),
        }
    }

    #[test]
    fn listed_statements_are_accepted() {
        let mut bad = Vec::new();
        for sql in [
            "CREATE TABLE audit (tenant_id text, note text)",
            "create table if not exists t (tenant_id text)",
            "CREATE TABLE email_outbox (id uuid primary key, user_id uuid null, email text not null check (char_length(email) <= 254), recipient_hash bytea not null, template text not null, token bytea null, sent_at timestamptz null)",
            "CREATE TABLE o (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), at timestamptz NOT NULL DEFAULT now(), n numeric(10,2), u uuid REFERENCES users (id), UNIQUE (tenant_id, id), CHECK (n >= 0))",
            "ALTER TABLE products ADD COLUMN ends_at timestamptz",
            "alter table users add column phone text null check (char_length(phone) <= 20)",
            "ALTER TABLE products ALTER COLUMN name SET NOT NULL",
            "alter table users alter column password drop not null",
            "ALTER TABLE products ALTER COLUMN name SET DEFAULT ''",
            "ALTER TABLE products ALTER COLUMN name DROP DEFAULT",
            "ALTER TABLE products ADD CONSTRAINT c CHECK (price >= 0)",
            "ALTER TABLE products ADD CONSTRAINT u UNIQUE (tenant_id, sku)",
            "create unique index users_email_key on users (tenant_id, email)",
            "DROP INDEX audit_note_p",
            "DROP INDEX IF EXISTS audit_note_p",
            "UPDATE t SET note = 'x' WHERE note IS NULL",
            "-- note\nCREATE TABLE c (tenant_id text);",
            "/* lead */ CREATE   TABLE\n\tc2 (tenant_id text) -- trail",
            "CrEaTe TaBlE \"mixed\" (tenant_id TEXT)",
        ] {
            if !matches!(classify(sql), Ok(Placement::InTransaction(_))) {
                bad.push(format!("{sql}: {:?}", classify(sql)));
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn each_refused_kind_is_rejected() {
        let mut bad = Vec::new();
        for (sql, what) in [
            ("DELETE FROM t WHERE true", "DELETE"),
            ("TRUNCATE t", "TRUNCATE"),
            ("GRANT SELECT ON t TO PUBLIC", "GRANT"),
            ("REVOKE SELECT ON t FROM PUBLIC", "REVOKE"),
            ("ALTER TABLE t OWNER TO postgres", "not allowed"),
            ("ALTER TABLE t DISABLE ROW LEVEL SECURITY", "ROW LEVEL SECURITY"),
            ("ALTER TABLE t NO FORCE ROW LEVEL SECURITY", "not allowed"),
            ("ALTER TABLE t ENABLE ROW LEVEL SECURITY", "ROW LEVEL SECURITY"),
            ("DROP POLICY tenant_isolation ON t", "not allowed"),
            ("CREATE POLICY open ON t USING (true)", "not allowed"),
            ("ALTER POLICY tenant_isolation ON t USING (true)", "not allowed"),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$", "CREATE FUNCTION"),
            ("CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql SECURITY DEFINER AS 'SELECT 1'", "not allowed"),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE sql SET search_path = public AS 'SELECT 1'", "not allowed"),
            ("CREATE PROCEDURE p() LANGUAGE sql AS $$ SELECT 1 $$", "not allowed"),
            ("CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()", "not allowed"),
            ("CREATE TYPE mood AS ENUM ('a', 'b')", "not allowed"),
            ("CREATE SEQUENCE s", "not allowed"),
            ("CREATE VIEW v AS SELECT 1", "not allowed"),
            ("CREATE EXTENSION pgcrypto", "not allowed"),
            ("CREATE SCHEMA x", "not allowed"),
            ("CREATE ROLE r", "not allowed"),
            ("ALTER ROLE r SUPERUSER", "not allowed"),
            ("ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO PUBLIC", "not allowed"),
            ("COMMENT ON TABLE t IS 'x'", "not allowed"),
            ("INSERT INTO t VALUES ('a')", "not allowed"),
            ("SELECT pg_sleep(1)", "not allowed"),
            ("CALL p()", "not allowed"),
            ("DO $$ BEGIN END $$", "not allowed"),
            ("SET search_path TO x", "GUC"),
            ("SET LOCAL ROLE postgres", "not allowed"),
            ("RESET search_path", "not allowed"),
            ("BEGIN", "not allowed"),
            ("COMMIT", "not allowed"),
            ("DROP TABLE t", "deprecation flow"),
            ("DROP SCHEMA t_x CASCADE", "not allowed"),
            ("DROP INDEX i CASCADE", "CASCADE"),
            ("ALTER TABLE t DROP COLUMN x", "not allowed"),
            ("ALTER TABLE t RENAME TO u", "not allowed"),
            ("ALTER TABLE t SET (fillfactor = 50)", "not allowed"),
            ("ALTER TABLE t ALTER COLUMN x SET DATA TYPE bigint", "not allowed"),
            ("ALTER TABLE t ALTER COLUMN x TYPE text USING (x::text)", "not allowed"),
            ("UPDATE t SET note = 'x'", "WHERE"),
            ("UPDATE t SET note = set_config('app.tenant_id', 'x', true) WHERE true", "set_config"),
            ("UPDATE t SET tenant_id = 'other' WHERE true", "tenant_id"),
            ("UPDATE t SET note = 'x' WHERE true RETURNING note", "RETURNING"),
            ("UPDATE t SET note = o.note FROM other o WHERE true", "FROM"),
            ("UPDATE t_other.t SET note = 'x' WHERE true", "schema-qualified"),
            ("CREATE TABLE t_other.x (a text)", "schema-qualified"),
            ("CREATE TABLE \"T_Other\" (a text)", "plain lowercase"),
            ("CREATE TABLE pg_x (a text)", "reserved"),
            ("CREATE TABLE vibe_challenge_replay (a text)", "reserved"),
            ("CREATE TEMP TABLE scratch (x int)", "CREATE TABLE clause"),
            ("CREATE UNLOGGED TABLE u (x int)", "not allowed"),
            ("CREATE TABLE c AS SELECT 1", "not allowed"),
            ("CREATE TABLE w (x int) WITH (fillfactor = 10)", "not allowed"),
            ("CREATE TABLE r (x regclass)", "type"),
            ("CREATE TABLE r (x other.mood)", "type"),
            ("CREATE TABLE d (x text DEFAULT current_setting('app.tenant_id'))", "current_setting"),
            ("CREATE TABLE d (x int DEFAULT pg_backend_pid())", "pg_backend_pid"),
            ("CREATE TABLE d (x int GENERATED ALWAYS AS IDENTITY)", "not allowed"),
            ("CREATE TABLE f (x uuid REFERENCES t_other.users (id))", "schema-qualified"),
            ("ALTER TABLE t ADD COLUMN y text DEFAULT set_config('a','b',false)", "set_config"),
            ("CREATE TABLE a (tenant_id text); COMMIT", "more than one command"),
            ("CREATE TABLE a (tenant_id text); CREATE TABLE b (tenant_id text)", "more than one command"),
            ("CREATE TABLE a (tenant_id text);; SELECT 1", "not allowed"),
            ("CREATE TABLE zq (tenant_id text, a$x$ int); COMMIT; BEGIN", "not allowed"),
            ("CREATE TABLE (", "does not parse"),
            ("UPDATE t SET note = 'x' WHERE id IN (SELECT id FROM t_other.t)", "schema-qualified"),
            ("UPDATE t SET note = 'x' WHERE id IN (SELECT id FROM vibe_challenge_replay)", "reserved"),
            ("UPDATE t SET note = 'x' WHERE id IN (SELECT oid FROM pg_class)", "reserved"),
        ] {
            match classify(sql) {
                Err(err) if err.contains(what) && err.contains("not allowed in migrations") => {}
                other => bad.push(format!("{sql}: {other:?}")),
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
        assert_eq!(classify("   ").unwrap_err(), "empty statement");
        assert_eq!(
            classify("-- only a comment").unwrap_err(),
            "empty statement"
        );
    }

    fn refused(sql: &str, what: &str) -> Option<String> {
        match classify(sql) {
            Err(err) if err.contains(what) && err.contains("not allowed in migrations") => None,
            other => Some(format!("{sql}: {other:?}")),
        }
    }

    #[test]
    fn update_targets_are_single_unqualified_columns() {
        let bad: Vec<_> = [
            (
                "UPDATE t SET t.tenant_id = 'x' WHERE true",
                "unqualified column",
            ),
            (
                "UPDATE t SET \"t\".\"tenant_id\" = 'x' WHERE true",
                "unqualified column",
            ),
            ("UPDATE t SET t.note = 'x' WHERE true", "unqualified column"),
            ("UPDATE t AS z SET z.tenant_id = 'x' WHERE true", "alias"),
            ("UPDATE t z SET note = 'x' WHERE true", "alias"),
            (
                "UPDATE t SET (tenant_id, note) = ('x', 'y') WHERE true",
                "unqualified column",
            ),
            (
                "UPDATE t SET (note) = ('y') WHERE true",
                "unqualified column",
            ),
            ("UPDATE t SET TENANT_ID = 'x' WHERE true", "tenant_id"),
            ("UPDATE t SET \"tenant_id\" = 'x' WHERE true", "tenant_id"),
            ("UPDATE t SET Tenant_Id = 'x' WHERE true", "tenant_id"),
            (
                "UPDATE t SET \"Tenant_Id\" = 'x' WHERE true",
                "plain lowercase",
            ),
            (
                "UPDATE t SET note = 'x', tenant_id = 'y' WHERE true",
                "tenant_id",
            ),
        ]
        .into_iter()
        .filter_map(|(sql, what)| refused(sql, what))
        .collect();
        assert!(bad.is_empty(), "{bad:#?}");
        in_tx("UPDATE t SET note = 'x', \"done\" = true WHERE note IS NULL");
    }

    #[test]
    fn update_only_and_other_table_forms_are_refused() {
        let bad: Vec<_> = [
            ("UPDATE ONLY t SET note = 'x' WHERE true", "not allowed"),
            ("UPDATE only t SET note = 'x' WHERE true", "not allowed"),
            ("UPDATE \"only\" SET note = 'x' WHERE true", "reserved"),
            ("UPDATE only SET note = 'x' WHERE true", "reserved"),
            ("UPDATE t AS x SET note = 'x' WHERE true", "alias"),
        ]
        .into_iter()
        .filter_map(|(sql, what)| refused(sql, what))
        .collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn accepted_sql_round_trips_to_the_same_tree() {
        for sql in [
            "UPDATE t SET note = 'x' WHERE note IS NULL",
            "CREATE TABLE o (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), at timestamptz DEFAULT now())",
            "ALTER TABLE products ALTER COLUMN name SET DEFAULT ''",
            "CREATE UNIQUE INDEX u ON t (a)",
            "DROP INDEX IF EXISTS p",
        ] {
            let out = in_tx(sql);
            let again = Parser::parse_sql(&PostgreSqlDialect {}, &out).unwrap();
            let first = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
            assert_eq!(again.len(), 1, "{out}");
            if !matches!(first[0], Statement::CreateIndex(_)) {
                assert_eq!(again, first, "{sql} -> {out}");
            }
            assert_eq!(in_tx(&out), out, "{out}");
        }
    }

    #[test]
    fn defaults_are_allowlisted_forms() {
        for sql in [
            "CREATE TABLE d (a text DEFAULT 'x', b int DEFAULT -1, c bool DEFAULT false, e text DEFAULT NULL)",
            "CREATE TABLE d (a timestamptz DEFAULT now(), b timestamptz DEFAULT CURRENT_TIMESTAMP, c uuid DEFAULT gen_random_uuid())",
            "CREATE TABLE d (a jsonb DEFAULT '{}'::jsonb, b numeric DEFAULT 1.5)",
            "ALTER TABLE t ALTER COLUMN a SET DEFAULT 0",
        ] {
            in_tx(sql);
        }
        let bad: Vec<_> = [
            "CREATE TABLE d (a text DEFAULT CURRENT_ROLE)",
            "CREATE TABLE d (a text DEFAULT current_user)",
            "CREATE TABLE d (a text DEFAULT SESSION_USER)",
            "CREATE TABLE d (a text DEFAULT USER)",
            "CREATE TABLE d (a text DEFAULT CURRENT_SCHEMA)",
            "CREATE TABLE d (a text DEFAULT current_schema())",
            "CREATE TABLE d (a text DEFAULT CURRENT_CATALOG)",
            "CREATE TABLE d (a text DEFAULT current_database())",
            "CREATE TABLE d (a text DEFAULT lower('X'))",
            "CREATE TABLE d (a text DEFAULT 'a' || 'b')",
            "CREATE TABLE d (a text DEFAULT (SELECT 1))",
            "CREATE TABLE d (a text DEFAULT 'x'::regclass)",
            "CREATE TABLE d (a timestamptz DEFAULT now(1))",
            "ALTER TABLE t ADD COLUMN a text DEFAULT current_role",
            "ALTER TABLE t ALTER COLUMN a SET DEFAULT session_user",
        ]
        .into_iter()
        .filter_map(|sql| refused(sql, "DEFAULT"))
        .collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn session_keywords_are_refused_in_expressions() {
        let bad: Vec<_> = [
            "CREATE TABLE c (a text CHECK (a <> current_role))",
            "ALTER TABLE t ADD CONSTRAINT c CHECK (a = current_user)",
            "UPDATE t SET note = current_user WHERE true",
            "UPDATE t SET note = 'x' WHERE note = session_user",
            "UPDATE t SET note = current_schema WHERE true",
            "UPDATE t SET note = 'x' WHERE note = current_catalog",
        ]
        .into_iter()
        .filter_map(|sql| refused(sql, "is not allowed in lock SQL"))
        .collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn tenant_id_keeps_not_null_and_no_default() {
        let bad: Vec<_> = [
            "ALTER TABLE t ALTER COLUMN tenant_id DROP NOT NULL",
            "ALTER TABLE t ALTER COLUMN \"tenant_id\" SET DEFAULT 'x'",
            "alter table t alter column TENANT_ID set default 'x'",
        ]
        .into_iter()
        .filter_map(|sql| refused(sql, "tenant_id"))
        .collect();
        assert!(bad.is_empty(), "{bad:#?}");
        in_tx("ALTER TABLE t ALTER COLUMN tenant_id SET NOT NULL");
    }

    #[test]
    fn platform_tables_are_refused_anywhere() {
        let bad: Vec<_> = [
            "UPDATE vibe_challenge_replay SET state = 'released' WHERE true",
            "UPDATE \"vibe_challenge_replay\" SET state = 'released' WHERE true",
            "UPDATE t SET note = 'x' WHERE EXISTS (SELECT 1 FROM vibe_challenge_replay)",
            "CREATE INDEX p ON vibe_challenge_replay (key)",
            "ALTER TABLE vibe_challenge_replay ADD COLUMN x text",
            "DROP INDEX vibe_challenge_replay_expires",
            "CREATE TABLE f (k bytea REFERENCES vibe_challenge_replay (key))",
            "UPDATE t SET note = 'x' WHERE id IN (SELECT id FROM _platform_x)",
            "UPDATE t SET note = 'x' WHERE id IN (SELECT oid FROM pg_class)",
        ]
        .into_iter()
        .filter_map(|sql| refused(sql, "not allowed"))
        .collect();
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn literals_are_not_read_as_keywords() {
        in_tx("UPDATE t SET note = 'DELETE FROM x; SECURITY DEFINER; OWNER TO y' WHERE true");
        in_tx("CREATE TABLE c (note text DEFAULT 'drop policy; commit')");
    }

    #[test]
    fn executed_sql_is_printed_from_the_parse_tree() {
        assert_eq!(
            in_tx("create table  /* c */ t (tenant_id text) -- x"),
            "CREATE TABLE t (tenant_id TEXT)"
        );
    }

    #[test]
    fn index_placement_comes_from_the_parsed_unique_flag() {
        assert_eq!(
            classify("CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS u ON t (a)"),
            Ok(Placement::InTransaction(
                "CREATE UNIQUE INDEX IF NOT EXISTS u ON t(a)".into()
            ))
        );
        assert_eq!(
            classify("/* index */ create\nunique   index u on t (a)"),
            Ok(Placement::InTransaction(
                "CREATE UNIQUE INDEX u ON t(a)".into()
            ))
        );
        assert_eq!(
            classify("CREATE INDEX CONCURRENTLY IF NOT EXISTS p ON products (name)"),
            Ok(Placement::AfterCommit(ConcurrentIndex {
                name: "p".into(),
                sql: "CREATE INDEX CONCURRENTLY IF NOT EXISTS p ON products(name)".into()
            }))
        );
        assert_eq!(
            classify("create index p on products using btree (lower(name)) where name is not null"),
            Ok(Placement::AfterCommit(ConcurrentIndex {
                name: "p".into(),
                sql: "CREATE INDEX CONCURRENTLY p ON products USING btree (lower(name)) WHERE name IS NOT NULL".into()
            }))
        );
        // A table or column named like a keyword does not change the class.
        assert!(matches!(
            classify("CREATE INDEX unique_idx ON \"unique\" (\"concurrently\")"),
            Ok(Placement::AfterCommit(ConcurrentIndex { ref name, .. })) if name == "unique_idx"
        ));
        for sql in [
            "CREATE INDEX CONCURRENTLY p ON t_other.notes (x)",
            "CREATE INDEX p ON \"t_other\".\"notes\" (x)",
            "CREATE INDEX ON notes (x)",
            "CREATE INDEX p ON notes USING custom_am (x)",
            "CREATE INDEX p ON notes (x) WITH (fillfactor = 10)",
            "CREATE INDEX p ON notes (current_setting('x'))",
            "CREATE INDEX t_other.p ON notes (x)",
        ] {
            assert!(classify(sql).is_err(), "{sql}");
        }
        assert_eq!(
            index_kind("create unique index u on t (a)"),
            Some((true, false))
        );
        assert_eq!(
            index_kind("CREATE INDEX CONCURRENTLY p ON t (a)"),
            Some((false, true))
        );
        assert_eq!(index_kind("CREATE TABLE t (a text)"), None);
    }

    #[test]
    fn invalid_index_rebuild_keeps_the_placement_rule() {
        assert_eq!(
            rebuild_placement("CREATE UNIQUE INDEX u ON t_a.t USING btree (a)"),
            Ok(Placement::InTransaction(
                "CREATE UNIQUE INDEX u ON t_a.t USING btree (a)".into()
            ))
        );
        assert_eq!(
            rebuild_placement("CREATE INDEX p ON t_a.t USING btree (a)"),
            Ok(Placement::AfterCommit(ConcurrentIndex {
                name: "p".into(),
                sql: "CREATE INDEX CONCURRENTLY p ON t_a.t USING btree (a)".into()
            }))
        );
        assert!(rebuild_placement("CREATE TABLE x (a int)").is_err());
        assert!(
            rebuild_placement("CREATE INDEX p ON t_a.t USING btree (current_setting('x'))")
                .is_err()
        );
        assert!(rebuild_placement(
            "CREATE INDEX p ON t_a.t USING btree (a) WHERE (a <> current_user)"
        )
        .is_err());
    }
}
