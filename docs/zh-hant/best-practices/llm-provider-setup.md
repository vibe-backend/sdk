# LLM provider 設定（編譯器 endpoint）

## 用途

編譯器使用 LLM 把自然語言 source `.md` 轉換成 lock md（§2.1、§2A.6）。因此，初始化專案和執行編譯都需要一個 **OpenAI 相容的 base URL 及 API key**。本頁說明如何安全地提供它們，並列出常見 provider 的現成設定。

> ⚠️ **規格狀態。** 規格 v1.5（`docs/spec-v1.5.md`）**沒有**定義如何設定編譯器 endpoint 或 key；它只要求固定的模型 snapshot、`compiler-hash`、確定性設定，以及沒有網絡和機密資料的沙盒（§2A.6、§2A.7）。以下變數名稱依照**規格 v1.7**（[`docs/spec-v1.7.md`](../../spec-v1.7.md)，Decision 57、§2A.8），該版本現已合併。本頁依據 v1.7 草稿撰寫，尚未按定稿文字重新核對（例如 Decisions 58–59 細化了確定性參數及請求路徑）；如有出入，以規格為準：
>
> - 中立名稱 `LLM_PROVIDER`（預設 `openai`）、`LLM_BASE_URL`、`LLM_API_KEY`（必填）、`LLM_MODEL`（必填，固定 snapshot）。
> - 只有在 `LLM_PROVIDER=openai` 時，才接受 `OPENAI_BASE_URL`、`OPENAI_API_KEY`、`OPENAI_MODEL` 作為**別名**；`LLM_*` 優先，值有衝突則屬錯誤。
>
> 規格 v1.7 透過 `LLM_PROVIDER` 支援三個 provider：`openai`（預設；涵蓋 OpenAI 及所有 OpenAI 相容 endpoint，使用 Bearer 認證）、`anthropic` 和 `gemini`。OpenAI 相容的範例使用 `OPENAI_*` 別名；Anthropic 和 Gemini 必須使用 `LLM_*`。

## 自然語言 source 範例

這裡沒有 source `.md`：endpoint 屬於設定，而不是定義。設定存放在環境變數中。

`.env.example`（會提交，不含真實值）

```dotenv
LLM_PROVIDER=openai
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_API_KEY=replace-me
OPENAI_MODEL=gpt-4.1-2025-04-14
```

`.env`（只存於本機，**已加入 gitignore**）包含真實的 key。請確保 `.gitignore` 中有：

```text
.env
```

在 CI 中，請把這些值設定為 **CI secrets**（例如 GitHub Actions 的 `secrets.OPENAI_API_KEY`），並只提供給編譯 job：

```yaml
- name: Compile locks
  env:
    LLM_PROVIDER: openai
    OPENAI_BASE_URL: ${{ vars.OPENAI_BASE_URL }}   # not secret: repository variable
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
    OPENAI_MODEL: gpt-4.1-2025-04-14
  run: echo "run the SDK compile command here"
```

> 兩個規格版本都沒有定義 SDK CLI 的編譯指令名稱；指令推出後，請把佔位符換成真正的指令。

## 編譯後的 lock md 範例

endpoint 只會透過 `compiler` frontmatter 及 `compiler-hash` 影響 lock（§2A.1）。在 v1.5 中是：

````markdown
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
````

規格 v1.7 另外會記錄 provider 及 endpoint host（只記 host，絕不記 key）：

````markdown
compiler: { name: md-logic, version: 2.0.0, model-snapshot: <model id reported by the provider>, llm-provider: openai, endpoint-host: api.openai.com, endpoint-private: false, llm-seed: 0, sent-params: { temperature: 0, top_p: 1, seed: 0 } }
````

## Provider 範例

各 base URL 已按各 provider 的官方文件核對（2026 年 10 月）。模型名稱僅為**範例**；請一律固定使用不會浮動的 snapshot id（v1.5 §2A.6 禁止浮動別名）。

### OpenAI

