# vNext semantic decision pipeline (Jev / LLM) — separate proposal

- **Status:** Discussion draft, split out of [`../vnext-binary-api-architecture.md`](../vnext-binary-api-architecture.md) (PR #122 review). Not approved; pending its own spec PR.
- **Scope:** Optional server-side inference. Independent of the binary API, SDK, and generated Markdown.

## 1. Optional semantic decision pipeline

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

## 2. Acceptance checks moved from the binary API draft

- Provider-disabled deployments pass deterministic and explicit-choice conformance tests.
- Jev timeout, unavailable, uncertain, and unsupported outcomes exercise the configured fallback policy without leaking provider-specific errors to SDK callers.
- LLM fallback is never invoked when disabled, and high-risk actions cannot be enabled solely by either provider.
- Semantic decisions include auditable provider/policy provenance without raw Markdown in standard logs.
