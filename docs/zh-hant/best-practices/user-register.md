# 註冊：`user.self-register`、`user.complete-verification` 與 admin `user.register`

## 用途

讓訪客註冊，同時避免帳戶預先劫持（pre-hijacking）和電郵列舉。規格 v1.7 把公開註冊定義為兩步流程（§12.11、Decisions 52–56、67）：

1. **註冊**（`POST /auth/v1/{slug}/register` → `user.self-register`）：**只接受電郵和名稱，絕不接受密碼**。它建立一個未驗證、沒有密碼的用戶（若電郵已存在而未驗證，則只輪換驗證 token），放入一封電郵，而且無論電郵是否已存在，**一律回 202 和相同的 body**（Decisions 56、67）。
2. **驗證並設定密碼**（`POST /auth/v1/{slug}/verify-email` → `user.complete-verification`）：電郵連結開啟設定密碼頁面；以一條 UPDATE 同時設定 `password` **和** `email_verified_at`，並清除 token。之後撤銷該用戶所有 refresh token 家族（Decision 67）。

登入只接受已驗證的用戶（Decision 55，見 [user-login.md](user-login.md)）。Admin 路徑使用另一個 method `user.register`，可回 409 `EMAIL_TAKEN`，之後接邀請流程（Decision 69，見 [auth-jwt.md](auth-jwt.md)）。公開 method 絕不可透露電郵是否存在（§12.11）。

§12.11 的前置條件：migration `0004-add-email-verification`（可為 null 的 `password`、`email_verified_at`、`email_verify_hash`、`email_verify_expires_at`、`email_verify_purpose`，以及 `email_outbox` 表），`email-outbox` model 及其 `enqueue` method（電郵由平台郵件 worker 寄出，而非外部 `call`；Decision 54）。

## 自然語言 source 範例

`models/user/methods/self-register.md`

```text
# self-register
A visitor registers; only email and name are accepted, no password.
Input: email (valid email, at most 254 characters), name (1 to 50 characters).

Steps:
1. Generate an email verification token valid for 24 hours.
2. Create the user, unverified and without a password. If the email already exists but is unverified, only replace the token and expiry (the old token stops working immediately) and change nothing else; if it is already verified, or has an unexpired admin invite, change nothing.
3. Enqueue one email with email-outbox.enqueue (purpose register): new or unverified users get the verification email (with the token); verified users or users with an unexpired invite get an "you already have an account" notice.
4. Never reveal whether the email already exists; the output is always empty.
```

`models/user/methods/complete-verification.md`

```text
# complete-verification
Confirm the email with the token from the email link and set the password at the same time. Used for both registration and admin invites.
Input: token (string, 43 characters), password (string).
This method confirms verification and also sets the password.

Steps:
1. Always hash the password first (fail WEAK_PASSWORD if it does not meet the platform password floor).
2. If the token is correct, unexpired and not yet used, set the password, record the verification time and clear the token in a single update; otherwise fail INVALID_TOKEN.
```

`api/auth/v1/register.md`（路由 `POST /auth/v1/{slug}/register`）

```text
# Register
POST, path /auth/v1/{slug}/register.
Public, no auth, tenant is determined by slug.
Creates an unverified user; public writes are allowed; no password is written. Captcha is always required.
At most 5 per minute per IP and 60 per minute per tenant; use the platform default daily write quota; audit, recording only a hash of the email.

Request body: email (valid email, at most 254 characters), name (1 to 50 characters).
Register with user.self-register.
Whether or not the email already exists, return 202 with the same message "If the details are correct, you will receive a confirmation email".
```

`api/auth/v1/verify-email.md`（路由 `POST /auth/v1/{slug}/verify-email`）

```text
# Verify email and set password
POST, path /auth/v1/{slug}/verify-email.
Public, no auth, tenant is determined by slug.
The email link opens a set-password page; after the user enters a password, the page sends the token and the password here.
Sets the password and confirms the email; public writes, password writes and verification writes are allowed. No captcha (no INSERT).
At most 10 per minute per IP and 120 per minute per tenant; audit without plaintext token or email.

Request body: token (43-character base64url), password (1 to 256 characters).
Use user.complete-verification. Return 204 on success.
A wrong, expired or used token returns 422 INVALID_TOKEN; a weak password returns 422 WEAK_PASSWORD.
```

## 編譯後的 lock md 範例

