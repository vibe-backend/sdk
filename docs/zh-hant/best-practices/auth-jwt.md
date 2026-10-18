# Auth pipeline：JWT

## 用途

要求 API 定義下的每個 endpoint 都帶有效的 JWT。`auth` 是內置 pipeline（§4），其 lock 由平台提供並簽名；你只需啟用它並傳入參數（`mode`、`roles`，以及選填的 `iss`、`aud`、`jwks-uri`）（§4.1、Decision 44）。在 `_definition.md` 宣告一次，即由所有 version 和 feature 繼承（§3），需要時再按 feature 限制 roles。

範例 feature 是 §11.4 的 admin **create-user** endpoint。v1.7 起它不再設定密碼：它建立一個未驗證、沒有密碼的用戶，並寄出**邀請**（Decision 69）。

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
The request body must contain email (valid email) and name (1 to 50 characters).
Create the user with user.register (no password, unverified), then send an invitation with user.invite; on success return 201 with the new user's id, email and name, plus Location.
If the email is already taken return 409 EMAIL_TAKEN; validation failure returns 422.
```

## 編譯後的 lock md 範例

`api/users/_definition.lock.md`（僅列出 section；frontmatter 依照 §2A.1，`kind: api`、`scope: definition`）

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

沒有指定 `iss`／`aud`／`jwks-uri` 時使用平台預設值：`iss` = `https://auth.{platform-domain}/{project}`、`aud` = `{project}`、`jwks-uri` = `{iss}/.well-known/jwks.json`（§4.1）。

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
  - { id: user/methods/invite, kind: method, hash: sha256:<...> }
  - { id: user/methods/register, kind: method, hash: sha256:c3e1...90 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
vibe-version: 1.0.0-rc.3
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
- key: user

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| body.email | string | format: email | yes |
| body.name | string | min: 1, max: 50 | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 201 | { id, email, name } | Location |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { id: u.id, email: u.email, name: u.name }

## Steps
### s1 method
- op: method
- target: user.register
- args: transform.in
- out: u
- source-lines: 4-4

### s2 method
- op: method
- target: user.invite
- args: { id: u.id }
- out: (none)
- source-lines: 4-4

### s3 respond
- op: respond
- status: 201
- value: transform.out
- location: /users/v2/{u.id}
- source-lines: 4-4

## Errors
| status | code | from |
| --- | --- | --- |
| 409 | EMAIL_TAKEN | EMAIL_TAKEN |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

> ⚠️ 規格本身 §11.4 的 lock 只有 `user.register` 一個 step，但它的 source 寫明「再用 user.invite 寄邀請信」。省略邀請 step 的 lock 會比 source **少**；本頁加上 `s2`，令 lock 與 source 及 Decision 69 一致。`user.invite` 的輸入（`{ id }`）和 `key: user` 限流鍵僅作示意，見規格缺口。

## 邀請流程（Decision 69、§12.11.9）

1. Admin 呼叫 create-user → `user.register` 建立**沒有密碼**、`email_verified_at = null` 的用戶（自 migration `0004` 起 `password` 欄位可為 null，§12.11.1）。
2. `user.invite` 用 `random-token` 產生 token，把其 hash 寫入 `users.email_verify_hash`，`email_verify_expires_at` 設為 **72 小時**後，`email_verify_purpose = invite`，再經 `email-outbox.enqueue`（lock 寫死 `purpose: invite`）以 `invite` 範本排入郵件。
3. 用戶打開邀請連結，在與自行註冊相同的設定密碼頁設定密碼，頁面送到 `POST /auth/v1/{slug}/verify-email` → `user.complete-verification`。一個 UPDATE 同時設定 `password` **和** `email_verified_at`；之後撤銷所有 refresh token 家族（Decision 67）。見 [user-register.md](user-register.md)。
4. 在此之前用戶沒有密碼且未驗證，因此無法登入（Decision 55）。
5. 重新邀請會覆寫 token hash 和到期時間，舊邀請即時失效。過期未接受的邀請，由與未驗證自行註冊相同的清理 job 匿名化並軟刪除（§12.11.4）。
6. 有未過期邀請的電郵被公開註冊時，邀請不受影響；註冊者只會收到中性的「已經有帳戶」通知（§12.11.4）。

## 安全考量

- **預設拒絕**：除非明確移除，每個 endpoint 都經過 `auth`；移除（`-auth`）必須同時 `public: true`（§3、§4、§10.3）。
- **次序**：`auth` 最先執行（`must-run: first`，§4.4）。
- **JWT 驗證**（§4.1、Decision 44）：`iss` 必須完全相等；`aud` 必須包含設定值；`jwks-uri` 必須是 https；JWKS 快取 10 分鐘，遇到未知 `kid` 即重新拉取（同一 issuer 最少相隔 60 秒），拉取失敗時最多沿用舊快取 1 小時，之後所有 token 回 401；`exp`／`nbf`／`iat` 容許 60 秒時鐘偏差；alg 只准 ES256 或 RS256（RSA 至少 2048 bit），禁止 `none`／HS*，header 必須有 `kid`；`exp` 距離 `iat` 超過 1 小時的 token 一律拒絕。
- 修改 `iss`、`aud` 或 `jwks-uri`（例如接外部 IdP）屬**高風險**安全項目（§2A.5）。
- **Roles**：`roles-claim` 預設為 `roles`；`roles: []` 報 error（§4）。新增、修改或放寬 role 檢查屬高風險（§2A.5）。
- **Context**：`auth` 注入 `ctx.tenant_id` 和 `ctx.user`；Logic 不可修改（§4、§12.5）。
- **Admin 不設密碼**：admin 永遠不會設定或看到用戶的密碼；用戶經邀請連結自行設定（Decision 69）。admin 路徑經認證且只限 admin，因此可以回 409 `EMAIL_TAKEN`；公開路徑則永遠不可以（§12.11）。
- **錯誤**：缺少或無效 token 回 401 並帶 `WWW-Authenticate`（RFC 6750），role 不足回 403（§8）。
- Token 永不寫入 log（§9）。

## 規格缺口

- §11.4 的 lock 省略了其 source 描述的 `user.invite` step（lock 比 source 少）。本頁已補上。
- 規格沒有提供 `user.register`（admin）或 `user.invite` 的 lock；它們的 `## Signature`（例如邀請的輸入）**未有規定**。
- §11.4 的 `p2 rate-limit` 沒有 `key`，但 §4.3 規定 `key` 必填。本頁的 `key: user` 屬假設。
