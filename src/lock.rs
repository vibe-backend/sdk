use crate::hashutil::sha256_prefixed;
use serde::{Deserialize, Serialize};

pub const COMPILER_NAME: &str = "md-logic";
pub const COMPILER_VERSION: &str = "0.1.0";
pub const PROMPT_VERSION: &str = "prompt-1";
pub const LOCK_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Provider,
    Pipeline,
    Model,
    Method,
    Migration,
    Api,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Pipeline => "pipeline",
            Self::Model => "model",
            Self::Method => "method",
            Self::Migration => "migration",
            Self::Api => "api",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "provider" => Self::Provider,
            "pipeline" => Self::Pipeline,
            "model" => Self::Model,
            "method" => Self::Method,
            "migration" => Self::Migration,
            "api" => Self::Api,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Definition,
    Version,
    Feature,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Version => "version",
            Self::Feature => "feature",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "definition" => Self::Definition,
            "version" => Self::Version,
            "feature" => Self::Feature,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceType {
    Natural,
    Structured,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Structured => "structured",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "natural" => Self::Natural,
            "structured" => Self::Structured,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Approved,
    Rejected,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalMode {
    Manual,
    Auto,
}

impl ApprovalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "manual" => Self::Manual,
            "auto" => Self::Auto,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    High,
    Medium,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "high" => Self::High,
            "medium" => Self::Medium,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    pub kind: Kind,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerStamp {
    pub name: String,
    pub version: String,
    pub model_snapshot: String,
    pub llm_provider: String,
    pub endpoint_host: String,
    pub endpoint_private: bool,
    pub seed: u64,
}

impl CompilerStamp {
    pub fn deterministic() -> Self {
        Self {
            name: COMPILER_NAME.into(),
            version: COMPILER_VERSION.into(),
            model_snapshot: "deterministic".into(),
            llm_provider: "none".into(),
            endpoint_host: "none".into(),
            endpoint_private: false,
            seed: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityItem {
    pub item: String,
    pub risk: Risk,
    pub reason: String,
    pub approved_by: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub lock_version: u32,
    pub kind: Kind,
    pub scope: Option<Scope>,
    pub id: String,
    pub source: String,
    pub source_hash: String,
    pub source_type: SourceType,
    pub dependencies: Vec<Dependency>,
    pub compiler: CompilerStamp,
    pub compiler_hash: String,
    pub status: Status,
    pub approval_mode: ApprovalMode,
    pub approved_by: Vec<String>,
    pub approved_at: String,
    pub approval_ref: String,
    pub approved_commit: String,
    pub security_items: Vec<SecurityItem>,
    pub signature: String,
}

impl Meta {
    pub fn pending(
        kind: Kind,
        scope: Option<Scope>,
        id: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            lock_version: LOCK_VERSION,
            kind,
            scope,
            id: id.into(),
            source: source.into(),
            source_hash: String::new(),
            source_type: SourceType::Structured,
            dependencies: Vec::new(),
            compiler: CompilerStamp::deterministic(),
            compiler_hash: String::new(),
            status: Status::Pending,
            approval_mode: ApprovalMode::Manual,
            approved_by: Vec::new(),
            approved_at: "(none)".into(),
            approval_ref: "(none)".into(),
            approved_commit: "(none)".into(),
            security_items: Vec::new(),
            signature: "(none)".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bullet {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub heading: String,
    pub bullets: Vec<Bullet>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub bullets: Vec<Bullet>,
    pub table: Option<Table>,
    pub blocks: Vec<Block>,
    pub none: bool,
}

impl Section {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            bullets: Vec::new(),
            table: None,
            blocks: Vec::new(),
            none: false,
        }
    }

    pub fn none(name: impl Into<String>) -> Self {
        let mut section = Self::new(name);
        section.none = true;
        section
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.bullets
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.value.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.none || (self.bullets.is_empty() && self.table.is_none() && self.blocks.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockDoc {
    pub meta: Meta,
    pub sections: Vec<Section>,
}

pub fn section_order(kind: Kind, scope: Option<Scope>) -> &'static [&'static str] {
    match kind {
        Kind::Provider => &["Connection", "Isolation", "Access", "Limits", "Migrations"],
        Kind::Pipeline => &["Parameters", "Order", "Steps"],
        Kind::Model => &["Table", "Fields", "CRUD", "Policies"],
        Kind::Method => &["Signature", "Calls", "Steps"],
        Kind::Migration => &["Target", "Up", "Verify", "Data"],
        Kind::Api => match scope {
            Some(Scope::Definition) => &["Providers", "Pipelines", "Defaults"],
            Some(Scope::Version) => &["Pipelines", "Defaults", "Lifecycle"],
            Some(Scope::Feature) | None => &[
                "Route",
                "Access",
                "Pipelines",
                "Limits",
                "Request",
                "Response",
                "Transform",
                "Steps",
                "Errors",
            ],
        },
    }
}

pub fn compiler_hash(stamp: &CompilerStamp) -> String {
    let raw = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        stamp.llm_provider,
        stamp.endpoint_host,
        stamp.endpoint_private,
        stamp.model_snapshot,
        PROMPT_VERSION,
        stamp.name,
        stamp.version,
        stamp.seed
    );
    sha256_prefixed(raw.as_bytes())
}

/// Hash of the definition a dependent should pin.
///
/// Approval metadata and the signature are excluded so a later approval does
/// not force every dependent to be rewritten.
pub fn content_hash(doc: &LockDoc) -> String {
    let raw = format!(
        "{}\n{}\n{}\n{}",
        doc.meta.kind.as_str(),
        doc.meta.id,
        doc.meta.source_hash,
        render_sections(&doc.sections)
    );
    sha256_prefixed(raw.as_bytes())
}

pub fn render_lock(doc: &LockDoc) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&render_meta(&doc.meta));
    out.push_str("---\n");
    let body = render_sections(&doc.sections);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn render_meta(meta: &Meta) -> String {
    let mut out = String::new();
    push_line(&mut out, "lock-version", &meta.lock_version.to_string());
    push_line(&mut out, "kind", meta.kind.as_str());
    if let Some(scope) = meta.scope {
        push_line(&mut out, "scope", scope.as_str());
    }
    push_line(&mut out, "id", &meta.id);
    push_line(&mut out, "source", &meta.source);
    push_line(&mut out, "source-hash", &meta.source_hash);
    push_line(&mut out, "source-type", meta.source_type.as_str());
    if meta.dependencies.is_empty() {
        push_line(&mut out, "dependencies", "[]");
    } else {
        out.push_str("dependencies:\n");
        let mut deps = meta.dependencies.clone();
        deps.sort_by(|a, b| a.id.cmp(&b.id));
        for dep in deps {
            out.push_str(&format!(
                "  - {{ id: {}, kind: {}, hash: {} }}\n",
                dep.id,
                dep.kind.as_str(),
                dep.hash
            ));
        }
    }
    out.push_str(&format!(
        "compiler: {{ name: {}, version: {}, model-snapshot: {}, llm-provider: {}, endpoint-host: {}, endpoint-private: {}, seed: {} }}\n",
        meta.compiler.name,
        meta.compiler.version,
        meta.compiler.model_snapshot,
        meta.compiler.llm_provider,
        meta.compiler.endpoint_host,
        meta.compiler.endpoint_private,
        meta.compiler.seed
    ));
    push_line(&mut out, "compiler-hash", &meta.compiler_hash);
    push_line(&mut out, "status", meta.status.as_str());
    push_line(&mut out, "approval-mode", meta.approval_mode.as_str());
    push_line(&mut out, "approved-by", &render_list(&meta.approved_by));
    push_line(&mut out, "approved-at", &meta.approved_at);
    push_line(&mut out, "approval-ref", &meta.approval_ref);
    push_line(&mut out, "approved-commit", &meta.approved_commit);
    if meta.security_items.is_empty() {
        push_line(&mut out, "security-items", "[]");
    } else {
        out.push_str("security-items:\n");
        for item in &meta.security_items {
            out.push_str(&format!(
                "  - {{ item: {}, risk: {}, reason: {}, approved-by: {} }}\n",
                item.item,
                item.risk.as_str(),
                quote_inline(&item.reason),
                render_list(&item.approved_by)
            ));
        }
    }
    push_line(&mut out, "signature", &meta.signature);
    out
}

fn push_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(value);
    out.push('\n');
}

fn render_list(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    format!("[{}]", items.join(", "))
}

fn quote_inline(value: &str) -> String {
    if value.is_empty() {
        "\"\"".into()
    } else if value
        .chars()
        .any(|c| c.is_whitespace() || c == ',' || c == ':' || c == '"' || c == '{' || c == '}')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.into()
    }
}

pub fn render_sections(sections: &[Section]) -> String {
    let mut out = String::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str("## ");
        out.push_str(&section.name);
        out.push('\n');
        if section.is_empty() {
            out.push_str("(none)\n");
            continue;
        }
        for bullet in &section.bullets {
            out.push_str("- ");
            out.push_str(&bullet.key);
            out.push_str(": ");
            out.push_str(&bullet.value);
            out.push('\n');
        }
        if let Some(table) = &section.table {
            if !section.bullets.is_empty() {
                out.push('\n');
            }
            out.push_str(&render_table(table));
        }
        for block in &section.blocks {
            out.push('\n');
            out.push_str("### ");
            out.push_str(&block.heading);
            out.push('\n');
            for bullet in &block.bullets {
                out.push_str("- ");
                out.push_str(&bullet.key);
                out.push_str(": ");
                out.push_str(&bullet.value);
                out.push('\n');
            }
        }
    }
    out
}

fn render_table(table: &Table) -> String {
    let mut out = String::new();
    out.push_str(&render_row(&table.headers));
    let sep: Vec<String> = table.headers.iter().map(|_| "---".into()).collect();
    out.push_str(&render_row(&sep));
    for row in &table.rows {
        out.push_str(&render_row(row));
    }
    out
}

fn render_row(cells: &[String]) -> String {
    let mut out = String::from("|");
    for cell in cells {
        out.push(' ');
        out.push_str(cell);
        out.push_str(" |");
    }
    out.push('\n');
    out
}

pub fn parse_lock(text: &str) -> Result<LockDoc, String> {
    let (front, body) = split_frontmatter(text)?;
    let meta = parse_meta(&front)?;
    let sections = parse_sections(body)?;
    Ok(LockDoc { meta, sections })
}

pub fn parse_sections(text: &str) -> Result<Vec<Section>, String> {
    let text = strip_frontmatter_block(text);
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    let mut in_block = false;
    let mut table_lines: Vec<String> = Vec::new();

    let flush_table = |section: &mut Section, lines: &mut Vec<String>| -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        section.table = Some(parse_table(lines)?);
        lines.clear();
        Ok(())
    };

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("### ") {
            if let Some(section) = current.as_mut() {
                flush_table(section, &mut table_lines)?;
                section.blocks.push(Block {
                    heading: trimmed.trim_start_matches("### ").trim().to_string(),
                    bullets: Vec::new(),
                });
                in_block = true;
            } else {
                return Err(format!("line {line_no}: block outside a section"));
            }
            continue;
        }
        if trimmed.starts_with("## ") {
            if let Some(mut section) = current.take() {
                flush_table(&mut section, &mut table_lines)?;
                sections.push(section);
            }
            current = Some(Section::new(trimmed.trim_start_matches("## ").trim()));
            in_block = false;
            continue;
        }
        if trimmed.starts_with("# ") {
            continue;
        }
        let Some(section) = current.as_mut() else {
            return Err(format!(
                "line {line_no}: content outside a section: {trimmed}"
            ));
        };
        if trimmed == "(none)" {
            section.none = true;
            continue;
        }
        if trimmed.starts_with('|') {
            table_lines.push(trimmed.to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            flush_table(section, &mut table_lines)?;
            let (key, value) = split_bullet(rest)
                .ok_or_else(|| format!("line {line_no}: bullet must be `- key: value`"))?;
            let bullet = Bullet { key, value };
            if in_block {
                section
                    .blocks
                    .last_mut()
                    .ok_or_else(|| format!("line {line_no}: bullet without a block"))?
                    .bullets
                    .push(bullet);
            } else {
                section.bullets.push(bullet);
            }
            continue;
        }
        return Err(format!(
            "line {line_no}: unstructured text is not valid lock markdown"
        ));
    }
    if let Some(mut section) = current.take() {
        flush_table(&mut section, &mut table_lines)?;
        sections.push(section);
    }
    Ok(sections)
}

