# `sign-token`：token 簽發 capability

## 用途

把 JWT 簽發集中在一個標記了 `capabilities: [sign-token]` 的 model method 中。只有這類 method 可以使用 `sign-jwt` op；任何其他需要 token 的 method 或 pipeline，都要透過 `method` op 呼叫它（Decisions 5、30；§12.6）。

## 自然語言 source 範例

`models/user/methods/sign-token.md`

```text
# sign-token
Sign an access token for a user.
This method is responsible for signing tokens.
Input: sub (user id, uuid).
Output: token (access token) and refresh_token.
The token is valid for 1 hour, uses ES256, and the key is JWT_SIGNING_KEY.
```

## 編譯後的 lock md 範例

`models/user/methods/sign-token.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/sign-token
source: models/user/methods/sign-token.md
source-hash: sha256:3b90...d4
source-type: natural
dependencies:
  - { id: user, kind: model, hash: sha256:aa41...0f }
vibe-version: 1.0.0-rc.2
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:22:00+08:00
approval-ref: pr#125
approved-commit: a1b2c125
security-items:
  - { item: sign-token, risk: high, reason: "Sole token issuance entry point", approved-by: [jack@example, mei@example] }
signature: MEQCIA...
---
## Signature
- input: { sub: { type: uuid, required: true } }
- output: { token: string, refresh_token: string }
- errors: []
- writes: false
- capabilities: [sign-token]

## Calls
(none)

## Steps
### s1 query
- op: query
- table: users
- sql: select id::text as id from users where id = $1::uuid and deleted_at is null
- params: [input.sub]
- out: u
- source-lines: 1-2

### s2 sign-jwt
- op: sign-jwt
- alg: ES256
- key-ref: JWT_SIGNING_KEY
- claims: { sub: u.id }
- platform-claims: [iss, aud, exp, nbf, iat, jti, roles, tenant_id]
- platform-claims-source: { roles: users.roles where id = sub, tenant_id: ctx.tenant_id }
- ttl: 1h
- ttl-cap: 1h
- issue-refresh: platform-decided
- out: token
- source-lines: 1-5

### s3 respond
- op: respond
- value: { token: token.access, refresh_token: token.refresh }
- source-lines: 4-4
````

呼叫方的用法如下（取自 `user.login`）：

````markdown
### s5 method
- op: method
- target: user.sign-token
- args: { sub: u.id }
- out: t
- source-lines: 10-10
````

`issue-refresh` 由平台按呼叫來源決定：由登入呼叫時為 `new-family`，由 `token-refresh` pipeline 注入時為 `rotate`，其他情況為 `none`；method 永遠不可自行選擇 `rotate`（§4A.1）。

## 安全考量

- **明確的 capability**：只有在 `## Signature` 中宣告了 `capabilities: [sign-token]` 的 method 才可以使用 `sign-jwt`；名稱無關重要（名為 `sign-token` 但沒有此標記的 method 會被拒絕）（§2A.4、§12.6）。這個 capability 必須屬於 **model 層**的 method（§10.8）。
- 請把 capability 寫成一句獨立清晰的句子（「此 method 負責簽發 token」）。如果 lock 會簽發 token，但 source 從未提及，語意一致性檢查會報錯（§2A.7、§10.1）。
- **Claim 白名單**：method 只可以傳入 `sub` 及平台允許的自訂 claim（例如 `sid`）。`roles` 由平台從資料庫讀取（在當前租戶中查找 `sub`）。`tenant_id` 只有一個來源：`ctx.tenant_id`，由平台從 slug（公開）或已驗證的 token（auth）解析；絕不取自用戶端、method 或 `users` 表（Decision 62）。`iss`、`aud`、`exp`、`nbf`、`iat`、`jti` 由平台加上。傳入其中任何一項都屬錯誤（§12.6、§10.8）。
- **TTL**：access token 上限為 1 小時（§4A.1、§10.8）。
- **演算法／金鑰**：只限 JWT，使用 RS256 或 ES256；私鑰屬平台機密，以 `key-ref` 引用，絕不直接寫入（§12.6、§9B）。ES256 私鑰如果無法簽名（結尾有多餘資料、私鑰純量超出範圍），`vibe run` 啟動時就會拒絕。
- `capabilities: [sign-token]`、claim 白名單及 token ttl 都屬於**高風險**安全項目（需兩位非作者確認）（§2A.5）。
- token 永遠不會寫入日誌（§9）。
- ⚠️ **任何呼叫方都可以為任意 `sub` 簽發 token**：`user.sign-token` 把 `sub` 當作一般輸入，並不會驗證呼叫方是否已認證該用戶。任何能呼叫它的 method 或 pipeline，都可以為租戶內任何用戶簽發 token。請把呼叫方減至最少（`user.login`、`token-refresh`），審核每一條新指向它的 `## Calls` 關係，並留意有效權限摘要中的「簽發 token」一欄（§10.10）。規格沒有為 capability method 提供呼叫方白名單。

## 規格缺口

- 平台完整的**自訂 claim 白名單**沒有列出（只以 `sid` 作為例子）。
- `sign-jwt` 的參數 `platform-claims-source` 使用了一個在其他地方沒有定義的偽查詢（`users.roles where id = sub`）。
- **限制呼叫方**：沒有方法限制哪些 method／pipeline 可以呼叫 `sign-token` method。
- method 中的 `respond` 是否需要 `status` 未有規定（規格中 method 的 respond 均省略了它）。
