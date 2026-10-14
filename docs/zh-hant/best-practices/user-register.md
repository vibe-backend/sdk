# `user.register`

## 用途

以單一 model method 建立新用戶帳戶，並強制執行電郵唯一性及平台密碼底線。規格對 `user.register` 的描述是：「如果電郵已被使用則以 EMAIL_TAKEN 失敗，否則建立用戶並回傳 id、email、name」，編譯成一條 SELECT 檢查和一條參數化 INSERT（`writes: true`）（§11.7）。它由 `create-user` 等 api feature 呼叫（§11.4，見 [auth-jwt.md](auth-jwt.md)）。

## 自然語言 source 範例

`models/user/methods/register.md`

```text
# register
Register a new user.
Input: email (valid email, required), name (1 to 50 characters, required), password (required).
Output: id, email, name.

Steps:
1. Look up the user by email; if the email is already taken fail EMAIL_TAKEN.
2. Hash the password.
3. Create the user and return id, email and name.
```

## 編譯後的 lock md 範例

`models/user/methods/register.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/register
source: models/user/methods/register.md
source-hash: sha256:c3e1...90
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example]
approved-at: 2026-10-14T08:15:00+08:00
approval-ref: pr#124
approved-commit: a1b2c124
security-items:
  - { item: write-sql, risk: medium, reason: "Inserts new user rows", approved-by: [jack@example] }
signature: MEUCIQ...
---
## Signature
- input: { email: { type: string, format: email, required: true }, name: { type: string, min: 1, max: 50, required: true }, password: { type: string, min: 12, max: 256, required: true } }
- output: { id, email, name }
- errors: [EMAIL_TAKEN, WEAK_PASSWORD]
- writes: true

## Calls
- methods: (none)
- external: (none)

## Steps
### s1 query
- op: query
- provider: main
- table: users
- sql: `select id from users where email = $1`
- params: [input.email]
- writes: false
- out: existing
- source-lines: 6-6

### s2 validate
- op: validate
- when: `existing != null`
- fail: EMAIL_TAKEN
- source-lines: 6-6

### s3 hash-password
- op: hash-password
- cost: high
- plain: input.password
- out: pw
- source-lines: 7-7

### s4 query
- op: query
- provider: main
- table: users
- sql: `insert into users (email, name, password) values ($1, $2, $3) returning id, email, name`
- params: [input.email, input.name, pw]
- writes: true
- columns: [email, name, password]
- out: u
- source-lines: 8-8

### s5 respond
- op: respond
- value: { id: u.id, email: u.email, name: u.name }
- source-lines: 8-8
````

## 安全考量

- **密碼底線**：最少 12 個、最多 256 個字元，不強制複雜度，必須檢查是否為已外洩密碼（k-anonymity，只有雜湊值的前綴會離開平台）。`hash-password` 在雜湊前強制執行這些規則，不符合時以 `WEAK_PASSWORD`（422）失敗（§4A.2）。物件只可以**收緊**（例如 `min: 16`）；低於底線的 lock 屬於錯誤。
- `type: password` 欄位以 argon2id 雜湊（參數由平台管理），永遠不會出現在 SELECT 結果、method 輸出、回應或日誌中（§12.2）。在輸出中回傳 `password` 屬於錯誤（§10.8）。
- 系統欄位（`id`、`tenant_id`、`created_at` 等）永遠不由輸入寫入；`tenant_id` 來自 RLS context（§7.1、§12.9）。
- 在 api lock 中對應 method 錯誤：`EMAIL_TAKEN` → 409，`WEAK_PASSWORD` → 422；未對應的錯誤會變成 500（§12.4）。
- **公開自助註冊**（呼叫 `user.register` 的公開端點）屬於含 INSERT 的公開寫入，因此需要 `public-writes: true`、`challenge: captcha | pow`、IP 加租戶限流、每日配額、`audit-log` 及 `writable-columns`（§7.2）。`roles` 在公開寫入黑名單上，所以公開註冊絕不可設定角色。見 [public-writes.md](public-writes.md)。
- **洩漏電郵是否存在／公開註冊的回應**：`EMAIL_TAKEN`（409）會透露某個電郵地址已經註冊。對於**公開**自助註冊，建議做法（規格以外）是一律回傳相同的 **202 Accepted**，再以電郵通知（新地址收到「完成註冊」，已存在的地址收到「你已經有帳戶」），使 HTTP 回應永遠不會透露帳戶是否存在。v1.5 只對登入強制要求防止帳戶枚舉（§8、§12.6），也沒有定義發送電郵的 op，因此這個流程**未有規定**（見規格缺口）。只限管理員的 `create-user` 範例可以保留 409。
- 單一職責：只對一個資料表執行一條寫入 SQL，符合規定（§12.3a）。

## 規格缺口

- 規格沒有展示 `user.register` 的 lock，只有一行描述（§11.7）。上面的 lock 是依照 §2A 規則撰寫的示意。
- **`hash-password` op 的參數**（`plain`、`out`、`cost`）未有規定，只規定了其行為（§4A.2）。它的結果是以參數傳入，還是由平台在寫入時雜湊 `password` 欄位，並不明確（§12.2 說寫入時「一律使用 argon2id」）。
- `input.password` 應在 `## Signature` 中帶 `min: 12`，還是檢查只存在於 `hash-password` 中，未有規定。
- §7.2 把 `password` 列入公開寫入黑名單，「由平台 `hash-password` 處理的登入類除外」；公開**註冊**是否算作「登入類」**並不明確**（§7.2 把登入類定義為含 `verify-password` 步驟的端點）。
- **重複檢查與軟刪除**：s1 刻意**不**以 `deleted_at is null` 篩選，因為 `email` 上的 `unique` 約束仍然涵蓋已軟刪除的資料列，INSERT 無論如何都會失敗。已軟刪除（或已依 §12.10 刪除，電郵會被替換成隨機唯一值）的用戶，其電郵能否重新註冊，**未有規定**。
- **公開註冊防止枚舉**：v1.5 沒有「先回 202 再發電郵」的模式，也沒有讓 method 發送電郵的 op；如何實作**未有規定**。
- **argon2id 參數及重新雜湊**：§12.2 說 argon2id 參數由平台管理，但具體數值，以及參數變更後是否會在登入時自動重新雜湊，**未有規定**（由規格負責人決定）。