`models/user/methods/self-register.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/self-register
source: models/user/methods/self-register.md
source-hash: sha256:d3c9...10
source-type: natural
dependencies:
  - { id: email-outbox/methods/enqueue, kind: method, hash: sha256:81aa...c2 }
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
vibe-version: 1.0.0-rc.4
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T15:40:00+08:00
approval-ref: pr#150
approved-commit: a1b2c150
security-items:
  - { item: write-sql, risk: medium, reason: "Create unverified user or rotate verification token", approved-by: [jack@example] }
signature: MEYCIQ...
---
## Signature
- input: { email: { type: string, format: email, max: 254, required: true }, name: { type: string, min: 1, max: 50, required: true } }
- output: {}
- errors: []
- writes: true

## Calls
- methods: [email-outbox.enqueue]
- external: (none)

## Steps
### s1 random-token
- op: random-token
- bytes: 32
- out: vt
- source-lines: 5-5

### s2 query
- op: query
- provider: main
- table: users
- sql: `insert into users (email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose) values ($1, $2, $3, now() + interval '24 hours', 'register') on conflict (tenant_id, email) do update set email_verify_hash = excluded.email_verify_hash, email_verify_expires_at = excluded.email_verify_expires_at, email_verify_purpose = 'register' where users.email_verified_at is null and users.deleted_at is null and not (users.email_verify_purpose = 'invite' and users.email_verify_expires_at > now()) returning id`
- params: [input.email, input.name, vt.hash]
- writes: true
- scope: pk
- columns: { insert: [email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose], update: [email_verify_hash, email_verify_expires_at, email_verify_purpose] }
- out: ins
- source-lines: 6-6

### s3 method
- op: method
- target: email-outbox.enqueue
- args: { user_id: ins.id, email: input.email, token: vt.value, purpose: register }
- out: (none)
- source-lines: 7-7

### s4 respond
- op: respond
- value: {}
- source-lines: 8-8
````

`models/user/methods/complete-verification.lock.md`（僅列出 section；frontmatter 依照 §2A.1；安全項目 `set-password` 和 `confirm-verification`，均為高風險）

````markdown
## Signature
- input: { token: { type: string, length: 43, pattern: base64url, sensitive: true, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: {}
- errors: [WEAK_PASSWORD, INVALID_TOKEN]
- writes: true
- capabilities: [set-password, confirm-verification]

## Calls
(none)

## Steps
### s1 hash-password
- op: hash-password
- cost: high
- plain: input.password
- policy: platform-floor
- out: pw
- source-lines: 6-6

### s2 hash-token
- op: hash-token
- value: input.token
- out: th
- source-lines: 7-7

### s3 query
- op: query
- provider: main
- table: users
- sql: `update users set password = $1, email_verified_at = now(), email_verify_hash = null, email_verify_expires_at = null, email_verify_purpose = null where email_verify_hash = $2 and email_verify_expires_at > now() and email_verified_at is null and deleted_at is null returning id`
- params: [pw, th]
- writes: true
- scope: pk
- columns: [password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose]
- password-source: s1
- out: upd
- source-lines: 7-7

### s4 validate
- op: validate
- when: `upd.id == null`
- fail: INVALID_TOKEN
- source-lines: 7-7

### s5 respond
- op: respond
- value: {}
- source-lines: 7-7
````

`api/auth/v1/register.lock.md`（僅列出 section；安全項目 `public`、`auth-removed`、`public-writes`，均為高風險）

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/register

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_writer
- writable-columns: { users: { insert: [email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose], update: [email_verify_hash, email_verify_expires_at, email_verify_purpose] }, email_outbox: { insert: [user_id, email, recipient_hash, template, token] } }
- effective-grant: public_writer [users: INSERT(email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose), UPDATE(email_verify_hash, email_verify_expires_at, email_verify_purpose); email_outbox: INSERT(user_id, email, recipient_hash, template, token)]
- challenge: { type: captcha, when: always }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required
- min-response: 300ms

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 60
- key: tenant

### p4 audit-log
- event: public-register
- hash: [req.body.email]

## Limits
- provider: main
- timeout: 10s
- body: 4KB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | | yes |
| body.email | string | format: email, max: 254, untrusted | yes |
| body.name | string | min: 1, max: 50, untrusted | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 202 | { message: "If the details are correct, you will receive a confirmation email" } | |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { message: "If the details are correct, you will receive a confirmation email" }

## Steps
### s1 method
- op: method
- target: user.self-register
- args: transform.in
- out: (none)
- source-lines: 7-7

### s2 respond
- op: respond
- status: 202
- value: transform.out
- source-lines: 8-8

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

`api/auth/v1/verify-email.lock.md`（只列 `## Access` 和 `## Errors`；安全項目 `public`、`auth-removed`、`public-writes`、`password-write`、`verification-write`，均為高風險，剛好是每個 PR 5 項的上限）

````markdown
## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- password-write: allow
- verification-write: allow
- role: public_writer
- writable-columns: { users: { update: [password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose] } }
- effective-grant: public_writer [users: UPDATE(password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose)]
- challenge: { type: none }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required
- min-response: 300ms

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | WEAK_PASSWORD | WEAK_PASSWORD |
| 422 | INVALID_TOKEN | INVALID_TOKEN |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

它的 pipelines 為 `-auth`、按 `ip` 每分鐘 10 次的 `rate-limit`、按 `tenant` 每分鐘 120 次的 `rate-limit`，以及 `redact: [req.body.token, req.body.password]` 的 `audit-log`；body 上限 2KB；成功回 204（§12.11.7）。

