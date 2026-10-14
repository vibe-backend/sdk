# 認證 pipeline：JWT

## 用途

要求某個 API 定義下的每個端點都必須帶有效的 JWT。`auth` 是內建 pipeline（§4），其 lock 由平台提供並簽名；你只需要啟用它並傳入參數（例如 `mode`、`roles`）。在 `_definition.md` 宣告一次，所有版本和 feature 都會繼承（§3），有需要時再按 feature 收窄角色。

## 自然語言 source 範例

`api/users/_definition.md`

```text
# users
User management API. Allowed providers: main and enterprise; default is main.
All endpoints require JWT authentication.
```

`api/users/v2/create-user/create-user.md`（路由 `POST /users/v2/create-user`）

```text
# Create user
POST. Only admin may use it, at most 30 requests per minute.
The request body must contain email (valid email), name (1 to 50 characters) and password (12 to 256 characters).
Create the user with user.register; on success return 201 with the new user's id, email and name, plus Location.
If the email is already taken return 409 EMAIL_TAKEN; a weak or breached password returns 422 WEAK_PASSWORD; validation failure returns 422.
```

## 編譯後的 lock md 範例

`api/users/_definition.lock.md`（僅列 section；frontmatter 依 §2A.1，`kind: api`、`scope: definition`）

````markdown
## Providers
- allowlist: [main, enterprise]
- default: main

## Pipelines
### p1 auth
- mode: jwt

## Defaults
- timeout: 10s
- body: 1MB
````

`api/users/v2/create-user/create-user.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: users/v2/create-user
source: api/users/v2/create-user/create-user.md
source-hash: sha256:7f02...aa
source-type: natural
dependencies:
  - { id: user/methods/register, kind: method, hash: sha256:c3e1...90 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:10:00+08:00
approval-ref: pr#122
approved-commit: a1b2c122
security-items: [{ item: roles, risk: high, reason: "Only admin may create users", approved-by: [jack@example, mei@example] }]
signature: MEUCIG...
---
## Route
- method: POST
- path: /users/v2/create-user

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt
- roles: [admin]

### p2 rate-limit
- per-minute: 30

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| body.email | string | format: email | yes |
| body.name | string | min: 1, max: 50 | yes |
| body.password | string | min: 12, max: 256 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 201 | { id, email, name } | Location |

## Transform
- in: { email: req.body.email, name: req.body.name, password: req.body.password }
- out: { id: u.id, email: u.email, name: u.name }

## Steps
### s1 method
- op: method
- target: user.register
- args: transform.in
- out: u
- source-lines: 4-4

### s2 respond
- op: respond
- status: 201
- value: transform.out
- location: /users/v2/{u.id}
- source-lines: 4-4

## Errors
| status | code | from |
|---|---|---|
| 409 | EMAIL_TAKEN | user.register |
| 422 | WEAK_PASSWORD | user.register |
| 422 | VALIDATION_FAILED | platform |
````

## 安全考量

- **預設拒絕**：除非明確移除，每個端點都會經過 `auth`；移除它（`-auth`）必須同時設定 `public: true`（§3、§4、§10.3）。
- **次序**：`auth` 必須在每個 `method` 步驟及所有其他邏輯之前執行（`## Order` 宣告 `must-run: first`）（§4）。
- **演算法**：只允許 RS256 和 ES256；禁止 `none`，也不可與 HS 混用。必須驗證 `iss`、`aud`、`exp` 和 `nbf`。支援 JWKS 輪換及快取 TTL（§4）。使用其他演算法或缺少 claim 檢查屬於部署錯誤（§10.3）。
- **角色**：`roles-claim` 預設為 `roles`；`roles: []` 屬於錯誤（§4）。新增、修改、放寬或移除角色檢查屬於**高風險**安全項目，需要兩位非作者確認（§2A.5）。
- **注入 context**：認證成功後，`auth` 會注入 `ctx.tenant_id` 和 `ctx.user`；業務邏輯不能修改它們（§4、§12.5）。method 不會繼承 api pipeline，只會收到 `ctx`（§12.5）。
- **錯誤**：未認證／token 無效回傳 401，並附 `WWW-Authenticate`（RFC 6750）；已認證但無權限回傳 403（§0.1、§8）。認證失敗時絕不區分「用戶不存在」和「憑證錯誤」（§8）。
- **日誌**：token 永遠不會寫入日誌（§9）。

## 規格缺口

- 規格只展示了 `mode: jwt` 和 `roles` 這兩個 `auth` 參數。`auth` pipeline 的 `## Parameters` schema 中，issuer、audience、JWKS URL 及 JWKS 快取 TTL 的參數名稱**未有規定**（內建 lock 由平台提供）。請勿在 source 中自行杜撰，應向平台查詢內建 `auth` 的參數 schema。
- 規格自己在 §11.4 的 `create-user` 範例沒有 `password`，但用戶需要密碼才能登入；本頁加上了這個欄位，使呼叫符合 `user.register` 的 `## Signature`（見 [user-register.md](user-register.md)）。至於由管理員建立的用戶是否應改為收到重設密碼連結，規格未有規定。
- `roles-claim` 如何覆寫（lock 中的參數名稱）未有展示。
