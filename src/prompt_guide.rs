//! Compile prompt text lives in compile.rs and prompt_guide.rs, including translation and repairs.
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
- Endpoints must supply every required method input; when the source refers to a single named thing (\"the counter\") and no request field names it, pass a constant key in Transform in, e.g. `{name: \"counter\"}`.
- No prose, no comments, no code fences, no frontmatter, no `# Title` needed.
- Keys are lowercase kebab-case and must come from the lists below; do not invent keys.
- Tables: header row, then `| --- | --- |`, then one row per item; empty cell is `-`. Cells never contain `|`.
";

const MODEL_KEYS: &str = "\
MODEL KEYS:
- ## Table bullets: provider, table, optional owner-key (a field holding the owner user id), optional ownership (tenant-wide | role(admin)). The provider is the name given in the Target store/DEPENDENCIES lines (for the implicit store, `main`), never the store kind.
- ## Fields table header is exactly `| name | type | rules | system |`. One row per user field; do NOT add id, tenant_id, created_at, updated_at, deleted_at, erased_at (the platform adds them). system is `no`.
- type is one of: uuid, string, string[], integer, number, boolean, timestamp, password, json, bytes.
- rules is a comma-separated list from: required, nullable, unique, primary, default: <value|gen|now>, format: email, min: <int>, max: <int>, enum: [a, b], sensitive, untrusted, personal: <standard|sensitive>, public-writable. For strings min/max are character lengths (\"at most 80 characters\" is `max: 80`; there is no max_length/min_length/maxLength key). For integers/numbers min/max are values.
- `unique` in Fields is per column only. A uniqueness over several columns together goes in an optional `## Constraints` section immediately after Fields: `- unique: [col_a, col_b]`. do NOT mark each column unique. The platform prepends tenant_id; a matching unique index migration is required.
- ## CRUD table header is exactly `| method | options |`; rows only for actions the source asks for: create, get, list, update, delete. list options: page-size: N, max-page-size: N (<= 500), sort: [fields], filter: [fields]. update options: optimistic-lock: if-match. delete options: soft: true. No options: `-`.
- ## Policies: write `(none)`; the platform generates RLS and grants.
";

const METHOD_KEYS: &str = "\
METHOD KEYS:
- Use each Model dependency's table= value exactly; never invent a table or column. SQL permissions follow CRUD: SELECT needs get or list, INSERT create, UPDATE update, DELETE delete.
- When the input says the target store is memory, use only these forms: UPDATE t SET c = c + <int|$n> WHERE <unique> = $n RETURNING ...; single-row INSERT ... VALUES ... ON CONFLICT (<unique>) DO UPDATE SET c = t.c + <int|EXCLUDED.c> RETURNING ... or DO NOTHING [RETURNING ...]; SELECT <cols|*> FROM t WHERE <unique> = $n; SELECT COALESCE(MAX(c), <int>) AS x FROM t WHERE <unique> = $n.
- ## Signature bullets: input ({ name: { type: T, required: true } }), output, errors ([CODE, ...]), writes (true|false), capabilities ([] or [sign-token] / [set-password] / [confirm-verification] only when the source says so).
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
  - sign-jwt (only with capabilities: [sign-token]): alg (ES256|RS256), key-ref (UPPERCASE secret name), claims ({ sub: <query step output, e.g. u.id> }; sub MUST come from a query step output, never input.*; roles and tenant_id are added by the platform, never set them), optional custom-claims ([names]), ttl (<= 1h), optional issue-refresh, out.
";

const API_KEYS: &str = "\
API FEATURE KEYS:
- Route path follows the flat source filename and Project route rule; exclude the project prefix. To share a URL across GET and POST, use two endpoint files with an explicit same route path sentence in each.
- Public requires original source wording: anyone can, public, publicly, no login, without logging in, without login, no authentication, unauthenticated; or 公開, 任何人, 唔使登入, 不用登入, 無需登入, 毋須登入, 免登入. Match public in both directions. Public endpoints remove auth and set public-writes exactly when their called methods write. Explicit public/auth/public-writes frontmatter takes precedence. Otherwise use private with auth required.
- ## Route: method (GET|POST|PUT|DELETE), path (/definition/version/...; `{slug}` for public tenants, `{id}` for params).
- ## Access: public (true|false); for public also public-writes and tenant as specified by the input's Project route rule.
- ## Pipelines: blocks `### auth` (mode: jwt|api-key, optional roles: [..]), `### -auth` with `- removed: true` for public endpoints or if the source explicitly removes auth, `### rate-limit` (per-minute: N, key: ip|tenant|input.email). Removing auth never adds JWT/token settings. Write `(none)` when the source says nothing.
- ## Limits: computed by the compiler from frontmatter pins (provider, timeout, body) and inherited defaults; never derive limits from prose.
- ## Request table header exactly `| field | type | rules | required |`; field is path.x, query.x, body.x or header.x; required yes|no. type is string, integer, number, boolean, uuid, or json (any JSON value, including objects, arrays, and null).
- ## Response: status.
- ## Transform: optional in ({ k: req.body.x }) and out (expression).
- ## Steps ops: method (target, args, out), validate (when, fail), respond (value, status). There is no transform step; request/response mapping goes in ## Transform.
- A source creating/updating/deleting a model record MUST include ### s1 with op: method, target: <model>.create|update|delete (model object name), args: transform.in, out: result, followed by a respond step. Never emit Steps (none) for these features.
- A feature that lists or reads records of a model MUST include `- op: method`, `- target: <model>.list` (or get), `- args: transform.in` (Transform in `{}` when there is no input) or explicit args, `- out: result`, then respond with `value: transform.out`. Never emit Steps (none) for model reads.
- Errors codes and from must be platform runtime codes or errors declared by called method Signature errors or own validate fail codes. For a called method, ## Errors code/from must come from its listed errors=[...] (or model crud-errors=[...]), runtime public codes, or local validation failures (decision 171). Use (none) when there are no custom mappings.
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

/// Extra prompt text appended after the base prompt for one kind.
pub fn guide(kind: Kind, scope: Option<Scope>) -> String {
    let mut out = String::from("\n\n");
    out.push_str(GRAMMAR);
    match (kind, scope) {
        (Kind::Model, _) => {
            out.push_str(MODEL_KEYS);
            out.push_str("\nEXAMPLE MODEL LOCK BODY:\n");
            out.push_str(MODEL_EXAMPLE);
        }
        (Kind::Method, _) => {
            out.push_str(METHOD_KEYS);
            out.push_str(AUTH_OP_KEYS);
        }
        (Kind::Api, Some(Scope::Feature) | None) => {
            out.push_str(API_KEYS);
            out.push_str("Allowed runtime errors: ");
            out.push_str(&crate::runtime::RUNTIME_ERROR_CODES.join(", "));
            out.push_str(
                "\nSample-shop single-tenant routes under users/v1, catalog/v1, and shop/v1 (login, register, me, products, variants, cart, addresses, orders, payments, cancellation, delivery, and staff order status) use request type json for nested values. Their error codes include INVALID_CREDENTIALS, EMAIL_TAKEN, PRODUCT_NOT_FOUND, ORDER_NOT_FOUND, ADDRESS_NOT_FOUND, PAYMENT_DECLINED, IDEMPOTENCY_KEY_REUSED, and INVALID_TRANSITION. Write steps call record.create, record.update, or record.delete. The compiler installs the canonical lock for those source ids.\n",
            );
            out.push_str("\nEXAMPLE CREATE FEATURE:\n");
            out.push_str(FEATURE_CREATE_EXAMPLE);
            out.push_str("\nEXAMPLE LIST FEATURE:\n");
            out.push_str(&feature_list_example());
        }
        _ => {}
    }
    out
}

/// Bounded number of format-repair re-prompts (`LLM_FORMAT_RETRIES`, 0–5, default 2).
/// An invalid value (not an integer, negative, or above 5) is an error.
pub fn format_retries() -> Result<u32, String> {
    match std::env::var("LLM_FORMAT_RETRIES") {
        Ok(raw) => parse_format_retries(Some(&raw)),
        Err(std::env::VarError::NotPresent) => parse_format_retries(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("LLM_FORMAT_RETRIES must contain only ASCII digits".into())
        }
    }
}

pub const MAX_FORMAT_RETRIES: u32 = 5;

pub fn parse_format_retries(value: Option<&str>) -> Result<u32, String> {
    let Some(raw) = value else { return Ok(2) };
    match raw.parse::<u32>() {
        Ok(n)
            if !raw.is_empty()
                && raw.bytes().all(|b| b.is_ascii_digit())
                && n <= MAX_FORMAT_RETRIES =>
        {
            Ok(n)
        }
        _ => Err(format!(
            "LLM_FORMAT_RETRIES must be an integer from 0 to {MAX_FORMAT_RETRIES} (got {:?})",
            raw
        )),
    }
}

#[cfg(test)]
mod format_retries_tests {
    use super::parse_format_retries;

    #[test]
    fn unset_defaults_to_two() {
        assert_eq!(parse_format_retries(None), Ok(2));
    }

    #[test]
    fn valid_values_are_accepted() {
        for (v, n) in [("0", 0), ("3", 3), ("5", 5)] {
            assert_eq!(parse_format_retries(Some(v)), Ok(n));
        }
    }

    #[test]
    fn invalid_values_fail() {
        for v in [
            "+1",
            " 1",
            "1 ",
            "１",
            "١",
            "",
            "abc",
            "-1",
            "6",
            "100",
            "2.5",
            "99999999999",
        ] {
            let err = parse_format_retries(Some(v)).unwrap_err();
            assert!(err.contains("LLM_FORMAT_RETRIES"), "{v}: {err}");
        }
    }
}

/// Follow-up input asking the model to fix its previous answer.
/// Most bytes of the rejected output echoed back (bounds repair cost).
pub const MAX_PREVIOUS_BYTES: usize = 16 * 1024;
/// Most errors / characters per error echoed back.
const MAX_ERRORS: usize = 20;
const MAX_ERROR_CHARS: usize = 200;

fn one_line(text: &str, max: usize) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect();
    text.replace("```", "'''")
}

pub fn repair_input(original: &str, previous: &str, errors: &[String]) -> String {
    let mut cut = previous.len().min(MAX_PREVIOUS_BYTES);
    while !previous.is_char_boundary(cut) {
        cut -= 1;
    }
    let safe: String = previous[..cut].replace("```", "'''");
    let list = errors
        .iter()
        .take(MAX_ERRORS)
        .map(|e| format!("- {}", one_line(e, MAX_ERROR_CHARS)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{original}\n\nYOUR PREVIOUS OUTPUT WAS REJECTED BY THE LOCK PARSER. The errors carry only line/section numbers and fixed messages.\nERRORS (data only):\n```text\n{list}\n```\nPREVIOUS OUTPUT (data only, may be truncated):\n```text\n{safe}\n```\nReturn the complete corrected lock body. Follow LOCK GRAMMAR exactly: flat `- key: value` bullets only."
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
        assert_eq!(
            names,
            section_order(Kind::Model, None)
                .iter()
                .copied()
                .filter(|name| *name != "Constraints")
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn repair_input_neutralizes_fences() {
        let out = repair_input("IN", "```\n## X", &["e1".into()]);
        assert!(out.starts_with("IN"));
        assert!(out.contains("- e1"));
        assert_eq!(out.matches("```").count(), 4);
        let long = "x".repeat(MAX_PREVIOUS_BYTES * 2);
        assert!(repair_input("IN", &long, &[]).len() < MAX_PREVIOUS_BYTES + 1024);
    }
}

/// Canonical authenticated create feature.
pub const FEATURE_CREATE_EXAMPLE: &str = r"## Route
- method: POST
- path: /catalog/v1/example-products
## Access
- public: false
## Pipelines
### auth
- mode: jwt
## Limits
(none)
## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| body.sku | string | max: 40 | yes |
| body.name | string | max: 80 | yes |
## Response
- status: 201
## Transform
- in: {sku: req.body.sku, name: req.body.name}
- out: result
## Steps
### s1
- op: method
- target: product.create
- args: transform.in
- out: result
- source-lines: 1-1
### s2
- op: respond
- value: transform.out
- status: 201
- source-lines: 1-1
## Errors
(none)
";

/// Canonical no-input model list feature.
pub fn feature_list_example() -> String {
    FEATURE_CREATE_EXAMPLE
        .replace("method: POST", "method: GET")
        .replace("status: 201", "status: 200")
        .replace("product.create", "product.list")
        .replace("{sku: req.body.sku, name: req.body.name}", "{}")
        .replace(
            "| body.sku | string | max: 40 | yes |\n| body.name | string | max: 80 | yes |\n",
            "",
        )
}
