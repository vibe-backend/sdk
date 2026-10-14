# 限流

## 用途

使用內建的 `rate-limit` pipeline（§4），或以 `rate-limit-check` op 組合的自訂 pipeline（§2A.4，例如 §11.2 的 `login-guard`），按 IP、租戶及帳戶限制請求頻率。限流設定按 `_definition` → `_version` → feature 繼承（§3）。

## 自然語言 source 範例

`api/users/v1/_version.md`

```text
# users v1
At most 60 requests per minute. This version is deprecated and will be retired on 2027-06-30.
```

`pipelines/login-guard.md`（自訂 pipeline）

```text
# login-guard
Login protection: at most 10 per minute per IP, at most 5 per minute per email.
Parameters: ip-per-minute (integer, default 10), account-per-minute (integer, default 5).
Must run before all methods, and record an audit entry (without the password).
```

同時使用 IP 和租戶 key 的 feature（取自 `submit-review.md`）：

```text
At most 5 per minute per IP and at most 100 per minute per tenant.
```

## 編譯後的 lock md 範例

`api/users/v1/_version.lock.md`（僅列 section；`scope: version`）

````markdown
## Pipelines
### p1 rate-limit
- per-minute: 60

## Defaults
(none)

## Lifecycle
- deprecated: true
- sunset: 2027-06-30
````

`pipelines/login-guard.lock.md`

````markdown
---
lock-version: 1
kind: pipeline
id: login-guard
source: pipelines/login-guard.md
source-hash: sha256:51bd...07
source-type: natural
dependencies: []
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example]
approved-at: 2026-10-14T08:05:00+08:00
approval-ref: pr#121
approved-commit: a1b2c121
security-items: [{ item: rate-limit-new, risk: medium, reason: "Login brute-force protection", approved-by: [jack@example] }]
signature: MEYCIQ...
---
## Parameters
| name | type | default | min | max |
|---|---|---|---|---|
| ip-per-minute | integer | 10 | 1 | 600 |
| account-per-minute | integer | 5 | 1 | 60 |

## Order
- must-run: before-steps
- after: [auth]

## Steps
### s1 rate-limit-check
- op: rate-limit-check
- key: req.ip
- per-minute: params.ip-per-minute
- source-lines: 2-2

### s2 rate-limit-check
- op: rate-limit-check
- key: req.body.email
- per-minute: params.account-per-minute
- source-lines: 2-2

### s3 audit
- op: audit
- event: login-attempt
- fields: [req.ip, req.body.email]
- redact: [req.body.password]
- source-lines: 4-4
````

帳戶 key `input.email` 會先正規化（去除前後空白、轉為小寫）再做 HMAC；明文永不儲存（§4.3）。審計步驟只記錄電郵的 HMAC，絕不記錄明文（Decision 68）。

feature 層級的 IP 加租戶限流（`## Pipelines` 節錄）：

````markdown
### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 100
- key: tenant
````

## 參數 schema（§4.3、Decision 65）

| 參數 | 類型 | 必填 | 說明 |
| --- | --- | --- | --- |
| `per-minute` / `per-hour` / `per-day` | 正整數 | 必須剛好一個 | 每個 step 一個時間窗 |
| `key` | `ip` \| `tenant` \| `user` \| `api-key` \| `family` \| `input.{field}` | 是 | `user` 需要 `auth`；`family` 只配合 `token-refresh`；`input.{field}` 先正規化再做 HMAC |
| `burst` | 正整數 | 否 | 預設為時間窗上限，不可超過它 |
| `fail` | 錯誤碼 | 否 | 預設 `RATE_LIMITED`（429 加 `Retry-After`） |

計數使用按 `tenant_id` 儲存的滑動時間窗。`key: ip` 使用平台按受信任 proxy 設定解析出的 IP，絕不採用用戶端提供的 header。一個端點可以有多個 `rate-limit` step；超出任何一個都會拒絕請求。

pipeline lock 的 `## Order` 只取一個 `must-run:` 值：`first`（只限 `auth` 或取代它的平台 pipeline）、`before-steps`、`after-steps`、`last` 或 `any`（§4.4）。

## 安全考量

- 超出限流時回傳 **429**，並附 `Retry-After`（由平台加上）（§8）。
- **公開端點**一律按 IP 限流，亦可加上按租戶限流。**公開寫入**必須同時有 IP **和**租戶限流，並設每日配額（§7.2、§10.7）。
- **登入**必須按 IP 及按帳戶限流（4A.3：每個 IP 每分鐘 10 次）（§12.6）。
- 公開路徑上 `cost: high` 的步驟（`verify-password`、`hash-password`、大型查詢）會自動使用端點上限的一半（§7.2、Decision 39）。
- pipeline 參數會按 `## Parameters` 驗證；未知參數或類型錯誤均屬錯誤（§4）。
- **放寬**限流、timeout、body 大小或配額，屬於中風險安全項目（§2A.5）。內層可以覆寫個別設定，但不可超過平台上限（§3）。
- 絕不可把機密資料寫入日誌；審計步驟中要遮蓋密碼（§9、§11.2）。

## 規格缺口

- 規格自己的 `_version` 範例（§11.3）寫的 `rate-limit` 只有 `per-minute` 而沒有 `key`，但 §4.3 規定 `key` 必填；本頁加上了 `key: ip`。
- 時間窗上限的平台最大值**未有規定**（只展示了 `login-guard` 本身 `## Parameters` 的 min／max）。
- pipeline 中的 `input.{field}` 指的是請求 body 還是被呼叫 method 的輸入，沒有明確說明（§11.2 在 `req.body.email` 請求中使用 `input.email`）。