```dotenv
OPENAI_BASE_URL=https://api.openai.com/v1
OPENAI_API_KEY=sk-...your-key...
OPENAI_MODEL=gpt-4.1-2025-04-14
```

- 認證：在 `POST {base}/chat/completions` 上使用 `Authorization: Bearer {key}`（v1.7 §2A.8.2）。本頁所有 OpenAI 相容的 provider 都使用同一個 Bearer header。
- 請使用帶日期的 snapshot，而不是 `gpt-4.1` 這類沒有日期的別名。
- OpenAI 把 `seed` 標示為 beta／盡力而為；不保證確定性。

### Anthropic

Anthropic 和 Gemini 的部分使用 `LLM_*` 名稱：`OPENAI_*` 別名**只**在 `LLM_PROVIDER=openai` 時接受，配搭其他 provider 設定它們屬於錯誤（v1.7 §2A.8.3）。

```dotenv
LLM_PROVIDER=anthropic
LLM_BASE_URL=https://api.anthropic.com
LLM_API_KEY=sk-ant-...your-key...
LLM_MODEL=claude-sonnet-4-6
```

- 請求：`POST {base}/v1/messages`，header 為 `x-api-key: {key}`、`anthropic-version: 2023-06-01`（不是 Bearer）。base URL **不含** `/v1`，由路徑加上（v1.7 §2A.8.2）。
- 規格 v1.7 接受 Anthropic 文件列明為固定 snapshot 的無日期 id（4.6 代及之後）；絕不可使用 `*-latest` 別名。
- 較新的模型只接受 `temperature: 1`，也沒有 seed，因此不保證輸出確定；CI 重新編譯結果不一致會封鎖部署。

### Gemini

```dotenv
LLM_PROVIDER=gemini
LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta
LLM_API_KEY=your-gemini-api-key
LLM_MODEL=gemini-2.5-pro
```

- 請求：`POST {base}/models/{LLM_MODEL}:generateContent`，header 為 `x-goog-api-key: {key}`。絕不可把 key 放在 `?key=` query string 中，因為它會出現在日誌裡（v1.7 §2A.8.2）。
- 請使用穩定的模型 id，而不是 `-latest` 或 `-preview` 名稱。
- 接受 `seed`，但只屬盡力而為。

### Azure OpenAI

```dotenv
OPENAI_BASE_URL=https://YOUR-RESOURCE-NAME.openai.azure.com/openai/v1/
OPENAI_API_KEY=your-azure-openai-key
OPENAI_MODEL=your-deployment-name
```

- 使用 Azure 的 v1 API（沒有 `api-version` 參數）；`*.services.ai.azure.com/openai/v1/` 也可以使用。
- `OPENAI_MODEL` 是你的**部署名稱**；請在部署上固定模型版本並停用自動升級，否則名稱背後的模型可能會改變。
- ⚠️ 規格 v1.7 把 Azure OpenAI 列為**不在 v1 範圍內**。在規格另有說明前，請視為不支援。

### OpenRouter

```dotenv
OPENAI_BASE_URL=https://openrouter.ai/api/v1
OPENAI_API_KEY=sk-or-...your-key...
OPENAI_MODEL=openai/gpt-4o-2024-11-20
```

- 請使用帶日期的 slug（這裡是 `openai/gpt-4o-2024-11-20`），與上面帶日期的 OpenAI snapshot 一致。`openai/gpt-4.1` 這類沒有日期的 slug 屬於浮動別名；請在目錄（`GET https://openrouter.ai/api/v1/models`）中找出所需模型帶日期的 slug，如果沒有，就不要用 OpenRouter 編譯要部署的 lock。
- OpenRouter 可能把同一個模型 slug 路由到不同的上游 provider，也提供 `~...-latest` 別名。兩者都會破壞確定性：絕不可使用 `latest` 別名，如必須用它編譯，請固定上游 provider。

### Groq

```dotenv
OPENAI_BASE_URL=https://api.groq.com/openai/v1
OPENAI_API_KEY=gsk_...your-key...
OPENAI_MODEL=llama-3.3-70b-versatile
```

