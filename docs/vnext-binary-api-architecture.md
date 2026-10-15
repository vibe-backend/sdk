# vibe-backend Binary API vNext — Architecture Discussion Draft

- **Status:** Discussion draft; not approved for implementation
- **Draft date:** 2026-10-15 (UTC)
- **Baseline:** Latest `main` containing PR #119 merge commit `39179a873c290af7fdce8dc5674107c4540815c6`
- **Implementation state:** Documentation proposal only; no runtime implementation, milestone, or delivery ETA is approved

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

- MessagePack request and response bodies.
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

- Optional server-side semantic decision pipeline for Markdown feature inference:
  - Jev is the preferred provider when configured.
  - An LLM may be configured as a fallback only for uncertain or unsupported Jev outcomes.
  - Deployments remain functional when neither provider is configured.
- Zstandard compression, enabled only above a benchmark-derived threshold.
- Batch requests, if measurements show meaningful header/round-trip savings.

### Deferred

- Bidirectional streaming.
- Large binary results carried inside SSE.
- Base64-encoded MessagePack events.
- Custom MessagePack extension types unless a concrete schema need is proven.
- Automatic retry of non-idempotent writes.
- Large default documentation or tutorials.

## 3. Architectural principles

1. **Semantic interface first:** Users and AI consume SDK methods and semantic values, not field numbers or raw MessagePack.
2. **One schema authority:** The canonical schema drives server codecs, SDK codecs, compatibility tests, errors, and documentation.
3. **Compact wire, simple guide:** Compactness belongs on the wire; simplicity belongs in the generated AI interface.
4. **SSE for control and progress:** SSE improves perceived latency, but final binary output remains a normal MessagePack response.
5. **Explicit compatibility:** No implicit tolerance based only on successful decoding.
6. **Bound every resource:** Decode depth, sizes, stream duration, reconnect window, event retention, result retention, and concurrent operations must have limits.
7. **Debugging without leaking:** Human-readable diagnostics are explicit, authorized, redacted, and disabled by default in production.
8. **Optional intelligence:** Jev and LLM integrations are replaceable server-side providers, never SDK or protocol dependencies.
9. **Policy is authoritative:** Probabilistic provider output cannot bypass deterministic validation, authorization, safety controls, or explicit user choices.

## 4. Proposed system model

```text
AI / application
      | semantic inputs and outputs
      v
Generated SDK
  - input validation
  - MessagePack codec
  - protocol/schema checks
  - operation lifecycle
  - fetch-based SSE client
  - reconnect/deduplication
  - error conversion
      |
      +-- POST binary request --------------------------+
      |                                                |
      +-- GET SSE progress/control stream -------------+--> vibe-backend
      |                                                |
      +-- GET binary final result ----------------------+
```

The SDK must make a fast operation appear as a normal request and a long operation appear as a promise and/or async event iterator. The AI-facing Markdown does not explain transport mechanics.

### 4.1 Optional semantic decision pipeline

Some operations may need to infer from user-authored Markdown whether a feature is required. That inference is a backend concern and is separate from binary encoding, transport negotiation, and generated AI guidance.

```text
Markdown + explicit feature preference
      |
      v
Deterministic validation and policy pre-checks
      |
      +-- explicit or determinable outcome ------------------+
      |                                                      |
      +-- ambiguous --> Jev (when configured)                |
                            |                                 |
                            +-- sufficient result ------------+
                            |                                 |
                            +-- uncertain/unsupported         |
                                  |                           |
                                  +--> LLM fallback           |
                                       (when configured)      |
                                                            v
                                             Authoritative backend policy
                                                            |
                                                            v
                                               Provider-neutral decision
```

Provider order and absence rules:

1. Deterministic checks always run for syntax, schema, declared feature preferences, permissions, safety constraints, size, and compatibility.
2. If the outcome remains ambiguous and Jev is configured, the backend calls Jev as the preferred semantic provider.
3. An LLM is called only when it is configured as a fallback and Jev is unavailable, unsupported, uncertain, or below the deployment's confidence threshold. A deployment may explicitly allow the LLM fallback when Jev is not configured; this must not make either provider mandatory.
4. The backend policy layer makes the final decision. If no provider is configured, or confidence remains insufficient, it returns `needs_review` or requires an explicit feature choice.
5. Explicit user choices take precedence within authorization and safety constraints. Provider output alone cannot enable billing, elevate permissions, delete data, or trigger other high-risk behavior.

The SDK does not call Jev or an LLM, select providers, interpret provider-specific confidence, or own fallback policy. It submits Markdown plus an optional explicit feature preference and receives a provider-neutral result. The stable contract should use semantic fields such as:

```json
{
  "feature": "binary_api",
  "decision": "required",
  "reason_code": "SEMANTIC_FEATURE_MATCH",
  "decision_source": "jev",
  "fallback_used": false,
  "policy_version": "1"
}
```

`decision` is one of `required`, `not_required`, or `needs_review`. `decision_source` is diagnostic provenance, not a client routing instruction; clients must tolerate new source values. Exact probabilities should be omitted unless they have a stable, calibrated meaning across provider and model versions.

Jev and LLM adapters implement an internal `DecisionProvider` boundary with bounded inputs, timeouts, stable normalized outcomes, and provider-specific failures translated to backend error/review semantics. Raw Markdown is sent to an external provider only when deployment privacy, residency, retention, and consent policy permit it. Prefer derived structured signals when they are sufficient. Record provider type, provider/model version, policy version, fallback use, latency, and reason code without logging raw sensitive content.

