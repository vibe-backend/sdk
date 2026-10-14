use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::diag::{Diag, Report};
use crate::discover::{self, SourceFile};
use crate::hashutil::source_hash;
use crate::llm::{LlmAdapter, LlmConfig, LlmRequest};
use crate::lock::{
    compiler_hash, content_hash, find_section, find_section_mut, is_structured_source, render_lock,
    Block, Bullet, CompilerStamp, Dependency, Kind, LockDoc, Meta, Scope, Section, SourceType,
    Table, COMPILER_NAME, COMPILER_VERSION, PROMPT_VERSION,
};
use crate::scan;
use crate::security::{self, pipeline_name};

pub struct CompileOptions<'a> {
    pub root: &'a Path,
    pub llm: Option<&'a dyn LlmAdapter>,
    pub llm_config: Option<&'a LlmConfig>,
    /// Compare with locks already on disk and do not write.
    pub check: bool,
}

pub struct CompileOutput {
    pub files: Vec<String>,
    pub report: Report,
}

pub async fn compile_project(opts: &CompileOptions<'_>) -> Result<CompileOutput, Report> {
    let sources = discover::discover(opts.root)?;
    let mut report = Report::new();
    let stamp = stamp_from(opts.llm_config);
    let mut docs: HashMap<String, LockDoc> = HashMap::new();
    let mut ordered: Vec<String> = Vec::new();

    let mut ranked = sources.clone();
    ranked.sort_by_key(|source| (rank(source), source.rel.clone()));

    for source in &ranked {
        let hits = scan::scan_text(&source.combined_text());
        for hit in &hits {
            let diag = if hit.fatal {
                Diag::error(&source.rel, &hit.message)
            } else {
                Diag::warning(&source.rel, &hit.message)
            };
            report.push(diag);
        }
        if hits.iter().any(|hit| hit.fatal) {
            continue;
        }
        let structured = is_structured_source(&source.combined_text());
        if !structured && opts.llm.is_none() {
            report.push(Diag::error(
                &source.rel,
                "natural-language source needs an LLM adapter (set LLM_API_KEY and LLM_MODEL)",
            ));
            continue;
        }
        if !structured {
            if let Some(config) = opts.llm_config {
                if !crate::llm::sampling_registered(&config.provider, &config.model) {
                    report.push(Diag::warning(
                        &source.rel,
                        "model is not in the capability table; sampling parameters were not sent",
                    ));
                }
            }
        }
        let sections = if structured {
            match crate::lock::parse_sections(&source.combined_text()) {
                Ok(sections) => sections,
                Err(err) => {
                    report.push(Diag::error(&source.rel, err));
                    continue;
                }
            }
        } else {
            match llm_sections(opts, source, &docs).await {
                Ok((sections, fingerprint)) => {
                    if let Some(fingerprint) = fingerprint {
                        report.push(Diag::warning(
                            &source.rel,
                            format!("openai system_fingerprint {fingerprint}"),
                        ));
                    }
                    sections
                }
                Err(diag) => {
                    report.push(diag);
                    continue;
                }
            }
        };
        let mut doc = LockDoc {
            meta: Meta::pending(source.kind, source.scope, &source.id, &source.rel),
            sections,
        };
        doc.meta.source_hash = source_hash(&source.combined_text());
        doc.meta.source_type = if structured {
            SourceType::Structured
        } else {
            SourceType::Natural
        };
        doc.meta.compiler = stamp.clone();
        doc.meta.compiler_hash = compiler_hash(&stamp);
        if let Err(diags) = normalize(&mut doc, source, &docs) {
            report.extend(diags);
            continue;
        }
        ensure_source_lines(&mut doc.sections);
        if hits.iter().any(|hit| !hit.fatal) {
            doc.meta.security_items.push(crate::lock::SecurityItem {
                item: "prompt-injection".into(),
                risk: crate::lock::Risk::Medium,
                reason: String::new(),
                approved_by: Vec::new(),
            });
        }
        ordered.push(source.id.clone());
        docs.insert(source.id.clone(), doc);
    }

    if report.has_errors() {
        return Err(report);
    }

    for id in &ordered {
        let deps = match dependencies(id, &docs) {
            Ok(deps) => deps,
            Err(diag) => {
                report.push(diag);
                continue;
            }
        };
        let doc = docs.get_mut(id).expect("doc inserted");
        doc.meta.dependencies = deps;
        let mut items = security::extract_items(doc);
        if doc
            .meta
            .security_items
            .iter()
            .any(|item| item.item == "prompt-injection")
        {
            items.push(crate::lock::SecurityItem {
                item: "prompt-injection".into(),
                risk: crate::lock::Risk::Medium,
                reason: String::new(),
                approved_by: Vec::new(),
            });
        }
        items.sort_by(|a, b| a.item.cmp(&b.item));
        items.dedup_by(|a, b| a.item == b.item);
        doc.meta.security_items = items;
        semantic_check(&docs[id], &ranked, &mut report);
    }

    if report.has_errors() {
        return Err(report);
    }

    let mut files = Vec::new();
    for id in &ordered {
        let doc = &docs[id];
        let rel = lock_rel(&doc.meta.source);
        let path = opts.root.join(&rel);
        let text = render_lock(doc);
        if opts.check {
            match fs::read_to_string(&path) {
                Ok(existing) => {
                    if let Ok(parsed) = crate::lock::parse_lock(&existing) {
                        let same_items = security::same_items(
                            &parsed.meta.security_items,
                            &doc.meta.security_items,
                        );
                        if content_hash(&parsed) != content_hash(doc)
                            || parsed.meta.compiler_hash != doc.meta.compiler_hash
                            || parsed.meta.source_hash != doc.meta.source_hash
                            || !same_items
                        {
                            report.push(Diag::error(
                                &rel,
                                "recompile does not match the lock on disk",
                            ));
                        }
                    } else {
                        report.push(Diag::error(&rel, "existing lock cannot be parsed"));
                    }
                }
                Err(_) => report.push(Diag::error(&rel, "lock is missing for determinism check")),
            }
        } else if let Err(err) = fs::write(&path, &text) {
            report.push(Diag::error(&rel, format!("cannot write lock: {err}")));
        }
        files.push(rel);
    }
    if report.has_errors() {
        return Err(report);
    }
    Ok(CompileOutput { files, report })
}

