# vibe-backend Binary API vNext — Architecture Discussion Draft

- **Status:** Discussion draft; not approved for implementation
- **Draft date:** 2026-10-15 (UTC)
- **Baseline:** Latest `main` containing PR #119 merge commit `39179a873c290af7fdce8dc5674107c4540815c6`
- **Implementation state:** Documentation proposal only; no runtime implementation, milestone, or delivery ETA is approved
- **Protocol choices:** Every protocol choice in this document (media types, envelopes, encoding, headers, URLs, error mapping, limits) is a **proposal pending a spec PR**. Nothing here amends `spec-v1.8.md` until a spec PR is merged.

## 0. Confirmed direction

The following direction is confirmed and is the frame for the rest of this draft:

1. **v2 stays REST.** Resources are addressed by path and HTTP verb (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`), the same model as the spec v1.8 routes. There is no generic `/v2/operations` RPC envelope.
2. **Bodies are binary.** Request and response bodies use a binary encoding: MessagePack or CBOR. The comparison is in §5.0; the final choice is pending (§15).
3. **Generated Markdown API description.** SDK generation also emits a small, automatically generated Markdown description of the REST resources for frontend AI agents (§9).

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

- Binary (MessagePack or CBOR, §5.0) request and response bodies on REST resources.
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
- Batch requests, if measurements show meaningful header/round-trip savings.

### Deferred

- Bidirectional streaming.
- Large binary results carried inside SSE.
- Base64-encoded binary events.
- Custom MessagePack extension types or CBOR tags unless a concrete schema need is proven.
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
  - binary codec (MessagePack or CBOR)
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

Draft assessment, not a decision: MessagePack has a simpler grammar and slightly smaller output; CBOR has a standard, deterministic encoding and a tag registry, which help if bodies are hashed (for example the request hash in spec decision 104 idempotency). Whichever is chosen, the decoder profile must forbid indefinite-length items, unknown tags/extension types, and duplicate keys, and enforce §11 limits. The choice is a product/architecture decision listed in §15.

### 5.1 Media types and headers

Initial proposal:

```http
Content-Type: application/vnd.vibe.v2+msgpack   # or application/vnd.vibe.v2+cbor, per §5.0
Accept: application/vnd.vibe.v2+msgpack
Vibe-Protocol-Version: 2
Vibe-Schema-Fingerprint: <compact fingerprint>
Content-Encoding: zstd  # optional; never assumed
```

Open question: whether protocol version belongs only in headers, in a compact envelope, or both. A small in-body version marker is more robust when bodies are stored or relayed independently of headers, but adds bytes.

### 5.2 Logical envelopes

Illustrative only; final numeric field assignments belong in the canonical schema.

```text
Request  = <resource input record>   # path + verb select the operation; no operation_id envelope
Success  = [request_id, result]
Failure  = [request_id, error_code, error_fields]
Accepted = [request_id, operation_id, event_url, result_url, cancel_url]   # all URLs same-origin, see §8.3
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
- SDKs must preserve enough context to return `SCHEMA_MISMATCH` or `UNSUPPORTED_PROTOCOL`, rather than a misleading domain error.

### 5.4 Compression

MessagePack is not automatically smaller after compression than compressed JSON. The implementation must benchmark representative payload distributions.

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
- Bounded event replay window.
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
- Follow `event_url`, `result_url`, `cancel_url`, and `Location` **only** when their origin (scheme, host, port) exactly equals the configured API origin; otherwise stop and return `UNTRUSTED_URL` without sending any credentials. Relative URLs are resolved against the configured origin. Redirects to another origin are not followed with credentials.
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

Errors: `UNAUTHORIZED`, `FORBIDDEN`, `RATE_LIMITED`, `INTERNAL`
````

### 9.3 Size policy

Open decision: enforce a token/byte budget for the whole index and per operation. If the API surface becomes large, generate:

1. A tiny operation index.
2. One small operation detail file loaded only when needed.

The generator must fail CI when required semantic documentation is absent or exceeds agreed budgets, rather than silently producing a large guide.

## 10. Errors

Error codes align with `spec-v1.8.md` §8; v2 changes only the body encoding, not the codes or status mapping. The binary error body carries the same fields as the v1.8 JSON error (`code`, `message`, `request_id`); `message` stays safe and short.

| Status | Code | Source |
| --- | --- | --- |
| 400 | `MALFORMED_PAYLOAD` (proposed), `INVALID_CURSOR` (decision 106), `IDEMPOTENCY_KEY_REQUIRED` (decision 104) | v1.8 §8, 2.3b |
| 401 | `UNAUTHORIZED` (with `WWW-Authenticate`) | v1.8 §8 |
| 403 | `FORBIDDEN` | v1.8 §8 |
| 404 | `NOT_FOUND` | v1.8 §8 / 7.2 |
| 409 | `UNIQUE_VIOLATION` (decision 107), `IDEMPOTENCY_IN_PROGRESS` (decision 104), declared business conflicts | v1.8 §8 |
| 412 / 428 | `If-Match` mismatch / required | v1.8 §8, decision 48 |
| 413 | `PAYLOAD_TOO_LARGE` | v1.8 §8 |
| 415 | `UNSUPPORTED_MEDIA_TYPE` (unknown binary media type) | v1.8 §8 |
| 422 | validation codes, `IDEMPOTENCY_KEY_REUSED` (decision 104), `EMPTY_PATCH` | v1.8 §8 |
| 429 | `RATE_LIMITED` (with `Retry-After`) | v1.8 §8 / 4.3 |
| 500 | `INTERNAL` | v1.8 §8 |
| 503 | `TX_CONFLICT` (decision 100), temporarily unavailable | v1.8 §8 |
| 504 | `TX_TIMEOUT` (decision 100) | v1.8 §8 |

New v2-only codes (proposed, pending spec PR): `UNSUPPORTED_PROTOCOL`, `SCHEMA_MISMATCH`, `MALFORMED_PAYLOAD`, `JOB_EXPIRED`, `CANCELLED`, and SDK-side `UNTRUSTED_URL` (§8.3, never sent on the wire). The earlier generic codes `INVALID_INPUT`, `LIMITED`, `CONFLICT`, `FAILED`, and `TEMPORARILY_UNAVAILABLE` are dropped in favour of the v1.8 codes above.

Idempotency (decision 104) applies unchanged to v2 `POST`/`PATCH`: same `Idempotency-Key` header, same `(tenant, subject, route, key)` scope. The request hash is computed over the decoded canonical record, not the raw bytes, so equivalent encodings of the same input hash equally (this favours a deterministic encoding, §5.0).

## 11. Security and abuse controls

Binary formats and persistent streams expand the attack surface. Required gates:

- **Tenant isolation (spec decisions 23, 62, 97):** the tenant comes only from the URL slug (public endpoints) or from a verified token (authenticated endpoints), resolved by the platform into `ctx.tenant_id`. A tenant in the binary body, a header, or a query parameter is never trusted; a body field named `tenant_id` is rejected as in decision 23. Each request runs on the per-tenant role connection of decision 97.
- Jobs, SSE events, and results are scoped to `(tenant, subject)`: storage keys include both; every events, result, status, and cancel request re-checks that the caller's tenant and subject match the job's owner, and a mismatch returns `NOT_FOUND` (no existence leak). A job ID from one tenant is never valid in another.

- Authentication and authorization on request, event stream, status, result, and cancellation endpoints.
- Ownership checks must not rely only on possession of an operation ID.
- Cryptographically unguessable operation IDs, without treating them as credentials.
- Maximum raw body size, decompressed body size, nesting depth, collection length, string/binary length, and decode time.
- Reject duplicate or invalid keys according to a canonical policy.
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

Representative datasets must include tiny CRUD payloads, common requests, nested structures, large text/binary fields, error responses, and batched candidates.

Provisional acceptance targets, subject to measurement and product confirmation:

- At least 30% median payload reduction versus uncompressed JSON on representative non-trivial bodies.
- Report comparison against compressed JSON; no claim of benefit without that baseline.
- No more than 10% p95 latency regression for small requests compared with the current path.
- First meaningful progress for accepted operations is emitted as soon as a real state transition is available; set an absolute SLO after baseline measurement.
- SSE reconnect recovers without duplicate semantic callbacks.
- Older SDKs tolerate new optional fields.
- Incompatible schemas fail explicitly before returning a misleading result.
- Malformed payloads fail safely and consistently across backend implementations.
- Generated default Markdown passes an agreed operation-level and total token/byte budget.
- Cross-tenant tests: a job, event stream, or result of tenant A is never readable or cancellable from tenant B.
- SDK refuses `event_url`/`result_url`/`cancel_url` on a different origin.
- No credentials, reusable stream tokens, or raw sensitive inputs appear in standard logs.

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

1. MessagePack or CBOR (§5.0)?
2. Which canonical schema language/tool owns numeric IDs and code generation?
3. Are envelopes positional arrays, integer-key maps, or a hybrid?
4. Is the v2 marker in the media type only (`application/vnd.vibe.v2+…`), in the path, or both, given that `{version}` in the v1.8 path is the definition version?
5. What exact compatibility guarantees apply across SDK and server versions?
6. Which resources qualify for asynchronous jobs and SSE?
7. What are event replay, job expiration, and result retention durations?
8. Which SSE authentication strategy is valid for each supported runtime?
9. What payload distributions and current production baselines define success?
10. What byte/token budgets apply to the generated AI Markdown?
11. Is batching part of the first release or a measured follow-up?
12. Does a v2 resource also accept JSON (content negotiation) when the binary codec is unavailable, or is v2 binary-only?

## 16. Suggested first design spike

Use two representative operations:

- **Fast:** small request and response, to measure binary-encoding overhead and compatibility behavior.
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
