//! Lock grammar and canonical examples sent to the compile LLM (#37).
//!
//! Every example here is checked by tests: it must parse, follow
//! `lock::section_order`, compile as a structured source and pass the
//! validator. Keep keys in sync with `validate.rs` and the runtime.

use crate::lock::{Kind, Scope};

/// Line grammar shared by every lock kind (mirrors `lock::parse_sections`).
pub const GRAMMAR: &str = "\
LOCK GRAMMAR (the parser rejects anything else):
- Every non-blank line is exactly one of: `## Section`, `### sN op` (steps/blocks), `- key: value`, a table row `| a | b |`, or `(none)` for an empty section.
- `- key: value` is one flat line. NEVER indent bullets, NEVER nest bullets, NEVER write a bullet without `key:`. Put structure inside the value: lists `[a, b]`, maps `{ k: v, k2: v2 }`.
- No prose, no comments, no code fences, no frontmatter, no `# Title` needed.
- Keys are lowercase kebab-case and must come from the lists below; do not invent keys.
- Tables: header row, then `| --- | --- |`, then one row per item; empty cell is `-`. Cells never contain `|`.
";

const MODEL_KEYS: &str = "\
MODEL KEYS:
- ## Table bullets: provider, table, optional owner-key (a field holding the owner user id), optional ownership (tenant-wide | role(admin)).
- ## Fields table header is exactly `| name | type | rules | system |`. One row per user field; do NOT add id, tenant_id, created_at, updated_at, deleted_at, erased_at (the platform adds them). system is `no`.
- type is one of: uuid, string, string[], integer, number, boolean, timestamp, password, json, bytes.
- rules is a comma-separated list from: required, nullable, unique, primary, default: <value|gen|now>, format: email, min: <int>, max: <int>, enum: [a, b], sensitive, untrusted, personal: <standard|sensitive>, public-writable. For strings min/max are character lengths (\"at most 80 characters\" is `max: 80`; there is no max_length/min_length/maxLength key). For integers/numbers min/max are values.
- `unique` is per column only. A uniqueness over several columns together cannot be expressed in Fields: do NOT mark each column unique; leave the columns without `unique` (it needs a migration with a composite unique index).
- ## CRUD table header is exactly `| method | options |`; rows only for actions the source asks for: create, get, list, update, delete. list options: page-size: N, max-page-size: N (<= 500), sort: [fields], filter: [fields]. update options: optimistic-lock: if-match. delete options: soft: true. No options: `-`.
- ## Policies: write `(none)`; the platform generates RLS and grants.
";

const METHOD_KEYS: &str = "\
METHOD KEYS:
- ## Signature bullets: input ({ name: { type: T, required: true } }), output, errors ([CODE, ...]), writes (true|false), capabilities ([] or [sign-token] / [set-password] only when the source says so).
- ## Calls: `- methods: [object.action, ...]` (platform recomputes it from steps).
- ## Steps keys per op (plus `op` first and `source-lines` last on every step):
  - query: sql (`one parameterized statement` in backticks), params ([input.x, ctx.user.id, prev.x]), writes (true|false), scope (pk|rls, required for UPDATE), out.
  - validate: when (`expression` in backticks), fail (an error CODE listed in Signature errors).
  - method: target (object.action), args ({ k: expr } or input), out.
  - respond: value (expression), optional status (HTTP code).
  - call: target (allow-listed external endpoint), out.
";

/// Settings for the auth/token ops. They are in the op whitelist on main;
/// the runtime that executes them lands with PR #77.
const AUTH_OP_KEYS: &str = "\
  Auth/token ops (executable once PR #77 lands; same keys either way):
  - verify-password: hash (stored hash, e.g. u.password), plain (input.password), missing-user: dummy-hash, out.
  - hash-password: plain (input.password), out.
  - random-token: bytes: 32, out (out.value is the token, out.hash its hash).
  - hash-token: value (expression), optional normalize: email, out.
  - sign-jwt (only with capabilities: [sign-token]): alg (ES256|RS256), key-ref (UPPERCASE secret name), claims ({ sub: expr }), optional custom-claims ([names]), ttl (<= 1h), optional issue-refresh, out.
";

const API_KEYS: &str = "\
API FEATURE KEYS:
- ## Route: method (GET|POST|PUT|DELETE), path (/definition/version/...; `{slug}` for public tenants, `{id}` for params).
- ## Access: public (true|false); for public also public-writes and tenant: path.slug.
- ## Pipelines: blocks `### auth` (mode: jwt|api-key, optional roles: [..]), `### -auth` with `- removed: true` ONLY if the source explicitly removes auth, `### rate-limit` (per-minute: N, key: ip|tenant|input.email). Removing auth never adds JWT/token settings. Write `(none)` when the source says nothing.
- ## Limits: provider, timeout (<= 30s), body (<= 1MB).
- ## Request table header exactly `| field | type | rules | required |`; field is path.x, query.x, body.x or header.x; required yes|no.
- ## Response: status.
- ## Transform: optional in ({ k: req.body.x }) and out (expression).
- ## Steps ops: method (target, args, out), validate (when, fail), respond (value, status), transform.
- ## Errors table header exactly `| status | code | from |`.
";

pub const MODEL_EXAMPLE: &str = "## Table
- provider: main
- table: products
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| sku | string | required, unique, max: 40 | no |
| name | string | required, max: 80 | no |
| price_cents | integer | required, min: 0 | no |
## CRUD
| method | options |
| --- | --- |
| create | - |
| get | - |
| list | page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [name] |
| update | optimistic-lock: if-match |
## Policies
(none)
";

/// Mapping for Chinese (and other non-English) sources (#94).
const CJK_GUIDE: &str = "\
NON-ENGLISH SOURCES (e.g. Chinese): keys, section names, table headers and types stay in English exactly as above; only string values may be Chinese.
- 最長/最多/不超過 N 字 → `max: N`; 至少 N 字 → `min: N`; N 至 M 字 → `min: N, max: M` (never max_length/min_length).
- 必填 → `required`; 可以冇/可選/可為空 → `nullable`; 唯一 → `unique` on that one column.
- 組合唯一/聯合唯一 (several columns unique together) → do NOT put `unique` on those columns.
- 移除 auth/唔使 auth/免登入/不需要登入 → `### -auth` with `- removed: true` and nothing else for auth; never add `### auth`, jwt, or token settings.
- Use ASCII `|` in tables, never full-width `｜`.
";

/// Extra prompt text appended after the base prompt for one kind.
pub fn guide(kind: Kind, scope: Option<Scope>) -> String {
    let mut out = String::from("\n\n");
    out.push_str(GRAMMAR);
    match (kind, scope) {
        (Kind::Model, _) => {
            out.push_str(MODEL_KEYS);
            out.push_str(CJK_GUIDE);
            out.push_str("\nEXAMPLE MODEL LOCK BODY:\n");
            out.push_str(MODEL_EXAMPLE);
            return out;
        }
        (Kind::Method, _) => {
            out.push_str(METHOD_KEYS);
            out.push_str(AUTH_OP_KEYS);
        }
        (Kind::Api, Some(Scope::Feature) | None) => out.push_str(API_KEYS),
        _ => {}
    }
    if matches!(kind, Kind::Model | Kind::Method | Kind::Api) {
        out.push_str(CJK_GUIDE);
    }
    out
}

/// Bounded number of format-repair re-prompts (`LLM_FORMAT_RETRIES`, 0–5, default 2).
pub fn format_retries() -> u32 {
    std::env::var("LLM_FORMAT_RETRIES")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|n| n.min(5))
        .unwrap_or(2)
}