- Groq 會把 `temperature: 0` 轉換成 `1e-8`，並拒絕部分 OpenAI 欄位（`logprobs`、`logit_bias`、`n > 1`）。
- 固定模型 id 前，請先確認它沒有退役或成為別名。

### Ollama（本機）

```dotenv
OPENAI_BASE_URL=http://localhost:11434/v1
OPENAI_API_KEY=ollama
OPENAI_MODEL=qwen2.5-coder:32b-instruct-q8_0
```

- 不需要 key；任何佔位值都可以。
- 請使用完整 tag，絕不使用 `:latest`。
- 只限本機：純 `http` 只適用於 loopback。以本機 endpoint 編譯的 lock 只供開發使用；CI／production 編譯應使用 `https` 公開 host（v1.7 禁止部署在本機編譯的 lock）。

### LM Studio（本機）

```dotenv
OPENAI_BASE_URL=http://localhost:1234/v1
OPENAI_API_KEY=lm-studio
OPENAI_MODEL=qwen2.5-coder-32b-instruct
```

- 請先在 LM Studio 中啟動本機伺服器；模型 id 必須與已載入的模型完全相同。
- 與 Ollama 一樣只限本機使用。

### vLLM（自行託管）

```dotenv
OPENAI_BASE_URL=http://localhost:8000/v1
OPENAI_API_KEY=replace-me
OPENAI_MODEL=Qwen/Qwen2.5-Coder-32B-Instruct
```

- `8000` 是 vLLM 的預設連接埠。如果啟動伺服器時使用了 `--api-key`，請使用該值；否則任何佔位值都可以。
- 啟動伺服器時請固定模型的 **revision**（Hugging Face commit）；只用 repo 名稱的話，內容可能會改變。
- 如供多人共用或 CI 使用，請把 vLLM 放在有認證的 HTTPS 後面。

## 安全考量

- **絕不提交 key**：不可出現在 source、lock、manifest、日誌、部署報告、PR 留言或錯誤訊息中（§9B；v1.7 Decision 57）。只提交 `.env.example`。
- 把 `.env` 加入 `.gitignore`；在 CI 中使用只限編譯 job 的加密 secrets，並且不要把它們輸出。
- 編譯器 key 只是**編譯時**的機密。執行階段不應需要它；v1.7 規定，如果執行階段存在任何 `LLM_*`／`OPENAI_*` 變數，部署就會失敗。
- 編譯器沙盒除了（在 v1.7 中）所設定的 endpoint host 之外沒有網絡；v1.5 則說編譯器**完全沒有網絡**（§2A.7），這與呼叫託管 LLM 互相矛盾，見規格缺口。
- 固定模型 snapshot（§2A.6）。更換 provider、host 或模型都會改變 `compiler-hash`，並強制全面重新編譯及重新確認。
- 不要把含有機密資料的 source 貼進 prompt；編譯器本身已會對 provider source 中的明文憑證報錯（§5）。
- 如 key 曾出現在日誌或 commit 中，請立即輪換，並啟用機密掃描。

## 規格缺口

- v1.5 **沒有**規定如何設定編譯器的 LLM endpoint、key 或模型；這裡所有變數名稱都來自規格 v1.7（Decision 57）。
- v1.5 §2A.7 說編譯器「沒有網絡」，這與呼叫遠端 LLM API 互相矛盾；v1.7 把它放寬為「只准連線到 endpoint host」。
- v1.5 要求確定性編譯（temperature 0、固定 seed、固定 snapshot），但大部分託管 provider（OpenAI、Groq、OpenRouter）只提供盡力而為的確定性，這與「CI 重新編譯結果不一致即封鎖部署，且不設 override」（§2A.6）配合得並不好。
- SDK 的 init／compile CLI 指令未有規定。
- v1.5 沒有處理 Azure OpenAI 及 OpenRouter 的 provider 路由問題；v1.7 把 Azure 列為不在範圍內。
