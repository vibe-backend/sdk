# `user.login`

## 用途

以電郵加密碼認證**已驗證**的用戶，並回傳 access token 和 refresh token。`user.login` 是一個 model method；它透過 `method` op 呼叫 `user.sign-token`（唯一可以簽發 token 的 method）。它經由一個**公開**的 api feature 提供（用戶此時尚未登入），因此寫入 `last_login_at` 屬於公開寫入（§12.8）。

在 v1.7，登入只接受電郵已驗證的用戶（`email_verified_at is not null`）；未驗證的用戶會收到與密碼錯誤完全相同的 401 `INVALID_CREDENTIALS`（Decision 55）。由於公開註冊和 admin 邀請都在同一條 UPDATE 中設定 `password` 和 `email_verified_at`（Decisions 67、69），帳戶要麼兩者皆有，要麼兩者皆無。見 [user-register.md](user-register.md)。

## 自然語言 source 範例

`models/user/methods/login.md`

```text
# login
Log in with email and password.
Input: email (valid email, required), password (1 to 256 characters, required).
Output: user (id, email, name, roles), token and refresh_token.

Steps:
1. Find the user by email; only verified, non-deleted users count.
2. Compare the password; if the user is not found or the password is wrong, fail INVALID_CREDENTIALS.
3. On success record the last login time.
4. Sign tokens with user.sign-token and return user, token and refresh_token.
```

`api/auth/v1/login/login.md`（路由 `POST /auth/v1/{slug}/login`）

```text
# Login
POST, path /auth/v1/{slug}/login.
Public, no auth, tenant is determined by slug.
This endpoint writes data (records the last login time); public writes are allowed.
Protect with login-guard: 10 per IP per minute, 5 per account per minute; also at most 300 per tenant per minute.

Request body: email (valid email, required), password (1 to 256 characters, required).
Log in with user.login; on success return 200 with token, refresh_token and user (id, email, name, roles).
Wrong credentials return 401 INVALID_CREDENTIALS; validation failure returns 422.
```

## 編譯後的 lock md 範例

`models/user/methods/login.lock.md`

> **注意：** `INVALID_CREDENTIALS` 是**自行定義的錯誤碼**，由 `user.login` method 在其 `## Signature` 的 `errors` 中宣告，並在 api lock 中對應到 401。它不是平台保留的錯誤碼，不過規格在帳戶鎖定的回應中也沿用了同一個名稱（§4A.3）。

