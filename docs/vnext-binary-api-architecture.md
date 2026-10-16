# vibe-backend Binary API vNext — Architecture Discussion Draft

- **Status:** Discussion draft; not approved for implementation
- **Draft date:** 2026-10-15 (UTC); revised 2026-10-16 (PM directions, review round 2)
- **Baseline:** `main` at `37198cabe8562edfff2a0ef6f1a3b91355c8bc2e` (merged into this branch), spec `docs/spec-v1.8.md`
- **Scope:** vNext only, **not in v1 scope**.
- **Implementation state:** Documentation proposal only; no runtime implementation, milestone, or delivery ETA is approved
- **Protocol choices:** Every protocol choice in this document (media types, envelopes, encoding, headers, URLs, error mapping, limits) is a **proposal pending a spec PR**. Nothing here amends `spec-v1.8.md` until a spec PR is merged.

## 0. Confirmed direction

The following direction is confirmed and is the frame for the rest of this draft:

1. **v2 stays REST.** Resources are addressed by path and HTTP verb (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`), the same model as the spec v1.8 routes. There is no generic `/v2/operations` RPC envelope.
2. **Bodies are binary.** Request and response bodies use a binary encoding: MessagePack or CBOR. The comparison is in §5.0; the PM chose CBOR (§0.1).
3. **Generated Markdown API description.** SDK generation also emits a small, automatically generated Markdown description of the REST resources for frontend AI agents (§9).

### 0.1 PM-confirmed directions (2026-10-16)

PM-confirmed directions on 2026-10-16. They remain part of this **proposal**, are **not in v1 scope**, and become normative only through a spec PR.

1. **Encoding:** CBOR (RFC 8949) using **core deterministic encoding** (RFC 8949 §4.2.1), with the decoder profile in §5.0a.
2. **Version marker:** the v2 marker is carried **only** in the media type, `application/vnd.vibe.v2+cbor`. The path `{version}` remains the definition version from spec v1.8; there is no `/v2/` path segment and no in-body version field.
3. **JSON also accepted:** v2 resources also accept and return JSON (`application/vnd.vibe.v2+json`) selected through `Content-Type`/`Accept`, with identical semantics. CBOR is opt-in. Both are decoded into the same canonical data model (§5.0a).
4. **Retention defaults:** event replay 24h, job expiry 24h, result retention 7d. Tenants may only **lower** these values, never raise them.
5. **AI Markdown budget:** the generated Markdown is capped at **64 KB per endpoint**. Exceeding it is a generation error; output is never silently truncated.
6. **No batching** in the first release.

## 1. Intent

Design a new API/SDK generation model optimized for machine use:

1. API request and response payloads do not need to be human-readable.
2. Wire payloads should be materially smaller than the current text representation.
3. Generated SDKs own validation, binary encoding/decoding, version checks, SSE lifecycle, and error conversion.
4. SDK generation emits a very small Markdown guide telling an AI how to call operations and interpret semantic results.
5. Long-running work should provide immediate, reliable progress so perceived delay is reduced.

Non-goal: binary encoding is not encryption, authentication, authorization, or a security boundary.

## 2. Proposed scope

### Required for the first vNext release

- CBOR (core deterministic encoding, opt-in) and JSON request and response bodies on REST resources (§0.1, §5.0a).
- A canonical, versioned schema from which backend and SDK codecs are generated.
- Stable numeric field IDs with explicit compatibility rules.
- SDK-generated semantic APIs that hide transport details.
- Minimal generated AI Markdown.
- Stable structured error codes.
- Request/operation IDs for correlation.
- Long-running operation resource model.
- SSE progress/control stream for long-running operations.
- Resume, cancellation, terminal-state, expiration, and result-retention behavior.
- Schema fingerprint and protocol compatibility check.
- Benchmarks against JSON and compressed JSON.

### Conditional

- Zstandard compression, enabled only above a benchmark-derived threshold.

### Deferred

- Batch requests (PM 2026-10-16: not in the first release).

- Bidirectional streaming.
- Large binary results carried inside SSE.
- Base64-encoded binary events.
- CBOR tags beyond the §5.0a allowlist unless a concrete schema need is proven.
- Automatic retry of non-idempotent writes.
- Large default documentation or tutorials.

## 3. Architectural principles

1. **Semantic interface first:** Users and AI consume SDK methods and semantic values, not field numbers or raw binary bodies.
2. **One schema authority:** The canonical schema drives server codecs, SDK codecs, compatibility tests, errors, and documentation.
3. **Compact wire, simple guide:** Compactness belongs on the wire; simplicity belongs in the generated AI interface.
4. **SSE for control and progress:** SSE improves perceived latency, but final binary output remains a normal binary REST response.
5. **Explicit compatibility:** No implicit tolerance based only on successful decoding.
6. **Bound every resource:** Decode depth, sizes, stream duration, reconnect window, event retention, result retention, and concurrent operations must have limits.
7. **Debugging without leaking:** Human-readable diagnostics are explicit, authorized, redacted, and disabled by default in production.
8. **REST resources:** Each operation is a resource path plus HTTP verb; no generic RPC envelope.
9. **Tenant isolation:** Tenant comes only from the slug or a verified token (§11).
10. **Policy is authoritative:** Deterministic validation and authorization always decide; nothing in the request body can override them.

## 4. Proposed system model

```text
AI / application
      | semantic inputs and outputs
      v
Generated SDK
  - input validation
  - CBOR / JSON codec (one canonical model)
  - protocol/schema checks
  - operation lifecycle
  - fetch-based SSE client
  - reconnect/deduplication
  - error conversion
      |
      +-- REST verb + binary body on a resource --------+
      |                                                |
      +-- GET SSE progress stream (same origin) -------+--> vibe-backend
      |                                                |
      +-- GET binary result resource (same origin) ----+
```

The SDK must make a fast operation appear as a normal request and a long operation appear as a promise and/or async event iterator. The AI-facing Markdown does not explain transport mechanics.

### 4.1 Semantic decision pipeline (moved)

The optional Jev/LLM semantic decision pipeline is out of scope for this document. It now lives in a separate proposal: [`proposals/vnext-semantic-decision-pipeline.md`](proposals/vnext-semantic-decision-pipeline.md). Nothing in the binary API, SDK, or generated Markdown depends on it.

## 5. Wire protocol proposal

### 5.0 MessagePack vs CBOR

Both are compact, schemaless binary encodings with a JSON-like data model. Comparison for this project (to be confirmed by the §13 benchmarks):

| Aspect | MessagePack | CBOR (RFC 8949) |
| --- | --- | --- |
| Standardization | Community spec (msgpack.org) | IETF standard (RFC 8949), IANA tag registry |
| Size | Very compact; small ints and short strings in 1 byte overhead | Comparable; within a few percent of MessagePack on typical payloads |
| Deterministic encoding | No standard canonical form; must define our own | Defined "core deterministic encoding" (RFC 8949 §4.2), useful for hashing and signatures |
| Extensibility | Extension types (application-defined, -1..127 app range) | Registered tags (e.g. date/time, bignum, decimal) |
| Integer / map keys | Integer keys supported | Integer keys supported |
| Duplicate keys | Not defined by spec; decoder policy needed | Spec says invalid in well-formed "valid" data; decoders can reject |
| Indefinite-length items | No | Yes (must be disabled/limited by our decode policy) |
| Ecosystem | Mature libraries in JS, Rust, Go, Python; wide adoption | Mature libraries in JS, Rust, Go, Python; used by COSE/WebAuthn |
| Browser support | Library required | Library required |
| Security review | Simpler grammar | More features (tags, indefinite length) to restrict |

Outcome (PM 2026-10-16, §0.1): **CBOR with core deterministic encoding**, chosen mainly because it has a standard canonical form for the decision 104 request hash. MessagePack is not used.

### 5.0a Decoder profile and canonical form

This profile is a prerequisite for the decision 104 request hash and applies to both CBOR and JSON input. Default limits are platform values (proposal); tenants may only lower them.

| Limit | Default |
| --- | --- |
| Raw body size | 1 MiB (spec v1.8 413 rule applies) |
| Decompressed body size | 1 MiB; compression ratio above 20:1 rejected |
| Nesting depth (maps + arrays) | 32 |
| Map entries per map | 1,024 |
| Array elements per array | 10,000 |
| Text string length | 256 KiB UTF-8 bytes |
| Byte string length | 1 MiB (bounded by body size) |
| Total items in one body | 100,000 |
| Decode time | 50 ms CPU |

Rules:

- **Well-formed and valid only.** Length claims are checked against remaining bytes before allocation.
- **No indefinite-length items** (major types 2–5 with additional info 31) and no `break` outside them: reject.
- **Duplicate map keys are rejected** (after canonicalization of the key), in CBOR and JSON alike.
- **Map keys:** text strings or unsigned integers only (field IDs). Other key types are rejected.
- **Tag allowlist:** none by default. Only tags explicitly listed in the canonical schema are allowed; timestamps are integer epoch seconds, not tagged. Any other tag is rejected. Simple values other than `false`, `true`, `null` are rejected; `undefined` is rejected.
- **UTF-8:** text strings must be valid UTF-8 (no overlong forms, no surrogates); invalid input is rejected, never replaced. No Unicode normalization is applied (values are compared as bytes).
- **Integers:** integers within ±(2^53 − 1) are canonical integers; fields typed `int64` may use the full 64-bit range (in JSON carried as a decimal string); bignum tags are rejected. Integers are never encoded as floats.
- **Floats:** allowed only for schema fields typed `float`. Canonical form uses the shortest of half/single/double that preserves the value exactly (RFC 8949 §4.2.2). `NaN` and ±Infinity are rejected. `-0.0` is normalized to `0.0`. A float with an integral value in an integer field is rejected (no silent conversion).
- **Canonical form for hashing:** after decoding and schema validation, the request record is re-encoded with RFC 8949 core deterministic encoding: preferred (shortest) argument encoding, definite lengths, map keys sorted by the bytewise lexicographic order of their deterministic encodings. The decision 104 request hash is `SHA-256` over this canonical CBOR, so the same input sent as JSON or CBOR, in any key order, produces the same hash.
- **JSON input** is parsed with the same limits, duplicate-key rejection, UTF-8 and number rules (JSON numbers map to integers or floats per schema field type), converted to the same canonical model, and hashed the same way.
- Violations return 400 `MALFORMED_PAYLOAD` (proposed) or 413; error bodies never echo input.

### 5.1 Media types and headers

Proposal (PM direction 2026-10-16: the v2 marker is only in the media type):

```http
Content-Type: application/vnd.vibe.v2+cbor      # opt-in; or application/vnd.vibe.v2+json
Accept: application/vnd.vibe.v2+cbor
Vibe-Schema-Fingerprint: <compact fingerprint>
Content-Encoding: zstd  # optional; never assumed
```

- The path `{version}` stays the definition version; there is no `Vibe-Protocol-Version` header and no in-body version.
- An unknown media type returns 415; an unsatisfiable `Accept` returns 406 (proposed).
- JSON and CBOR responses carry identical semantics (§5.0a).

### 5.2 Logical envelopes

Illustrative only; final numeric field assignments belong in the canonical schema.

```text
Request  = <resource input record>   # path + verb select the operation; no operation_id envelope
Success  = [request_id, result]
Failure  = [request_id, error_code, error_fields]
Accepted = [request_id, job_id, events_url, result_url, cancel_url]   # = Location .../jobs/{job_id} and its /events, /result; cancel = DELETE on Location; all same-origin, §8.3
```

Envelope arrays minimize key overhead. Domain records may use numeric-key maps when optional/evolving fields make positional arrays unsafe.

### 5.3 Field compatibility rules

- Assign stable numeric IDs from the canonical schema.
- Never reuse a retired field ID.
- New optional fields must not break older readers.
- Unknown optional fields are ignored unless marked critical.
- Missing and explicit `null` are distinct.
- A required field cannot be added within a compatible schema revision.
- Type changes require a new field ID or breaking protocol version.
- Enum values are append-only within a compatible revision; unknown values surface as a typed compatibility error if they cannot be preserved.
- SDKs must preserve enough context to return `SCHEMA_MISMATCH` or 415/406 for an unsupported media type, rather than a misleading domain error.

### 5.4 Compression

A binary encoding is not automatically smaller than compressed JSON after compression. The implementation must benchmark representative payload distributions for both media types.

Rules proposed:

- No compression for small bodies by default.
- Negotiate supported compression at SDK initialization or capability discovery.
- Apply Zstandard only above a measured size threshold and only where CPU/latency trade-offs pass acceptance criteria.
- Enforce compressed and decompressed size limits.
- Do not reflect secret-derived data into attacker-controlled compression contexts.

## 6. Request execution modes

### 6.1 Fast operation

For work expected to complete quickly:

```text
SDK -> <VERB> /{definition}/{version}/{slug}/{resource}[/{id}] with binary body
Server -> 2xx binary body, or 4xx/5xx binary error (§10)
```

Do not create an SSE stream for routine fast CRUD.

### 6.2 Long-running operation

```text
1. SDK -> POST /{definition}/{version}/{slug}/{resource} with binary body
2. Server -> 202 Accepted, Location: .../{resource}/jobs/{job_id}, compact metadata
3. SDK -> GET .../{resource}/jobs/{job_id}/events using fetch-based SSE
4. Server -> progress/stage/heartbeat/terminal events
5. SDK -> GET .../{resource}/jobs/{job_id}/result with binary Accept
6. Cancel: DELETE .../{resource}/jobs/{job_id}
```

The transition to asynchronous execution may be operation-defined or decided by the server. The SDK presents one stable semantic method either way.

### 6.3 Operation state machine

```text
accepted -> running -> completed
                    -> failed
                    -> cancelled
                    -> expired
```

Terminal states are immutable. Cancellation is best-effort: a race may return a completed result if completion wins before cancellation is committed.

## 7. SSE design

### 7.1 Event types

Keep events small and non-sensitive:

```text
progress   integer or bounded structured progress
stage      stable stage code
heartbeat  comment line, no semantic payload
complete   result available
failed     stable error code and safe fields
cancelled  terminal cancellation
```

No large final result or Base64 binary payload is sent through SSE.

### 7.2 Reliability requirements

- Monotonically increasing event IDs per operation.
- Resume with `Last-Event-ID`.
- SDK deduplicates replayed events.
- Ordered delivery within one operation.
- Heartbeats sized to survive intended proxy/load-balancer idle timeouts.
- Explicit terminal event followed by stream closure.
- Bounded event replay window: 24h default (tenants may only lower it, §0.1).
- When resume is no longer possible, SDK fetches operation status and reconciles the terminal state.
- Client backoff includes jitter and respects server retry hints within bounded limits.
- Maximum stream duration and reconnect attempts are configurable.

### 7.3 Authentication

Prefer a fetch-based SSE client when bearer authentication is required because native browser `EventSource` cannot set arbitrary authorization headers.

Permitted approaches, in preference order pending deployment constraints:

1. Fetch-based SSE with the normal authorization header.
2. Secure same-site cookie where the application architecture supports it and CSRF controls are correct.
3. Short-lived, single-operation, audience-bound stream capability.

Never put a reusable access token in a URL. Stream capabilities must be short-lived, scoped to one operation, non-refreshable, and safe against leakage through logs/referrers.

## 8. SDK contract

### 8.1 Basic interface

```ts
const result = await vibe.generate({ prompt }, {
  onProgress(event) {
    updateUI(event.progress)
  }
})
```

### 8.2 Advanced interface

```ts
for await (const event of vibe.generateStream({ prompt })) {
  if (event.type === "progress") updateUI(event.progress)
  if (event.type === "complete") use(event.result)
}
```

### 8.3 SDK responsibilities

- Validate semantic inputs before encoding.
- Encode/decode the chosen binary format (§5.0) using generated codecs.
- Apply limits before allocation where possible.
- Send protocol version and schema fingerprint.
- Handle synchronous and accepted-operation responses behind one API.
- Authenticate SSE without reusable URL tokens.
- Reconnect, resume, deduplicate, and reconcile state.
- Fetch and decode the final result.
- **Same-origin rule for `events_url`, `result_url`, `cancel_url`, and `Location`:** relative URLs are resolved against the configured API origin (WHATWG URL parsing). The SDK then compares the origin tuple (scheme, host, port) by exact equality after normalization:
  - **Scheme:** lowercased; must be `https` (`http` allowed only when the configured origin itself is `http://localhost` in dev). An `https`→`http` downgrade is always rejected.
  - **Host:** lowercased ASCII; IDN hosts are converted to punycode (UTS #46 / IDNA, as the WHATWG URL parser does) before comparison; a trailing dot is not stripped (so `api.example.com.` differs). IP literals compare in their canonical serialized form.
  - **Port:** default ports are normalized (`https` 443, `http` 80), so `https://api.example.com` equals `https://api.example.com:443`; any other port must match exactly.
  - **Userinfo:** any URL with `user:pass@` is rejected.
  - Any mismatch: stop and return `UNTRUSTED_URL` (SDK-side), sending no request and no credentials.
- **Redirects:** cross-origin redirects are **never followed** (`redirect: "manual"`), returning `UNTRUSTED_URL`. Same-origin redirects are re-checked with the rule above at every hop, with a limit of 3 hops.
- Expose cancellation and timeouts.
- Convert transport/protocol errors into stable semantic SDK errors.
- Provide an opt-in redacted diagnostic formatter.
- Never require AI callers to interpret binary bodies, SSE framing, numeric field IDs, or transport URLs.

## 9. Generated Markdown for AI

SDK generation automatically produces a Markdown description of the v2 REST resources for frontend AI agents. It is generated from the canonical schema, never hand-written.

### 9.1 Design constraints

The default guide should contain only:

- Operation name and one-line purpose.
- Required inputs.
- Optional inputs only when commonly useful.
- Semantic return shape.
- Stable expected error codes.
- One minimal example.
- A short indication that progress may occur, if relevant.

Exclude by default:

- Binary encoding details.
- HTTP headers and endpoints.
- Numeric field IDs.
- SSE reconnection rules.
- Full internal schemas.
- Tutorials and implementation rationale.

### 9.2 Example

````md
# Vibe API

## generate
Create a result from a prompt.

Input: `prompt`
Optional: `model`
Returns: `{id, output, status}`
Progress: may be reported before completion.

```ts
const result = await vibe.generate({prompt})
```

Errors: `UNAUTHORIZED`, `FORBIDDEN`, `RATE_LIMITED`, `INTERNAL`  <!-- `UNAUTHORIZED`/`FORBIDDEN` are proposed names, see §10 -->
````

### 9.3 Size policy

PM direction (2026-10-16): the generated Markdown is capped at **64 KB per endpoint**; exceeding it fails generation with an error and is never silently truncated. If the API surface becomes large, generate:

1. A tiny operation index.
2. One small operation detail file loaded only when needed.

The generator must fail CI when required semantic documentation is absent or exceeds agreed budgets, rather than silently producing a large guide.

## 10. Errors

Error codes align with `spec-v1.8.md` §8; v2 changes only the body encoding, not the codes or status mapping. The binary error body carries the same fields as the v1.8 JSON error (`code`, `message`, `request_id`); `message` stays safe and short.

Codes marked *(proposed)* are not literally named in v1.8 §8 (which defines statuses, plus some codes); they need the spec PR.

| Status | Code | Source |
| --- | --- | --- |
| 400 | `MALFORMED_PAYLOAD` *(proposed)*; `INVALID_CURSOR` (decision 106, §12.9); `IDEMPOTENCY_KEY_REQUIRED` (decision 104, §2.3b) | v1.8 §8 400 |
| 401 | `UNAUTHORIZED` *(proposed)*, with `WWW-Authenticate` | v1.8 §8 401 |
| 403 | `FORBIDDEN` *(proposed)* | v1.8 §8 403 |
| 404 | `NOT_FOUND` *(proposed)* | v1.8 §8 404 / 7.2 |
| 405 | `METHOD_NOT_ALLOWED` *(proposed)*, with `Allow` | v1.8 §8 405 |
| 406 | `NOT_ACCEPTABLE` *(proposed)* | new (media type negotiation, §5.1) |
| 409 | `UNIQUE_VIOLATION` (decision 107), `IDEMPOTENCY_IN_PROGRESS` (decision 104), declared business conflicts | v1.8 §8 409 |
| 412 | `PRECONDITION_FAILED` *(proposed)* | v1.8 §8 412, decision 48 |
| 413 | `PAYLOAD_TOO_LARGE` *(proposed)* | v1.8 §8 413 |
| 415 | `UNSUPPORTED_MEDIA_TYPE` *(proposed)* | v1.8 §8 415 |
| 422 | declared validation codes, `IDEMPOTENCY_KEY_REUSED` (decision 104), `EMPTY_PATCH` | v1.8 §8 422 |
| 428 | `PRECONDITION_REQUIRED` *(proposed)* | v1.8 §8 428 |
| 429 | `RATE_LIMITED` (spec §4.3 default), with `Retry-After` | v1.8 §8 429 |
| 500 | `INTERNAL` | v1.8 §8 500 |
| 503 | `TX_CONFLICT` (decision 100); other unavailability `UNAVAILABLE` *(proposed)* | v1.8 §8 503 |
| 504 | `TX_TIMEOUT` (decision 100) | v1.8 §8 504 |

New v2-only codes (proposed, pending spec PR): `SCHEMA_MISMATCH`, `MALFORMED_PAYLOAD`, `JOB_EXPIRED`, `CANCELLED`, and SDK-side `UNTRUSTED_URL` (§8.3, never sent on the wire). The earlier generic codes `INVALID_INPUT`, `LIMITED`, `CONFLICT`, `FAILED`, and `TEMPORARILY_UNAVAILABLE` are dropped. `JOB_EXPIRED` is returned (404 or 410, proposed) after the 24h job expiry or 7d result retention.

Idempotency (decision 104) applies unchanged to v2 `POST`/`PATCH`: same `Idempotency-Key` header, same `(tenant, subject, route, key)` scope. The request hash is `SHA-256` over the RFC 8949 core deterministic re-encoding of the decoded record (§5.0a), not the raw bytes, so the same input in JSON or CBOR and in any key order hashes equally.

## 11. Security and abuse controls

Binary formats and persistent streams expand the attack surface. Required gates:

- **Tenant isolation (spec decisions 23, 62, 97):** the tenant comes only from the URL slug (public endpoints) or from a verified token (authenticated endpoints), resolved by the platform into `ctx.tenant_id`. A tenant in the binary body, a header, or a query parameter is never trusted; a body field named `tenant_id` is rejected as in decision 23. Each request runs on the per-tenant role connection of decision 97.
- Jobs, SSE events, and results are scoped to `(tenant, subject)`: storage keys include both; every events, result, status, and cancel request re-checks that the caller's tenant and subject match the job's owner, and a mismatch returns `NOT_FOUND` (no existence leak). A job ID from one tenant is never valid in another.
- **Slug/token tenant mismatch:** on an authenticated request, if the tenant resolved from the path slug differs from the tenant in the verified token, the request is **rejected**, never resolved by letting one source win. Response is 404 `NOT_FOUND` (same as an unknown slug), so it does not reveal whether the slug's tenant exists; the detail goes to the audit log only.
- Decoder limits and canonical form: §5.0a.
- Authentication and authorization on request, event stream, status, result, and cancellation endpoints.
- Ownership checks must not rely only on possession of an operation ID.
- Cryptographically unguessable operation IDs, without treating them as credentials.
- Maximum raw body size, decompressed body size, nesting depth, collection length, string/binary length, and decode time (defaults in §5.0a).
- Reject duplicate keys, indefinite-length items, non-allowlisted tags, and invalid UTF-8 (§5.0a).
- No unbounded allocation based on encoded length claims.
- Per-principal limits for running operations, queued work, SSE connections, reconnect rate, and stored results/events.
- Redacted logging; raw bodies off by default.
- Diagnostics gated by environment and authorization.
- Stable error responses must not disclose schema internals or sensitive values.
- Cancellation and result endpoints must be auditable.
- Fuzz tests and malicious corpus tests for every supported decoder implementation.

## 12. Observability

Collect without logging raw sensitive payloads:

- request ID and operation ID;
- protocol version and schema fingerprint;
- encoded and decoded size buckets;
- compression ratio and CPU time;
- decode/validation duration;
- time to accepted response;
- time to first meaningful progress;
- total completion time;
- SSE reconnect and resume outcomes;
- terminal state and stable error code;
- result retrieval delay and expiration count.

## 13. Benchmark and acceptance plan

Representative datasets must include tiny CRUD payloads, common requests, nested structures, large text/binary fields, and error responses, in both CBOR and JSON.

Provisional acceptance targets, subject to measurement and product confirmation:

- At least 30% median payload reduction versus uncompressed JSON on representative non-trivial bodies.
- Report comparison against compressed JSON; no claim of benefit without that baseline.
- No more than 10% p95 latency regression for small requests compared with the current path.
- First meaningful progress for accepted operations is emitted as soon as a real state transition is available; set an absolute SLO after baseline measurement.
- SSE reconnect recovers without duplicate semantic callbacks.
- Older SDKs tolerate new optional fields.
- Incompatible schemas fail explicitly before returning a misleading result.
- Malformed payloads fail safely and consistently across backend implementations.
- Generated Markdown stays within 64 KB per endpoint; an oversized endpoint fails generation (test).
- No credentials, reusable stream tokens, or raw sensitive inputs appear in standard logs.

### 13.1 Required test cases for the spike

These are required test cases, not just acceptance targets. The spike is not complete until each passes:

1. **Cross-tenant read:** tenant B's token cannot `GET` tenant A's job status, `/events`, or `/result`; each returns 404 `NOT_FOUND`, identical to an unknown job ID.
2. **Cross-tenant cancel:** tenant B cannot `DELETE` tenant A's job; 404, and A's job continues.
3. **Same tenant, other subject:** subject Y cannot read or cancel subject X's job in the same tenant; 404.
4. **Slug/token mismatch:** a token for tenant A on a path slug for tenant B is rejected with 404, for an existing and a non-existent B alike (identical responses).
5. **Tenant in body/header/query** is ignored or rejected (`tenant_id` body field → 422, decision 23).
6. **Origin rule:** SDK returns `UNTRUSTED_URL` for a different host, different port, `https`→`http`, userinfo, IDN homograph vs punycode mismatch; accepts `:443` vs no port.
7. **Redirects:** cross-origin redirect not followed; same-origin chain of 4 hops rejected; 3 accepted.
8. **Decoder profile:** each §5.0a limit at limit (accepted) and limit+1 (rejected); duplicate keys, indefinite-length, unknown tag, invalid UTF-8, NaN, Infinity rejected; `-0.0` normalized.
9. **Hash equivalence:** the same record as JSON, CBOR, and CBOR with different key order yields the same decision 104 request hash; a changed value yields a different one.
10. **Retention:** events replay up to 24h, job expiry 24h, result 7d; a tenant setting above the default is rejected, below is honoured.

## 14. Rollout proposal

v2 is a new REST API version served alongside v1; rollout is measured:

1. Define representative traffic corpus and current baselines.
2. Freeze canonical schema rules and compatibility test matrix.
3. Build a small vertical prototype for one fast and one long-running operation.
4. Run codec fuzzing and security review before production traffic.
5. Release behind capability negotiation/feature flag to selected SDK versions.
6. Observe payload, CPU, latency, reconnect, and failure metrics.
7. Expand operation coverage only after acceptance gates pass.
8. Set deprecation policy for the previous protocol separately; do not infer it from vNext availability.

## 15. Decisions required before implementation

Decided by the PM on 2026-10-16 (§0.1): encoding (CBOR), version marker (media type only), JSON acceptance (yes), retention defaults, AI Markdown budget (64 KB/endpoint), batching (not in first release).

Still open:

1. Which canonical schema language/tool owns numeric IDs and code generation?
2. Are envelopes positional arrays, integer-key maps, or a hybrid?
3. What exact compatibility guarantees apply across SDK and server versions?
4. Which resources qualify for asynchronous jobs and SSE?
5. Which SSE authentication strategy is valid for each supported runtime?
6. What payload distributions and current production baselines define success?
7. Confirm the §5.0a default limits and whether `JOB_EXPIRED` is 404 or 410.

## 16. Suggested first design spike

Use two representative operations:

- **Fast:** small request and response, to measure CBOR vs JSON overhead and compatibility behavior.
- **Long-running:** accepted operation with real progress, reconnect, cancellation, and binary final result.

Deliverables of the spike:

- canonical schema sample;
- generated backend and one SDK codec;
- minimal generated AI Markdown;
- benchmark report against JSON and compressed JSON;
- malformed-input/fuzz results;
- SSE resume/cancel test results;
- decision record answering Section 15.

The spike is not a commitment to ship the protocol. Implementation planning begins only after the discussion draft and decision record are approved.
