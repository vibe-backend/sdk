# LLM provider 設定（compiler endpoint）

## 用途

compiler 使用 LLM 把自然語言 source `.md` 轉為 lock md（§2.1、§2A.6）。互動建立 source 同重新產生過時自然語言 lock 需要 LLM provider、endpoint、key 和固定的 model（§2A.8、Decision 57）。endpoint **只在 deploy／compile 時**使用；fresh-lock build 嘅驗證同 dev manifest 輸出唔會呼叫 LLM，runtime 都不使用它，runtime 亦絕不會取得任何 LLM key（§2A.8.1、§2A.8.5）。

## 自然語言 source 範例

這裡沒有 source `.md`：endpoint 是設定，而不是定義。它放在環境變數中（§2A.8.3）：

| 變數 | 必填 | 規則 |
| --- | --- | --- |
| `LLM_PROVIDER` | 否 | `openai`（預設）、`anthropic` 或 `gemini` |
| `LLM_BASE_URL` | 否 | 預設為 provider 的 base URL（見下表） |
| `LLM_API_KEY` | 是 | 只可經環境變數、`.env`（gitignored）或 CI secret 提供；不需要 key 的本地 API 可填任何佔位值 |
| `LLM_MODEL` | 是 | provider 文件列為**非浮動**的 model id／snapshot id（Decision 58） |
| `LLM_TIMEOUT` | 否 | 預設 60s，上限 300s |
| `LLM_MAX_RETRIES` | 否 | 網絡重試次數，預設 2，上限 5 |
| `LLM_SEED` | 否 | 只在 model 支援 seed 時傳送；預設 `0`；記喺 build-record `compilers`，唔計入 `vibe-version`（決定 174） |
| `LLM_ALLOW_PRIVATE_HOST` | 否 | 預設 `false`；只限本地開發 |

`OPENAI_BASE_URL`、`OPENAI_API_KEY`、`OPENAI_MODEL` 是只在 `LLM_PROVIDER=openai` 時接受的**別名**。`LLM_*` 優先；兩者都設定而值不同屬錯誤。配合其他 provider 設定任何 `OPENAI_*` 也屬錯誤（§2A.8.3）。

`.env.example`（提交到 repo，不含真實值；取自 §2A.8.5）

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

`.env`（只在本地）存放真實 key，必須列入 `.gitignore`；repo 只可包含 `.env.example`。

GitHub Actions（取自 §2A.8.8；`vibe@0.1.0` 是規格中示意性、暫定的 CLI 名稱和版本；一律固定確切版本，並以完整 commit SHA 固定 actions）：

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

Fork PR 沒有 secrets，因此只執行 validator，不重新編譯（§2A.8.5）。deploy job 不可設定任何 `LLM_*` 或 `OPENAI_*` 變數。

## 編譯後的 lock md 範例

決定 174：provider、public endpoint host、model、seed 同 sent parameters 記喺 build record 同 manifest provenance 嘅 `compilers` map。Lock 只記 `vibe-version`：正式版（包括 0.x）用 major.minor，pre-release 用完整版本，必須完全一致（決定 176）。

決定 180 更新決定 174：private endpoint 只記 `endpoint-private: true`，host 永遠唔以明文、hash 或任何衍生值記喺 build record、dev／release manifest provenance 或產生嘅 project／artifact 檔案；public host 照舊。舊 record／manifest 仍然 parse，舊 signed dev manifest 仍然驗簽；build 重寫時刪走 private host。唯一例外係 `.vibe/cache` key，cache 永遠唔 commit、唔打包；`vibe build -o <manifest-path>`／`--release` 只寫 manifest，runtime artifact loader 只讀 manifest 同 release signature bundle，唔讀 cache。互動設定可能寫 `LLM_BASE_URL` 入 `.env`，呢個用戶設定保持不變。

````markdown
vibe-version: 1.0.0-rc.7
````

`model-snapshot` 是 **provider 回報**的 model id（`modelReported`），而非用戶輸入；若與 `LLM_MODEL` 不同（例如 provider 解析了別名）屬錯誤。`sent-params` 是 adapter 實際送出的參數集（Decision 58、§2A.8.6）。