This pipeline is optional. Without Jev and without an LLM, deterministic rules, explicit feature preferences, and `needs_review` preserve a complete supported behavior. Provider availability must not change protocol compatibility or make generated SDK installation depend on provider packages.

## 5. Wire protocol proposal

### 5.1 Media types and headers

Initial proposal:

```http
Content-Type: application/vnd.vibe.msgpack
Accept: application/vnd.vibe.msgpack
Vibe-Protocol-Version: 2
Vibe-Schema-Fingerprint: <compact fingerprint>
Content-Encoding: zstd  # optional; never assumed
```

Open question: whether protocol version belongs only in headers, in a compact envelope, or both. A small in-body version marker is more robust when bodies are stored or relayed independently of headers, but adds bytes.

### 5.2 Logical envelopes

Illustrative only; final numeric field assignments belong in the canonical schema.

```text
Request  = [protocol_version, request_id, operation_id, input]
Success  = [request_id, result]
Failure  = [request_id, error_code, error_fields]
Accepted = [request_id, operation_id, event_url, result_url, cancel_url]
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
SDK -> POST MessagePack request
Server -> 200 MessagePack success/failure
```

Do not create an SSE stream for routine fast CRUD.

### 6.2 Long-running operation

```text
1. SDK -> POST /v2/operations with MessagePack body
2. Server -> 202 Accepted with compact operation metadata
3. SDK -> GET /v2/operations/{id}/events using fetch-based SSE
4. Server -> progress/stage/heartbeat/terminal events
5. SDK -> GET /v2/operations/{id}/result with MessagePack Accept
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
- Encode/decode MessagePack using generated codecs.
- Apply limits before allocation where possible.
- Send protocol version and schema fingerprint.
- Handle synchronous and accepted-operation responses behind one API.
- Authenticate SSE without reusable URL tokens.
- Reconnect, resume, deduplicate, and reconcile state.
- Fetch and decode the final result.
- Expose cancellation and timeouts.
- Convert transport/protocol errors into stable semantic SDK errors.
- Provide an opt-in redacted diagnostic formatter.
- Never require AI callers to interpret MessagePack, SSE framing, numeric field IDs, or transport URLs.

## 9. Generated Markdown for AI

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

- MessagePack details.
- HTTP headers and endpoints.
- Numeric field IDs.
- SSE reconnection rules.
- Full internal schemas.
- Tutorials and implementation rationale.

### 9.2 Example

```md
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

Errors: `INVALID_INPUT`, `UNAUTHORIZED`, `LIMITED`, `FAILED`
```

### 9.3 Size policy

Open decision: enforce a token/byte budget for the whole index and per operation. If the API surface becomes large, generate:

1. A tiny operation index.
2. One small operation detail file loaded only when needed.

The generator must fail CI when required semantic documentation is absent or exceeds agreed budgets, rather than silently producing a large guide.

## 10. Errors

Minimum protocol/SDK error taxonomy:

- `INVALID_INPUT`
- `UNAUTHORIZED`
- `FORBIDDEN`
- `NOT_FOUND`
- `CONFLICT`
- `LIMITED`
- `UNSUPPORTED_PROTOCOL`
- `SCHEMA_MISMATCH`
- `MALFORMED_PAYLOAD`
- `PAYLOAD_TOO_LARGE`
- `OPERATION_EXPIRED`
- `CANCELLED`
- `FAILED`
- `TEMPORARILY_UNAVAILABLE`

Wire errors carry only a stable code and bounded structured safe fields. Human-readable localized prose is produced by the SDK/application where needed, not required on the wire.

## 11. Security and abuse controls

Binary formats and persistent streams expand the attack surface. Required gates:

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
- Provider-disabled deployments pass deterministic and explicit-choice conformance tests.
- Jev timeout, unavailable, uncertain, and unsupported outcomes exercise the configured fallback policy without leaking provider-specific errors to SDK callers.
- LLM fallback is never invoked when disabled, and high-risk actions cannot be enabled solely by either provider.
- Semantic decisions include auditable provider/policy provenance without raw Markdown in standard logs.
- No credentials, reusable stream tokens, or raw sensitive inputs appear in standard logs.

## 14. Rollout proposal

Prefer an additive, measurable rollout unless product explicitly chooses a clean break:

1. Define representative traffic corpus and current baselines.
2. Freeze canonical schema rules and compatibility test matrix.
3. Build a small vertical prototype for one fast and one long-running operation.
4. Run codec fuzzing and security review before production traffic.
5. Release behind capability negotiation/feature flag to selected SDK versions.
6. Observe payload, CPU, latency, reconnect, and failure metrics.
7. Expand operation coverage only after acceptance gates pass.
8. Set deprecation policy for the previous protocol separately; do not infer it from vNext availability.

## 15. Decisions required before implementation

1. Is vNext a clean breaking protocol or an additive negotiated protocol?
2. Which canonical schema language/tool owns numeric IDs and code generation?
3. Are envelopes positional arrays, integer-key maps, or a hybrid?
4. Is protocol version stored in headers, body, or both?
5. What exact compatibility guarantees apply across SDK and server versions?
6. Which operations qualify for asynchronous execution and SSE?
7. What are event replay, operation expiration, and result retention durations?
8. Which SSE authentication strategy is valid for each supported runtime?
9. What payload distributions and current production baselines define success?
10. What byte/token budgets apply to the generated AI Markdown?
11. Is batching part of the first release or a measured follow-up?
12. What is the fallback behavior when MessagePack/SSE is unavailable?

## 16. Suggested first design spike

Use two representative operations:

- **Fast:** small request and response, to measure MessagePack overhead and compatibility behavior.
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