fn rank(source: &SourceFile) -> u8 {
    match (source.kind, source.scope) {
        (Kind::Provider, _) => 0,
        (Kind::Pipeline, _) => 1,
        (Kind::Migration, _) => 2,
        (Kind::Model, _) => 3,
        (Kind::Method, _) => 4,
        (Kind::Api, Some(Scope::Definition)) => 5,
        (Kind::Api, Some(Scope::Version)) => 6,
        (Kind::Api, _) => 7,
    }
}

fn stamp_from(config: Option<&LlmConfig>) -> CompilerStamp {
    match config {
        Some(config) => CompilerStamp {
            name: COMPILER_NAME.into(),
            version: COMPILER_VERSION.into(),
            model_snapshot: config.model.clone(),
            llm_provider: crate::llm::lock_provider(config),
            endpoint_host: config.endpoint_host.clone(),
            endpoint_private: config.endpoint_private,
            seed: config.seed,
            sent_params: crate::llm::sent_params(config, &config.model).to_map(),
        },
        None => CompilerStamp::deterministic(),
    }
}

async fn llm_sections(
    opts: &CompileOptions<'_>,
    source: &SourceFile,
    docs: &HashMap<String, LockDoc>,
) -> Result<(Vec<Section>, Option<String>), Diag> {
    let adapter = opts
        .llm
        .ok_or_else(|| Diag::error(&source.rel, "natural-language source needs an LLM adapter"))?;
    let config = opts
        .llm_config
        .ok_or_else(|| Diag::error(&source.rel, "natural-language source needs LLM config"))?;
    if adapter.capabilities().tools {
        return Err(Diag::error(
            &source.rel,
            "LLM adapter must not advertise tools",
        ));
    }
    let request = LlmRequest {
        system: system_prompt(source.kind, source.scope),
        input: llm_input(source, docs),
        model: config.model.clone(),
        seed: config.seed,
        timeout: config.timeout,
    };
    let response = adapter
        .compile(&request)
        .await
        .map_err(|err| Diag::error(&source.rel, err))?;
    let sections = crate::lock::parse_sections(&strip_fence(&response.text)).map_err(|err| {
        Diag::error(
            &source.rel,
            format!("LLM output is not lock markdown: {err}"),
        )
    })?;
    Ok((sections, response.system_fingerprint))
}