## Providers（Decisions 57–59、§2A.8.2）

請求路徑和 header 由 SDK adapter 固定（Decision 59）：

| `LLM_PROVIDER` | 涵蓋 | 預設 base URL | 請求 | 認證 header |
| --- | --- | --- | --- | --- |
| `openai`（預設） | OpenAI 及所有 OpenAI 相容 endpoint，包括 Ollama、LM Studio、vLLM | `https://api.openai.com/v1` | `POST {base}/chat/completions` | `Authorization: Bearer {LLM_API_KEY}` |
| `anthropic` | Anthropic Messages API | `https://api.anthropic.com` | `POST {base}/v1/messages` | `x-api-key: {LLM_API_KEY}`、`anthropic-version: 2023-06-01` |
| `gemini` | Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` | `POST {base}/models/{LLM_MODEL}:generateContent` | `x-goog-api-key: {LLM_API_KEY}`（絕不放在 query string） |

`{base}` 是去掉結尾 `/` 的 `LLM_BASE_URL`（或預設值）。`openai` 的 base 包含 `/v1`；`anthropic` 的 base 不包含（路徑會加上 `/v1`）；`gemini` 的 base 包含 `/v1beta`。`openai` 只使用 Chat Completions；Responses API 和 `azure-openai` provider 屬未來工作（§14）。

**確定性參數**（Decision 58）：adapter 只在其內置 capability 表顯示 model 支援時，才傳送 `temperature`／`top_p`／`top_k`／`seed`；絕不強制傳送 model 會拒絕的值。不在表內的 model 只收到必要參數（並出 warning）。確定性只由 CI 重新編譯比對保證：任何不一致都會阻止部署，不可覆蓋（§2A.6）。已知限制（§2A.8.2）：較新的 Anthropic model 只接受 `temperature: 1` 且沒有 seed；OpenAI 的 `seed` 只屬盡力而為；Gemini 接受 `seed`，但同樣只屬盡力而為。

### OpenAI

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_API_KEY=sk-...your-key...
OPENAI_MODEL=gpt-4.1-2025-04-14
```

- 沒有日期的 OpenAI 別名（例如 `gpt-4.1`）是浮動的，會被拒絕；請使用 snapshot id（Decision 58 以 provider 文件判斷，而非看有沒有日期）。
- adapter 把回應的 `system_fingerprint` 寫入 compile 報告（不寫入 `vibe-version`）。

### Anthropic

```dotenv
LLM_PROVIDER=anthropic
LLM_BASE_URL=https://api.anthropic.com
LLM_API_KEY=sk-ant-...your-key...
LLM_MODEL=claude-sonnet-4-6
```

- 必須使用 `LLM_*` 名稱；配合此 provider 使用 `OPENAI_*` 屬錯誤。
- Anthropic 文件列為固定 snapshot 的無日期 id（4.6 世代起）可以使用；`*-latest` 別名不可以（§2A.8.3）。

### Gemini

```dotenv
LLM_PROVIDER=gemini
LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta
LLM_API_KEY=your-gemini-api-key
LLM_MODEL=gemini-2.5-pro
```

- 使用穩定的 model id，不要用 `-latest`／`-preview` 名稱。key 放在 `x-goog-api-key`，絕不用 `?key=`。

### OpenRouter（經 `openai`）

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://openrouter.ai/api/v1
OPENAI_API_KEY=sk-or-...your-key...
OPENAI_MODEL=openai/gpt-4o-2024-11-20
```

- 使用 `GET https://openrouter.ai/api/v1/models` 列出的有日期 slug；無日期 slug 和 `~...-latest` 別名是浮動的。OpenRouter 亦可能把同一 slug 路由到不同上游，因此回報的 model id 可能與 `LLM_MODEL` 不符（錯誤），重新編譯結果也可能不同；除非固定上游，否則不要用於可部署的 lock。

