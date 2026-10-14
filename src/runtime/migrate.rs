use crate::manifest::Manifest;

use super::exec::PgExec;
use super::slugs::valid_tenant;
use super::sql::{
    bootstrap_statements, quote, runtime_grant_statements, runtime_schema_statements, Col,
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
    Ok(log)
}