pub fn system_prompt(kind: Kind, scope: Option<Scope>) -> String {
    let sections = crate::lock::section_order(kind, scope).join(", ");
    format!(
        "You are {COMPILER_NAME} {COMPILER_VERSION} prompt {PROMPT_VERSION}. \
Compile the source into lock markdown sections only. \
No frontmatter, no prose, no code fences. \
Required sections in order: {sections}. \
Use `- key: value` bullets, `### sN op` steps, and markdown tables where the section needs them. \
Allowed method ops: query, validate, call, method, respond, verify-password, hash-password, random-token, hash-token, sign-jwt. \
Allowed api ops: method, validate, transform, respond. \
Method op settings: query {{ sql, params: [exprs], table, writes: true only for INSERT/UPDATE, out }} (PostgreSQL only; runs as public_reader, or public_writer when writes). \
validate {{ when, fail: ERROR_CODE }}; when is one of `X is empty`, `X is not empty`, `X == true|false|null`, `X != true|false|null`. \
verify-password {{ hash: expr, plain: expr, missing-user: dummy-hash, out }} gives true/false. \
hash-password {{ plain: expr, out }}. random-token {{ out }} gives {{ value, hash }} (32 bytes; store hash, a bytea). \
hash-token {{ value: expr, normalize: email (optional), out }}. \
sign-jwt {{ alg: ES256|RS256, key-ref: ENV_NAME, claims: {{ sub: expr, ... }}, custom-claims: [names], ttl: <=1h, out }} needs capabilities: [sign-token]; roles come from users.roles, never from claims; output {{ access_token, token_type, expires_in, expires_at }}. \
sign-jwt issue-refresh: platform-decided only when an api route has a token-refresh pipeline {{ sign-method, refresh-ttl, idle-ttl, reuse-grace, transport: body }} (public, auth removed); a token-revoke pipeline {{ scope: family|all }} needs auth. \
Expressions can read input.*, earlier step outputs, and ctx.user.sub, ctx.user.roles, ctx.user.tenant. \
SQL must be a single parameterized statement using $1, $2. \
Never emit DELETE, TRUNCATE, GRANT, REVOKE, or DDL outside a migration. \
Do not invent public access, removed auth, writes, token signing, or external calls unless the source says so. \
Every step needs `op` and `source-lines`."
    )
}

fn llm_input(source: &SourceFile, docs: &HashMap<String, LockDoc>) -> String {
    let mut deps = String::new();
    for (id, doc) in docs.iter().take(12) {
        deps.push_str(&format!("- {id} kind {}\n", doc.meta.kind.as_str()));
    }
    format!(
        "PATH: {}\nKIND: {}\nID: {}\nDEPENDENCIES:\n{}\nSOURCE:\n{}",
        source.rel,
        source.kind.as_str(),
        source.id,
        deps,
        source.combined_text()
    )
}

fn strip_fence(text: &str) -> String {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    let rest = rest.find('\n').map(|idx| &rest[idx + 1..]).unwrap_or(rest);
    rest.trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
        .to_string()
}

fn normalize(
    doc: &mut LockDoc,
    source: &SourceFile,
    docs: &HashMap<String, LockDoc>,
) -> Result<(), Vec<Diag>> {
    match source.kind {
        Kind::Provider => normalize_provider(doc, source),
        Kind::Model => normalize_model(doc, source),
        Kind::Method => normalize_method(doc),
        Kind::Api => match source.scope {
            Some(Scope::Definition) => normalize_definition(doc),
            Some(Scope::Version) => normalize_version(doc),
            _ => normalize_feature(doc, source, docs),
        },
        Kind::Pipeline => ensure_order(doc, source),
        Kind::Migration => ensure_order(doc, source),
    }
}

fn normalize_provider(doc: &mut LockDoc, source: &SourceFile) -> Result<(), Vec<Diag>> {
    ensure_order(doc, source)?;
    if let Some(isolation) = find_section_mut(&mut doc.sections, "Isolation") {
        upsert_bullet(isolation, "rls", "forced");
        if isolation.get("mode").is_none() {
            upsert_bullet(isolation, "mode", "schema");
        }
    }
    if let Some(access) = find_section_mut(&mut doc.sections, "Access") {
        if access.get("shared").is_none() {
            upsert_bullet(access, "shared", "false");
        }
        if access.get("shared") != Some("true") && access.get("allowed-endpoints").is_none() {
            upsert_bullet(access, "allowed-endpoints", "(none)");
        }
    }
    Ok(())
}