### Groq（經 `openai`）

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://api.groq.com/openai/v1
OPENAI_API_KEY=gsk_...your-key...
OPENAI_MODEL=llama-3.3-70b-versatile
```

- Groq 會把 `temperature: 0` 轉為 `1e-8`，並拒絕部分 OpenAI 欄位；不在 adapter capability 表內的 model 本來就不會收到取樣參數（Decision 58）。

### Ollama、LM Studio、vLLM（本地，經 `openai`）

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=http://localhost:11434/v1   # Ollama; LM Studio: http://localhost:1234/v1; vLLM: http://localhost:8000/v1
OPENAI_API_KEY=local-placeholder
OPENAI_MODEL=qwen2.5-coder:32b-instruct-q8_0
```

- 使用完整 tag，絕不用 `:latest`。Ollama 只回報 tag，因此 `model-snapshot` 記錄的是 tag；同一 tag 可能被重新 pull 成不同檔案（§2A.8.3）。
- 純 `http` 只准用於 loopback（`localhost`、`127.0.0.1`、`::1`）；其他私有 host 需要 `LLM_ALLOW_PRIVATE_HOST=true`；兩者都只限本地開發，並記錄 `endpoint-private: true`（§2A.8.4）。
- 以私有 endpoint 編譯的 lock **不可部署**；請在 CI 以公開 https endpoint 重新編譯（§2A.8.4）。

## 安全考量

- **絕不提交 key**：不可出現在 repo、source、lock、manifest、log、部署報告、PR 留言或錯誤訊息；錯誤只可說「`LLM_API_KEY` 無效或缺少」（§2A.8.5）。CI 會對 repo、lock、manifest、部署報告和 CI log 做 secret 掃描，懷疑有 key 即令 build 失敗。
- `.env` 必須被 gitignore；從 `.gitignore` 移除它或改變 CI secret 用法受 CODEOWNERS 保護（§2A.8.5、§9B）。
- 開發環境以外，runtime 不可有任何 `LLM_*` 或 `OPENAI_*` 變數；在開發環境，`vibe run` 會出 warning 並在啟動前移除這些變數（§2A.8.5）。
- **Sandbox**：compiler 的對外網絡只限 endpoint host；沒有 tools、function calling 或 streaming；除 `LLM_API_KEY` 外沒有其他 secret（§2A.7、§2A.8.6）。
- CI 和 production 編譯必須用公開 host 的 `https://`；在 CI 使用 loopback、私有 host 或 `LLM_ALLOW_PRIVATE_HOST=true` 屬錯誤（§2A.8.4）。
- `LLM_ALLOW_PRIVATE_HOST=true` 亦適用於解析至非公開地址的網域名稱，例如 fake-IP DNS 代理；只限本地開發。即使名稱看似公開，編譯結果亦會記錄 `endpoint-private: true`。 公開網域名稱仍須使用 HTTPS；CI 和 production 拒絕此旗標。
- 本地測試私有 endpoint 編譯的 lock，可用 `vibe build`。Manifest 簽署和其他驗證檢查維持不變；CI 同 release build 拒絕 private-endpoint lock。 Dev manifest 的簽署內容包含 `build_mode: "dev"`，`vibe run` 只在開發環境接受 （可配合 `--db memory`）；release manifest 不接受私有 endpoint lock。
- 更換 provider、endpoint host 或 model 屬 **major** 變更：所有 lock 重新編譯，涉及自然語言／安全的 lock 需要重新審批。只改 base URL 的 path 不屬 major，但 CI 確定性檢查仍須通過（§2A.8.7）。

## Release host allowlist

`vibe build --release` 只會把 LLM API key 同編譯 prompt 送到寫死喺 `RELEASE_LLM_HOSTS`（`src/llm.rs`）嘅 hostname。呢份清單唔會從環境變數、project 檔或者 `LLM_BASE_URL` 讀取。設定 `LLM_RELEASE_HOSTS` 或者任何其他變數都唔可以加 host。

比對係 URL hostname 嘅 ASCII 大小寫不敏感完全相等。path 同 query 唔參與決定。冇 wildcard、子字串或者後綴匹配。`*.clinicone.me` 唔准。