## 安全考量

- **註冊時不收密碼**（Decision 67）：`user.self-register` 沒有 `set-password` capability，也沒有 `hash-password` step。body 含 `password`（或 `tenant_id`、`id`、`created_by`，或任何未宣告欄位如 `roles`）會回 422（§12.11.6）。
- **防預先劫持**：攻擊者用受害人電郵註冊，只會得到一個沒有憑證的帳戶；受害人註冊時 token 會被輪換，攻擊者的 token 隨即失效。密碼只能經電郵連結設定（§12.11.4）。
- **Upsert 限制**：`ON CONFLICT DO UPDATE` 只可更新 `email_verify_hash`、`email_verify_expires_at`、`email_verify_purpose`，而且只限未驗證、未刪除、沒有未過期邀請的列。validator 檢查 `columns.update` ⊆ `columns.insert`，且不含密碼、`name`、`email` 或黑名單欄位。因 `(tenant_id, email)` 是唯一鍵，`scope: pk` 成立（§12.11.4）。
- **防列舉**（Decision 56）：每條路徑（新用戶、未驗證、已驗證、已邀請）都做一次 upsert 加一次 outbox insert，回傳相同的 202 body，並補足至 300ms。郵件 worker 依 `email-outbox.enqueue` 的衍生規則選擇範本（`verify-email`、`already-registered`、`invite`）（§12.11.3）。
- **原子地設定密碼與驗證**（Decisions 53、67）：只有同時具備 `set-password` 和 `confirm-verification` capability 的 method，才可在公開路徑寫入 `password`／`email_verified_at`；密碼值必須來自同一 method 的 `hash-password`（`password-source: s1`）；WHERE 使用 `hash-token` 的輸出、檢查有效期和未驗證狀態，並清除 token hash；api 必須宣告 `password-write: allow` 和 `verification-write: allow`（Decision 47、§7.2）。永遠不會出現「已驗證但沒有密碼」或「有密碼但未驗證」的狀態，令 Decision 55 成立。
- **固定工作量**：每次 verify-email 呼叫都執行 `hash-password`（即使 token 錯誤），使用固定大小的 argon2 worker pool；回應補足至 300ms，超過 300ms 則補至下一個 100ms 倍數（Decision 66）。
- **密碼政策**（§4A.2、Decision 46）：12–256 字元，洩漏密碼檢查（k-anonymity），argon2id：memory 64 MiB、iterations 3、parallelism 1、16 byte salt、32 byte 輸出、PHC 格式；參數改變時於登入自動 rehash。
- **Token**（Decision 52）：`random-token` 產生 32 byte，只以 43 字元 base64url 字串提供；DB 只存 `HMAC-SHA-256(pepper, token)`。`vt.value` 只可傳入 `sensitive` 的 method 輸入（此處為 `email-outbox.enqueue`），絕不可進入 `respond`、log 或明文 SQL 欄位。outbox token 寄出後立即以加密粉碎（crypto-shred）處理（Decision 54）。
- **連結格式**：`https://{frontend}/verify#token={token}`；token 放在 fragment，因此不會進入伺服器 log 或 `Referer`（§12.11.7）。
- **審計**：絕不記錄明文電郵（`hash: [req.body.email]`）、token 或密碼（Decision 68）。
- **撤銷 session**：驗證成功後，平台撤銷該用戶所有 refresh 家族（`revoke_reason: email-verified`）（Decision 67）。
- **清理**：超過 `email_verify_expires_at` 的未驗證帳戶，由具 `erasure` role 的平台 job 匿名化（電郵 → `erased+{id}@invalid`，清除 token hash）並軟刪除；絕不 DELETE（Decisions 13、67）。
- **限流**：register 每 IP 每分鐘 5 次、每租戶每分鐘 60 次（沒有 `cost: high` step，因此不減半）；verify-email 每分鐘 10／120 次，因 `hash-password` 為 `cost: high` 實際減半為 5／60（Decisions 25、39）。
- **PR 拆分**：本範例需要 4 個 PR（migration；model + methods；register api；verify-email api），以符合每個 PR 最多 5 個安全項目（§12.11.8）。
- **Admin 路徑**：`user.register`（admin、需認證）可回 409 `EMAIL_TAKEN`；它不設定密碼，之後接 `user.invite`（Decision 69）。

## 規格缺口

- 規格**沒有展示** admin `user.register` 的 lock 和 `user.invite` 的 lock（§11.4、§12.11.9 只以文字描述）。
- verify-email 的實際限流因 `cost: high` 減半（§12.11.7）而 register 不減半，取決於被呼叫 method 內的 step；validator 如何把 method 的 `cost: high` 傳遞到 api 限流沒有明確說明。
- 對已軟刪除或已刪除資料的用戶呼叫 `complete-verification` 時，用戶端只會看到通用的 422 `INVALID_TOKEN`，沒有另外的指引。
