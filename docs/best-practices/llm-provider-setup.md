# LLM provider setup (compiler endpoint)

## Purpose

The compiler turns natural-language source `.md` into lock md using an LLM (§2.1, §2A.6). Initializing a project and running a compile therefore need an **OpenAI-compatible base URL and API key**. This page shows how to supply them safely and lists ready-made settings for common providers.

> ⚠️ **Spec status.** The merged spec (`docs/spec-v1.5.md`) does **not** define how the compiler endpoint or key is configured; it only requires a pinned model snapshot, `compiler-hash`, deterministic settings and a sandbox with no network or secrets (§2A.6, §2A.7). The variable names below follow the architect's **proposed** spec v1.7 (open PR #5, Decision 57, §2A.8), which is **not merged yet** and may change:
>
> - Neutral names `LLM_PROVIDER` (default `openai`), `LLM_BASE_URL`, `LLM_API_KEY` (required), `LLM_MODEL` (required, pinned snapshot).
> - `OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_MODEL` are accepted as **aliases** only when `LLM_PROVIDER=openai`; `LLM_*` wins, and conflicting values are an error.
>
> The v1.7 draft supports three providers via `LLM_PROVIDER`: `openai` (default; covers OpenAI and every OpenAI-compatible endpoint, Bearer auth), `anthropic` and `gemini`. The OpenAI-compatible examples use the `OPENAI_*` aliases; Anthropic and Gemini must use `LLM_*`.

## Natural-language source example

There is no source `.md` for this: the endpoint is configuration, not a definition. Configuration lives in environment variables.

`.env.example` (committed, no real values)

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_API_KEY=replace-me
OPENAI_MODEL=gpt-4.1-2025-04-14
```

`.env` (local only, **gitignored**) contains the real key. Make sure `.gitignore` has:

```text
.env
```

In CI, set the values as **CI secrets** (e.g. GitHub Actions `secrets.OPENAI_API_KEY`) and expose them only to the compile job:

```yaml
- name: Compile locks
  env:
    LLM_PROVIDER: openai
    OPENAI_BASE_URL: ${{ vars.OPENAI_BASE_URL }}   # not secret: repository variable
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
    OPENAI_MODEL: gpt-4.1-2025-04-14
  run: echo "run the SDK compile command here"
```

> The SDK CLI command name for compiling is not defined in either spec version; replace the placeholder with the real command once it exists.

## Compiled lock md example

The endpoint affects the lock only through the `compiler` frontmatter and `compiler-hash` (§2A.1). In v1.5 this is:

````markdown
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
````

Proposed v1.7 (PR #5, unmerged) also records the provider and endpoint host (host only, never the key):

````markdown
compiler: { name: md-logic, version: 2.0.0, model-snapshot: <model id reported by the provider>, llm-provider: openai, endpoint-host: api.openai.com, endpoint-private: false, llm-seed: 0, sent-params: { temperature: 0, top_p: 1, seed: 0 } }
````

## Provider examples

Base URLs were checked against each provider's official docs (October 2026). Model names are **examples**; always pin a non-floating snapshot id (v1.5 §2A.6 forbids floating aliases).

### OpenAI

```dotenv
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_API_KEY=sk-...your-key...
OPENAI_MODEL=gpt-4.1-2025-04-14
```

- Auth: `Authorization: Bearer {key}` on `POST {base}/chat/completions` (v1.7 draft §2A.8.2). Every OpenAI-compatible provider on this page uses the same Bearer header.
- Use a dated snapshot, not an undated alias such as `gpt-4.1`.
- OpenAI documents `seed` as beta / best-effort; determinism is not guaranteed.

### Anthropic

The Anthropic and Gemini sections use `LLM_*` names: the `OPENAI_*` aliases are accepted **only** with `LLM_PROVIDER=openai`, and setting them with another provider is an error (v1.7 draft §2A.8.3).

```dotenv
LLM_PROVIDER=anthropic
LLM_BASE_URL=https://api.anthropic.com
LLM_API_KEY=sk-ant-...your-key...
LLM_MODEL=claude-sonnet-4-6
```

- Request: `POST {base}/v1/messages` with headers `x-api-key: {key}`, `anthropic-version: 2023-06-01` (not Bearer). The base URL has **no** `/v1`; the path adds it (v1.7 draft §2A.8.2).
- The v1.7 draft accepts undated ids that Anthropic documents as fixed snapshots (4.6 generation onward); never use `*-latest` aliases.
- Newer models accept only `temperature: 1` and have no seed, so outputs are not guaranteed deterministic; CI recompile mismatches block deploys.

### Gemini

```dotenv
LLM_PROVIDER=gemini
LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta
LLM_API_KEY=your-gemini-api-key
LLM_MODEL=gemini-2.5-pro
```

- Request: `POST {base}/models/{LLM_MODEL}:generateContent` with header `x-goog-api-key: {key}`. Never put the key in the `?key=` query string, because it ends up in logs (v1.7 draft §2A.8.2).
- Use a stable model id, not `-latest` or `-preview` names.
- `seed` is accepted but only best-effort.

### Azure OpenAI

```dotenv
OPENAI_BASE_URL=https://YOUR-RESOURCE-NAME.openai.azure.com/openai/v1/
OPENAI_API_KEY=your-azure-openai-key
OPENAI_MODEL=your-deployment-name
```

- Uses Azure's v1 API (no `api-version` parameter); `*.services.ai.azure.com/openai/v1/` also works.
- `OPENAI_MODEL` is your **deployment name**; pin the model version on the deployment and disable auto-upgrade, otherwise the model behind the name can change.
- ⚠️ Proposed v1.7 lists Azure OpenAI as **out of v1 scope**. Treat it as unsupported until the spec says otherwise.

### OpenRouter

```dotenv
OPENAI_BASE_URL=https://openrouter.ai/api/v1
OPENAI_API_KEY=sk-or-...your-key...
OPENAI_MODEL=openai/gpt-4o-2024-11-20
```

- Use a dated slug (here `openai/gpt-4o-2024-11-20`), consistent with the dated OpenAI snapshot above. Undated slugs such as `openai/gpt-4.1` are floating aliases; check the catalog (`GET https://openrouter.ai/api/v1/models`) for a dated slug of the model you want, and don't use OpenRouter for deployable locks if none exists.
- OpenRouter can route one model slug to different upstream providers and offers `~...-latest` aliases. Both break determinism: never use `latest` aliases, and pin the upstream provider if you must use it for compiling.

