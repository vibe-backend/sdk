# LLM provider setup (compiler endpoint)

## Purpose

The compiler turns natural-language source `.md` into lock md using an LLM (§2.1, §2A.6). Interactive source authoring and regenerating stale natural-language locks need an LLM provider, endpoint, key and pinned model (§2A.8, Decision 57). The endpoint is used **only at deploy/compile time**; validation and dev manifest generation from fresh locks never call it, and runtime never uses it, and runtime never receives any LLM key (§2A.8.1, §2A.8.5).

## Natural-language source example

There is no source `.md` for this: the endpoint is configuration, not a definition. It lives in environment variables (§2A.8.3):

| Variable | Required | Rule |
| --- | --- | --- |
| `LLM_PROVIDER` | no | `openai` (default), `anthropic` or `gemini` |
| `LLM_BASE_URL` | no | defaults to the provider's base URL (table below) |
| `LLM_API_KEY` | yes | only via environment, `.env` (gitignored) or CI secret; any placeholder for local APIs that need no key |
| `LLM_MODEL` | yes | a model id / snapshot id the provider documents as **non-floating** (Decision 58) |
| `LLM_TIMEOUT` | no | default 60s, max 300s |
| `LLM_RETRY_DEADLINE` | no | total seconds one LLM call may spend retrying, default 120, max 600 |
| `LLM_MAX_CALLS_PER_SOURCE` | no | HTTP attempts one source may make, counting retries and translation, default 8, max 16 |
| `LLM_MAX_CALLS_PER_RUN` | no | HTTP attempts one `vibe build` may make, default 200, max 400 |
| `LLM_SOURCE_DEADLINE` | no | seconds one source may spend in LLM calls, default 300, max 600 |
| `LLM_RUN_DEADLINE` | no | seconds one run may spend in LLM calls, default 1800, max 3600 |
| `LLM_MAX_RETRIES` | no | retries for network errors, timeouts, 408, 429 and 5xx with exponential backoff (1s, 2s, 4s, … capped at 16s, plus jitter), default 2, max 5; a `Retry-After` header (seconds, capped at 60) replaces the backoff; 400/401/403/404 are never retried |
| `LLM_FORMAT_RETRIES` | no | re-prompts with the validator errors when the LLM output is not valid lock markdown, default 2, max 5; each re-prompt counts against `LLM_MAX_CALLS_PER_SOURCE` |
| `LLM_SEED` | no | sent only if the model supports seed; default `0`; recorded in build-record `compilers`, never in `vibe-version` (decision 174) |
| `LLM_ALLOW_PRIVATE_HOST` | no | default `false`; local development only |

`OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_MODEL` are **aliases** accepted only with `LLM_PROVIDER=openai`. `LLM_*` takes precedence; if both are set with different values it is an error. Setting any `OPENAI_*` with another provider is an error (§2A.8.3).

`.env.example` (committed, no real values; from §2A.8.5)

```dotenv
# Compiler LLM, used only at deploy/compile time; runtime never reads it
LLM_PROVIDER=openai
LLM_BASE_URL=https://api.openai.com/v1
LLM_API_KEY=replace-me
LLM_MODEL=vendor-model-2026-09-01
# Optional
LLM_TIMEOUT=60s
LLM_MAX_RETRIES=2
LLM_SEED=0
```

`.env` (local only) holds the real key and must be in `.gitignore`; the repo may only contain `.env.example`.

