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

> ⚠️ **私隱：** 這個規格範例（§11.2）以明文審計 `req.body.email`。§12.10 規定個人資料絕不可寫入日誌，只可保留假名化 id。審計紀錄並未明言等同日誌，但請把這點視為衝突：在規格釐清前，最好不要審計原始電郵（見規格缺口）。
>
> ⚠️ **Key 正規化：** 像 `req.body.email` 這樣的帳戶 key，在計數前應先正規化（例如去除前後空白、轉為小寫），否則攻擊者可以利用大小寫變化繞過每個帳戶的限流。規格沒有在 `rate-limit-check` 中定義正規化。

feature 層級的 IP 加租戶限流（`## Pipelines` 節錄）：

````markdown
### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 100
- key: tenant
````

## 安全考量

- 超出限流時回傳 **429**，並附 `Retry-After`（由平台加上）（§8）。
- **公開端點**一律按 IP 限流，亦可加上按租戶限流。**公開寫入**必須同時有 IP **和**租戶限流，並設每日配額（§7.2、§10.7）。
- **登入**必須按 IP 及按帳戶限流（4A.3：每個 IP 每分鐘 10 次）（§12.6）。
- 公開路徑上 `cost: high` 的步驟（`verify-password`、`hash-password`、大型查詢）會自動使用端點上限的一半（§7.2、Decision 39）。
- pipeline 參數會按 `## Parameters` 驗證；未知參數或類型錯誤均屬錯誤（§4）。
- **放寬**限流、timeout、body 大小或配額，屬於中風險安全項目（§2A.5）。內層可以覆寫個別設定，但不可超過平台上限（§3）。
- 絕不可把機密資料寫入日誌；審計步驟中要遮蓋密碼（§9、§11.2）。

## 規格缺口

- 內建 `rate-limit` pipeline 的完整參數 schema 尚未公開；範例中只出現 `per-minute` 和 `key`（`ip`、`tenant`）。每秒／每小時的時間窗及突發（burst）設定**未有規定**。
- 省略 `key` 時的預設值（例如上面的 `_version` 只有 `per-minute`）**未有規定**。
- `login-guard` 宣告 `must-run: before-steps`，而 `auth` 使用 `must-run: first`；`## Order` 的完整取值範圍沒有列出。
- 限流的平台上限未有規定。
- **私隱衝突**：§11.2 的 `login-guard` 審計 `req.body.email`（個人資料，依 §12.10 屬 `personal: standard`），但 §12.10 禁止在日誌中出現個人資料，並要求刪除審計只用假名化 id。`audit` 紀錄是否算作日誌，以及 `audit` 是否支援把欄位假名化，**未有規定**；應由規格負責人決定。
- `rate-limit-check` key 的正規化（大小寫、空白、Unicode）**未有規定**。