fn split_bullet(text: &str) -> Option<(String, String)> {
    let (key, value) = text.split_once(':')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

fn parse_table(lines: &[String]) -> Result<Table, String> {
    if lines.len() < 2 {
        return Err("table needs a header and a separator".into());
    }
    let headers = split_row(&lines[0]);
    let rows = lines
        .iter()
        .skip(2)
        .map(|line| split_row(line))
        .filter(|row| !row.iter().all(|cell| cell.chars().all(|c| c == '-')))
        .map(|row| {
            row.into_iter()
                .map(|cell| if cell == "-" { String::new() } else { cell })
                .collect()
        })
        .collect();
    Ok(Table { headers, rows })
}

fn split_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn strip_frontmatter_block(text: &str) -> &str {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---") {
            let after = &rest[idx + 4..];
            return after.strip_prefix('\n').unwrap_or(after);
        }
    }
    text
}

fn split_frontmatter(text: &str) -> Result<(String, &str), String> {
    let trimmed = text.trim_start();
    let rest = trimmed
        .strip_prefix("---\n")
        .ok_or("lock is missing frontmatter")?;
    let idx = rest.find("\n---").ok_or("lock frontmatter is not closed")?;
    let front = rest[..idx].to_string();
    let body = rest[idx + 4..].strip_prefix('\n').unwrap_or("");
    Ok((front, body))
}