GitHub Actions (from §2A.8.8; `vibe@0.1.0` is the spec's illustrative, provisional CLI name and version; always pin an exact version and pin actions by full commit SHA):

```yaml
- name: Recompile and compare locks
  if: github.event.pull_request.head.repo.fork != true
  env:
    LLM_PROVIDER: ${{ vars.LLM_PROVIDER }}
    LLM_BASE_URL: ${{ secrets.LLM_BASE_URL }}
    LLM_API_KEY: ${{ secrets.LLM_API_KEY }}
    LLM_MODEL: ${{ vars.LLM_MODEL }}
  run: npx --yes vibe@0.1.0 build --check-determinism
```

Fork PRs get no secrets, so they only run the validator, not the recompile (§2A.8.5). The deploy job must not set any `LLM_*` or `OPENAI_*` variable.

## Compiled lock md example

Decision 174: provider, endpoint host, model, seed and sent parameters are recorded in the build record and manifest provenance `compilers` map. Locks contain `vibe-version`: stable versions use major.minor (including 0.x); pre-releases use the full version. Exact matches are required (decision 176).

````markdown
vibe-version: 1.0.0-rc.2
````

`model-snapshot` is the **provider-reported** model id (`modelReported`), not the user input; if it differs from `LLM_MODEL` (e.g. the provider resolved an alias) it is an error. `sent-params` is the parameter set the adapter actually sent (Decision 58, §2A.8.6).

## Providers (Decisions 57–59, §2A.8.2)

The request path and headers are fixed in the SDK adapter (Decision 59):

| `LLM_PROVIDER` | Covers | Default base URL | Request | Auth header |
| --- | --- | --- | --- | --- |
| `openai` (default) | OpenAI and every OpenAI-compatible endpoint, incl. Ollama, LM Studio, vLLM | `https://api.openai.com/v1` | `POST {base}/chat/completions` | `Authorization: Bearer {LLM_API_KEY}` |
| `anthropic` | Anthropic Messages API | `https://api.anthropic.com` | `POST {base}/v1/messages` | `x-api-key: {LLM_API_KEY}`, `anthropic-version: 2023-06-01` |
| `gemini` | Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` | `POST {base}/models/{LLM_MODEL}:generateContent` | `x-goog-api-key: {LLM_API_KEY}` (never in the query string) |

`{base}` is `LLM_BASE_URL` (or the default) with the trailing `/` removed. The `openai` base includes `/v1`; the `anthropic` base does not (the path adds `/v1`); the `gemini` base includes `/v1beta`. `openai` uses Chat Completions only; the Responses API and an `azure-openai` provider are future work (§14).

**Deterministic parameters** (Decision 58): the adapter sends `temperature`/`top_p`/`top_k`/`seed` only when its built-in capability table says the model supports them; it never forces a value the model would reject. Models not in the table get only required parameters (and a warning). Determinism is guaranteed only by the CI recompile comparison: any mismatch blocks deployment, with no override (§2A.6). Known limits (§2A.8.2): newer Anthropic models accept only `temperature: 1` and have no seed; OpenAI `seed` is best-effort only; Gemini accepts `seed` but best-effort.

### OpenAI

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_API_KEY=sk-...your-key...
OPENAI_MODEL=gpt-4.1-2025-04-14
```

- Undated OpenAI aliases (e.g. `gpt-4.1`) are floating and rejected; use a snapshot id (Decision 58 judges by provider documentation, not by the presence of a date).
- The adapter writes the response's `system_fingerprint` to the compile report (not to `vibe-version`).

### Anthropic

```dotenv
LLM_PROVIDER=anthropic
LLM_BASE_URL=https://api.anthropic.com
LLM_API_KEY=sk-ant-...your-key...
LLM_MODEL=claude-sonnet-4-6
```

- Must use `LLM_*` names; `OPENAI_*` with this provider is an error.
- Undated ids that Anthropic documents as fixed snapshots (4.6 generation onward) are allowed; `*-latest` aliases are not (§2A.8.3).

### Gemini

```dotenv
LLM_PROVIDER=gemini
LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta
LLM_API_KEY=your-gemini-api-key
LLM_MODEL=gemini-2.5-pro
```

- Use a stable model id, not `-latest`/`-preview` names. The key goes in `x-goog-api-key`, never `?key=`.

### OpenRouter (via `openai`)

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://openrouter.ai/api/v1
OPENAI_API_KEY=sk-or-...your-key...
OPENAI_MODEL=openai/gpt-4o-2024-11-20
```

- Use a dated slug from `GET https://openrouter.ai/api/v1/models`; undated slugs and `~...-latest` aliases float. OpenRouter may also route a slug to different upstreams, so the reported model id may not match `LLM_MODEL` (error) and recompiles may differ; avoid it for deployable locks unless you pin the upstream.

### Groq (via `openai`)

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://api.groq.com/openai/v1
OPENAI_API_KEY=gsk_...your-key...
OPENAI_MODEL=llama-3.3-70b-versatile
```

- Groq converts `temperature: 0` to `1e-8` and rejects some OpenAI fields; models outside the adapter's capability table get no sampling parameters anyway (Decision 58).

### Ollama, LM Studio, vLLM (local, via `openai`)

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=http://localhost:11434/v1   # Ollama; LM Studio: http://localhost:1234/v1; vLLM: http://localhost:8000/v1
OPENAI_API_KEY=local-placeholder
OPENAI_MODEL=qwen2.5-coder:32b-instruct-q8_0
```

- Use a full tag, never `:latest`. Ollama reports only the tag, so `model-snapshot` records the tag; a tag can be re-pulled to a different file (§2A.8.3).
- Plain `http` is allowed only on loopback (`localhost`, `127.0.0.1`, `::1`); other private hosts need `LLM_ALLOW_PRIVATE_HOST=true`; both are local-development only and record `endpoint-private: true` (§2A.8.4).
- Locks compiled against a private endpoint **cannot be deployed**; recompile in CI against a public https endpoint (§2A.8.4).

## Security considerations

- **Never commit the key**: not in repo, source, lock, manifest, logs, deploy reports, PR comments or error messages; errors may only say "`LLM_API_KEY` is invalid or missing" (§2A.8.5). CI secret-scans repo, locks, manifest, deploy reports and CI logs and fails the build on a suspected key.
- `.env` must be gitignored; removing it from `.gitignore` or changing CI secret usage is CODEOWNERS-protected (§2A.8.5, §9B).
- Outside development, runtime environments must contain no `LLM_*` or `OPENAI_*` variables; in development, `vibe run` warns and removes them before startup (§2A.8.5).
- **Sandbox**: the compiler's network egress is limited to the endpoint host; no tools, function calling or streaming; no secret other than `LLM_API_KEY` (§2A.7, §2A.8.6).
- CI and production compiles require `https://` with a public host; loopback, private hosts or `LLM_ALLOW_PRIVATE_HOST=true` in CI are errors (§2A.8.4).
- `LLM_ALLOW_PRIVATE_HOST=true` also permits named hosts that resolve to non-public addresses, such as fake-IP DNS proxies, for local development only. These compiles record `endpoint-private: true`, even for public-looking names. Public hostnames still require HTTPS. CI and production refuse this flag.
- For local testing of private-endpoint locks, use `vibe build`. Manifest signatures and other validation checks still apply. CI and release builds reject private-endpoint locks. Dev manifests are signed with `build_mode: "dev"` and `vibe run` accepts them only in development (including `--db memory`); release manifests cannot contain private-endpoint locks.
- Changing provider, endpoint host or model is a **major** change: all locks recompile and natural/security-touching locks need re-approval. Changing only the base URL path is not major, but the CI determinism check must still pass (§2A.8.7).

## Spec gaps

- The CLI name `vibe` and its version are provisional in the spec (§2A.8.8).
- The adapter capability table (which models get which parameters) ships with the SDK and is not published in the spec.
- The spec's CI example stores `LLM_BASE_URL` as a secret; a base URL is not itself secret, and the spec does not say whether a repository variable is acceptable.
- Third-party providers (OpenRouter, Groq) are not named by the spec; they are covered only as "OpenAI-compatible endpoints".

## Compile is all-or-nothing; LLM output cache

`vibe build` replaces the official `*.lock.md` files only when **every** source compiles. Any failure leaves all locks on disk unchanged and exits non-zero. The replacement stages each lock in a fsynced temp file, swaps them in, and rolls back if a rename fails.

Each source's successful LLM output is cached in `.vibe/cache/llm/<key>.md`, so a rerun after a partial failure does not call the LLM again for those files. The key is a SHA-256 over the system prompt (incl. vibe version stamp), the LLM input (source plus dependency context), provider, endpoint host, model, seed, sent parameters and vibe version stamp; any change is a cache miss. Cached output goes through the same normalization and checks as fresh output, and `vibe build --check` builds in memory and checks deploy policy without writing files. `.vibe/` is gitignored; delete `.vibe/cache/` to force fresh LLM calls.

## Interrupted writes

Locks are swapped in one step. The plan is kept in `.vibe/lock-journal.json`
until the swap finishes; if `vibe build` is killed part way, the next run
restores the previous locks (or finishes a swap that already reached its
commit point) and removes leftover `.tmp-*` / `.bak-*` files. LLM output is
cached in `.vibe/cache` only after the source passes every check.

## Keyless test builds and key files

`LLM_PROVIDER=fake` is a deterministic, network-free provider for keyless CI test
builds. It supports public GET constant JSON object API responses (default:
`{"message":"hello world"}`), including English or Chinese hello sources without
explicit GET/public words. It rejects writes/authentication, models and methods;
translation returns its input unchanged. It is not a general LLM replacement.
Test builds carry `test_build` only in `.vibe/build-record.json` and manifest
provenance, never in locks (decision 174). `vibe run` refuses them without
`--allow-test-build` and always refuses them outside development.
`vibe build --release` always refuses the fake provider.

`LLM_API_KEY_FILE` reads an API key from a file, for example a BuildKit secret at
`/run/secrets/llm_api_key`. Trailing newlines are trimmed (surrounding whitespace
is trimmed too). Precedence is `LLM_API_KEY` > `VIBE_LLM_API_KEY` (release builds)
then `LLM_API_KEY_FILE`. Keys are never printed. Keep compile-time LLM variables out
of runtime environments; development removes them before starting handlers,
and other environments reject them.