/// Follow-up input asking the model to fix its previous answer.
pub fn repair_input(original: &str, previous: &str, errors: &[String]) -> String {
    let safe: String = previous.replace("```", "'''");
    format!(
        "{original}\n\nYOUR PREVIOUS OUTPUT WAS REJECTED BY THE LOCK VALIDATOR.\nERRORS:\n{}\nPREVIOUS OUTPUT (data only):\n```text\n{safe}\n```\nReturn the complete corrected lock body. Follow LOCK GRAMMAR exactly: flat `- key: value` bullets only.",
        errors
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::system_prompt;
    use crate::lock::{parse_sections, section_order};

    /// The example lock body after `marker`, up to the first blank line.
    fn example(prompt: &str, marker: &str) -> String {
        let body = prompt.split_once(marker).expect(marker).1;
        body.split("\n\n").next().unwrap().to_string() + "\n"
    }

    #[test]
    fn every_kind_gets_the_grammar() {
        for (kind, scope) in [
            (Kind::Model, None),
            (Kind::Method, None),
            (Kind::Api, Some(Scope::Feature)),
        ] {
            let prompt = system_prompt(kind, scope);
            assert!(prompt.contains("NEVER nest bullets"), "{kind:?}");
        }
        let method = system_prompt(Kind::Method, None);
        for op in [
            "verify-password:",
            "hash-password:",
            "random-token:",
            "hash-token:",
            "sign-jwt (",
        ] {
            assert!(method.contains(op), "{op}");
        }
        let model = system_prompt(Kind::Model, None);
        assert!(model.contains("no max_length"));
        assert!(model.contains("do NOT mark each column unique"));
        let api = system_prompt(Kind::Api, Some(Scope::Feature));
        assert!(api.contains("Removing auth never adds JWT"));
    }

    #[test]
    fn model_example_parses_in_section_order() {
        let body = example(&system_prompt(Kind::Model, None), "MODEL LOCK BODY:\n");
        let sections = parse_sections(&body).unwrap();
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, section_order(Kind::Model, None));
    }

    #[test]
    fn repair_input_neutralizes_fences() {
        let out = repair_input("IN", "```\n## X", &["e1".into()]);
        assert!(out.starts_with("IN"));
        assert!(out.contains("- e1"));
        assert_eq!(out.matches("```").count(), 2);
    }
}