fn parse_meta(front: &str) -> Result<Meta, String> {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut list_key: Option<String> = None;
    let mut list_items: Vec<String> = Vec::new();
    let flush_list =
        |key: &mut Option<String>, items: &mut Vec<String>, fields: &mut Vec<(String, String)>| {
            if let Some(key) = key.take() {
                fields.push((key, items.join("\n")));
                items.clear();
            }
        };
    for line in front.lines() {
        if let Some(rest) = line.strip_prefix("  - ") {
            if list_key.is_some() {
                list_items.push(rest.trim().to_string());
                continue;
            }
        }
        flush_list(&mut list_key, &mut list_items, &mut fields);
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if value.is_empty() {
                list_key = Some(key.to_string());
            } else {
                fields.push((key.to_string(), value.to_string()));
            }
        }
    }
    flush_list(&mut list_key, &mut list_items, &mut fields);
    let get = |name: &str| -> Result<String, String> {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| format!("frontmatter missing {name}"))
    };
    let kind = Kind::parse(&get("kind")?).ok_or("unknown kind")?;
    let scope = match fields.iter().find(|(k, _)| k == "scope") {
        Some((_, value)) => Some(Scope::parse(value).ok_or("unknown scope")?),
        None => None,
    };
    let dependencies = parse_dependencies(&get("dependencies").unwrap_or_else(|_| "[]".into()))?;
    let security = parse_security(&get("security-items").unwrap_or_else(|_| "[]".into()))?;
    let compiler = parse_compiler(&get("compiler")?)?;
    Ok(Meta {
        lock_version: get("lock-version")?
            .parse()
            .map_err(|_| "bad lock-version")?,
        kind,
        scope,
        id: get("id")?,
        source: get("source")?,
        source_hash: get("source-hash")?,
        source_type: SourceType::parse(&get("source-type")?).ok_or("bad source-type")?,
        dependencies,
        compiler,
        compiler_hash: get("compiler-hash")?,
        status: Status::parse(&get("status")?).ok_or("bad status")?,
        approval_mode: ApprovalMode::parse(&get("approval-mode")?).ok_or("bad approval-mode")?,
        approved_by: parse_string_list(&get("approved-by")?)?,
        approved_at: get("approved-at")?,
        approval_ref: get("approval-ref")?,
        approved_commit: get("approved-commit")?,
        security_items: security,
        signature: get("signature")?,
    })
}