### Groq

```dotenv
OPENAI_BASE_URL=https://api.groq.com/openai/v1
OPENAI_API_KEY=gsk_...your-key...
OPENAI_MODEL=llama-3.3-70b-versatile
```

- Groq converts `temperature: 0` to `1e-8`, and rejects some OpenAI fields (`logprobs`, `logit_bias`, `n > 1`).
- Check that the model id is not retired/aliased before pinning it.

### Ollama (local)

```dotenv
OPENAI_BASE_URL=http://localhost:11434/v1
OPENAI_API_KEY=ollama
OPENAI_MODEL=qwen2.5-coder:32b-instruct-q8_0
```

- No key is needed; use any placeholder.
- Use a full tag, never `:latest`.
- Local-only: plain `http` works only on loopback. Locks compiled against a local endpoint are for development; CI/production compiles should use an `https` public host (proposed v1.7 forbids deploying locally compiled locks).

### LM Studio (local)

```dotenv
OPENAI_BASE_URL=http://localhost:1234/v1
OPENAI_API_KEY=lm-studio
OPENAI_MODEL=qwen2.5-coder-32b-instruct
```

- Start the local server in LM Studio first; the model id must match the loaded model exactly.
- Same local-only caveats as Ollama.

### vLLM (self-hosted)

```dotenv
OPENAI_BASE_URL=http://localhost:8000/v1
OPENAI_API_KEY=replace-me
OPENAI_MODEL=Qwen/Qwen2.5-Coder-32B-Instruct
```

- `8000` is vLLM's default port. If you start the server with `--api-key`, use that value; otherwise any placeholder works.
- Pin the model **revision** (Hugging Face commit) when starting the server; the repo name alone can change.
- For shared/CI use, put vLLM behind HTTPS with authentication.

## Security considerations

- **Never commit the key**: not in source, lock, manifest, logs, deploy reports, PR comments or error messages (§9B; proposed v1.7 Decision 57). Commit only `.env.example`.
- Keep `.env` in `.gitignore`; in CI use encrypted secrets scoped to the compile job, and do not echo them.
- The compiler key is a **compile-time** secret only. Runtime must not need it; proposed v1.7 makes the deploy fail if any `LLM_*`/`OPENAI_*` variable is present at runtime.
- The compiler sandbox has no network except (in proposed v1.7) the configured endpoint host; v1.5 says the compiler has **no network at all** (§2A.7), which conflicts with calling a hosted LLM — see Spec gaps.
- Pin the model snapshot (§2A.6). Changing provider, host or model changes `compiler-hash` and forces a full recompile and re-approval.
- Do not paste source containing secrets into prompts; the compiler already errors on plaintext credentials in provider source (§5).
- Rotate keys if they ever appear in a log or commit, and enable secret scanning.

## Spec gaps

- v1.5 does **not** specify how the compiler LLM endpoint, key or model are configured; all variable names here come from **unmerged** PR #5 (v1.7 draft).
- v1.5 §2A.7 says the compiler has "no network", which contradicts calling a remote LLM API; v1.7 draft relaxes this to "egress only to the endpoint host".
- v1.5 requires deterministic compilation (temperature 0, fixed seed, fixed snapshot), but most hosted providers (OpenAI, Groq, OpenRouter) only offer best-effort determinism, which interacts badly with "CI recompile mismatch blocks deploy with no override" (§2A.6).
- The SDK init/compile CLI commands are not specified.
- Azure OpenAI and OpenRouter provider routing are not addressed by v1.5; the v1.7 draft puts Azure out of scope.