fn normalize_model(doc: &mut LockDoc, source: &SourceFile) -> Result<(), Vec<Diag>> {
    let mut diags = Vec::new();
    let fields = find_section(&doc.sections, "Fields").and_then(|section| section.table.clone());
    let Some(table) = fields else {
        diags
            .push(Diag::error(&source.rel, "model is missing a Fields table").at_section("Fields"));
        return Err(diags);
    };
    if table
        .rows
        .iter()
        .any(|row| row.first().map(String::as_str) == Some("tenant_id"))
    {
        diags.push(
            Diag::error(
                &source.rel,
                "tenant_id is platform-owned and cannot be declared",
            )
            .at_section("Fields"),
        );
    }
    let user_rows: Vec<Vec<String>> = table
        .rows
        .into_iter()
        .filter(|row| {
            !matches!(
                row.first().map(String::as_str),
                Some("id")
                    | Some("created_at")
                    | Some("updated_at")
                    | Some("deleted_at")
                    | Some("erased_at")
                    | Some("tenant_id")
            )
        })
        .map(pad_field_row)
        .collect();
    for row in &user_rows {
        if !known_type(&row[1]) {
            diags.push(
                Diag::error(&source.rel, format!("unknown field type {}", row[1]))
                    .at_section("Fields"),
            );
        }
    }
    let owner_key = find_section(&doc.sections, "Table")
        .and_then(|section| section.get("owner-key"))
        .filter(|value| !value.is_empty() && *value != "(none)")
        .map(str::to_string);
    if let Some(key) = &owner_key {
        if key != "id"
            && !user_rows
                .iter()
                .any(|row| row.first().map(String::as_str) == Some(key.as_str()))
        {
            diags.push(
                Diag::error(&source.rel, format!("owner-key {key} is not a field"))
                    .at_section("Table"),
            );
        }
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    let user_rows: Vec<Vec<String>> = user_rows
        .into_iter()
        .map(|mut row| {
            if owner_key.as_deref() == Some(row[0].as_str()) {
                row[3] = "yes".into();
            }
            row
        })
        .collect();
    let mut rows = vec![field_row("id", "uuid", "primary, default: gen", "yes")];
    rows.extend(user_rows);
    rows.push(field_row("created_at", "timestamp", "default: now", "yes"));
    rows.push(field_row("updated_at", "timestamp", "default: now", "yes"));
    rows.push(field_row("deleted_at", "timestamp", "nullable", "yes"));
    rows.push(field_row("erased_at", "timestamp", "nullable", "yes"));
    rows.push(field_row("tenant_id", "string", "platform", "yes"));
    if let Some(section) = find_section_mut(&mut doc.sections, "Fields") {
        section.table = Some(Table {
            headers: vec![
                "name".into(),
                "type".into(),
                "rules".into(),
                "system".into(),
            ],
            rows,
        });
    }
    if find_section(&doc.sections, "CRUD").is_none() {
        doc.sections.push(Section::none("CRUD"));
    }
    let policies = Section {
        name: "Policies".into(),
        bullets: vec![
            Bullet {
                key: "rls".into(),
                value: "enable, force".into(),
            },
            Bullet {
                key: "policy".into(),
                value: "tenant_id = current_setting('app.tenant_id', false)".into(),
            },
            Bullet {
                key: "grants".into(),
                value: "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer []"
                    .into(),
            },
        ],
        table: None,
        blocks: Vec::new(),
        none: false,
    };
    if let Some(section) = find_section_mut(&mut doc.sections, "Policies") {
        *section = policies;
    } else {
        doc.sections.push(policies);
    }
    reorder(doc, source)
}

fn normalize_method(doc: &mut LockDoc) -> Result<(), Vec<Diag>> {
    let calls = derived_calls(find_section(&doc.sections, "Steps"));
    if let Some(section) = find_section_mut(&mut doc.sections, "Calls") {
        *section = calls;
    } else {
        doc.sections.push(calls);
    }
    let order = crate::lock::section_order(Kind::Method, None);
    doc.sections = reorder_named(&doc.sections, order);
    Ok(())
}

fn derived_calls(steps: Option<&Section>) -> Section {
    let mut methods = Vec::new();
    let mut external = Vec::new();
    if let Some(steps) = steps {
        for block in &steps.blocks {
            let op = bullet_in(block, "op");
            if op == "method" {
                if let Some(target) = block.bullets.iter().find(|b| b.key == "target") {
                    methods.push(target.value.clone());
                }
            }
            if op == "call" {
                if let Some(target) = block.bullets.iter().find(|b| b.key == "target") {
                    external.push(target.value.clone());
                }
            }
        }
    }
    Section {
        name: "Calls".into(),
        bullets: vec![
            Bullet {
                key: "methods".into(),
                value: render_inline_list(&methods),
            },
            Bullet {
                key: "external".into(),
                value: if external.is_empty() {
                    "(none)".into()
                } else {
                    render_inline_list(&external)
                },
            },
        ],
        table: None,
        blocks: Vec::new(),
        none: false,
    }
}

fn normalize_definition(doc: &mut LockDoc) -> Result<(), Vec<Diag>> {
    if find_section(&doc.sections, "Defaults").is_none() {
        doc.sections.push(defaults_section("10s", "1MB"));
    }
    doc.sections = reorder_named(
        &doc.sections,
        crate::lock::section_order(Kind::Api, Some(Scope::Definition)),
    );
    Ok(())
}

fn normalize_version(doc: &mut LockDoc) -> Result<(), Vec<Diag>> {
    if find_section(&doc.sections, "Pipelines").is_none() {
        doc.sections.push(Section::none("Pipelines"));
    }
    if find_section(&doc.sections, "Defaults").is_none() {
        doc.sections.push(Section::none("Defaults"));
    }
    if find_section(&doc.sections, "Lifecycle").is_none() {
        doc.sections.push(Section {
            name: "Lifecycle".into(),
            bullets: vec![
                Bullet {
                    key: "deprecated".into(),
                    value: "false".into(),
                },
                Bullet {
                    key: "sunset".into(),
                    value: "(none)".into(),
                },
            ],
            table: None,
            blocks: Vec::new(),
            none: false,
        });
    }
    doc.sections = reorder_named(
        &doc.sections,
        crate::lock::section_order(Kind::Api, Some(Scope::Version)),
    );
    Ok(())
}

fn normalize_feature(
    doc: &mut LockDoc,
    source: &SourceFile,
    docs: &HashMap<String, LockDoc>,
) -> Result<(), Vec<Diag>> {
    let mut parts = source.id.split('/');
    let definition = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let def_id = format!("{definition}/_definition");
    let ver_id = format!("{definition}/{version}/_version");
    let mut diags = Vec::new();
    let def = docs.get(&def_id);
    let ver = docs.get(&ver_id);
    if def.is_none() {
        diags.push(Diag::error(&source.rel, format!("missing {def_id}")));
    }
    if ver.is_none() {
        diags.push(Diag::error(&source.rel, format!("missing {ver_id}")));
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    let own = find_section(&doc.sections, "Pipelines").cloned();
    let merged = merge_pipelines(
        def.and_then(|doc| find_section(&doc.sections, "Pipelines")),
        ver.and_then(|doc| find_section(&doc.sections, "Pipelines")),
        own.as_ref(),
    );
    if let Some(section) = find_section_mut(&mut doc.sections, "Pipelines") {
        *section = merged;
    } else {
        doc.sections.push(merged);
    }
    if find_section(&doc.sections, "Access").is_none() {
        doc.sections.push(Section {
            name: "Access".into(),
            bullets: vec![Bullet {
                key: "public".into(),
                value: "false".into(),
            }],
            table: None,
            blocks: Vec::new(),
            none: false,
        });
    }
    if let Some(access) = find_section_mut(&mut doc.sections, "Access") {
        if access.get("public").is_none() {
            upsert_bullet(access, "public", "false");
        }
        if access.get("public") == Some("true") && access.get("min-response").is_none() {
            let login = source.id.ends_with("/login");
            upsert_bullet(
                access,
                "min-response",
                if login { "500ms" } else { "300ms" },
            );
        }
        if access.get("external-calls").is_none() {
            upsert_bullet(access, "external-calls", "forbidden");
        }
        if access.get("challenge").is_none() {
            upsert_bullet(access, "challenge", "{ type: none, when: always }");
        }
    }
    let timeout = inherited_default(def, ver, "timeout").unwrap_or_else(|| "10s".into());
    let body = inherited_default(def, ver, "body").unwrap_or_else(|| "1MB".into());
    if let Some(limits) = find_section_mut(&mut doc.sections, "Limits") {
        if limits.get("timeout").is_none() {
            upsert_bullet(limits, "timeout", &timeout);
        }
        if limits.get("body").is_none() {
            upsert_bullet(limits, "body", &body);
        }
    }
    if find_section(&doc.sections, "Errors").is_none() {
        doc.sections.push(Section::none("Errors"));
    }
    if find_section(&doc.sections, "Limits").is_none() {
        let provider = def
            .and_then(|doc| find_section(&doc.sections, "Providers"))
            .and_then(|section| section.get("default"))
            .unwrap_or("main");
        doc.sections.push(Section {
            name: "Limits".into(),
            bullets: vec![
                Bullet {
                    key: "provider".into(),
                    value: provider.into(),
                },
                Bullet {
                    key: "timeout".into(),
                    value: timeout,
                },
                Bullet {
                    key: "body".into(),
                    value: body,
                },
            ],
            table: None,
            blocks: Vec::new(),
            none: false,
        });
    }
    let order = crate::lock::section_order(Kind::Api, Some(Scope::Feature));
    for name in order {
        if find_section(&doc.sections, name).is_none() {
            diags.push(
                Diag::error(&source.rel, format!("missing section {name}")).at_section(*name),
            );
        }
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    doc.sections = reorder_named(&doc.sections, order);
    Ok(())
}

fn inherited_default(def: Option<&LockDoc>, ver: Option<&LockDoc>, key: &str) -> Option<String> {
    let from = |doc: Option<&LockDoc>| {
        doc.and_then(|doc| find_section(&doc.sections, "Defaults"))
            .and_then(|section| section.get(key).map(str::to_string))
    };
    from(ver).or_else(|| from(def))
}

fn merge_pipelines(def: Option<&Section>, ver: Option<&Section>, own: Option<&Section>) -> Section {
    let mut blocks: Vec<Block> = Vec::new();
    let mut removed_auth = false;
    for source in [def, ver, own].into_iter().flatten() {
        if source.none {
            continue;
        }
        for block in &source.blocks {
            let name = pipeline_name(&block.heading);
            if name.starts_with('-') {
                let real = name.trim_start_matches('-').to_string();
                if real == "auth" {
                    removed_auth = true;
                }
                blocks.retain(|existing| pipeline_name(&existing.heading) != real);
                blocks.push(block.clone());
                continue;
            }
            // Rate limits add up: every rate-limit block is its own limit.
            if name == "rate-limit" {
                blocks.push(block.clone());
                continue;
            }
            if let Some(existing) = blocks
                .iter_mut()
                .find(|existing| pipeline_name(&existing.heading) == name)
            {
                *existing = block.clone();
            } else {
                blocks.push(block.clone());
            }
        }
    }
    let has_auth = blocks
        .iter()
        .any(|block| pipeline_name(&block.heading) == "auth");
    if !has_auth && !removed_auth {
        blocks.insert(
            0,
            Block {
                heading: "auth".into(),
                bullets: vec![Bullet {
                    key: "mode".into(),
                    value: "jwt".into(),
                }],
            },
        );
    }
    for (index, block) in blocks.iter_mut().enumerate() {
        let name = pipeline_name(&block.heading);
        block.heading = format!("p{} {name}", index + 1);
    }
    let none = blocks.is_empty();
    Section {
        name: "Pipelines".into(),
        bullets: Vec::new(),
        table: None,
        blocks,
        none,
    }
}

fn ensure_order(doc: &mut LockDoc, source: &SourceFile) -> Result<(), Vec<Diag>> {
    reorder(doc, source)
}

fn reorder(doc: &mut LockDoc, source: &SourceFile) -> Result<(), Vec<Diag>> {
    let order = crate::lock::section_order(source.kind, source.scope);
    for name in order {
        if find_section(&doc.sections, name).is_none() {
            return Err(vec![Diag::error(
                &source.rel,
                format!("missing section {name}"),
            )
            .at_section(*name)]);
        }
    }
    doc.sections = reorder_named(&doc.sections, order);
    Ok(())
}

fn reorder_named(sections: &[Section], order: &[&str]) -> Vec<Section> {
    let mut out = Vec::new();
    for name in order {
        if let Some(section) = sections.iter().find(|section| section.name == *name) {
            out.push(section.clone());
        }
    }
    out
}

fn ensure_source_lines(sections: &mut [Section]) {
    for section in sections {
        for block in &mut section.blocks {
            if !block.bullets.iter().any(|b| b.key == "source-lines") {
                block.bullets.push(Bullet {
                    key: "source-lines".into(),
                    value: "1-1".into(),
                });
            }
        }
    }
}

fn dependencies(id: &str, docs: &HashMap<String, LockDoc>) -> Result<Vec<Dependency>, Diag> {
    let doc = docs.get(id).expect("doc");
    let mut ids: Vec<(String, Kind)> = Vec::new();
    match doc.meta.kind {
        Kind::Provider | Kind::Pipeline => {}
        Kind::Migration => {
            if let Some(provider) =
                find_section(&doc.sections, "Target").and_then(|s| s.get("provider"))
            {
                ids.push((provider.to_string(), Kind::Provider));
            }
        }
        Kind::Model => {
            if let Some(provider) =
                find_section(&doc.sections, "Table").and_then(|s| s.get("provider"))
            {
                ids.push((provider.to_string(), Kind::Provider));
            }
        }
        Kind::Method => {
            if let Some(object) = id.split('/').next() {
                ids.push((object.to_string(), Kind::Model));
            }
            if let Some(calls) = find_section(&doc.sections, "Calls") {
                if let Some(list) = calls.get("methods") {
                    for target in parse_name_list(list) {
                        if let Some(dep) = method_id(&target) {
                            ids.push((dep, Kind::Method));
                        }
                    }
                }
            }
        }
        Kind::Api => match doc.meta.scope {
            Some(Scope::Definition) => {
                if let Some(providers) = find_section(&doc.sections, "Providers") {
                    if let Some(list) = providers.get("allowlist") {
                        for name in parse_name_list(list) {
                            ids.push((name, Kind::Provider));
                        }
                    }
                }
            }
            Some(Scope::Version) => {
                if let Some((definition, _)) = id.split_once('/') {
                    ids.push((format!("{definition}/_definition"), Kind::Api));
                }
            }
            _ => {
                let mut parts = id.split('/');
                let definition = parts.next().unwrap_or("");
                let version = parts.next().unwrap_or("");
                ids.push((format!("{definition}/_definition"), Kind::Api));
                ids.push((format!("{definition}/{version}/_version"), Kind::Api));
                if let Some(limits) = find_section(&doc.sections, "Limits") {
                    if let Some(provider) = limits.get("provider") {
                        ids.push((provider.to_string(), Kind::Provider));
                    }
                }
                if let Some(steps) = find_section(&doc.sections, "Steps") {
                    for block in &steps.blocks {
                        if let Some(target) = block.bullets.iter().find(|b| b.key == "target") {
                            if let Some(dep) = method_id(&target.value) {
                                if docs.contains_key(&dep) {
                                    ids.push((dep, Kind::Method));
                                }
                            }
                            if let Some(object) = target.value.split('.').next() {
                                if docs.contains_key(object) {
                                    ids.push((object.to_string(), Kind::Model));
                                }
                            }
                        }
                    }
                }
            }
        },
    }
    let mut deps = Vec::new();
    for (dep_id, kind) in ids {
        let Some(dep) = docs.get(&dep_id) else {
            return Err(Diag::error(
                &doc.meta.source,
                format!("missing dependency {dep_id}"),
            ));
        };
        if dep.meta.kind != kind {
            return Err(Diag::error(
                &doc.meta.source,
                format!("dependency {dep_id} has the wrong kind"),
            ));
        }
        deps.push(Dependency {
            id: dep_id,
            kind,
            hash: content_hash(dep),
        });
    }
    deps.sort_by(|a, b| a.id.cmp(&b.id));
    deps.dedup_by(|a, b| a.id == b.id);
    Ok(deps)
}

fn method_id(target: &str) -> Option<String> {
    let (object, method) = target.split_once('.')?;
    if method.contains('.') {
        return None;
    }
    Some(format!("{object}/methods/{method}"))
}

fn semantic_check(doc: &LockDoc, sources: &[SourceFile], report: &mut Report) {
    let Some(source) = sources.iter().find(|source| source.id == doc.meta.id) else {
        return;
    };
    let text = source.combined_text().to_lowercase();
    let mut require = |needle: &[&str], item: &str| {
        if doc.meta.security_items.iter().any(|sec| sec.item == item)
            && !needle.iter().any(|word| text.contains(word))
        {
            report.push(Diag::error(
                &doc.meta.source,
                format!("lock declares {item} but the source does not mention it"),
            ));
        }
    };
    require(&["public", "公開"], "public");
    require(
        &[
            "-auth",
            "唔使 auth",
            "no auth",
            "without auth",
            "public",
            "公開",
        ],
        "auth-removed",
    );
    require(
        &[
            "write", "寫", "insert", "update", "create", "delete", "save",
        ],
        "write-sql",
    );
    require(&["sign", "簽", "token"], "sign-token");
    require(&["call", "http", "對外", "外部"], "external-call");
    require(&["password", "密碼"], "password-write");
    require(&["write", "寫", "public", "公開"], "public-writes");
    require(&["ownership", "tenant-wide", "owner"], "ownership");
}

fn lock_rel(source_rel: &str) -> String {
    source_rel.trim_end_matches(".md").to_string() + ".lock.md"
}

fn upsert_bullet(section: &mut Section, key: &str, value: &str) {
    if let Some(bullet) = section.bullets.iter_mut().find(|bullet| bullet.key == key) {
        bullet.value = value.into();
    } else {
        section.bullets.push(Bullet {
            key: key.into(),
            value: value.into(),
        });
    }
}

fn bullet_in(block: &Block, key: &str) -> String {
    block
        .bullets
        .iter()
        .find(|bullet| bullet.key == key)
        .map(|bullet| bullet.value.clone())
        .unwrap_or_default()
}

fn pad_field_row(row: Vec<String>) -> Vec<String> {
    let mut row = row;
    while row.len() < 4 {
        row.push(String::new());
    }
    row.truncate(4);
    row
}

fn field_row(name: &str, ty: &str, rules: &str, system: &str) -> Vec<String> {
    vec![name.into(), ty.into(), rules.into(), system.into()]
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

fn defaults_section(timeout: &str, body: &str) -> Section {
    Section {
        name: "Defaults".into(),
        bullets: vec![
            Bullet {
                key: "timeout".into(),
                value: timeout.into(),
            },
            Bullet {
                key: "body".into(),
                value: body.into(),
            },
        ],
        table: None,
        blocks: Vec::new(),
        none: false,
    }
}

fn render_inline_list(items: &[String]) -> String {
    if items.is_empty() {
        "[]".into()
    } else {
        format!("[{}]", items.join(", "))
    }
}

fn parse_name_list(raw: &str) -> Vec<String> {
    crate::lock::parse_string_list(raw).unwrap_or_default()
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    fn block(heading: &str, bullets: &[(&str, &str)]) -> Block {
        Block {
            heading: heading.into(),
            bullets: bullets
                .iter()
                .map(|(k, v)| Bullet {
                    key: (*k).into(),
                    value: (*v).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn every_rate_limit_block_is_kept() {
        let own = Section {
            name: "Pipelines".into(),
            bullets: Vec::new(),
            table: None,
            blocks: vec![
                block("p1 login-guard", &[("ip-per-minute", "10")]),
                block("p2 rate-limit", &[("per-minute", "5"), ("key", "ip")]),
                block("p3 rate-limit", &[("per-minute", "300"), ("key", "tenant")]),
            ],
            none: false,
        };
        let merged = merge_pipelines(None, None, Some(&own));
        let rates: Vec<_> = merged
            .blocks
            .iter()
            .filter(|b| pipeline_name(&b.heading) == "rate-limit")
            .collect();
        assert_eq!(rates.len(), 2, "{:?}", merged.blocks);
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    #[test]
    fn stamp_records_the_responses_style_and_its_params() {
        let mut config = LlmConfig {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: "sk-test-value".into(),
            model: "gpt-5.1-2026-01-01".into(),
            timeout: std::time::Duration::from_secs(5),
            max_retries: 0,
            seed: 7,
            endpoint_host: "api.openai.com".into(),
            endpoint_private: false,
            api_style: "responses".into(),
        };
        let stamp = stamp_from(Some(&config));
        assert_eq!(stamp.llm_provider, "openai-responses");
        assert!(!stamp.sent_params.contains_key("seed"));
        assert_eq!(
            stamp
                .sent_params
                .get("reasoning_effort")
                .map(String::as_str),
            Some("none")
        );
        config.api_style = "chat".into();
        let stamp = stamp_from(Some(&config));
        assert_eq!(stamp.llm_provider, "openai");
        assert_eq!(stamp.sent_params.get("seed").map(String::as_str), Some("7"));
    }
}