fn parse_dependencies(raw: &str) -> Result<Vec<Dependency>, String> {
    if raw.trim() == "[]" {
        return Ok(Vec::new());
    }
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let map = parse_inline_map(line.trim())?;
            let kind = Kind::parse(map_get(&map, "kind")?).ok_or("bad dependency kind")?;
            Ok(Dependency {
                id: map_get(&map, "id")?.to_string(),
                kind,
                hash: map_get(&map, "hash")?.to_string(),
            })
        })
        .collect()
}

fn parse_security(raw: &str) -> Result<Vec<SecurityItem>, String> {
    if raw.trim() == "[]" {
        return Ok(Vec::new());
    }
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let map = parse_inline_map(line.trim())?;
            let risk = Risk::parse(map_get(&map, "risk")?).ok_or("bad risk")?;
            Ok(SecurityItem {
                item: map_get(&map, "item")?.to_string(),
                risk,
                reason: unquote(map_get(&map, "reason")?),
                approved_by: parse_string_list(map_get(&map, "approved-by")?)?,
            })
        })
        .collect()
}

fn parse_compiler(raw: &str) -> Result<CompilerStamp, String> {
    let map = parse_inline_map(raw)?;
    Ok(CompilerStamp {
        name: map_get(&map, "name")?.to_string(),
        version: map_get(&map, "version")?.to_string(),
        model_snapshot: map_get(&map, "model-snapshot")?.to_string(),
        llm_provider: map_get(&map, "llm-provider")?.to_string(),
        endpoint_host: map_get(&map, "endpoint-host")?.to_string(),
        endpoint_private: map_get(&map, "endpoint-private")? == "true",
        seed: map_get(&map, "seed")?.parse().map_err(|_| "bad seed")?,
    })
}

