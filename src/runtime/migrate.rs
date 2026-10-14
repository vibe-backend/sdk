use crate::manifest::Manifest;

use super::exec::PgExec;
use super::slugs::valid_tenant;
use super::sql::{
    bootstrap_statements, quote, rls_statements, runtime_grant_statements,
    runtime_schema_statements, Col,
};

/// Bootstraps one tenant schema and applies migrations, connected as the
/// owner role. Grants the runtime role SELECT/INSERT/UPDATE only.
/// Returns progress lines.
pub async fn migrate_tenant(
    exec: &PgExec,
    manifest: &Manifest,
    tenant: &str,
    runtime_role: &str,
) -> Result<Vec<String>, String> {
    if !valid_tenant(tenant) {
        return Err("tenant id must match ^[a-z0-9]{8,32}$".into());
    }
    let mut log = Vec::new();
    let schema = format!("t_{tenant}");
    for model in &manifest.models {
        let fields: Vec<Col> = model
            .fields
            .iter()
            .map(|field| Col {
                name: field.name.clone(),
                ty: field.ty.clone(),
            })
            .collect();
        let uniques: Vec<String> = model
            .fields
            .iter()
            .filter(|field| field.unique)
            .map(|field| field.name.clone())
            .collect();
        exec.batch(&bootstrap_statements(
            &schema,
            &model.table,
            &fields,
            &uniques,
        )?)
        .await?;
        exec.batch(&runtime_schema_statements(&schema, runtime_role)?)
            .await?;
        exec.batch(&runtime_grant_statements(
            &schema,
            &model.table,
            runtime_role,
        )?)
        .await?;
        log.push(format!("bootstrapped {schema}.{}", model.table));
    }
    // Migration DDL names tables unqualified; resolve them in the tenant schema.
    exec.batch(&[format!("SET search_path TO {}", quote(&schema)?)])
        .await?;
    for migration in &manifest.migrations {
        exec.batch(&migration.statements).await?;
        log.push(format!("applied {}", migration.id));
    }
    exec.batch(&["RESET search_path".to_string()]).await?;
    log.extend(protect_schema(exec, &schema).await?);
    Ok(log)
}

/// After migrations: every table in the tenant schema gets ENABLE + FORCE RLS
/// and the tenant policy. A table without `tenant_id`, or any view,
/// materialized view or foreign table (which RLS cannot protect), fails.
async fn protect_schema(exec: &PgExec, schema: &str) -> Result<Vec<String>, String> {
    let unsafe_rels = exec
        .query_texts(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('v','m','f') ORDER BY 1",
            schema,
        )
        .await?;
    if !unsafe_rels.is_empty() {
        return Err(format!(
            "{schema}: views, materialized views and foreign tables bypass row level security and are not allowed ({})",
            unsafe_rels.join(", ")
        ));
    }
    let missing = exec
        .query_texts(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r','p') AND NOT EXISTS ( \
               SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid \
               AND a.attname = 'tenant_id' AND NOT a.attisdropped) ORDER BY 1",
            schema,
        )
        .await?;
    if !missing.is_empty() {
        return Err(format!(
            "{schema}: table(s) without a tenant_id column cannot get the tenant policy ({}); add tenant_id in the migration",
            missing.join(", ")
        ));
    }
    let tables = exec
        .query_texts(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r','p') ORDER BY 1",
            schema,
        )
        .await?;
    let mut log = Vec::new();
    for table in tables {
        exec.batch(&rls_statements(schema, &table)?).await?;
        log.push(format!("row level security on {schema}.{table}"));
    }
    Ok(log)
}