| Hostname | 角色 |
| --- | --- |
| `api.openai.com` | OpenAI |
| `api.anthropic.com` | Anthropic |
| `generativelanguage.googleapis.com` | Gemini |
| `ai-proxy-sub2api.clinicone.me` | ClinicOne OpenAI 相容 proxy |

Release URL 仲要通過以下檢查：

- Scheme 必須係 `https`。純 `http` 會拒絕。
- TLS 證書會驗證。無效證書唔會接受。
- Allowlist 只比對 hostname。帶 userinfo（`user:pass@host`）會拒絕。
- Port 只准 https 預設 443。非標準 port（例如 `:8443`）會拒絕。
- Host 必須係 DNS 名稱。IP literal 會拒絕。
- 唔會跟 redirect。Redirect 去任何其他 host，包括唔喺 allowlist 嘅 host，都會令 release build 失敗。

**供應鏈風險。** `ai-proxy-sub2api.clinicone.me` 喺 release 信任邊界上。佢唔係 OpenAI、Anthropic 或者 Google 官方 endpoint。呢個 proxy 一旦被入侵，就會影響 release 編譯：API key 同源碼 prompt 都會經過佢。呢個條目只允許呢個精確 hostname。

## 規格缺口

- CLI 名稱 `vibe` 及其版本在規格中屬暫定（§2A.8.8）。
- adapter capability 表（哪些 model 收到哪些參數）隨 SDK 提供，沒有在規格中公開。
- 規格的 CI 範例把 `LLM_BASE_URL` 存為 secret；base URL 本身並非機密，規格亦沒有說明可否改用 repository variable。
- 規格沒有點名第三方 provider（OpenRouter、Groq）；它們只以「OpenAI 相容 endpoint」涵蓋。

## 跑 project 同 artifact（決定 177）

本地流程係互動 `vibe` → `vibe build` → `vibe run` →
`curl http://127.0.0.1:8080/hello/v1/dev/hello`。
Project mode 驗證 lock，喺記憶體產生 dev manifest，永遠唔叫 LLM。
Lock 缺少、過時或者版本唔啱會報錯，提示行 `vibe build`。

- `--project <dir>` 揀 project，預設目前目錄。
- `--artifact <path>` 接受 manifest 檔或者包含 `manifest.json` 嘅目錄，
  可以係 `vibe build -o` 或者 release artifact；release bundle 係
  `<manifest>.sigstore.json` 或 `VIBE_MANIFEST_BUNDLE`，Sigstore 同 attestation 驗證照舊。
- `--addr <address>` 預設 `127.0.0.1:8080`。
- `--db memory|postgres` 或環境變數 `VIBE_DB` 揀 store；預設 memory 只限開發。
  PostgreSQL 用現有 `VIBE_DATABASE_URL` 或 `DATABASE_URL` 同租戶設定。
- `--allow-test-build` 只喺 development 准許 test build。

## 冇 key 嘅測試 build 同 key file

`LLM_PROVIDER=fake` 係確定性、唔連網嘅 provider，畀 CI 做冇 key 嘅測試 build。
只支援 public GET 常數 JSON object API response，預設 `{"message":"hello world"}`；
英文或者中文 hello source 唔使寫 GET/public 字眼。寫入、認證、model 同 method 一律拒絕；
翻譯原樣回傳輸入，唔係通用 LLM。`test_build` 只記喺 `.vibe/build-record.json`
同 manifest provenance，永遠唔入 lock（決定 174）。`vibe run` 冇
`--allow-test-build` 就拒絕，非 development 一律拒絕。
`vibe build --release` 永遠拒絕 fake provider。

`LLM_API_KEY_FILE` 從檔案讀 key，例如 BuildKit secret `/run/secrets/llm_api_key`。
尾部換行會 trim（頭尾空白亦會 trim）。優先次序係 `LLM_API_KEY` >
`VIBE_LLM_API_KEY`（release build）> `LLM_API_KEY_FILE`；key 永遠唔顯示。
Runtime 唔應該有編譯用 LLM 變數；development 喺啟動 handler 前移除，其他環境拒絕。
