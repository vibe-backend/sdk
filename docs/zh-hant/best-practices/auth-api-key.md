# Auth pipeline：API key

## 用途

以 API key 而非用戶 JWT 認證機器對機器的呼叫者。API key 模式屬於內置 `auth` pipeline：`auth: { mode: api-key, scopes: [...] }`（§4.2、Decision 43），由 `verify-api-key` pipeline op 驗證（§2A.4）。

## 自然語言 source 範例

`api/partners/_definition.md`

```text
# partners
Partner integration API. Only uses provider main.
All endpoints require an API key with partners:read scope.
```

`api/partners/v1/list-orders/list-orders.md`（路由 `GET /partners/v1/list-orders`）

```text
# List partner orders
GET. Requires an API key with partners:read scope. At most 60 requests per minute per API key.
Query: page (integer, from 1) and page-size (integer, 1 to 100).
List orders with order.list and return 200 with items, page, page_size and has_more.
```

## 編譯後的 lock md 範例

`api/partners/_definition.lock.md`（僅列出 section；`kind: api`、`scope: definition`）

````markdown
## Providers
- allowlist: [main]
- default: main

## Pipelines
### p1 auth
- mode: api-key
- scopes: [partners:read]

## Defaults
- timeout: 10s
- body: 1MB
````

`api/partners/v1/list-orders/list-orders.lock.md`（僅列出 section；`kind: api`、`scope: feature`）

````markdown
## Route
- method: GET
- path: /partners/v1/list-orders

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: api-key
- scopes: [partners:read]

### p2 rate-limit
- per-minute: 60
- key: api-key

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| query.page | integer | min: 1 | no |
| query.page-size | integer | min: 1, max: 100 | no |

## Response
| status | schema | headers |
| --- | --- | --- |
| 200 | { items, page, page_size, has_more } | |

## Transform
- in: { page: req.query.page, page-size: req.query.page-size }
- out: { items: r.items, page: r.page, page_size: r.page_size, has_more: r.has_more }

## Steps
### s1 method
- op: method
- target: order.list
- args: transform.in
- out: r
- source-lines: 4-4

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 4-4

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

## 安全考量

平台規則（§4.2、Decision 43）：

- **傳送**：只接受 header `X-API-Key: <key>`。key 出現在 query string 或 body 會回 400，而且不寫入 log。
- **格式**：`mbk_{env}_{key_id}_{secret}`；`env` 為 `live` 或 `test`；`key_id` 為 12 個小寫 base32 字元；`secret` 為 32 byte 隨機值的 base62。`test` key 不可在 live 環境使用。
- **Scope**：`{definition}:{read|write|admin}` 或 `{definition}:*`；`admin` 包含 `write`，`write` 包含 `read`；不准全域 `*`。修改 API key scope 屬**高風險**安全項目（§2A.5）。
- **儲存**：只存 `HMAC-SHA-256(pepper, secret)`（pepper 在平台 KMS）；以 `key_id` 查找，再做固定時間比對。
- **有效期與輪換**：預設 90 日，上限 365 日；每個 client 最多 2 條有效 key；舊 key 與新 key 預設重疊 7 日（上限 30 日），之後失效。
- **撤銷**：平台 UI／CLI `api-key revoke {key_id}`；30 秒內所有 instance 生效；以 UPDATE `revoked_at` 記錄，絕不 DELETE。
- 會記錄 `last_used_at`；key 和 secret 永不寫入 log。
- **Context**：`ctx.tenant_id` 來自 key 綁定的租戶；`ctx.user` 為 `{ type: api-key, key_id, scopes }`（§4.2）。
- 按 key **限流**使用 `key: api-key`（§4.3）。
- 私隱刪除會撤銷被刪除主體的所有 API key（§12.10 步驟 5）。
- 缺少或無效 key → 401；scope 不足 → 403（§8）。

## 規格缺口

- §8 要求每個 401 都依 RFC 6750 帶 `WWW-Authenticate`（Bearer 方案），這與 API key 認證並不明顯相符；API key 模式的 challenge 值**未有規定**。
- API key 如何簽發（除 `revoke` 外的 UI／CLI 指令名稱）**未有規定**。
- `_definition` 與 feature 都宣告 `scopes` 時，繼承（§3）是合併還是覆蓋**未有規定**。
