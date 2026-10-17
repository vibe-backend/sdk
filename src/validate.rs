use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use regex::Regex;
use std::sync::LazyLock;

use crate::diag::{Diag, Report};
use crate::discover::{self, SourceFile};
use crate::hashutil::source_hash;
use crate::lock::{
    compiler_hash, find_section, parse_lock, section_order, Kind, LockDoc, Scope, SourceType,
    LOCK_VERSION,
};
use crate::scan;
use crate::security::{self, pipeline_name};

static FORBIDDEN_SQL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(delete|truncate|grant|revoke)\b").expect("regex"));
static DDL_SQL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(create|alter|drop|comment|copy|vacuum)\b").expect("regex")
});
static PARAM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$([1-9][0-9]*)").expect("regex"));

/// Strict validation shared by source pins, locks and interactive generation.
pub(crate) fn check_limit_value(key: &str, value: &str) -> Result<(), String> {
    let example = match key {
        "timeout" => "10s",
        "body" => "64kb",
        "provider" => "main",
        _ => return Ok(()),
    };
    let valid = if key == "provider" {
        crate::discover::check_kebab(value).is_ok()
    } else if key == "body" {
        parse_size_bytes(value).is_some_and(|n| n > 0 && n <= 1_048_576)
    } else {
        let strict = ["ms", "s", "m", "h"].iter().any(|unit| {
            value
                .strip_suffix(unit)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
        strict && crate::llm::parse_duration_ms(value).is_some_and(|ms| ms > 0 && ms <= 30_000)
    };
    if valid {
        Ok(())
    } else {
        Err(format!(
            "{key}: {value:?} is invalid; use e.g. `{key}: {example}` ({})",
            if key == "timeout" {
                "timeout cannot exceed 30s; must be positive"
            } else if key == "body" {
                "body limit cannot exceed 1MB; must be positive"
            } else {
                "lowercase kebab-case name"
            }
        ))
    }
}

pub struct ValidateOptions<'a> {
    pub root: &'a Path,
    pub deploy: bool,
    pub dev: bool,
}

/// Sources plus the exact lock text read for each (None when missing).
pub struct LoadedProject {
    pub sources: Vec<SourceFile>,
    pub lock_texts: Vec<Option<String>>,
    /// `.vibe/build-record.json` as read with the locks (None when missing).
    pub build_record: Option<String>,
}

/// Discovers sources and reads every lock once.
pub fn load_project(root: &Path) -> Result<LoadedProject, Report> {
    let sources = discover::discover(root)?;
    let lock_texts = sources
        .iter()
        .map(|source| fs::read_to_string(root.join(lock_rel(&source.rel))).ok())
        .collect();
    let build_record = fs::read_to_string(root.join(crate::buildrecord::PATH)).ok();
    Ok(LoadedProject {
        sources,
        lock_texts,
        build_record,
    })
}

pub fn validate_project(opts: &ValidateOptions<'_>) -> Result<Report, Report> {
    let loaded = load_project(opts.root)?;
    validate_loaded(opts, &loaded).0
}

/// Validates already loaded lock texts (nothing is re-read from disk except
/// to look for orphan locks) and returns the parsed docs by source id, so a
/// caller (manifest) acts on exactly what was validated.
pub fn validate_loaded(
    opts: &ValidateOptions<'_>,
    loaded: &LoadedProject,
) -> (Result<Report, Report>, HashMap<String, LockDoc>) {
    let sources = &loaded.sources;
    let mut report = Report::new();
    let mut docs: HashMap<String, LockDoc> = HashMap::new();
    let mut source_by_id: HashMap<String, SourceFile> = HashMap::new();

    for (source, text) in sources.iter().zip(&loaded.lock_texts) {
        source_by_id.insert(source.id.clone(), source.clone());
        if source.kind == Kind::Api && source.scope == Some(Scope::Feature) {
            match crate::compile::parse_source_frontmatter(&source.text) {
                Ok(front) => {
                    for (key, value) in front {
                        if let Err(error) = check_limit_value(&key, &value) {
                            report.push(Diag::error(&source.rel, error));
                        }
                    }
                }
                Err(error) => report.push(Diag::error(&source.rel, error)),
            }
        }

        for hit in scan::scan_text(&source.combined_text()) {
            if hit.fatal {
                report.push(Diag::error(&source.rel, hit.message));
            } else {
                report.push(Diag::warning(&source.rel, hit.message));
            }
        }
        let Some(text) = text else {
            report.push(Diag::error(&source.rel, "source has no lock file"));
            continue;
        };
        let doc = match parse_lock(text) {
            Ok(doc) => doc,
            Err(err) => {
                report.push(Diag::error(lock_rel(&source.rel), err));
                continue;
            }
        };
        check_lock(&doc, source, opts, &mut report);
        docs.insert(source.id.clone(), doc);
    }
    check_dependencies(&docs, sources, &mut report);
    check_inherited_auth(&docs, sources, &mut report);
    if opts.deploy {
        crate::buildrecord::check(loaded, &mut report);
    }
    check_orphans(opts.root, sources, &mut report);
    check_routes(&docs, &mut report);
    check_calls(&docs, &mut report);
    check_refresh_sign_methods(&docs, &mut report);
    check_targets(&docs, &mut report);
    check_migrations(&docs, &mut report);
    check_feature_method_writes(&docs, &mut report);
    check_method_inherited_writes(&docs, &mut report);
    let env_roles_claim = std::env::var("VIBE_JWT_ROLES_CLAIM").ok();
    check_claims_vs_roles_claims(&docs, env_roles_claim.as_deref(), &mut report);
    check_security_items_enriched(&docs, &source_by_id, opts, &mut report);
    if opts.deploy && !report.has_errors() {
        check_recompile(opts.root, sources, &docs, &mut report);
    }
    (report.into_result(), docs)
}

/// Spec 10.1 "hand edit" / "recompile mismatch": structured sources compile
/// deterministically, so recompile them in memory and require the lock body
/// to match. Natural-language locks are left to `vibe compile --check` in CI,
/// which has the LLM.
fn check_recompile(
    root: &Path,
    sources: &[SourceFile],
    docs: &HashMap<String, LockDoc>,
    report: &mut Report,
) {
    let natural: HashMap<String, LockDoc> = sources
        .iter()
        .filter(|source| !crate::lock::is_structured_source(&source.combined_text()))
        .filter_map(|source| Some((source.id.clone(), docs.get(&source.id)?.clone())))
        .collect();
    let fresh = match crate::compile::recompile_offline(root, sources, &natural) {
        Ok(fresh) => fresh,
        Err(err) => {
            for diag in err.errors() {
                report.push(diag.clone());
            }
            report.push(Diag::error("", "deploy recompile check failed"));
            return;
        }
    };
    for source in sources {
        if natural.contains_key(&source.id) {
            continue;
        }
        let (Some(lock), Some(fresh)) = (docs.get(&source.id), fresh.get(&source.id)) else {
            continue;
        };
        if crate::lock::content_hash(lock) != crate::lock::content_hash(fresh)
            || !crate::security::same_items(&lock.meta.security_items, &fresh.meta.security_items)
        {
            report.push(Diag::error(
                lock_rel(&source.rel),
                "lock does not match a recompile of its source (hand-edited?); run `vibe compile`",
            ));
        }
    }
}

pub(crate) fn check_lock(
    doc: &LockDoc,
    source: &SourceFile,
    opts: &ValidateOptions<'_>,
    report: &mut Report,
) {
    let file = lock_rel(&source.rel);
    if doc.meta.lock_version != LOCK_VERSION {
        report.push(Diag::error(&file, "unsupported lock-version"));
    }
    if doc.meta.kind != source.kind || doc.meta.scope != source.scope || doc.meta.id != source.id {
        report.push(Diag::error(
            &file,
            "kind, scope, or id does not match the path",
        ));
    }
    if doc.meta.source != source.rel {
        report.push(Diag::error(&file, "source path does not match the lock"));
    }
    for section in &doc.sections {
        for block in &section.blocks {
            for bullet in &block.bullets {
                if bullet.key == "source-lines" {
                    let expected = crate::source_lines::snap_source_lines(
                        &source.combined_text(),
                        Some(&bullet.value),
                    );
                    if bullet.value != expected {
                        report.push(Diag::error(&file, format!("{} / {}: source-lines `{}` must equal `{expected}`; run `vibe compile`", section.name, block.heading, bullet.value)));
                    }
                }
            }
        }
    }
    let hash = source_hash(&source.combined_text());
    if doc.meta.source_hash != hash {
        report.push(Diag::error(&file, "source-hash does not match the source"));
    }
    let structured = crate::lock::is_structured_source(&source.combined_text());
    if doc.meta.source_type == SourceType::Structured && !structured {
        report.push(Diag::error(
            &file,
            "source-type is structured but the source contains prose",
        ));
    }
    if doc.meta.compiler.name != crate::lock::COMPILER_NAME {
        report.push(Diag::error(&file, "compiler name is not md-logic"));
    }
    if compiler_hash(&doc.meta.compiler) != doc.meta.compiler_hash {
        report.push(Diag::error(
            &file,
            "compiler-hash does not match the compiler stamp",
        ));
    }
    if doc.meta.compiler.llm_provider == "none" && doc.meta.source_type == SourceType::Natural {
        report.push(Diag::error(
            &file,
            "natural locks must record an LLM provider",
        ));
    }
    if doc.meta.compiler.model_snapshot == "latest"
        || doc.meta.compiler.model_snapshot.ends_with(":latest")
        || doc.meta.compiler.model_snapshot.ends_with("-latest")
    {
        report.push(Diag::error(&file, "model snapshot is a floating alias"));
    }
    if opts.deploy && !opts.dev && doc.meta.compiler.endpoint_private {
        report.push(Diag::error(
            &file,
            "locks compiled against a private endpoint cannot be deployed",
        ));
    }
    let mut order = section_order(doc.meta.kind, doc.meta.scope).to_vec();
    if doc.meta.kind == Kind::Api
        && doc.meta.scope == Some(Scope::Feature)
        && find_section(&doc.sections, "Transform").is_none()
    {
        order.retain(|name| *name != "Transform");
    }
    if doc.meta.kind == Kind::Model && find_section(&doc.sections, "Constraints").is_none() {
        order.retain(|name| *name != "Constraints");
    }
    let names: Vec<&str> = doc
        .sections
        .iter()
        .map(|section| section.name.as_str())
        .collect();
    if names != order {
        report.push(Diag::error(
            &file,
            format!("sections must be exactly {}", order.join(", ")),
        ));
    }
    // Features are re-checked after all locks load (method write enrichment).
    if !(doc.meta.kind == Kind::Api && doc.meta.scope == Some(Scope::Feature)) {
        let mut expected = security::extract_items(doc);
        if scan::scan_text(&source.combined_text())
            .iter()
            .any(|hit| !hit.fatal)
        {
            expected.push(crate::lock::SecurityItem {
                item: "prompt-injection".into(),
                risk: crate::lock::Risk::Medium,
                reason: String::new(),
            });
        }
        if !security::same_items(&expected, &doc.meta.security_items) {
            report.push(Diag::error(
                &file,
                "security-items do not match the validator extraction",
            ));
        }
    }
    check_sections(doc, &file, report);
    check_variable_roots(doc, &file, report);
    if doc
        .meta
        .legacy_fields
        .iter()
        .any(|field| field == "translation")
    {
        report.push(Diag::warning(&file, "lock contains a `translation` field, which is no longer written (decision 165); run `vibe compile` to refresh it"));
    }
    if doc
        .meta
        .legacy_fields
        .iter()
        .any(|field| field != "translation")
    {
        let fields: Vec<String> = doc
            .meta
            .legacy_fields
            .iter()
            .filter(|field| field.as_str() != "translation")
            .map(|field| format!("`{field}`"))
            .collect();
        report.push(Diag::warning(
            &file,
            format!(
                "legacy approval fields are ignored: {}; recompile to drop them",
                fields.join(", ")
            ),
        ));
    }
}

/// Deterministic auth policy: a feature lock inherits the merged auth of all
/// its definition/version locks. Its roles must stay within the inherited
/// roles (a wider list fails, and so does an empty one unless the mode
/// changed), whatever its mode. A mode
/// change (a missing mode is `jwt`) must be recorded as a high-risk
/// `auth-mode-change` item, and dropping the auth pipeline without `-auth`
/// (itself a high-risk `auth-removed` item) fails closed.
fn check_inherited_auth(
    docs: &HashMap<String, LockDoc>,
    sources: &[SourceFile],
    report: &mut Report,
) {
    fn auth_blocks(doc: &LockDoc) -> Vec<&crate::lock::Block> {
        find_section(&doc.sections, "Pipelines")
            .map(|s| {
                s.blocks
                    .iter()
                    .filter(|b| {
                        let name = b.heading.split_whitespace().last().unwrap_or("");
                        name == "auth" || name == "-auth"
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    let roles_of = |block: &crate::lock::Block| -> Option<Vec<String>> {
        let raw = block_val(block, "roles");
        if raw.trim().is_empty() {
            return Some(Vec::new());
        }
        crate::lock::parse_string_list(&raw).ok()
    };
    for source in sources {
        let Some(doc) = docs.get(&source.id) else {
            continue;
        };
        if doc.meta.kind != Kind::Api || doc.meta.scope != Some(Scope::Feature) {
            continue;
        }
        let file = lock_rel(&source.rel);
        // Merge every parent auth block (definition and version): the union
        // of their roles is the inherited set.
        let mut inherited: Option<(Vec<String>, Vec<String>)> = None;
        for dep in &doc.meta.dependencies {
            let Some(parent) = docs.get(&dep.id) else {
                continue;
            };
            if parent.meta.kind != Kind::Api {
                continue;
            }
            for block in auth_blocks(parent) {
                if block.heading.ends_with("-auth") {
                    continue;
                }
                let Some(roles) = roles_of(block) else {
                    continue;
                };
                let entry = inherited.get_or_insert_with(|| (Vec::new(), Vec::new()));
                let mode = crate::security::auth_mode(&block_val(block, "mode"));
                if !entry.0.contains(&mode) {
                    entry.0.push(mode);
                }
                for role in roles {
                    if !entry.1.contains(&role) {
                        entry.1.push(role);
                    }
                }
            }
        }
        let Some((modes, roles)) = inherited else {
            continue;
        };
        let own = auth_blocks(doc);
        if own.iter().any(|b| b.heading.ends_with("-auth")) {
            continue; // explicit removal, reported as a high-risk item
        }
        let Some(block) = own.first() else {
            report.push(Diag::error(
                &file,
                "inherited auth pipeline is missing (use `-auth` to remove it explicitly)",
            ));
            continue;
        };
        // A mode change (for example jwt to api-key) is never a quiet
        // override: it must be recorded as a high-risk item, and the
        // inherited roles still apply to the feature.
        let own_mode = crate::security::auth_mode(&block_val(block, "mode"));
        let mode_changed = modes.iter().any(|mode| *mode != own_mode);
        if mode_changed
            && !doc
                .meta
                .security_items
                .iter()
                .any(|item| item.item == "auth-mode-change" && item.risk == crate::lock::Risk::High)
        {
            report.push(Diag::error(
                &file,
                format!(
                    "auth mode {own_mode} differs from the inherited mode [{}] and is not recorded as a high-risk item",
                    modes.join(", ")
                ),
            ));
        }
        match roles_of(block) {
            None => report.push(Diag::error(&file, "auth roles cannot be parsed")),
            // Under a recorded mode change (e.g. api-key with scopes) a
            // feature may omit roles; any roles it lists must still stay
            // within the inherited ones.
            Some(own_roles) if !roles.is_empty() && own_roles.is_empty() && !mode_changed => report
                .push(Diag::error(
                    &file,
                    format!(
                        "auth roles are missing; inherited roles are [{}]",
                        roles.join(", ")
                    ),
                )),
            Some(own_roles) if !roles.is_empty() => {
                if let Some(extra) = own_roles.iter().find(|r| !roles.contains(r)) {
                    report.push(Diag::error(
                        &file,
                        format!(
                            "auth role {extra} widens the inherited roles [{}]",
                            roles.join(", ")
                        ),
                    ));
                }
            }
            Some(_) => {}
        }
    }
}

/// Every `dependencies` hash must equal the dependency's current content
/// hash (spec 10.1 "stale").
fn check_dependencies(
    docs: &HashMap<String, LockDoc>,
    sources: &[SourceFile],
    report: &mut Report,
) {
    for source in sources {
        let Some(doc) = docs.get(&source.id) else {
            continue;
        };
        for dep in &doc.meta.dependencies {
            match docs.get(&dep.id) {
                None => report.push(Diag::error(
                    lock_rel(&source.rel),
                    format!("dependency {} has no lock", dep.id),
                )),
                Some(target) if crate::lock::content_hash(target) != dep.hash => {
                    report.push(Diag::error(
                        lock_rel(&source.rel),
                        format!(
                            "dependency {} hash does not match its lock (stale or edited); run `vibe compile`",
                            dep.id
                        ),
                    ))
                }
                Some(_) => {}
            }
        }
    }
}

fn check_sections(doc: &LockDoc, file: &str, report: &mut Report) {
    for section in &doc.sections {
        if matches!(section.name.as_str(), "Fields" | "Request") {
            if let Some(table) = &section.table {
                if let Some(index) = table.headers.iter().position(|h| h == "rules") {
                    for row in &table.rows {
                        for rule in crate::lock::split_top_level(
                            row.get(index).map(String::as_str).unwrap_or(""),
                        ) {
                            if let Some((key, _)) = rule.split_once(':') {
                                if let Some(replacement) = crate::zh::alias(key.trim()) {
                                    report.push(Diag::error(file, format!("rule alias {} is not allowed in locks; use {replacement}:", key.trim())).at_section(&section.name));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if doc.meta.kind == Kind::Model {
        if let Err(error) = crate::zh::constraints(doc) {
            report.push(Diag::error(file, error).at_section("Constraints"));
        }
    }

    match doc.meta.kind {
        Kind::Provider => check_provider(doc, file, report),
        Kind::Model => check_model(doc, file, report),
        Kind::Method => check_method(doc, file, report),
        Kind::Api if doc.meta.scope == Some(Scope::Feature) => check_feature(doc, file, report),
        Kind::Api => {}
        Kind::Pipeline => check_pipeline(doc, file, report),
        Kind::Migration => check_migration_sql(doc, file, report),
    }
}

fn check_provider(doc: &LockDoc, file: &str, report: &mut Report) {
    let Some(conn) = find_section(&doc.sections, "Connection") else {
        return;
    };
    if conn.get("type") != Some("postgres") {
        report.push(
            Diag::error(file, "only postgres providers are supported").at_section("Connection"),
        );
    }
    if let Some(secret) = conn.get("secret") {
        if !is_secret_ref(secret) || secret.to_ascii_lowercase().contains("tenant") {
            report.push(
                Diag::error(file, "connection secret must be an uppercase reference")
                    .at_section("Connection"),
            );
        }
    } else {
        report.push(
            Diag::error(file, "provider is missing a secret reference").at_section("Connection"),
        );
    }
    if let Some(isolation) = find_section(&doc.sections, "Isolation") {
        if !matches!(isolation.get("mode"), Some("schema") | Some("database")) {
            report.push(
                Diag::error(file, "isolation mode must be schema or database")
                    .at_section("Isolation"),
            );
        }
    }
    if let Some(access) = find_section(&doc.sections, "Access") {
        if access.get("shared") == Some("true") {
            match access.get("allowed-endpoints") {
                None | Some("(none)") | Some("[]") => {
                    report.push(
                        Diag::error(file, "shared providers need allowed-endpoints")
                            .at_section("Access"),
                    );
                }
                _ => {}
            }
        }
    }
}

fn check_model(doc: &LockDoc, file: &str, report: &mut Report) {
    let Some(fields) = find_section(&doc.sections, "Fields") else {
        return;
    };
    let Some(table) = &fields.table else {
        report.push(Diag::error(file, "model fields must be a table").at_section("Fields"));
        return;
    };
    let mut saw_tenant = false;
    for row in &table.rows {
        if row.len() < 2 {
            continue;
        }
        let name = &row[0];
        let ty = &row[1];
        if !known_type(ty) {
            report.push(Diag::error(file, format!("unknown field type {ty}")).at_section("Fields"));
        }
        if name == "tenant_id" {
            saw_tenant = true;
            if row.get(3).map(String::as_str) != Some("yes") {
                report.push(
                    Diag::error(file, "tenant_id must be a system field").at_section("Fields"),
                );
            }
        }
        if ty == "password" {
            if let Some(min) = rule_number(row.get(2).map(String::as_str).unwrap_or(""), "min") {
                if min < 12 {
                    report.push(
                        Diag::error(file, "password min length cannot be below 12")
                            .at_section("Fields"),
                    );
                }
            }
        }
        let rules = row.get(2).map(String::as_str).unwrap_or("");
        for key in ["min", "max"] {
            if let Some(problem) = bad_number_rule(rules, key) {
                report.push(
                    Diag::error(file, format!("field {name}: {problem}")).at_section("Fields"),
                );
            }
        }
        if let (Some(min), Some(max)) = (rule_number(rules, "min"), rule_number(rules, "max")) {
            if min > max {
                report.push(
                    Diag::error(
                        file,
                        format!("field {name}: min ({min}) is greater than max ({max})"),
                    )
                    .at_section("Fields"),
                );
            }
        }
    }
    if !saw_tenant {
        report.push(Diag::error(file, "model is missing tenant_id").at_section("Fields"));
    }
    match find_section(&doc.sections, "Table").and_then(|section| section.get("owner-key")) {
        Some(key) if !key.is_empty() && key != "(none)" => {
            let declared = table
                .rows
                .iter()
                .any(|row| row.first().map(String::as_str) == Some(key));
            let table_name = find_section(&doc.sections, "Table")
                .and_then(|section| section.get("table"))
                .unwrap_or("");
            if key == "id" {
                // Only the `users` table may scope rows by its own primary key
                // (spec §12.6a); elsewhere `id = ctx.user.id` is a mistake.
                if table_name != "users" {
                    report.push(
                        Diag::error(
                            file,
                            "owner-key: id is only allowed on the users table; use a column holding the owner's user id",
                        )
                        .at_section("Table"),
                    );
                }
            } else if !declared {
                report.push(
                    Diag::error(file, format!("owner-key {key} is not a field"))
                        .at_section("Table"),
                );
            }
        }
        _ => report.push(Diag::warning(
            file,
            "model has no owner-key; rows are shared inside the tenant",
        )),
    }
    if let Some(ownership) =
        find_section(&doc.sections, "Table").and_then(|section| section.get("ownership"))
    {
        if let Err(err) = crate::runtime::parse_ownership(ownership) {
            report.push(Diag::error(file, err).at_section("Table"));
        }
    }
    if let Some(crud) = find_section(&doc.sections, "CRUD") {
        if let Some(table) = &crud.table {
            for row in &table.rows {
                let method = row.first().map(String::as_str).unwrap_or("");
                let options = row.get(1).map(String::as_str).unwrap_or("");
                if method == "delete" && options.contains("soft: false") {
                    report.push(
                        Diag::error(file, "delete is always a soft delete").at_section("CRUD"),
                    );
                }
                if method == "update"
                    && options.contains("optimistic-lock")
                    && !options.contains("if-match")
                {
                    report.push(
                        Diag::error(file, "optimistic lock must use if-match").at_section("CRUD"),
                    );
                }
                if method == "list" {
                    if let Some(max) = rule_number(options, "max-page-size") {
                        if max > 500 {
                            report.push(
                                Diag::error(file, "max-page-size cannot exceed 500")
                                    .at_section("CRUD"),
                            );
                        }
                    }
                }
            }
        }
    }
    if let Some(policies) = find_section(&doc.sections, "Policies") {
        let policy = policies.get("policy").unwrap_or("");
        if !policy.contains("current_setting('app.tenant_id', false)") {
            report.push(
                Diag::error(file, "RLS policy must use current_setting(..., false)")
                    .at_section("Policies"),
            );
        }
        let grants = policies.get("grants").unwrap_or("").to_ascii_lowercase();
        if grants.contains("delete") || grants.contains("truncate") {
            report.push(
                Diag::error(file, "runtime grants cannot include DELETE or TRUNCATE")
                    .at_section("Policies"),
            );
        }
    }
}

fn check_method(doc: &LockDoc, file: &str, report: &mut Report) {
    if let Some(steps) = find_section(&doc.sections, "Steps") {
        for block in &steps.blocks {
            let sql = block_val(block, "sql");
            let sql_writes = crate::security::sql_writes(sql.trim_matches('`'));
            match crate::manifest::parse_writes(&block.bullets) {
                Err(err) => report.push(
                    Diag::error(file, err)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                ),
                // `op: method` steps may inherit writes from the callee; that
                // is checked once all locks are loaded (check_method_inherited_writes).
                Ok(Some(true)) if !sql_writes && block_val(block, "op") != "method" => report.push(
                    Diag::error(
                        file,
                        "step is marked `writes: true` but its SQL does not write",
                    )
                    .at_step(&block.heading)
                    .at_section("Steps"),
                ),
                Ok(marked) if sql_writes && marked != Some(true) => report.push(
                    Diag::error(file, "step SQL writes but is not marked `writes: true`")
                        .at_step(&block.heading)
                        .at_section("Steps"),
                ),
                _ => {}
            }
            if let Err(err) = crate::manifest::parse_status(&block.bullets) {
                report.push(
                    Diag::error(file, err)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
            }
            if block_val(block, "op") == "validate" {
                if let Some(w) = block.bullets.iter().find(|b| b.key == "when") {
                    if let Err(err) = crate::runtime::check_when(&w.value) {
                        report.push(
                            Diag::error(file, format!("when: {err}"))
                                .at_step(&block.heading)
                                .at_section("Steps"),
                        );
                    }
                }
            }
        }
    }
    let caps = find_section(&doc.sections, "Signature")
        .and_then(|section| section.get("capabilities"))
        .unwrap_or("");
    let writes = match find_section(&doc.sections, "Signature") {
        Some(sig) => match crate::manifest::parse_writes(&sig.bullets) {
            Ok(w) => w.unwrap_or(false),
            Err(err) => {
                report.push(Diag::error(file, err).at_section("Signature"));
                return;
            }
        },
        None => false,
    };
    let Some(steps) = find_section(&doc.sections, "Steps") else {
        report.push(Diag::error(file, "method is missing steps").at_section("Steps"));
        return;
    };
    let mut saw_write = false;
    let calls_method = steps.blocks.iter().any(|b| block_val(b, "op") == "method");
    for block in &steps.blocks {
        let op = block_val(block, "op");
        if !matches!(
            op.as_str(),
            "query"
                | "validate"
                | "call"
                | "method"
                | "respond"
                | "verify-password"
                | "hash-password"
                | "random-token"
                | "hash-token"
                | "sign-jwt"
        ) {
            report.push(
                Diag::error(file, format!("op {op} is not allowed on a method"))
                    .at_step(&block.heading),
            );
        }
        // Ops this build executes inside methods share the API step grammar.
        // #77 ops (query, verify-password, hash-password, random-token,
        // hash-token, sign-jwt) keep their own checks below.
        if matches!(op.as_str(), "method" | "validate" | "respond") {
            // Inside a method, `op: method` may mark an inherited write.
            for problem in api_step_problems(block, &op)
                .into_iter()
                .filter(|p| !(op == "method" && p.starts_with("unknown key `writes`")))
            {
                report.push(
                    Diag::error(file, problem)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
            }
        }
        for problem in op_expression_problems(block, &op) {
            report.push(
                Diag::error(file, problem)
                    .at_step(&block.heading)
                    .at_section("Steps"),
            );
        }
        if op == "sign-jwt" {
            if !caps.contains("sign-token") {
                report.push(
                    Diag::error(file, "sign-jwt requires capabilities: [sign-token]")
                        .at_step(&block.heading),
                );
            }
            let alg = block_val(block, "alg");
            // Omitted alg defaults to ES256 (decision 139).
            if !matches!(alg.as_str(), "" | "ES256" | "RS256") {
                report.push(
                    Diag::error(file, "sign-jwt alg must be ES256 or RS256")
                        .at_step(&block.heading),
                );
            }
            if !is_secret_ref(&block_val(block, "key-ref")) {
                report.push(
                    Diag::error(file, "sign-jwt key must be a key-ref").at_step(&block.heading),
                );
            }
            if !block_val(block, "roles").is_empty() {
                report.push(
                    Diag::error(
                        file,
                        "sign-jwt roles come from users.roles (read by the platform); remove `roles`",
                    )
                    .at_step(&block.heading),
                );
            }
            let issue_refresh = block_val(block, "issue-refresh");
            if !matches!(
                issue_refresh.as_str(),
                "" | "none" | "platform-decided" | "new-family"
            ) {
                report.push(
                    Diag::error(
                        file,
                        format!(
                            "sign-jwt issue-refresh must be none, platform-decided or new-family (got `{issue_refresh}`)"
                        ),
                    )
                    .at_step(&block.heading),
                );
            }
            let declared = crate::lock::parse_string_list(&block_val(block, "custom-claims"))
                .unwrap_or_default();
            if let Ok(pairs) = crate::lock::parse_inline_map(&block_val(block, "claims")) {
                let rows: Vec<String> = steps
                    .blocks
                    .iter()
                    .take_while(|b| b.heading != block.heading)
                    .filter(|b| block_val(b, "op") == "query")
                    .map(|b| block_val(b, "out"))
                    .filter(|o| !o.is_empty())
                    .collect();
                match pairs.iter().find(|(name, _)| name == "sub") {
                    Some((_, expr)) => {
                        if let Some(problem) = crate::runtime::sub_source_problem(expr, &rows) {
                            report.push(Diag::error(file, problem).at_step(&block.heading));
                        }
                    }
                    None => report.push(
                        Diag::error(file, "sign-jwt needs a sub claim").at_step(&block.heading),
                    ),
                }
                for (name, _) in pairs.iter().filter(|(name, _)| name != "sub") {
                    if let Some(problem) =
                        crate::runtime::custom_claim_problem(name, &declared, &["roles"])
                    {
                        report.push(Diag::error(file, problem).at_step(&block.heading));
                    }
                }
            }
            if let Some(ms) = crate::llm::parse_duration_ms(&block_val(block, "ttl")) {
                if ms > 3_600_000 {
                    report.push(
                        Diag::error(file, "access token ttl cannot exceed 1h")
                            .at_step(&block.heading),
                    );
                }
            }
        }
        if op == "query" {
            let sql = block_val(block, "sql").trim_matches('`').to_string();
            // Structural checks from the parse tree (sqlguard).
            match crate::sqlguard::analyze(&sql) {
                Ok(info) => {
                    saw_write |= info.writes;
                    if info.writes && block_val(block, "writes") != "true" {
                        report.push(
                            Diag::error(file, "a query step that writes needs writes: true")
                                .at_step(&block.heading)
                                .at_section("Steps"),
                        );
                    }
                    if info.targets_rows && block_val(block, "scope").is_empty() {
                        report.push(
                            Diag::error(file, "UPDATE/DELETE needs scope: pk or scope: rls")
                                .at_step(&block.heading),
                        );
                    }
                }
                Err(problem) => report.push(
                    Diag::error(file, problem)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                ),
            }
            for problem in sql_problems(&sql, &block_val(block, "params"), false) {
                report.push(
                    Diag::error(file, problem)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
            }
        }
    }
    if !writes && saw_write {
        report
            .push(Diag::error(file, "writes: false but a step writes SQL").at_section("Signature"));
    }
    // With `op: method` steps the write may be inherited from a callee;
    // check_method_inherited_writes decides once all locks are loaded.
    if writes && !saw_write && !calls_method {
        report
            .push(Diag::error(file, "writes: true but no step writes SQL").at_section("Signature"));
    }
}

/// Writes propagate up through `op: method`: a method that calls a writing
/// method writes too, so its Signature must say `writes: true`.
fn check_method_inherited_writes(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    let mut ids: Vec<&String> = docs.keys().collect();
    ids.sort();
    for id in ids {
        let doc = &docs[id];
        if doc.meta.kind != Kind::Method {
            continue;
        }
        let Some(steps) = find_section(&doc.sections, "Steps") else {
            continue;
        };
        if !steps.blocks.iter().any(|b| block_val(b, "op") == "method") {
            continue;
        }
        let file = lock_rel(&doc.meta.source);
        let Ok(declared) = find_section(&doc.sections, "Signature")
            .map(|sig| crate::manifest::parse_writes(&sig.bullets))
            .unwrap_or(Ok(None))
        else {
            continue; // reported by check_method
        };
        let declared = declared.unwrap_or(false);
        let mut any_write = false;
        let mut sql_write = false;
        for block in &steps.blocks {
            let sql = block_val(block, "sql");
            if crate::security::sql_writes(sql.trim_matches('`')) {
                any_write = true;
                sql_write = true;
            }
            if block_val(block, "op") != "method" {
                continue;
            }
            let callee = block_val(block, "target");
            let inherited = !callee.is_empty() && security::known_method_writes(docs, &callee);
            any_write |= inherited;
            match crate::manifest::parse_writes(&block.bullets) {
                Ok(Some(true)) if !inherited => report.push(
                    Diag::error(
                        &file,
                        format!(
                            "step is marked `writes: true` but method `{callee}` does not write"
                        ),
                    )
                    .at_step(&block.heading)
                    .at_section("Steps"),
                ),
                Ok(Some(false)) if inherited => report.push(
                    Diag::error(
                        &file,
                        format!("step calls write method `{callee}` but is marked `writes: false`"),
                    )
                    .at_step(&block.heading)
                    .at_section("Steps"),
                ),
                _ => {}
            }
        }
        // SQL-only mismatches are already reported by check_method.
        if !declared && any_write && !sql_write {
            report.push(
                Diag::error(&file, "writes: false but a called method writes")
                    .at_section("Signature"),
            );
        }
        if declared && !any_write {
            report.push(
                Diag::error(&file, "writes: true but no step writes SQL").at_section("Signature"),
            );
        }
    }
}

fn check_feature(doc: &LockDoc, file: &str, report: &mut Report) {
    let Some(route) = find_section(&doc.sections, "Route") else {
        return;
    };
    let method = route.get("method").unwrap_or("");
    if !matches!(method, "GET" | "POST" | "PUT" | "DELETE") {
        report.push(
            Diag::error(file, "route method must be GET, POST, PUT, or DELETE").at_section("Route"),
        );
    }
    if let Some(path) = route.get("path") {
        check_path(doc, path, file, report);
    } else {
        report.push(Diag::error(file, "route is missing a path").at_section("Route"));
    }
    let public =
        find_section(&doc.sections, "Access").and_then(|s| s.get("public")) == Some("true");
    let removed = security::auth_removed(find_section(&doc.sections, "Pipelines"));
    if removed && !public {
        report.push(Diag::error(file, "removing auth requires public: true").at_section("Access"));
    }
    if public {
        let path = route.get("path").unwrap_or("");
        if !path.contains("{slug}") {
            report.push(Diag::error(file, "public routes must include {slug}").at_section("Route"));
        }
        let tenant = find_section(&doc.sections, "Access")
            .and_then(|s| s.get("tenant"))
            .unwrap_or("");
        if !tenant.contains("path") {
            report.push(
                Diag::error(file, "public routes must take tenant from the path")
                    .at_section("Access"),
            );
        }
    }
    if let Some(pipes) = find_section(&doc.sections, "Pipelines") {
        let auth_removed = pipes.blocks.iter().any(|block| {
            let name = pipeline_name(&block.heading);
            name == "-auth"
                || (name == "auth"
                    && block
                        .bullets
                        .iter()
                        .any(|b| b.key == "removed" && b.value == "true"))
        });
        let auth_mode = pipes
            .blocks
            .iter()
            .filter(|block| pipeline_name(&block.heading) == "auth")
            .find_map(|block| block.bullets.iter().find(|b| b.key == "mode"))
            .map(|b| b.value.clone())
            .unwrap_or_else(|| "jwt".into());
        let mut refresh = None;
        let mut revoke = None;
        for block in &pipes.blocks {
            let name = pipeline_name(&block.heading);
            let parsed = match name.as_str() {
                "token-refresh" => crate::manifest::parse_refresh_block(&block.bullets)
                    .map(|spec| refresh = Some(spec)),
                "token-revoke" => crate::manifest::parse_revoke_block(&block.bullets)
                    .map(|spec| revoke = Some(spec)),
                _ => Ok(()),
            };
            if let Err(err) = parsed {
                report.push(Diag::error(file, err).at_step(&block.heading));
            }
        }
        if let Err(err) = crate::manifest::check_refresh_route(
            refresh.as_ref(),
            revoke.as_ref(),
            public,
            auth_removed,
        ) {
            report.push(Diag::error(file, err).at_section("Pipelines"));
        }
        let mut declared_rates = Vec::new();
        for block in &pipes.blocks {
            if pipeline_name(&block.heading) == "rate-limit" {
                let get = |k: &str| block.bullets.iter().find(|b| b.key == k);
                declared_rates.push(crate::manifest::RateSpec {
                    per_minute: get("per-minute")
                        .and_then(|b| b.value.parse().ok())
                        .unwrap_or(60),
                    key: get("key")
                        .map(|b| b.value.clone())
                        .unwrap_or_else(|| "ip".into()),
                });
            }
            for (name, key) in [
                ("ip-per-minute", "ip"),
                ("account-per-minute", "account"),
                ("tenant-per-minute", "tenant"),
            ] {
                if let Some(v) = block.bullets.iter().find(|b| b.key == name) {
                    if let Ok(per_minute) = v.value.parse() {
                        declared_rates.push(crate::manifest::RateSpec {
                            per_minute,
                            key: key.into(),
                        });
                    }
                }
            }
        }
        if let Err(err) = crate::manifest::check_rate_set(&declared_rates) {
            report.push(Diag::error(file, err).at_section("Pipelines"));
        }
        for block in &pipes.blocks {
            if pipeline_name(&block.heading) == "rate-limit" {
                if let Err(err) =
                    crate::manifest::check_rate_params(block.bullets.iter().map(|b| b.key.as_str()))
                {
                    report.push(Diag::error(file, err).at_section("Pipelines"));
                }
                if block.bullets.iter().any(|b| {
                    (b.key == "key" && b.value == "account") || b.key == "account-per-minute"
                }) {
                    report.push(
                        Diag::warning(
                            file,
                            "rate-limit key `account` is deprecated: it now means input.email (it used to share the tenant bucket); write `key: input.email`",
                        )
                        .at_section("Pipelines"),
                    );
                }
                if let Some(key) = block
                    .bullets
                    .iter()
                    .find(|b| b.key == "key" && !(b.value == "family" && refresh.is_some()))
                {
                    if let Err(err) =
                        crate::manifest::check_rate_key(&key.value, auth_removed, &auth_mode)
                    {
                        report.push(Diag::error(file, err).at_section("Pipelines"));
                    }
                }
            }
        }
        for block in &pipes.blocks {
            if pipeline_name(&block.heading) != "auth" {
                continue;
            }
            let get = |k: &str| {
                block
                    .bullets
                    .iter()
                    .find(|b| b.key == k)
                    .map(|b| b.value.trim().to_string())
                    .unwrap_or_default()
            };
            if let Some(problem) = crate::manifest::jwt_override_problem(
                &get("iss"),
                &get("aud"),
                &get("jwks-uri"),
                auth_removed,
                &auth_mode,
            ) {
                report.push(Diag::error(file, problem).at_section("Pipelines"));
            }
        }
        for block in &pipes.blocks {
            let name = pipeline_name(&block.heading);
            if name == "auth" {
                if let Some(roles) = block.bullets.iter().find(|b| b.key == "roles") {
                    if roles.value.trim() == "[]" {
                        report.push(
                            Diag::error(file, "roles cannot be empty").at_section("Pipelines"),
                        );
                    }
                }
            }
        }
    }
    if let Some(steps) = find_section(&doc.sections, "Steps") {
        for block in &steps.blocks {
            let op = block_val(block, "op");
            if op == "transform" {
                report.push(
                    Diag::error(file, "api step op `transform` was removed; delete this step and put the request/response mapping in `## Transform` (`- in: <expr>` / `- out: <expr>`)")
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
                continue;
            }
            if !matches!(op.as_str(), "method" | "validate" | "respond") {
                report.push(
                    Diag::error(file, format!("api step op {op} is not allowed"))
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
            }
            if let Some(target) = block.bullets.iter().find(|b| b.key == "target") {
                if target.value.contains('@') {
                    report.push(
                        Diag::error(file, "method targets cannot include a version")
                            .at_step(&block.heading),
                    );
                }
            }
            for problem in api_step_problems(block, &op) {
                report.push(
                    Diag::error(file, problem)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
            }
        }
    }
    if let Some(transform) = find_section(&doc.sections, "Transform") {
        for bullet in &transform.bullets {
            let key = bullet.key.as_str();
            if matches!(key, "in" | "out") && bullet.value.trim() != "(none)" {
                if let Err(err) = crate::runtime::check_expr(&bullet.value) {
                    report.push(
                        Diag::error(file, format!("transform {key}: {err}"))
                            .at_section("Transform"),
                    );
                }
            }
        }
    }
    if find_section(&doc.sections, "Request").is_none()
        || find_section(&doc.sections, "Response").is_none()
    {
        report.push(Diag::error(file, "feature locks need Request and Response"));
    }
    if let Some(table) = find_section(&doc.sections, "Request").and_then(|s| s.table.as_ref()) {
        for row in &table.rows {
            if let (Some(name), Some(ty)) = (row.first(), row.get(1)) {
                if !crate::runtime::REQUEST_TYPES.contains(&ty.as_str()) {
                    report.push(
                        Diag::error(
                            file,
                            format!(
                                "request field {name} has unsupported type {ty} (use one of: {})",
                                crate::runtime::REQUEST_TYPES.join(", ")
                            ),
                        )
                        .at_section("Request"),
                    );
                }
            }
        }
    }
    if let Some(limits) = find_section(&doc.sections, "Limits") {
        for key in ["provider", "timeout", "body"] {
            if let Some(value) = limits.get(key) {
                if let Err(error) = check_limit_value(key, value) {
                    report.push(Diag::error(file, error).at_section("Limits"));
                }
            }
        }
    }
}

fn check_path(doc: &LockDoc, path: &str, file: &str, report: &mut Report) {
    if path.contains("..") || path.contains('*') || path.contains('?') {
        report.push(Diag::error(file, "path cannot contain .., *, or ?").at_section("Route"));
    }
    if path == "/health" || path.starts_with("/_internal") {
        report.push(Diag::error(file, "path uses a reserved prefix").at_section("Route"));
    }
    let mut parts = doc.meta.id.split('/');
    let definition = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let prefix = format!("/{definition}/{version}");
    if path != prefix && !path.starts_with(&format!("{prefix}/")) {
        report.push(
            Diag::error(
                file,
                "path must stay inside the definition and version prefix",
            )
            .at_section("Route"),
        );
    }
}

fn check_pipeline(doc: &LockDoc, file: &str, report: &mut Report) {
    let Some(steps) = find_section(&doc.sections, "Steps") else {
        return;
    };
    for block in &steps.blocks {
        let op = block_val(block, "op");
        if matches!(
            op.as_str(),
            "query" | "call" | "refresh-rotate" | "refresh-revoke"
        ) {
            report.push(
                Diag::error(file, format!("pipeline cannot use op {op}")).at_step(&block.heading),
            );
        }
    }
}

fn check_migration_sql(doc: &LockDoc, file: &str, report: &mut Report) {
    let Some(up) = find_section(&doc.sections, "Up") else {
        return;
    };
    for block in &up.blocks {
        let sql = block_val(block, "sql").trim_matches('`').to_string();
        if sql.is_empty() {
            continue;
        }
        for problem in sql_problems(&sql, &block_val(block, "params"), true) {
            report.push(
                Diag::error(file, problem)
                    .at_step(&block.heading)
                    .at_section("Up"),
            );
        }
        let lower = sql.to_ascii_lowercase();
        if lower.contains("create index") && !lower.contains("concurrently") {
            report.push(
                Diag::error(file, "CREATE INDEX must be CONCURRENTLY").at_step(&block.heading),
            );
        }
    }
}

/// Every roles claim the runtime may read: `roles`, `VIBE_JWT_ROLES_CLAIM`
/// and each route's `roles-claim`.
pub fn configured_roles_claims(
    docs: &HashMap<String, LockDoc>,
    env_claim: Option<&str>,
) -> Vec<String> {
    let mut out = vec!["roles".to_string()];
    out.extend(
        env_claim
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string),
    );
    for doc in docs.values() {
        if let Some(pipes) = find_section(&doc.sections, "Pipelines") {
            for block in &pipes.blocks {
                let claim = block_val(block, "roles-claim");
                if !claim.is_empty() {
                    out.push(claim);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A `sign-jwt` custom claim may not share a name with any roles claim.
fn check_claims_vs_roles_claims(
    docs: &HashMap<String, LockDoc>,
    env_claim: Option<&str>,
    report: &mut Report,
) {
    let claims = configured_roles_claims(docs, env_claim);
    let claims: Vec<&str> = claims.iter().map(String::as_str).collect();
    for doc in docs.values().filter(|d| d.meta.kind == Kind::Method) {
        let Some(steps) = find_section(&doc.sections, "Steps") else {
            continue;
        };
        for block in steps
            .blocks
            .iter()
            .filter(|b| block_val(b, "op") == "sign-jwt")
        {
            let declared = crate::lock::parse_string_list(&block_val(block, "custom-claims"))
                .unwrap_or_default();
            let Ok(pairs) = crate::lock::parse_inline_map(&block_val(block, "claims")) else {
                continue;
            };
            for name in declared.iter().chain(pairs.iter().map(|(n, _)| n)) {
                if name == "sub" {
                    continue;
                }
                let all_declared = std::slice::from_ref(name);
                if let Some(problem) =
                    crate::runtime::custom_claim_problem(name, all_declared, &claims)
                {
                    if crate::runtime::custom_claim_problem(name, all_declared, &["roles"])
                        .is_none()
                    {
                        report.push(Diag::error(&doc.meta.source, problem).at_step(&block.heading));
                    }
                }
            }
        }
    }
}

fn check_routes(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    let mut seen: HashMap<String, String> = HashMap::new();
    for doc in docs.values() {
        if doc.meta.kind != Kind::Api || doc.meta.scope != Some(Scope::Feature) {
            continue;
        }
        let Some(route) = find_section(&doc.sections, "Route") else {
            continue;
        };
        let Some(method) = route.get("method") else {
            continue;
        };
        let Some(path) = route.get("path") else {
            continue;
        };
        let key = format!("{} {}", method, normalize_path(path));
        if let Some(prev) = seen.insert(key.clone(), doc.meta.id.clone()) {
            report.push(Diag::error(
                &doc.meta.source,
                format!("route {key} duplicates {prev}"),
            ));
        }
    }
}

/// Every `token-refresh` `sign-method` must name a method with a `sign-jwt`
/// step (a typo would otherwise only surface as a failing refresh call).
fn check_refresh_sign_methods(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    for doc in docs.values() {
        if doc.meta.kind != Kind::Api || doc.meta.scope != Some(Scope::Feature) {
            continue;
        }
        let Some(pipes) = find_section(&doc.sections, "Pipelines") else {
            continue;
        };
        for block in &pipes.blocks {
            if pipeline_name(&block.heading) != "token-refresh" {
                continue;
            }
            let target = block_val(block, "sign-method");
            let Some((object, method)) = target.split_once('.') else {
                if !target.is_empty() {
                    report.push(
                        Diag::error(
                            &doc.meta.source,
                            format!("token-refresh sign-method `{target}` is not a known method"),
                        )
                        .at_step(&block.heading),
                    );
                }
                continue;
            };
            let id = format!("{object}/methods/{method}");
            let problem = match docs.get(&id) {
                None => Some(format!(
                    "token-refresh sign-method `{target}` is not a known method"
                )),
                Some(m) => {
                    let signs = find_section(&m.sections, "Steps").is_some_and(|steps| {
                        steps
                            .blocks
                            .iter()
                            .any(|b| block_val(b, "op") == "sign-jwt")
                    });
                    (!signs).then(|| {
                        format!("token-refresh sign-method `{target}` has no sign-jwt step")
                    })
                }
            };
            if let Some(problem) = problem {
                report.push(Diag::error(&doc.meta.source, problem).at_step(&block.heading));
            }
        }
    }
}

/// CRUD actions every model provides without a method lock.
pub const CRUD_ACTIONS: &[&str] = &["create", "get", "list", "update", "delete"];

/// A feature step target must be `object.action`, where the action is a
/// built-in CRUD action on an existing model or an existing method lock.
pub fn check_method_target(target: &str, docs: &HashMap<String, LockDoc>) -> Result<(), String> {
    let target = target.trim();
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let Some((object, action)) = target.split_once('.') else {
        return Err(format!(
            "method target {target} must look like object.action"
        ));
    };
    if !ident(object) || !ident(action) {
        return Err(format!(
            "method target {target} must look like object.action"
        ));
    }
    let method = format!("{object}/methods/{action}");
    if docs
        .get(&method)
        .is_some_and(|d| d.meta.kind == Kind::Method)
    {
        return Ok(());
    }
    let model = docs.get(object).is_some_and(|d| d.meta.kind == Kind::Model);
    if model && CRUD_ACTIONS.contains(&action) {
        return Ok(());
    }
    if !model
        && !docs
            .keys()
            .any(|id| id.starts_with(&format!("{object}/methods/")))
    {
        return Err(format!(
            "unknown method target {target}: no model or methods named {object}"
        ));
    }
    Err(format!(
        "unknown method target {target}: {action} is not a CRUD action ({}) or a method lock",
        CRUD_ACTIONS.join(", ")
    ))
}

fn check_targets(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    for doc in docs.values() {
        let feature = doc.meta.kind == Kind::Api && doc.meta.scope == Some(Scope::Feature);
        let method = doc.meta.kind == Kind::Method;
        if !feature && !method {
            continue;
        }
        if method {
            // Calls.methods is the static call list the runtime binds at deploy.
            if let Some(list) = find_section(&doc.sections, "Calls").and_then(|s| s.get("methods"))
            {
                let targets = match crate::lock::parse_string_list(list) {
                    Ok(targets) => targets,
                    Err(_) => {
                        // Fail closed: a malformed list must not skip the check.
                        report.push(
                            Diag::error(
                                lock_rel(&doc.meta.source),
                                "Calls.methods must be a list like `[object.action, ...]`",
                            )
                            .at_section("Calls"),
                        );
                        Vec::new()
                    }
                };
                for target in targets {
                    if let Err(err) = check_method_target(&target, docs) {
                        report
                            .push(Diag::error(lock_rel(&doc.meta.source), err).at_section("Calls"));
                    }
                }
            }
        }
        let Some(steps) = find_section(&doc.sections, "Steps") else {
            continue;
        };
        for block in &steps.blocks {
            // In method locks only `op: method` targets are method ids;
            // `op: call` targets are external endpoints.
            if method && block_val(block, "op") != "method" {
                continue;
            }
            if let Some(target) = block.bullets.iter().find(|b| b.key == "target") {
                if let Err(err) = check_method_target(&target.value, docs) {
                    report.push(
                        Diag::error(lock_rel(&doc.meta.source), err)
                            .at_step(&block.heading)
                            .at_section("Steps"),
                    );
                }
            }
        }
    }
}

fn check_calls(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for doc in docs.values() {
        if doc.meta.kind != Kind::Method {
            continue;
        }
        let mut edges = Vec::new();
        if let Some(calls) = find_section(&doc.sections, "Calls") {
            if let Some(list) = calls.get("methods") {
                for target in crate::lock::parse_string_list(list).unwrap_or_default() {
                    if let Some((object, method)) = target.split_once('.') {
                        edges.push(format!("{object}/methods/{method}"));
                    }
                }
            }
        }
        edges.retain(|id| docs.contains_key(id));
        graph.insert(doc.meta.id.clone(), edges);
    }
    for start in graph.keys() {
        let mut stack = vec![(start.clone(), 0u32)];
        let mut visiting = HashSet::new();
        while let Some((node, depth)) = stack.pop() {
            if depth > 3 {
                report.push(Diag::error(node, "call depth exceeds 3"));
                break;
            }
            if !visiting.insert(node.clone()) {
                report.push(Diag::error(node, "method calls form a cycle"));
                break;
            }
            if let Some(next) = graph.get(&node) {
                for edge in next {
                    stack.push((edge.clone(), depth + 1));
                }
            }
            visiting.remove(&node);
        }
    }
}

fn check_migrations(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    for model in docs.values().filter(|doc| doc.meta.kind == Kind::Model) {
        let Ok(constraints) = crate::zh::constraints(model) else {
            continue;
        };
        let table_section = find_section(&model.sections, "Table");
        let table = table_section.and_then(|s| s.get("table")).unwrap_or("");
        let provider = table_section.and_then(|s| s.get("provider")).unwrap_or("");
        for columns in constraints {
            let mut expected = vec!["tenant_id".to_string()];
            expected.extend(columns.clone());
            let found = docs
                .values()
                .filter(|d| {
                    d.meta.kind == Kind::Migration
                        && find_section(&d.sections, "Target").and_then(|s| s.get("provider"))
                            == Some(provider)
                })
                .filter_map(|d| find_section(&d.sections, "Up"))
                .flat_map(|s| &s.blocks)
                .any(|b| crate::zh::unique_index_of(&block_val(b, "sql"), table, &expected));
            if !found {
                report.push(Diag::error(lock_rel(&model.meta.source), format!("model {} Constraints [{}] requires matching unique index on {table} ({}) on provider {provider}", model.meta.id, columns.join(", "), expected.join(", "))).at_section("Constraints"));
            }
        }
    }

    let mut by_provider: HashMap<String, Vec<u32>> = HashMap::new();
    for doc in docs.values() {
        if doc.meta.kind != Kind::Migration {
            continue;
        }
        let Some(seq) = doc
            .meta
            .id
            .split('/')
            .next_back()
            .and_then(|name| name.split('-').next())
        else {
            continue;
        };
        let Ok(n) = seq.parse::<u32>() else { continue };
        let provider = doc.meta.id.split('/').nth(1).unwrap_or("").to_string();
        by_provider.entry(provider).or_default().push(n);
    }
    for (provider, mut seqs) in by_provider {
        seqs.sort_unstable();
        for (idx, seq) in seqs.iter().enumerate() {
            let expected = (idx as u32) + 1;
            if *seq != expected {
                report.push(Diag::error(
                    format!("migrations/{provider}"),
                    format!("migration sequence jumps at {seq:04}"),
                ));
                break;
            }
        }
    }
}

fn check_orphans(root: &Path, sources: &[SourceFile], report: &mut Report) {
    let expected: HashSet<String> = sources.iter().map(|source| lock_rel(&source.rel)).collect();
    for dir in ["providers", "pipelines", "migrations", "models", "api"] {
        walk_locks(&root.join(dir), root, &expected, report);
    }
}

fn walk_locks(dir: &Path, root: &Path, expected: &HashSet<String>, report: &mut Report) {
    if !dir.exists() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_locks(&path, root, expected, report);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".lock.md") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if !expected.contains(&rel) {
            report.push(Diag::error(rel, "orphan lock has no source"));
        }
    }
}

pub fn sql_problems(sql: &str, params: &str, migration: bool) -> Vec<String> {
    let mut problems = Vec::new();
    let trimmed = sql.trim().trim_matches('`').trim();
    let cleaned = crate::security::strip_sql_noise(trimmed);
    let lower = cleaned.to_ascii_lowercase();
    // Allow `;` only inside DO $$ … $$ procedural bodies.
    if trimmed.contains(';') {
        let raw_lower = trimmed.to_ascii_lowercase();
        let do_body = raw_lower.contains("do $$") || raw_lower.contains("do $");
        if !do_body {
            problems.push("SQL must be a single statement".into());
        }
    }
    if FORBIDDEN_SQL.is_match(&cleaned) {
        problems.push("DELETE, TRUNCATE, GRANT, and REVOKE are forbidden".into());
    }
    static WHERE_WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)\bwhere\b").expect("regex"));
    static SELECT_INTO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bselect\b[^;]*\binto\b").expect("regex"));
    if !migration && (DDL_SQL.is_match(&cleaned) || SELECT_INTO.is_match(&cleaned)) {
        problems.push("DDL is only allowed in migration locks".into());
    }
    if crate::security::sql_writes(trimmed)
        && lower.contains("update")
        && !WHERE_WORD.is_match(&lower)
    {
        problems.push("UPDATE needs a WHERE clause".into());
    }
    if !migration {
        for problem in crate::sqlpolicy::problems(trimmed) {
            if !problems.contains(&problem) {
                problems.push(problem);
            }
        }
    }
    // Param hygiene for reads and writes, including WITH … forms.
    if !migration
        && (crate::security::sql_writes(trimmed)
            || lower.trim_start().starts_with("select")
            || lower.trim_start().starts_with("with")
            || lower.trim_start().starts_with("("))
    {
        let nums: Vec<usize> = PARAM
            .captures_iter(trimmed)
            .filter_map(|cap| cap.get(1).and_then(|m| m.as_str().parse().ok()))
            .collect();
        let declared = crate::lock::parse_string_list(params).unwrap_or_default();
        if params.is_empty() && !nums.is_empty() {
            problems.push("SQL parameters are not listed".into());
        } else if !params.is_empty() {
            let max = nums.iter().copied().max().unwrap_or(0);
            if max != declared.len() || nums.iter().collect::<HashSet<_>>().len() != nums.len() {
                problems.push("SQL $n parameters do not match the params list".into());
            }
            for expected in 1..=declared.len() {
                if !nums.contains(&expected) {
                    problems.push(format!("parameter ${expected} is unused"));
                    break;
                }
            }
        }
    }
    problems
}

/// SQL that writes somewhere other than a leading INSERT/UPDATE, such as
/// `WITH x AS (INSERT ...)`: a write hidden inside a read. Rejected.
pub fn writable_cte(sql: &str) -> bool {
    crate::security::sql_writes(sql) && !plain_write(sql)
}

/// A statement that is itself an INSERT or UPDATE.
pub fn plain_write(sql: &str) -> bool {
    let lower = sql.trim().trim_matches('`').trim().to_ascii_lowercase();
    lower.starts_with("insert") || lower.starts_with("update")
}

fn known_type(ty: &str) -> bool {
    matches!(
        ty,
        "uuid"
            | "string"
            | "string[]"
            | "integer"
            | "number"
            | "boolean"
            | "timestamp"
            | "password"
            | "json"
            | "bytes"
    )
}

fn is_secret_ref(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn rule_number(rules: &str, key: &str) -> Option<i64> {
    for part in rules.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&format!("{key}:")) {
            return rest
                .trim()
                .trim_matches(|c| c == '[' || c == ']')
                .parse()
                .ok();
        }
    }
    None
}

/// `min:` / `max:` present but not an integer — fail closed (do not ignore).
fn bad_number_rule(rules: &str, key: &str) -> Option<String> {
    for part in rules.split(',') {
        let part = part.trim();
        let Some(rest) = part.strip_prefix(&format!("{key}:")) else {
            continue;
        };
        let raw = rest.trim().trim_matches(|c| c == '[' || c == ']');
        if raw.is_empty() || raw.parse::<i64>().is_err() {
            return Some(format!("{key} value `{raw}` is not an integer"));
        }
    }
    None
}

/// Check expression variable roots against the names available in each scope.
fn check_variable_roots(doc: &LockDoc, file: &str, report: &mut Report) {
    let api = doc.meta.kind == Kind::Api && doc.meta.scope == Some(Scope::Feature);
    if !api && doc.meta.kind != Kind::Method {
        return;
    }
    let mut allowed: std::collections::BTreeSet<String> = if api {
        ["ctx", "req"].as_slice()
    } else {
        ["ctx", "input"].as_slice()
    }
    .iter()
    .map(|s| s.to_string())
    .collect();
    let check = |expr: &str,
                 when: bool,
                 location: &str,
                 allowed: &std::collections::BTreeSet<String>,
                 report: &mut Report| {
        if expr.trim().is_empty() || expr.trim() == "(none)" {
            return;
        }
        if let Ok(roots) = crate::runtime::expression_roots(expr, when) {
            for root in roots.difference(allowed) {
                report.push(Diag::error(
                    file,
                    format!(
                        "unknown variable `{root}` in {location}; defined here: {}",
                        allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
    };
    let transform = find_section(&doc.sections, "Transform");
    if api {
        if let Some(expr) = transform.and_then(|s| s.get("in")) {
            check(expr, false, "Transform in", &allowed, report);
        }
        allowed.insert("input".into());
    }
    let mut response_allowed = allowed.clone();
    if let Some(steps) = find_section(&doc.sections, "Steps") {
        for block in &steps.blocks {
            let op = block_val(block, "op");
            let name = block
                .heading
                .split_whitespace()
                .next()
                .unwrap_or(&block.heading);
            let keys: &[&str] = match op.as_str() {
                "method" | "call" => &["args"],
                "respond" => &["value"],
                "validate" => &["when"],
                "verify-password" => &["hash", "plain"],
                "hash-password" => &["plain"],
                "hash-token" => &["value"],
                _ => &[],
            };
            for key in keys {
                let expr = block_val(block, key);
                if (*key == "args" && expr == "transform.in")
                    || (api && *key == "value" && expr == "transform.out")
                {
                    continue;
                }
                check(
                    &expr,
                    *key == "when",
                    &format!("step {name} {op} {key}"),
                    &allowed,
                    report,
                );
            }
            if op == "query" {
                if let Ok(items) = crate::lock::parse_string_list(&block_val(block, "params")) {
                    for expr in items {
                        check(
                            &expr,
                            false,
                            &format!("step {name} query params"),
                            &allowed,
                            report,
                        );
                    }
                }
            }
            if op == "sign-jwt" {
                if let Ok(items) = crate::lock::parse_inline_map(&block_val(block, "claims")) {
                    for (_, expr) in items {
                        check(
                            &expr,
                            false,
                            &format!("step {name} sign-jwt claims"),
                            &allowed,
                            report,
                        );
                    }
                }
            }
            if op == "respond" {
                response_allowed = allowed.clone();
            }
            if (!api
                && matches!(
                    op.as_str(),
                    "method"
                        | "query"
                        | "verify-password"
                        | "hash-password"
                        | "random-token"
                        | "hash-token"
                        | "sign-jwt"
                ))
                || (api && op == "method")
            {
                let out = block_val(block, "out");
                if !out.is_empty() && out != "(none)" {
                    allowed.insert(out);
                }
            }
        }
    }
    if api {
        if let Some(expr) = transform.and_then(|s| s.get("out")) {
            if expr.trim() != "result" {
                check(expr, false, "Transform out", &response_allowed, report);
            }
        }
    }
}

/// What the runtime would fail on for an API feature step: unknown keys
/// (e.g. `method:` instead of `target:`), a missing target, expressions it
/// cannot parse, a validate step without `when`/`fail`, a bad status.
pub(crate) fn api_step_problems(block: &crate::lock::Block, op: &str) -> Vec<String> {
    let allowed: &[&str] = match op {
        "method" => &["target", "args", "out"],
        "validate" => &["when", "fail"],
        "respond" => &["value", "status"],
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for bullet in &block.bullets {
        let key = bullet.key.as_str();
        if key != "op" && key != "source-lines" && !allowed.contains(&key) {
            let hint = if key == "method" && op == "method" {
                " (use `target:`)"
            } else {
                ""
            };
            out.push(format!("unknown key `{key}` for op {op}{hint}"));
        }
    }
    let get = |k: &str| block_val(block, k);
    match op {
        "method" => {
            if get("target").trim().is_empty() {
                out.push("method step needs `target`".into());
            }
            let args = get("args");
            if !args.is_empty() && args != "transform.in" {
                if let Err(err) = crate::runtime::check_expr(&args) {
                    out.push(format!("args: {err}"));
                }
            }
        }
        "validate" => {
            match block.bullets.iter().find(|b| b.key == "when") {
                Some(w) => {
                    if let Err(err) = crate::runtime::check_when(&w.value) {
                        out.push(format!("when: {err}"));
                    }
                }
                None => out.push("validate step needs `when`".into()),
            }
            if get("fail").trim().is_empty() {
                out.push("validate step needs a `fail` error code".into());
            }
        }
        "respond" => {
            let value = get("value");
            if !value.is_empty() && value != "transform.out" {
                if let Err(err) = crate::runtime::check_expr(&value) {
                    out.push(format!("value: {err}"));
                }
            }
            let status = get("status");
            if !status.is_empty() && !status.parse::<u16>().is_ok_and(|s| (100..600).contains(&s)) {
                out.push(format!("status `{status}` is not an HTTP status"));
            }
        }
        _ => {}
    }
    out
}

/// Expression inputs evaluated by runtime::ops (including map/list entries).
pub(crate) fn op_expression_problems(block: &crate::lock::Block, op: &str) -> Vec<String> {
    let mut expressions = Vec::new();
    let mut problems = Vec::new();
    let keys: &[&str] = match op {
        "verify-password" => &["hash", "plain"],
        "hash-password" => &["plain"],
        "hash-token" => &["value"],
        _ => &[],
    };
    for key in keys {
        expressions.push((key.to_string(), block_val(block, key)));
    }
    if op == "query" {
        let raw = block_val(block, "params");
        if !raw.is_empty() {
            match crate::lock::parse_string_list(&raw) {
                Ok(items) => expressions.extend(items.into_iter().map(|v| ("params".into(), v))),
                Err(err) => problems.push(format!("params: {err}")),
            }
        }
    }
    if op == "sign-jwt" {
        let raw = block_val(block, "claims");
        if !raw.trim().is_empty() {
            match crate::lock::parse_inline_map(&raw) {
                Ok(items) => expressions.extend(items),
                Err(err) => problems.push(format!("claims: {err}")),
            }
        }
    }
    for (key, expr) in expressions {
        if let Err(err) = crate::runtime::check_expr(&expr) {
            problems.push(format!("{key}: {err}"));
        }
    }
    problems
}

fn block_val(block: &crate::lock::Block, key: &str) -> String {
    block
        .bullets
        .iter()
        .find(|bullet| bullet.key == key)
        .map(|bullet| bullet.value.clone())
        .unwrap_or_default()
}

pub fn normalize_path(path: &str) -> String {
    let mut path = urlencoding_decode(path).to_ascii_lowercase();
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path.replace(":id", "{id}")
}

fn urlencoding_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_string())
}

/// Parse digits with an optional case-insensitive b, kb or mb suffix.
/// Whitespace, signs and overflowing sizes are rejected.
pub fn parse_size_bytes(raw: &str) -> Option<u64> {
    let digits = raw.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let multiplier = match raw[digits..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1024,
        "mb" => 1_048_576,
        _ => return None,
    };
    raw[..digits].parse::<u64>().ok()?.checked_mul(multiplier)
}

pub fn parse_bytes(raw: &str) -> u64 {
    if let Some(bytes) = parse_size_bytes(raw) {
        return bytes;
    }
    // Preserve legacy whitespace, numeric and saturating uppercase-unit behavior.
    let raw = raw.trim();
    if let Some(n) = raw.strip_suffix("MB") {
        return n
            .trim()
            .parse::<u64>()
            .unwrap_or(0)
            .saturating_mul(1_048_576);
    }
    if let Some(n) = raw.strip_suffix("KB") {
        return n.trim().parse::<u64>().unwrap_or(0).saturating_mul(1024);
    }
    raw.parse().unwrap_or(0)
}

fn check_feature_method_writes(docs: &HashMap<String, LockDoc>, report: &mut Report) {
    for doc in docs.values() {
        if doc.meta.kind != Kind::Api || doc.meta.scope != Some(Scope::Feature) {
            continue;
        }
        let file = lock_rel(&doc.meta.source);
        let public =
            find_section(&doc.sections, "Access").and_then(|s| s.get("public")) == Some("true");
        let public_writes = find_section(&doc.sections, "Access")
            .and_then(|s| s.get("public-writes"))
            == Some("true");
        let Some(steps) = find_section(&doc.sections, "Steps") else {
            continue;
        };
        for block in &steps.blocks {
            let target = block_val(block, "target");
            if target.is_empty() {
                continue;
            }
            if !security::method_doc_writes(docs, &target) {
                continue;
            }
            if public && !public_writes {
                report.push(
                    Diag::error(
                        &file,
                        format!(
                            "public feature calls write method `{target}`; set public-writes: true"
                        ),
                    )
                    .at_step(&block.heading)
                    .at_section("Access"),
                );
            }
        }
    }
}

fn check_security_items_enriched(
    docs: &HashMap<String, LockDoc>,
    sources: &HashMap<String, SourceFile>,
    opts: &ValidateOptions<'_>,
    report: &mut Report,
) {
    let _ = opts;
    for doc in docs.values() {
        if doc.meta.kind != Kind::Api || doc.meta.scope != Some(Scope::Feature) {
            continue;
        }
        let Some(source) = sources.get(&doc.meta.id) else {
            continue;
        };
        let file = lock_rel(&source.rel);
        let mut expected = security::extract_items(doc);
        security::enrich_feature_writes(doc, docs, &mut expected);
        if security::auth_mode_changed(doc, docs) {
            expected.push(crate::lock::SecurityItem {
                item: "auth-mode-change".into(),
                risk: crate::lock::Risk::High,
                reason: String::new(),
            });
        }
        if scan::scan_text(&source.combined_text())
            .iter()
            .any(|hit| !hit.fatal)
        {
            expected.push(crate::lock::SecurityItem {
                item: "prompt-injection".into(),
                risk: crate::lock::Risk::Medium,
                reason: String::new(),
            });
        }
        expected.sort_by(|a, b| a.item.cmp(&b.item));
        expected.dedup_by(|a, b| a.item == b.item);
        if !security::same_items(&expected, &doc.meta.security_items) {
            report.push(Diag::error(
                &file,
                "security-items do not match the validator extraction",
            ));
        }
    }
}

fn lock_rel(source_rel: &str) -> String {
    source_rel.trim_end_matches(".md").to_string() + ".lock.md"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{Block, Bullet, Meta, Section};

    fn block(heading: &str, pairs: &[(&str, &str)]) -> Block {
        Block {
            heading: heading.into(),
            bullets: pairs
                .iter()
                .map(|(k, v)| Bullet {
                    key: (*k).into(),
                    value: (*v).into(),
                })
                .collect(),
        }
    }

    fn doc(
        kind: Kind,
        scope: Option<Scope>,
        id: &str,
        section: &str,
        blocks: Vec<Block>,
    ) -> LockDoc {
        LockDoc {
            meta: Meta::pending(kind, scope, id, format!("{id}.md")),
            sections: vec![Section {
                name: section.into(),
                bullets: Vec::new(),
                table: None,
                blocks,
                none: false,
            }],
        }
    }

    #[test]
    fn variable_roots_follow_runtime_scopes() {
        let mut lock = doc(
            Kind::Api,
            Some(Scope::Feature),
            "catalog/v1/hello",
            "Steps",
            vec![
                block(
                    "s1 method",
                    &[
                        ("op", "method"),
                        ("args", "{ x: input.x, y: req.body, z: ctx.user.sub }"),
                        ("out", "row"),
                    ],
                ),
                block("s2 respond", &[("op", "respond"), ("value", "foo.bar")]),
            ],
        );
        lock.sections.push(Section {
            name: "Transform".into(),
            bullets: vec![
                Bullet {
                    key: "in".into(),
                    value: "input.query.message".into(),
                },
                Bullet {
                    key: "out".into(),
                    value: "row".into(),
                },
            ],
            table: None,
            blocks: vec![],
            none: false,
        });
        let mut report = Report::new();
        check_variable_roots(&lock, "hello.lock.md", &mut report);
        let errors: Vec<_> = report.errors().map(|d| d.message.clone()).collect();
        assert_eq!(errors, vec!["unknown variable `input` in Transform in; defined here: ctx, req", "unknown variable `foo` in step s2 respond value; defined here: ctx, input, req, row"]);
        lock.sections[0].blocks[1].bullets[1].value =
            "{ value: row, literal: \"foo.bar\", ok: true, count: 2, nothing: null }".into();
        lock.sections[1].bullets[0].value =
            "{ value: req.query.message, user: ctx.user.sub }".into();
        let mut report = Report::new();
        check_variable_roots(&lock, "hello.lock.md", &mut report);
        assert!(!report.has_errors(), "{report}");
    }

    #[test]
    fn method_expression_inputs_fail_validation() {
        for (op, key) in [
            ("respond", "value"),
            ("method", "args"),
            ("hash-password", "plain"),
            ("verify-password", "hash"),
            ("hash-token", "value"),
            ("query", "params"),
            ("sign-jwt", "claims"),
        ] {
            let expr = match key {
                "params" => "[{a: }]",
                "claims" => "{sub: {a: }}",
                _ => "{a: }",
            };
            let body = format!("## Signature\n- writes: false\n## Steps\n### s1\n- op: {op}\n- target: product.get\n- {key}: {expr}\n");
            assert!(
                method_errors(&body).iter().any(|e| e.contains("expression")
                    || e.contains("object")
                    || e.contains("expected a name")),
                "{op}: {:?}",
                method_errors(&body)
            );
        }
        for key in ["in", "out"] {
            let d = parsed_doc(
                Kind::Api,
                Some(Scope::Feature),
                "hello/v1/get",
                &format!("## Transform\n- {key}: (none)\n"),
            );
            let mut report = Report::new();
            check_feature(&d, "hello", &mut report);
            assert!(!report.errors().any(|e| e.message.contains("transform")));
        }
    }

    #[test]
    fn removed_api_transform_step_has_one_actionable_error() {
        for keys in ["", "- in: req.body\n- out: input\n- target: bad@v1\n"] {
            let doc = parsed_doc(
                Kind::Api,
                Some(Scope::Feature),
                "hello/v1/get",
                &format!("## Route\n- method: GET\n- path: /hello/v1/get\n## Request\n(none)\n## Response\n(none)\n## Steps\n### s1\n- op: transform\n{keys}"),
            );
            let mut report = Report::new();
            check_feature(&doc, "hello.lock.md", &mut report);
            let errors: Vec<_> = report.errors().collect();
            assert_eq!(errors.len(), 1, "{report}");
            assert_eq!(errors[0].section, "Steps");
            assert_eq!(errors[0].step, "s1");
            assert_eq!(errors[0].message, "api step op `transform` was removed; delete this step and put the request/response mapping in `## Transform` (`- in: <expr>` / `- out: <expr>`)");
        }
    }

    #[test]
    fn literal_responses_and_malformed_transforms() {
        for (expr, valid) in [
            (r#"{"message": "hello world"}"#, true),
            ("{message: }", false),
        ] {
            let step = block("s1", &[("op", "respond"), ("value", expr)]);
            assert_eq!(api_step_problems(&step, "respond").is_empty(), valid);
        }
        for key in ["in", "out"] {
            for expr in ["", "{a: }", "[1]"] {
                let body = format!(
                    "## Route\n- method: GET\n- path: /hello/v1/greet\n\n## Transform\n- {key}: {expr}\n"
                );
                let doc = parsed_doc(Kind::Api, Some(Scope::Feature), "hello/v1/get", &body);
                let mut report = Report::new();
                check_feature(&doc, "hello.lock.md", &mut report);
                assert!(
                    report
                        .errors()
                        .any(|d| d.message.contains(&format!("transform {key}:"))),
                    "{expr}: {:?}",
                    report.diags
                );
            }
        }
        for (expr, valid) in [
            (r#"{"message": "hello world"}"#, true),
            ("{message: }", false),
        ] {
            let body = format!(
                "## Route\n- method: GET\n- path: /hello/v1/greet\n\n## Request\n(none)\n\n## Response\n(none)\n\n## Steps\n### reply\n- op: respond\n- value: {expr}\n"
            );
            let doc = parsed_doc(Kind::Api, Some(Scope::Feature), "hello/v1/get", &body);
            let mut report = Report::new();
            check_feature(&doc, "hello.lock.md", &mut report);
            assert_eq!(
                report.errors().any(|d| d.message.contains("value:")),
                !valid,
                "{:?}",
                report.diags
            );
            if valid {
                assert_eq!(report.errors().count(), 0, "{:?}", report.diags);
            }
        }
    }

    #[test]
    fn sign_jwt_rejects_literal_subject() {
        let body = "## Signature\n- capabilities: [sign-token]\n- writes: false\n\n## Steps\n### read\n- op: query\n- out: user\n\n### sign\n- op: sign-jwt\n- key-ref: JWT_KEY\n- claims: {sub: \"admin\"}\n";
        let doc = parsed_doc(Kind::Method, None, "user/methods/sign", body);
        let mut report = Report::new();
        check_method(&doc, "sign.lock.md", &mut report);
        assert!(
            report
                .errors()
                .any(|d| d.message.contains("sub must be a field")),
            "{:?}",
            report.diags
        );
        // The runtime calls this same guard before evaluating the subject.
        assert!(crate::runtime::sub_source_problem(r#""admin""#, &["user".into()]).is_some());
        assert!(crate::runtime::sub_source_problem(r#""user.id""#, &["user".into()]).is_some());
        assert!(crate::runtime::sub_source_problem("user.id", &["user".into()]).is_none());
    }

    fn sign_method(issue_refresh: &str) -> LockDoc {
        doc(
            Kind::Method,
            None,
            "user/methods/sign-token",
            "Steps",
            vec![block(
                "### s1",
                &[
                    ("op", "sign-jwt"),
                    ("key-ref", "secret:JWT_SIGNING_KEY"),
                    ("issue-refresh", issue_refresh),
                    ("source-lines", "3-9"),
                ],
            )],
        )
    }

    #[test]
    fn sign_jwt_issue_refresh_is_supported() {
        for mode in ["platform-decided", "new-family", "none"] {
            let mut report = Report::new();
            check_method(&sign_method(mode), "m", &mut report);
            assert!(
                !report
                    .diags
                    .iter()
                    .any(|d| d.message.contains("issue-refresh")),
                "{mode}: {:?}",
                report.diags
            );
        }
        let mut report = Report::new();
        check_method(&sign_method("rotate"), "m", &mut report);
        assert!(report.errors().any(|d| d.message.contains("issue-refresh")));
    }

    #[test]
    fn token_refresh_sign_method_must_exist_and_sign() {
        let route = |target: &str| {
            doc(
                Kind::Api,
                Some(Scope::Feature),
                "auth/refresh",
                "Pipelines",
                vec![block(
                    "### token-refresh",
                    &[("sign-method", target), ("source-lines", "1-4")],
                )],
            )
        };
        let mut docs = HashMap::new();
        docs.insert(
            "user/methods/sign-token".into(),
            sign_method("platform-decided"),
        );
        docs.insert("auth/refresh".into(), route("user.sign-token"));
        let mut report = Report::new();
        check_refresh_sign_methods(&docs, &mut report);
        assert!(!report.has_errors(), "{:?}", report.diags);
        docs.insert("auth/refresh".into(), route("user.sign-tokn"));
        let mut report = Report::new();
        check_refresh_sign_methods(&docs, &mut report);
        assert!(
            report
                .errors()
                .any(|d| d.message.contains("not a known method")),
            "{:?}",
            report.diags
        );
    }

    #[test]
    fn sign_jwt_alg_may_be_omitted_but_not_unknown() {
        // Decision 139 (#226): an omitted alg means ES256.
        let body = |alg: &str| {
            format!(
                "## Signature\n- input: {{email: string}}\n- output: {{t: string}}\n- writes: false\n- capabilities: [sign-token]\n\n## Steps\n\n### s1\n\n- op: sign-jwt\n{alg}- key-ref: VIBE_JWT_PRIVATE_KEY\n- out: t\n"
            )
        };
        let alg_errors = |text: String| {
            let lock = LockDoc {
                meta: Meta::pending(Kind::Method, None, "user/methods/issue", "issue.md"),
                sections: crate::lock::parse_sections(&text).unwrap(),
            };
            let mut report = Report::new();
            check_method(&lock, "issue.md", &mut report);
            report
                .errors()
                .filter(|d| d.message.contains("alg must be"))
                .count()
        };
        assert_eq!(alg_errors(body("")), 0);
        assert_eq!(alg_errors(body("- alg: ES256\n")), 0);
        assert_eq!(alg_errors(body("- alg: RS256\n")), 0);
        assert_eq!(alg_errors(body("- alg: HS256\n")), 1);
        assert_eq!(alg_errors(body("- alg: none\n")), 1);
    }

    #[test]
    fn delete_sql_is_rejected() {
        let problems = sql_problems("delete from users where id = $1", "[input.id]", false);
        assert!(problems.iter().any(|p| p.contains("DELETE")));
    }

    #[test]
    fn update_without_where_is_rejected() {
        let problems = sql_problems("update users set name = $1", "[input.name]", false);
        assert!(problems.iter().any(|p| p.contains("WHERE")));
    }

    fn parsed_doc(kind: Kind, scope: Option<Scope>, id: &str, body: &str) -> LockDoc {
        LockDoc {
            meta: crate::lock::Meta::pending(kind, scope, id, format!("{id}.md")),
            sections: crate::lock::parse_sections(body).unwrap(),
        }
    }

    fn target_docs() -> HashMap<String, LockDoc> {
        let mut docs = HashMap::new();
        docs.insert(
            "product".into(),
            parsed_doc(Kind::Model, None, "product", ""),
        );
        docs.insert(
            "product/methods/publish".into(),
            parsed_doc(Kind::Method, None, "product/methods/publish", ""),
        );
        docs
    }

    #[test]
    fn targets_accept_crud_actions_and_method_locks() {
        let docs = target_docs();
        for ok in [
            "product.list",
            "product.create",
            "product.get",
            "product.publish",
        ] {
            assert!(check_method_target(ok, &docs).is_ok(), "{ok}");
        }
    }

    #[test]
    fn unknown_actions_are_rejected() {
        let docs = target_docs();
        let err = check_method_target("product.nonexistent", &docs).unwrap_err();
        assert!(err.contains("product.nonexistent"), "{err}");
        for bad in [
            "product",
            "product.",
            ".list",
            "product.list.x",
            "order.list",
            "product.li st",
        ] {
            assert!(check_method_target(bad, &docs).is_err(), "{bad}");
        }
    }

    #[test]
    fn validate_reports_unknown_feature_targets() {
        let mut docs = target_docs();
        let body = "## Steps\n### s1\n- op: method\n- target: product.nonexistent\n";
        docs.insert(
            "catalog/v1/x".into(),
            parsed_doc(Kind::Api, Some(Scope::Feature), "api/catalog/v1/x/x", body),
        );
        let mut report = Report::new();
        check_targets(&docs, &mut report);
        assert!(
            report
                .errors()
                .any(|d| d.message.contains("product.nonexistent")),
            "{:?}",
            report.diags
        );
    }

    #[test]
    fn malformed_calls_methods_fails_closed() {
        for list in ["product.publish", "[product.publish", "{ a: b }"] {
            let mut docs = target_docs();
            let body = format!("## Calls\n- methods: {list}\n");
            docs.insert(
                "product/methods/x".into(),
                parsed_doc(Kind::Method, None, "product/methods/x", &body),
            );
            let mut report = Report::new();
            check_targets(&docs, &mut report);
            assert!(
                report
                    .errors()
                    .any(|d| d.message.contains("Calls.methods must be a list")),
                "{list}: {:?}",
                report.diags
            );
        }
    }

    #[test]
    fn validate_reports_unknown_method_lock_targets() {
        let mut docs = target_docs();
        let body = "## Signature\n- writes: false\n## Calls\n- methods: [product.ghost]\n## Steps\n### s1 method\n- op: method\n- target: nope.get\n- out: r\n### s2 call\n- op: call\n- target: https://example.com/hook\n";
        docs.insert(
            "product/methods/caller".into(),
            parsed_doc(Kind::Method, None, "models/product/methods/caller", body),
        );
        let mut report = Report::new();
        check_targets(&docs, &mut report);
        let msgs: Vec<&str> = report.errors().map(|d| d.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("nope.get")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("product.ghost")), "{msgs:?}");
        assert!(!msgs.iter().any(|m| m.contains("example.com")), "{msgs:?}");
    }

    const WRITER: &str = "## Signature\n- writes: true\n## Steps\n### s1 query\n- op: query\n- sql: `INSERT INTO product (name) VALUES ($1)`\n- params: [input.name]\n- writes: true\n";
    const READER: &str = "## Signature\n- writes: false\n## Steps\n### s1 query\n- op: query\n- sql: `SELECT id FROM product WHERE id = $1`\n- params: [input.id]\n";

    fn wrapper(sig: &str, target: &str, step_writes: Option<&str>) -> String {
        let marker = step_writes
            .map(|w| format!("- writes: {w}\n"))
            .unwrap_or_default();
        format!("## Signature\n- writes: {sig}\n## Steps\n### s1 method\n- op: method\n- target: {target}\n- out: r\n{marker}")
    }

    fn inherited_errors(methods: &[(&str, String)]) -> Vec<String> {
        let mut docs = HashMap::new();
        let mut report = Report::new();
        for (name, body) in methods {
            let id = format!("models/product/methods/{name}");
            let d = parsed_doc(Kind::Method, None, &id, body);
            check_method(&d, "x.lock.md", &mut report);
            docs.insert(format!("product/methods/{name}"), d);
        }
        check_method_inherited_writes(&docs, &mut report);
        report.errors().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn wrapper_inherits_writes_from_called_method() {
        let ok = inherited_errors(&[
            ("save", WRITER.into()),
            ("wrap", wrapper("true", "product.save", Some("true"))),
        ]);
        assert!(ok.is_empty(), "{ok:?}");
        let unmarked_step = inherited_errors(&[
            ("save", WRITER.into()),
            ("wrap", wrapper("true", "product.save", None)),
        ]);
        assert!(unmarked_step.is_empty(), "{unmarked_step:?}");
    }

    #[test]
    fn wrapper_marked_writes_false_fails() {
        let msgs = inherited_errors(&[
            ("save", WRITER.into()),
            ("wrap", wrapper("false", "product.save", None)),
        ]);
        assert!(
            msgs.iter()
                .any(|m| m == "writes: false but a called method writes"),
            "{msgs:?}"
        );
        let msgs = inherited_errors(&[
            ("save", WRITER.into()),
            ("wrap", wrapper("true", "product.save", Some("false"))),
        ]);
        assert!(
            msgs.iter().any(|m| m.contains("marked `writes: false`")),
            "{msgs:?}"
        );
    }

    #[test]
    fn wrapper_of_read_method_cannot_claim_writes() {
        let msgs = inherited_errors(&[
            ("get", READER.into()),
            ("wrap", wrapper("true", "product.get", Some("true"))),
        ]);
        assert!(
            msgs.iter()
                .any(|m| m.contains("`product.get` does not write")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m == "writes: true but no step writes SQL"),
            "{msgs:?}"
        );
        let ok = inherited_errors(&[
            ("get", READER.into()),
            ("wrap", wrapper("false", "product.get", None)),
        ]);
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn writes_inherit_through_multiple_levels() {
        let ok = inherited_errors(&[
            ("save", WRITER.into()),
            ("mid", wrapper("true", "product.save", None)),
            ("top", wrapper("true", "product.mid", None)),
        ]);
        assert!(ok.is_empty(), "{ok:?}");
        let msgs = inherited_errors(&[
            ("save", WRITER.into()),
            ("mid", wrapper("true", "product.save", None)),
            ("top", wrapper("false", "product.mid", None)),
        ]);
        assert_eq!(
            msgs,
            vec!["writes: false but a called method writes"],
            "{msgs:?}"
        );
    }

    #[test]
    fn inherited_writes_terminate_on_cycles() {
        let msgs = inherited_errors(&[
            ("a", wrapper("false", "product.b", None)),
            ("b", wrapper("false", "product.a", None)),
        ]);
        assert!(!msgs.iter().any(|m| m.contains("writes")), "{msgs:?}");
        let mut docs = HashMap::new();
        for (name, body) in [
            ("a", wrapper("false", "product.b", None)),
            (
                "b",
                format!(
                    "{}### s2 method\n- op: method\n- target: product.save\n- out: q\n",
                    wrapper("false", "product.a", None)
                ),
            ),
            ("save", WRITER.to_string()),
        ] {
            let id = format!("models/product/methods/{name}");
            docs.insert(
                format!("product/methods/{name}"),
                parsed_doc(Kind::Method, None, &id, &body),
            );
        }
        assert!(crate::security::method_doc_writes(&docs, "product.a"));
        assert!(crate::security::known_method_writes(&docs, "product.b"));
    }

    #[test]
    fn unknown_method_targets_fail_validation_and_count_as_writes() {
        let mut docs = HashMap::new();
        docs.insert(
            "product/methods/wrap".to_string(),
            parsed_doc(
                Kind::Method,
                None,
                "models/product/methods/wrap",
                &wrapper("false", "product.missing", None),
            ),
        );
        docs.insert(
            "product".to_string(),
            parsed_doc(Kind::Model, None, "models/product", "## Fields\n"),
        );
        // Validation rejects the unknown target...
        assert!(check_method_target("product.missing", &docs).is_err());
        assert!(check_method_target("nothing.here", &docs).is_err());
        // ...and risk grading never treats it as read-only.
        for target in ["product.missing", "nothing.here", "no-dot", "product.wrap"] {
            assert!(
                crate::security::method_doc_writes(&docs, target),
                "{target} must count as a write for risk"
            );
        }
        // Read-only CRUD on an existing model is not a write; write CRUD is.
        assert!(!crate::security::method_doc_writes(&docs, "product.get"));
        assert!(!crate::security::method_doc_writes(&docs, "product.list"));
        assert!(crate::security::method_doc_writes(&docs, "product.create"));
        // Validation-side inheritance ignores unresolved targets (reported above).
        assert!(!crate::security::known_method_writes(
            &docs,
            "product.missing"
        ));
    }

    fn method_errors(body: &str) -> Vec<String> {
        let d = parsed_doc(Kind::Method, None, "models/product/methods/x", body);
        let mut report = Report::new();
        check_method(&d, "x.lock.md", &mut report);
        report.errors().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn writes_must_match_what_the_steps_do() {
        let select = "### s1 query\n- op: query\n- sql: `select id from t where id = $1`\n- params: [input.id]\n";
        let insert = "### s1 query\n- op: query\n- sql: `insert into t (a) values ($1)`\n- params: [input.a]\n";
        // A read step marked as writing.
        let msgs = method_errors(&format!(
            "## Signature\n- writes: true\n## Steps\n{select}- writes: true\n"
        ));
        assert!(
            msgs.iter()
                .any(|m| m.contains("marked `writes: true` but its SQL does not write")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m == "writes: true but no step writes SQL"),
            "{msgs:?}"
        );
        // A write step without the marker, and a Signature without writes.
        let msgs = method_errors(&format!("## Signature\n- errors: []\n## Steps\n{insert}"));
        assert!(
            msgs.iter().any(|m| m.contains("not marked `writes: true`")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("writes: false but a step writes SQL")),
            "{msgs:?}"
        );
        // Malformed or duplicate values.
        let msgs = method_errors(&format!(
            "## Signature\n- writes: yes\n## Steps\n{insert}- writes: true\n"
        ));
        assert!(
            msgs.iter().any(|m| m.contains("invalid writes `yes`")),
            "{msgs:?}"
        );
        let msgs = method_errors(&format!(
            "## Signature\n- writes: true\n## Steps\n{insert}- writes: true\n- writes: false\n"
        ));
        assert!(
            msgs.iter().any(|m| m.contains("duplicate `writes`")),
            "{msgs:?}"
        );
        // Consistent locks pass.
        let msgs = method_errors(&format!(
            "## Signature\n- writes: true\n## Steps\n{insert}- writes: true\n- scope: pk\n"
        ));
        assert!(!msgs.iter().any(|m| m.contains("writes")), "{msgs:?}");
        let msgs = method_errors(&format!(
            "## Signature\n- writes: false\n## Steps\n{select}- writes: false\n"
        ));
        assert!(!msgs.iter().any(|m| m.contains("writes")), "{msgs:?}");
    }

    #[test]
    fn method_lock_steps_get_step_grammar_checks() {
        let body = "## Signature\n- writes: false\n## Calls\n- methods: []\n## Steps\n### s1 method\n- op: method\n- method: product.get\n- out: r\n### s2 validate\n- op: validate\n- when: `r == null`\n### s3 respond\n- op: respond\n- value: r\n- status: 999\n";
        let d = parsed_doc(Kind::Method, None, "models/product/methods/x", body);
        let mut report = Report::new();
        check_method(&d, "x.lock.md", &mut report);
        let msgs: Vec<&str> = report.errors().map(|d| d.message.as_str()).collect();
        for want in [
            "unknown key `method` for op method (use `target:`)",
            "method step needs `target`",
            "validate step needs a `fail` error code",
            "status `999` is not an HTTP status",
        ] {
            assert!(msgs.contains(&want), "{want} not in {msgs:?}");
        }
    }

    #[test]
    fn prompt_examples_pass_the_validator() {
        for (kind, scope, id) in [
            (Kind::Method, None, "product/methods/create"),
            (Kind::Api, Some(Scope::Feature), "catalog/v1/list-products"),
        ] {
            let prompt = crate::compile::system_prompt(kind, scope);
            let body = prompt.split_once("LOCK BODY:\n").expect("example").1;
            let lock = parsed_doc(kind, scope, id, body);
            let names: Vec<&str> = lock.sections.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, section_order(kind, scope), "{id}");
            let mut report = Report::new();
            check_sections(&lock, id, &mut report);
            let errors: Vec<_> = report.errors().collect();
            assert!(errors.is_empty(), "{id}: {errors:?}");
            let mut docs = target_docs();
            docs.insert(id.into(), lock);
            let mut report = Report::new();
            check_targets(&docs, &mut report);
            assert!(report.errors().next().is_none(), "{id}: {:?}", report.diags);
        }
    }

    #[test]
    fn with_update_is_a_write_and_needs_where() {
        let sql = "WITH x AS (SELECT 1) UPDATE products SET price_cents = 0";
        assert!(crate::security::sql_writes(sql));
        let problems = sql_problems(sql, "[]", false);
        assert!(problems.iter().any(|p| p.contains("WHERE")), "{problems:?}");
    }

    #[test]
    fn select_into_is_ddl_outside_migrations() {
        let problems = sql_problems("SELECT * INTO tmp FROM products", "[]", false);
        assert!(problems.iter().any(|p| p.contains("DDL")), "{problems:?}");
    }

    #[test]
    fn comments_do_not_trigger_forbidden_or_writes() {
        let problems = sql_problems("SELECT 1 -- delete later", "[]", false);
        assert!(
            !problems.iter().any(|p| p.contains("DELETE")),
            "{problems:?}"
        );
        assert!(!crate::security::sql_writes(
            "SELECT note = 'update me' FROM t"
        ));
    }

    fn method_with_sql(sql: &str) -> LockDoc {
        use crate::lock::{Block, Bullet, Meta};
        let bullet = |key: &str, value: &str| Bullet {
            key: key.into(),
            value: value.into(),
        };
        LockDoc {
            meta: Meta::pending(
                Kind::Method,
                None,
                "product/zero-price",
                "models/product/methods/zero-price/zero-price.md",
            ),
            sections: vec![Section {
                name: "Steps".into(),
                bullets: vec![],
                table: None,
                blocks: vec![Block {
                    heading: "s1".into(),
                    bullets: vec![
                        bullet("op", "query"),
                        bullet("sql", sql),
                        bullet("params", "[input.id]"),
                        bullet("scope", "pk"),
                    ],
                }],
                none: false,
            }],
        }
    }

    #[test]
    fn hand_edited_tautological_update_fails_the_method_check() {
        let mut report = Report::new();
        check_method(
            &method_with_sql("UPDATE products SET price_cents = 0 WHERE true OR id = $1"),
            "zero-price.lock.md",
            &mut report,
        );
        let text = report.to_string();
        assert!(text.contains("WHERE is always true"), "{text}");
        assert!(report.has_errors());

        let mut report = Report::new();
        check_method(
            &method_with_sql("UPDATE products SET price_cents = 0 WHERE id = $1"),
            "zero-price.lock.md",
            &mut report,
        );
        assert!(!report.to_string().contains("always true"), "{report}");
    }
}

#[cfg(test)]
mod limits_value_tests {
    use super::{check_limit_value, parse_bytes, parse_size_bytes};

    #[test]
    fn accepted_limits_match_runtime_parsers() {
        for (value, expected) in [
            ("64kb", 65_536),
            ("64KB", 65_536),
            ("512b", 512),
            ("1MB", 1_048_576),
            ("1mb", 1_048_576),
            ("4KB", 4096),
            ("512", 512),
            ("1B", 1),
            ("4kB", 4096),
        ] {
            assert!(check_limit_value("body", value).is_ok(), "{value}");
            assert_eq!(parse_size_bytes(value), Some(expected));
            assert_eq!(parse_bytes(value), expected);
        }
        for (value, expected) in [
            ("1ms", 1),
            ("500ms", 500),
            ("10s", 10_000),
            ("30s", 30_000),
            ("30000ms", 30_000),
        ] {
            assert!(check_limit_value("timeout", value).is_ok(), "{value}");
            assert_eq!(crate::llm::parse_duration_ms(value), Some(expected));
        }
    }

    #[test]
    fn strict_sizes_reject_malformed_and_overflowing_values() {
        for value in [
            "",
            "+1",
            " 1kb",
            "1kb ",
            "1 kb",
            "1.5mb",
            "1gb",
            "18446744073709551616",
            "18446744073709551615kb",
        ] {
            assert_eq!(parse_size_bytes(value), None, "{value}");
        }
        for (value, expected) in [
            (" 4 KB ", 4096),
            ("+4KB", 4096),
            ("18446744073709551615MB", u64::MAX),
            ("invalid", 0),
        ] {
            assert_eq!(parse_bytes(value), expected, "{value}");
        }
    }

    #[test]
    fn strict_limits_reject_malformed_zero_and_excess_values() {
        for (key, good, bad) in [
            (
                "timeout",
                vec!["500ms", "10s", "30s"],
                vec![
                    "garbage",
                    "0s",
                    "31s",
                    "1 s",
                    "10s junk",
                    "18446744073709551615s",
                ],
            ),
            (
                "body",
                vec!["64kb", "4KB", "1MB"],
                vec![
                    "garbage",
                    "0MB",
                    "2MB",
                    "1 MB",
                    "1MB junk",
                    "18446744073709551615MB",
                ],
            ),
            (
                "provider",
                vec!["main", "db-2"],
                vec!["Main", "db_name", "-db", "db--name", "db-", ""],
            ),
        ] {
            for value in good {
                assert!(check_limit_value(key, value).is_ok(), "{key}: {value}");
            }
            for value in bad {
                let error = check_limit_value(key, value).unwrap_err();
                assert!(
                    error.contains(key) && error.contains(value) && error.contains("e.g."),
                    "{error}"
                );
            }
        }
    }
}
