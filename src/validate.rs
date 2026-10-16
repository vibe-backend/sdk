use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use regex::Regex;
use std::sync::LazyLock;

use crate::diag::{Diag, Report};
use crate::discover::{self, SourceFile};
use crate::hashutil::source_hash;
use crate::lock::{
    compiler_hash, find_section, parse_lock, section_order, Kind, LockDoc, Scope, Section,
    SourceType, LOCK_VERSION,
};
use crate::scan;
use crate::security::{self, pipeline_name};

static FORBIDDEN_SQL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(delete|truncate|grant|revoke)\b").expect("regex"));
static DDL_SQL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(create|alter|drop|comment|copy|vacuum)\b").expect("regex")
});
static PARAM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$([1-9][0-9]*)").expect("regex"));

pub struct ValidateOptions<'a> {
    pub root: &'a Path,
    pub deploy: bool,
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
    check_targets(&docs, &mut report);
    check_migrations(&docs, &mut report);
    check_feature_method_writes(&docs, &mut report);
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

fn check_lock(doc: &LockDoc, source: &SourceFile, opts: &ValidateOptions<'_>, report: &mut Report) {
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
    if opts.deploy && doc.meta.compiler.endpoint_private {
        report.push(Diag::error(
            &file,
            "locks compiled against a private endpoint cannot be deployed",
        ));
    }
    let order = section_order(doc.meta.kind, doc.meta.scope);
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
    if !doc.meta.legacy_fields.is_empty() {
        let fields: Vec<String> = doc
            .meta
            .legacy_fields
            .iter()
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
    if let Some(status) = &doc.meta.legacy_status {
        if opts.deploy && status != "approved" {
            report.push(Diag::error(
                &file,
                format!("lock has legacy `status: {status}`; it cannot be deployed until it is recompiled from a reviewed source"),
            ));
        }
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
            if crate::security::sql_writes(sql.trim_matches('`'))
                && block_val(block, "writes") != "true"
            {
                report.push(
                    Diag::error(file, "step SQL writes but is not marked `writes: true`")
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
    let writes = find_section(&doc.sections, "Signature").and_then(|section| section.get("writes"))
        == Some("true");
    let Some(steps) = find_section(&doc.sections, "Steps") else {
        report.push(Diag::error(file, "method is missing steps").at_section("Steps"));
        return;
    };
    let mut saw_write = false;
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
            for problem in api_step_problems(block, &op) {
                report.push(
                    Diag::error(file, problem)
                        .at_step(&block.heading)
                        .at_section("Steps"),
                );
            }
        }
        if op == "sign-jwt" {
            if !caps.contains("sign-token") {
                report.push(
                    Diag::error(file, "sign-jwt requires capabilities: [sign-token]")
                        .at_step(&block.heading),
                );
            }
            let alg = block_val(block, "alg");
            if !matches!(alg.as_str(), "ES256" | "RS256") {
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
            if !matches!(block_val(block, "issue-refresh").as_str(), "" | "none") {
                report.push(
                    Diag::error(
                        file,
                        "sign-jwt issue-refresh is not supported by this runtime yet",
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
        if block_val(block, "writes") == "true" {
            saw_write = true;
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
    if writes && !saw_write {
        // Also count explicit writes: true steps without detectable SQL verbs.
        let marked = steps
            .blocks
            .iter()
            .any(|b| block_val(b, "writes") == "true");
        if !marked {
            report.push(
                Diag::error(
                    file,
                    "writes: true but no step writes SQL or sets writes: true",
                )
                .at_section("Signature"),
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
                if let Some(key) = block.bullets.iter().find(|b| b.key == "key") {
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
            if !matches!(op.as_str(), "method" | "validate" | "transform" | "respond") {
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
        for key in ["in", "out"] {
            if let Some(expr) = transform.get(key) {
                if let Err(err) = crate::runtime::check_expr(expr) {
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
        if let Some(ms) = limits
            .get("timeout")
            .and_then(crate::llm::parse_duration_ms)
        {
            if ms > 30_000 {
                report.push(Diag::error(file, "timeout cannot exceed 30s").at_section("Limits"));
            }
        }
        if let Some(body) = limits.get("body") {
            if parse_bytes(body) > 1_048_576 {
                report.push(Diag::error(file, "body limit cannot exceed 1MB").at_section("Limits"));
            }
        }
    }
    if find_section(&doc.sections, "Errors").is_some_and(Section::is_empty) {
        report.push(Diag::warning(file, "feature has no Errors section").at_section("Errors"));
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

/// What the runtime would fail on for an API feature step: unknown keys
/// (e.g. `method:` instead of `target:`), a missing target, expressions it
/// cannot parse, a validate step without `when`/`fail`, a bad status.
fn api_step_problems(block: &crate::lock::Block, op: &str) -> Vec<String> {
    let allowed: &[&str] = match op {
        "method" => &["target", "args", "out"],
        "validate" => &["when", "fail"],
        "respond" => &["value", "status"],
        "transform" => &[],
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

pub fn parse_bytes(raw: &str) -> u64 {
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

    fn doc(kind: Kind, scope: Option<Scope>, id: &str, body: &str) -> LockDoc {
        LockDoc {
            meta: crate::lock::Meta::pending(kind, scope, id, format!("{id}.md")),
            sections: crate::lock::parse_sections(body).unwrap(),
        }
    }

    fn target_docs() -> HashMap<String, LockDoc> {
        let mut docs = HashMap::new();
        docs.insert("product".into(), doc(Kind::Model, None, "product", ""));
        docs.insert(
            "product/methods/publish".into(),
            doc(Kind::Method, None, "product/methods/publish", ""),
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
            doc(Kind::Api, Some(Scope::Feature), "api/catalog/v1/x/x", body),
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
                doc(Kind::Method, None, "product/methods/x", &body),
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
            doc(Kind::Method, None, "models/product/methods/caller", body),
        );
        let mut report = Report::new();
        check_targets(&docs, &mut report);
        let msgs: Vec<&str> = report.errors().map(|d| d.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("nope.get")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("product.ghost")), "{msgs:?}");
        assert!(!msgs.iter().any(|m| m.contains("example.com")), "{msgs:?}");
    }

    #[test]
    fn method_lock_steps_get_step_grammar_checks() {
        let body = "## Signature\n- writes: false\n## Calls\n- methods: []\n## Steps\n### s1 method\n- op: method\n- method: product.get\n- out: r\n### s2 validate\n- op: validate\n- when: `r == null`\n### s3 respond\n- op: respond\n- value: r\n- status: 999\n";
        let d = doc(Kind::Method, None, "models/product/methods/x", body);
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
            let lock = doc(kind, scope, id, body);
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
