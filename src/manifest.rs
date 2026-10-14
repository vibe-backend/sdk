use std::collections::HashMap;
use std::fs;
use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diag::{Diag, Report};
use crate::discover;
use crate::lock::{find_section, parse_lock, Kind, LockDoc, Scope};
use crate::security::pipeline_name;
use crate::sign;
use crate::validate::{self, ValidateOptions};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub manifest_version: u32,
    pub routes: Vec<RouteSpec>,
    pub models: Vec<ModelSpec>,
    pub methods: Vec<MethodSpec>,
    pub migrations: Vec<MigrationSpec>,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RouteSpec {
    pub method: String,
    pub path: String,
    pub feature_id: String,
    pub public: bool,
    pub auth_removed: bool,
    pub auth_mode: String,
    pub roles: Vec<String>,
    pub scopes: Vec<String>,
    /// Claim that holds roles for this route (`roles-claim`); empty means the
    /// runtime default.
    #[serde(default)]
    pub roles_claim: String,
    pub rates: Vec<RateSpec>,
    pub body_limit: u64,
    pub request: Vec<ReqField>,
    pub transform_in: String,
    pub transform_out: String,
    pub steps: Vec<StepSpec>,
    pub errors: Vec<ErrMap>,
    pub min_response_ms: u64,
    pub public_writes: bool,
    pub challenge_type: String,
    pub challenge_when: String,
    pub deprecated: bool,
    pub sunset: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RateSpec {
    pub per_minute: u32,
    pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReqField {
    pub location: String,
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ErrMap {
    pub status: u16,
    pub code: String,
    pub from: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StepSpec {
    pub id: String,
    pub op: String,
    pub target: String,
    pub args: String,
    pub value: String,
    pub status: u16,
    pub sql: String,
    pub params: Vec<String>,
    pub out: String,
    pub fail: String,
    pub when_expr: String,
    pub writes: bool,
    pub table: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub provider: String,
    pub table: String,
    pub fields: Vec<FieldSpec>,
    pub crud: Vec<String>,
    pub list: ListOpts,
    pub sensitive_output: Vec<String>,
    /// Column filled from `ctx.user.id` and used to scope non-admin reads and writes.
    #[serde(default)]
    pub owner_key: Option<String>,
    /// Explicit relaxation: `tenant-wide` or `role(admin)`.
    #[serde(default)]
    pub ownership: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldSpec {
    pub name: String,
    pub ty: String,
    pub required: bool,
    pub unique: bool,
    pub primary: bool,
    pub system: bool,
    pub sensitive: bool,
    pub nullable: bool,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ListOpts {
    pub page_size: i64,
    pub max_page_size: i64,
    pub sort: Vec<String>,
    pub filter: Vec<String>,
}

impl Default for ListOpts {
    fn default() -> Self {
        Self {
            page_size: 20,
            max_page_size: 100,
            sort: vec!["created_at".into()],
            filter: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MethodSpec {
    pub target: String,
    pub writes: bool,
    pub input: Vec<ReqField>,
    pub errors: Vec<String>,
    pub capabilities: Vec<String>,
    pub steps: Vec<StepSpec>,
    pub crud_model: String,
    pub crud_action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MigrationSpec {
    pub id: String,
    pub provider: String,
    pub seq: String,
    pub statements: Vec<String>,
}

pub fn manifest_from_project(root: &Path, signing_key: &SigningKey) -> Result<Manifest, Report> {
    let verify = signing_key.verifying_key();
    validate::validate_project(&ValidateOptions {
        root,
        deploy: true,
        verify_key: Some(&verify),
    })?;
    let docs = load_docs(root)?;
    let mut manifest = build_manifest(&docs)?;
    manifest.signature = String::new();
    let bytes =
        serde_json::to_vec(&manifest).map_err(|err| single(format!("manifest json: {err}")))?;
    manifest.signature = sign::sign_bytes(&bytes, signing_key);
    Ok(manifest)
}

pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), Report> {
    let json = serde_json::to_vec_pretty(manifest).map_err(|err| single(err.to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| single(err.to_string()))?;
    }
    fs::write(path, json).map_err(|err| single(err.to_string()))
}

pub fn read_manifest(path: &Path, key: &VerifyingKey) -> Result<Manifest, Report> {
    let bytes = fs::read(path).map_err(|err| single(format!("cannot read manifest: {err}")))?;
    let mut manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|err| single(format!("manifest json: {err}")))?;
    let signature = manifest.signature.clone();
    manifest.signature.clear();
    let canonical = serde_json::to_vec(&manifest).map_err(|err| single(err.to_string()))?;
    if !sign::verify_bytes(&canonical, &signature, key) {
        return Err(single("manifest signature is invalid".into()));
    }
    manifest.signature = signature;
    Ok(manifest)
}

pub fn build_manifest(docs: &HashMap<String, LockDoc>) -> Result<Manifest, Report> {
    let mut report = Report::new();
    let mut models = Vec::new();
    let mut methods = Vec::new();
    let mut routes = Vec::new();
    let mut migrations = Vec::new();
    let mut ids: Vec<&LockDoc> = docs.values().collect();
    ids.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
    for doc in ids {
        match doc.meta.kind {
            Kind::Model => models.push(model_from(doc)),
            Kind::Method => methods.push(method_from(doc)),
            Kind::Migration => migrations.push(migration_from(doc)),
            Kind::Api if doc.meta.scope == Some(Scope::Feature) => match route_from(doc, docs) {
                Ok(route) => routes.push(route),
                Err(diag) => report.push(diag),
            },
            _ => {}
        }
    }
    for model in &models {
        for action in &model.crud {
            let target = format!("{}.{}", model.id, action);
            if methods.iter().any(|method| method.target == target) {
                continue;
            }
            methods.push(MethodSpec {
                target,
                writes: matches!(action.as_str(), "create" | "update" | "delete"),
                input: Vec::new(),
                errors: vec![
                    "NOT_FOUND".into(),
                    "CONFLICT".into(),
                    "PRECONDITION_FAILED".into(),
                ],
                capabilities: Vec::new(),
                steps: Vec::new(),
                crud_model: model.id.clone(),
                crud_action: action.clone(),
            });
        }
    }
    if report.has_errors() {
        return Err(report);
    }
    Ok(Manifest {
        manifest_version: MANIFEST_VERSION,
        routes,
        models,
        methods,
        migrations,
        signature: String::new(),
    })
}

fn load_docs(root: &Path) -> Result<HashMap<String, LockDoc>, Report> {
    let sources = discover::discover(root)?;
    let mut docs = HashMap::new();
    let mut report = Report::new();
    for source in sources {
        let path = root.join(source.rel.trim_end_matches(".md").to_string() + ".lock.md");
        let text = fs::read_to_string(&path).map_err(|err| single(err.to_string()))?;
        match parse_lock(&text) {
            Ok(doc) => {
                docs.insert(doc.meta.id.clone(), doc);
            }
            Err(err) => report.push(Diag::error(source.rel, err)),
        }
    }
    if report.has_errors() {
        Err(report)
    } else {
        Ok(docs)
    }
}

fn model_from(doc: &LockDoc) -> ModelSpec {
    let provider = find_section(&doc.sections, "Table")
        .and_then(|s| s.get("provider"))
        .unwrap_or("")
        .to_string();
    let table = find_section(&doc.sections, "Table")
        .and_then(|s| s.get("table"))
        .unwrap_or("")
        .to_string();
    let mut fields = Vec::new();
    if let Some(section) = find_section(&doc.sections, "Fields") {
        if let Some(table) = &section.table {
            for row in &table.rows {
                if row.len() < 2 {
                    continue;
                }
                let rules = row.get(2).map(String::as_str).unwrap_or("");
                let stringy = matches!(row[1].as_str(), "string" | "password" | "bytes");
                fields.push(FieldSpec {
                    name: row[0].clone(),
                    ty: row[1].clone(),
                    required: rules.split(',').any(|part| part.trim() == "required"),
                    unique: rules.split(',').any(|part| part.trim() == "unique"),
                    primary: rules.split(',').any(|part| part.trim() == "primary"),
                    system: row.get(3).map(String::as_str) == Some("yes"),
                    sensitive: rules.contains("sensitive"),
                    nullable: rules.contains("nullable"),
                    min: number_rule(rules, "min").filter(|_| !stringy).or_else(|| {
                        if stringy {
                            None
                        } else {
                            number_rule(rules, "min")
                        }
                    }),
                    max: if stringy {
                        None
                    } else {
                        number_rule(rules, "max")
                    },
                    format: text_rule(rules, "format"),
                });
                if stringy {
                    let field = fields.last_mut().expect("just pushed");
                    field.min = number_rule(rules, "min");
                    field.max = number_rule(rules, "max");
                }
            }
        }
    }
    let table_section = find_section(&doc.sections, "Table");
    let owner_key = table_section
        .and_then(|section| section.get("owner-key"))
        .filter(|value| !value.is_empty() && *value != "(none)")
        .map(str::to_string);
    let ownership = table_section
        .and_then(|section| section.get("ownership"))
        .unwrap_or("")
        .to_string();
    let mut crud = Vec::new();
    let mut list = ListOpts::default();
    let mut sensitive_output = Vec::new();
    if let Some(section) = find_section(&doc.sections, "CRUD") {
        if let Some(table) = &section.table {
            for row in &table.rows {
                let Some(action) = row.first() else { continue };
                if action.is_empty() {
                    continue;
                }
                crud.push(action.clone());
                let options = row.get(1).map(String::as_str).unwrap_or("");
                if action == "list" {
                    list.page_size = number_rule(options, "page-size").unwrap_or(20);
                    list.max_page_size = number_rule(options, "max-page-size")
                        .unwrap_or(100)
                        .min(500);
                    list.sort = bracket_list(options, "sort");
                    list.filter = bracket_list(options, "filter");
                    if list.sort.is_empty() {
                        list.sort.push("created_at".into());
                    }
                }
                if action == "get" {
                    sensitive_output = bracket_list(options, "sensitive-output");
                }
            }
        }
    }
    ModelSpec {
        id: doc.meta.id.clone(),
        provider,
        table,
        fields,
        crud,
        list,
        sensitive_output,
        owner_key,
        ownership,
    }
}

fn method_from(doc: &LockDoc) -> MethodSpec {
    let target = doc
        .meta
        .id
        .split_once("/methods/")
        .map(|(object, method)| format!("{object}.{method}"))
        .unwrap_or_else(|| doc.meta.id.clone());
    let writes =
        find_section(&doc.sections, "Signature").and_then(|s| s.get("writes")) == Some("true");
    let errors = find_section(&doc.sections, "Signature")
        .and_then(|s| s.get("errors"))
        .and_then(|raw| crate::lock::parse_string_list(raw).ok())
        .unwrap_or_default();
    let capabilities = find_section(&doc.sections, "Signature")
        .and_then(|s| s.get("capabilities"))
        .and_then(|raw| crate::lock::parse_string_list(raw).ok())
        .unwrap_or_default();
    MethodSpec {
        target,
        writes,
        input: Vec::new(),
        errors,
        capabilities,
        steps: steps_from(find_section(&doc.sections, "Steps")),
        crud_model: String::new(),
        crud_action: String::new(),
    }
}

fn migration_from(doc: &LockDoc) -> MigrationSpec {
    let provider = find_section(&doc.sections, "Target")
        .and_then(|s| s.get("provider"))
        .unwrap_or("")
        .to_string();
    let seq = find_section(&doc.sections, "Target")
        .and_then(|s| s.get("seq"))
        .unwrap_or("")
        .to_string();
    let mut statements = Vec::new();
    if let Some(up) = find_section(&doc.sections, "Up") {
        for block in &up.blocks {
            if let Some(sql) = block.bullets.iter().find(|b| b.key == "sql") {
                statements.push(sql.value.trim_matches('`').trim().to_string());
            }
        }
    }
    MigrationSpec {
        id: doc.meta.id.clone(),
        provider,
        seq,
        statements,
    }
}

fn route_from(doc: &LockDoc, docs: &HashMap<String, LockDoc>) -> Result<RouteSpec, Diag> {
    let route = find_section(&doc.sections, "Route")
        .ok_or_else(|| Diag::error(&doc.meta.id, "missing route"))?;
    let access = find_section(&doc.sections, "Access");
    let public = access.and_then(|s| s.get("public")) == Some("true");
    let mut auth_removed = false;
    let mut auth_mode = "jwt".to_string();
    let mut roles = Vec::new();
    let mut scopes = Vec::new();
    let mut roles_claim = String::new();
    let mut rates = Vec::new();
    if let Some(pipes) = find_section(&doc.sections, "Pipelines") {
        for block in &pipes.blocks {
            let name = pipeline_name(&block.heading);
            if name == "-auth"
                || (name == "auth"
                    && block
                        .bullets
                        .iter()
                        .any(|b| b.key == "removed" && b.value == "true"))
            {
                auth_removed = true;
            }
            if name == "auth" {
                if let Some(mode) = block.bullets.iter().find(|b| b.key == "mode") {
                    auth_mode = mode.value.clone();
                }
                if let Some(raw) = block.bullets.iter().find(|b| b.key == "roles") {
                    roles = crate::lock::parse_string_list(&raw.value).unwrap_or_default();
                }
                if let Some(raw) = block.bullets.iter().find(|b| b.key == "scopes") {
                    // A malformed scopes list is a build error, never "no scopes".
                    scopes = crate::lock::parse_string_list(&raw.value).map_err(|err| {
                        Diag::error(&doc.meta.id, format!("invalid scopes: {err}"))
                            .at_section("Pipelines")
                    })?;
                }
                if let Some(raw) = block.bullets.iter().find(|b| b.key == "roles-claim") {
                    roles_claim = raw.value.trim().to_string();
                }
            }
            if name == "rate-limit" {
                let per_minute = block
                    .bullets
                    .iter()
                    .find(|b| b.key == "per-minute")
                    .and_then(|b| b.value.parse().ok())
                    .unwrap_or(60);
                let key = block
                    .bullets
                    .iter()
                    .find(|b| b.key == "key")
                    .map(|b| b.value.clone())
                    .unwrap_or_else(|| "ip".into());
                rates.push(RateSpec { per_minute, key });
            }
            for (key_name, key) in [
                ("ip-per-minute", "ip"),
                ("account-per-minute", "account"),
                ("tenant-per-minute", "tenant"),
            ] {
                if let Some(value) = block.bullets.iter().find(|b| b.key == key_name) {
                    if let Ok(per_minute) = value.value.parse() {
                        rates.push(RateSpec {
                            per_minute,
                            key: key.into(),
                        });
                    }
                }
            }
        }
    }
    let body_limit = find_section(&doc.sections, "Limits")
        .and_then(|s| s.get("body"))
        .map(validate::parse_bytes)
        .unwrap_or(1_048_576);
    let transform = find_section(&doc.sections, "Transform");
    let (challenge_type, challenge_when) =
        parse_challenge(access.and_then(|s| s.get("challenge")).unwrap_or(""));
    let min_response_ms = access
        .and_then(|s| s.get("min-response"))
        .and_then(crate::llm::parse_duration_ms)
        .unwrap_or(if public { 300 } else { 0 });
    let (deprecated, sunset) = lifecycle(doc, docs);
    Ok(RouteSpec {
        method: route.get("method").unwrap_or("GET").to_string(),
        path: route.get("path").unwrap_or("/").to_string(),
        feature_id: doc.meta.id.clone(),
        public,
        auth_removed,
        auth_mode,
        roles,
        scopes,
        roles_claim,
        rates,
        body_limit,
        request: request_fields(find_section(&doc.sections, "Request")),
        transform_in: transform
            .and_then(|s| s.get("in"))
            .unwrap_or("")
            .to_string(),
        transform_out: transform
            .and_then(|s| s.get("out"))
            .unwrap_or("result")
            .to_string(),
        steps: steps_from(find_section(&doc.sections, "Steps")),
        errors: error_rows(find_section(&doc.sections, "Errors")),
        min_response_ms,
        public_writes: access.and_then(|s| s.get("public-writes")) == Some("true"),
        challenge_type,
        challenge_when,
        deprecated,
        sunset,
    })
}

fn lifecycle(doc: &LockDoc, docs: &HashMap<String, LockDoc>) -> (bool, String) {
    let mut parts = doc.meta.id.split('/');
    let definition = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let Some(ver) = docs.get(&format!("{definition}/{version}/_version")) else {
        return (false, "(none)".into());
    };
    let Some(section) = find_section(&ver.sections, "Lifecycle") else {
        return (false, "(none)".into());
    };
    (
        section.get("deprecated") == Some("true"),
        section.get("sunset").unwrap_or("(none)").to_string(),
    )
}

fn request_fields(section: Option<&crate::lock::Section>) -> Vec<ReqField> {
    let Some(section) = section else {
        return Vec::new();
    };
    let Some(table) = &section.table else {
        return Vec::new();
    };
    table
        .rows
        .iter()
        .filter_map(|row| {
            let raw = row.first()?.as_str();
            let (location, name) = raw.split_once('.')?;
            let rules = row.get(2).map(String::as_str).unwrap_or("");
            Some(ReqField {
                location: location.into(),
                name: name.into(),
                ty: row.get(1).cloned().unwrap_or_else(|| "string".into()),
                required: row.get(3).map(String::as_str) == Some("yes"),
                min: number_rule(rules, "min"),
                max: number_rule(rules, "max"),
                format: text_rule(rules, "format"),
            })
        })
        .collect()
}

fn error_rows(section: Option<&crate::lock::Section>) -> Vec<ErrMap> {
    let Some(section) = section else {
        return Vec::new();
    };
    let Some(table) = &section.table else {
        return Vec::new();
    };
    table
        .rows
        .iter()
        .filter_map(|row| {
            Some(ErrMap {
                status: row.first()?.parse().ok()?,
                code: row.get(1)?.clone(),
                from: row.get(2).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

fn steps_from(section: Option<&crate::lock::Section>) -> Vec<StepSpec> {
    let Some(section) = section else {
        return Vec::new();
    };
    section
        .blocks
        .iter()
        .map(|block| {
            let get = |key: &str| {
                block
                    .bullets
                    .iter()
                    .find(|b| b.key == key)
                    .map(|b| b.value.clone())
                    .unwrap_or_default()
            };
            let op = get("op");
            let id = block
                .heading
                .split_whitespace()
                .next()
                .unwrap_or("s")
                .to_string();
            StepSpec {
                id,
                op: op.clone(),
                target: get("target"),
                args: get("args"),
                value: get("value"),
                status: get("status")
                    .parse()
                    .unwrap_or(if op == "respond" { 200 } else { 0 }),
                sql: get("sql").trim_matches('`').trim().to_string(),
                params: crate::lock::parse_string_list(&get("params")).unwrap_or_default(),
                out: get("out"),
                fail: get("fail"),
                when_expr: get("when"),
                writes: get("writes") == "true",
                table: get("table"),
            }
        })
        .collect()
}

fn parse_challenge(raw: &str) -> (String, String) {
    let map = crate::lock::parse_inline_map(raw).unwrap_or_default();
    let ty = map
        .iter()
        .find(|(k, _)| k == "type")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "none".into());
    let when = map
        .iter()
        .find(|(k, _)| k == "when")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "always".into());
    (ty, when)
}

fn number_rule(rules: &str, key: &str) -> Option<i64> {
    for part in rules.split(',') {
        let part = part.trim();
        let rest = part.strip_prefix(&format!("{key}:"))?.trim();
        let number: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        if let Ok(value) = number.parse() {
            return Some(value);
        }
    }
    None
}

fn text_rule(rules: &str, key: &str) -> String {
    for part in rules.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&format!("{key}:")) {
            return rest.trim().to_string();
        }
    }
    String::new()
}

fn bracket_list(options: &str, key: &str) -> Vec<String> {
    let Some(idx) = options.find(&format!("{key}:")) else {
        return Vec::new();
    };
    let rest = &options[idx + key.len() + 1..];
    let rest = rest.trim();
    if let Some(start) = rest.find('[') {
        if let Some(end) = rest[start..].find(']') {
            return crate::lock::parse_string_list(&rest[start..start + end + 1])
                .unwrap_or_default();
        }
    }
    Vec::new()
}

fn single(message: String) -> Report {
    let mut report = Report::new();
    report.push(Diag::error("manifest", message));
    report
}

pub fn model<'a>(manifest: &'a Manifest, id: &str) -> Option<&'a ModelSpec> {
    manifest.models.iter().find(|model| model.id == id)
}

pub fn method<'a>(manifest: &'a Manifest, target: &str) -> Option<&'a MethodSpec> {
    manifest
        .methods
        .iter()
        .find(|method| method.target == target)
}

pub fn json_null() -> Value {
    Value::Null
}