fn map_get<'a>(map: &'a [(String, String)], key: &str) -> Result<&'a str, String> {
    map.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {key}"))
}

pub fn parse_inline_map(text: &str) -> Result<Vec<(String, String)>, String> {
    let text = text
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    split_top_level(text)
        .into_iter()
        .map(|part| {
            let (key, value) = part
                .split_once(':')
                .ok_or_else(|| format!("bad map entry {part}"))?;
            Ok((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

pub fn parse_string_list(text: &str) -> Result<Vec<String>, String> {
    let text = text.trim();
    if text == "[]" || text == "(none)" {
        return Ok(Vec::new());
    }
    let inner = text
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| format!("expected a list, got {text}"))?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(split_top_level(inner)
        .into_iter()
        .map(|part| unquote(part.trim()))
        .filter(|part| !part.is_empty())
        .collect())
}

pub fn split_top_level(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote = false;
    let mut escape = false;
    for c in text.chars() {
        if escape {
            cur.push(c);
            escape = false;
            continue;
        }
        if c == '\\' && quote {
            cur.push(c);
            escape = true;
            continue;
        }
        if c == '"' {
            quote = !quote;
            cur.push(c);
            continue;
        }
        if !quote && (c == '{' || c == '[') {
            depth += 1;
            cur.push(c);
            continue;
        }
        if !quote && (c == '}' || c == ']') {
            depth -= 1;
            cur.push(c);
            continue;
        }
        if !quote && depth == 0 && c == ',' {
            out.push(cur.trim().to_string());
            cur.clear();
            continue;
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

pub fn unquote(value: &str) -> String {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    value.to_string()
}

pub fn find_section<'a>(sections: &'a [Section], name: &str) -> Option<&'a Section> {
    sections.iter().find(|section| section.name == name)
}

pub fn find_section_mut<'a>(sections: &'a mut [Section], name: &str) -> Option<&'a mut Section> {
    sections.iter_mut().find(|section| section.name == name)
}

/// True when every content line is a heading, list, table row, or `(none)`.
pub fn is_structured_source(text: &str) -> bool {
    let text = strip_frontmatter_block(text);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#')
            || trimmed.starts_with("- ")
            || trimmed.starts_with('|')
            || trimmed == "(none)"
        {
            continue;
        }
        return false;
    }
    true
}

pub fn signing_payload(rendered_with_none: &str) -> String {
    rendered_with_none.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_frontmatter() {
        let mut meta = Meta::pending(Kind::Model, None, "product", "models/product/product.md");
        meta.source_hash = "sha256:abc".into();
        meta.source_type = SourceType::Structured;
        meta.compiler_hash = compiler_hash(&meta.compiler);
        meta.dependencies.push(Dependency {
            id: "main".into(),
            kind: Kind::Provider,
            hash: "sha256:def".into(),
        });
        meta.security_items.push(SecurityItem {
            item: "write-sql".into(),
            risk: Risk::Medium,
            reason: "creates rows".into(),
            approved_by: vec!["a@example".into()],
        });
        let doc = LockDoc {
            meta,
            sections: vec![Section {
                name: "Table".into(),
                bullets: vec![Bullet {
                    key: "provider".into(),
                    value: "main".into(),
                }],
                table: None,
                blocks: Vec::new(),
                none: false,
            }],
        };
        let text = render_lock(&doc);
        let parsed = parse_lock(&text).expect("parse");
        assert_eq!(parsed.meta.id, "product");
        assert_eq!(parsed.meta.dependencies[0].id, "main");
        assert_eq!(parsed.meta.security_items[0].reason, "creates rows");
        assert_eq!(parsed.sections[0].get("provider"), Some("main"));
        assert_eq!(render_lock(&parsed), text);
    }

    #[test]
    fn prose_is_natural() {
        assert!(is_structured_source(
            "# product\n\n## Table\n\n- provider: main\n"
        ));
        assert!(!is_structured_source("# product\n\nThis is a product.\n"));
    }
}