````markdown
---
lock-version: 1
kind: method
id: user/methods/login
source: models/user/methods/login.md
source-hash: sha256:e81f...2a
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
  - { id: user/methods/sign-token, kind: method, hash: sha256:3b90...d4 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:25:00+08:00
approval-ref: pr#125
approved-commit: a1b2c125
security-items:
  - { item: write-sql, risk: medium, reason: "Records last login time", approved-by: [jack@example] }
signature: MEUCIQ...
---
## Signature
- input: { email: { type: string, format: email, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: { user: { id, email, name, roles }, token: string, refresh_token: string }
- errors: [INVALID_CREDENTIALS]
- writes: true

## Calls
- methods: [user.sign-token]
- external: (none)

## Steps
### s1 query
- op: query
- provider: main
- table: users
- sql: `select id, email, name, roles, password from users where email = $1 and deleted_at is null and email_verified_at is not null`
- params: [input.email]
- writes: false
- out: u
- source-lines: 7-7

### s2 verify-password
- op: verify-password
- cost: high
- hash: u.password
- plain: input.password
- missing-user: dummy-hash
- out: ok
- source-lines: 8-8

### s3 validate
- op: validate
- when: `ok != true`
- fail: INVALID_CREDENTIALS
- source-lines: 8-8

### s4 query
- op: query
- provider: main
- table: users
- sql: `update users set last_login_at = now() where id = $1`
- params: [u.id]
- writes: true
- scope: pk
- columns: [last_login_at]
- out: (none)
- source-lines: 9-9

### s5 method
- op: method
- target: user.sign-token
- args: { sub: u.id }
- out: t
- source-lines: 10-10

### s6 respond
- op: respond
- value: { user: { id: u.id, email: u.email, name: u.name, roles: u.roles }, token: t.token, refresh_token: t.refresh_token }
- source-lines: 10-10
````

`api/auth/v1/login/login.lock.md`（僅列 section；frontmatter 依 §2A.1，`kind: api`、`scope: feature`，安全項目為 `public`、`auth-removed`、`public-writes`，全屬高風險）

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/login

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_reader + public_writer
- writable-columns: { users: [last_login_at] }
- challenge: { type: captcha, when: after-failures, after-failures: 3 }   # 4A.3
- min-response: 500ms
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required

## Pipelines
### p1 -auth
- removed: true

### p2 login-guard
- ip-per-minute: 10
- account-per-minute: 5

### p3 rate-limit
- per-minute: 300
- key: tenant

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | | yes |
| body.email | string | format: email | yes |
| body.password | string | min: 1, max: 256 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { token, refresh_token, user: { id, email, name, roles } } | |

## Transform
- in: { email: req.body.email, password: req.body.password }
- out: { token: result.token, refresh_token: result.refresh_token, user: result.user }

## Steps
### s1 method
- op: method
- target: user.login
- args: transform.in
- out: result
- source-lines: 7-7

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 7-7

## Errors
| status | code | from |
|---|---|---|
| 401 | INVALID_CREDENTIALS | INVALID_CREDENTIALS |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

上面的 `INVALID_CREDENTIALS` 就是 `user.login` 自行定義的錯誤碼（見 method lock 前的注意事項）。

model lock 把 `last_login_at` 標記為 `public-writable`，並在 `public_writer` 授權中包含 `UPDATE(last_login_at)`（§12.8），這本身也是一項高風險的 `public-writable-column` 項目。由登入呼叫 `user.sign-token` 會令 `sign-jwt` 在**新家族**中簽發 refresh token（`issue-refresh: new-family`，由平台按呼叫來源決定，§4A.1）。

## 安全考量

- **必須已驗證電郵**（Decision 55）：s1 查詢過濾 `email_verified_at is not null`；未驗證（或從未接受邀請）的用戶會落入同一條 `missing-user: dummy-hash` 路徑，因此耗時和回應都與密碼錯誤相同。
- **防止帳戶枚舉**：`verify-password` 以固定時間比較；用戶不存在時，平台會與一個假雜湊值比較（`missing-user: dummy-hash`）。兩種情況都回傳相同的 `INVALID_CREDENTIALS`（§12.6、§8）。
- **固定回應耗時**：登入類端點（任何含 `verify-password` 步驟的端點）會補足到 **500ms**（`min-response: 500ms`）；實際超過只會發出 warning（§0.1、§7.2、Decision 36）。
- **帳戶鎖定**：15 分鐘內失敗 5 次，帳戶鎖定 15 分鐘；同一帳戶或同一 IP 在 15 分鐘內失敗 3 次後須通過 captcha（`challenge: { type: captcha, when: after-failures, after-failures: 3 }`，Decision 49）；鎖定期間的回應與密碼錯誤完全相同（401 `INVALID_CREDENTIALS`，補足到 500ms）；經電郵重設連結可即時解鎖；鎖定與解鎖都會審計，並通知用戶（§4A.3）。
- **限流**：登入必須按 IP **和**按帳戶限流（例如 `login-guard`，每個 IP 每分鐘 10 次）（§4A.3、§12.6）。`verify-password` 屬 `cost: high`，在公開路徑上自動只有端點上限的一半（§7.2）。
- **公開寫入**：只有 `last_login_at` 可以寫入；`UPDATE` 使用 `scope: pk`（§7.2）。
- `password` 欄位只可在提供給 `verify-password` 時被選取（§10.8）；`login-guard` 會遮蓋 `req.body.password`，並只記錄電郵的 HMAC（Decision 68、§11.2）。
- 排除已軟刪除的用戶（`deleted_at is null`）。
- **密碼雜湊**（Decision 46）：argon2id m=64 MiB、t=3、p=1；登入成功時若已存的 PHC 參數不同，平台以 `credential_rehash` role 自動重新雜湊（§4A.2）。lock 不需宣告這項寫入。
- **Refresh token**：與 access token 一併回傳；只以 HMAC 雜湊存在 `_platform_refresh_tokens`（Decision 45）。見 [refresh-token-rotation.md](refresh-token-rotation.md)。
- 審核重點：`public: true`、`-auth`、`public-writes: true`（每項都屬高風險，需兩位確認），以及寫入 SQL（中風險）（§12.8）。

## 規格缺口

- 鎖定狀態和失敗次數如何儲存，以及 `challenge: { when: after-failures }` 相對於步驟如何評估，屬平台內部事宜，未以 lock 的形式表達。
- §12.8 使用了 `role: public_reader + public_writer`，但 `role` 值的語法在其他地方沒有定義。
- `login-guard` 的帳戶 key 是 `input.email`（§11.2），而登入 method 的輸入亦是 `email`；pipeline 如何在 method 執行前解析 `input.*`（相對於 `req.body.email`）沒有明確說明。
