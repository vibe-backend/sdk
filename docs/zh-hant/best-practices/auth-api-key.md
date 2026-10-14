# 認證 pipeline：API key

## 用途

以 API key 代替用戶 JWT，認證機器對機器的呼叫方。API key 驗證屬於內建 `auth` pipeline 的一部分：pipeline op 白名單包含 `verify-api-key`（§2A.4），而 §4 定義了 API key 的規則。

## 自然語言 source 範例

`api/partners/_definition.md`

```text
# partners
Partner integration API. Only uses provider main.
All endpoints require API key authentication.
```

`api/partners/v1/list-orders/list-orders.md`（路由 `GET /partners/v1/list-orders`）

```text
# List partner orders
GET. Requires an API key. At most 60 requests per minute.
Query: page (integer, from 1) and page-size (integer).
List orders with order.list and return 200 with items, page, page_size and has_more.
```

## 編譯後的 lock md 範例

`api/partners/_definition.lock.md`（僅列 section；`kind: api`、`scope: definition`）

````markdown
## Providers
- allowlist: [main]
- default: main

## Pipelines
### p1 auth
- mode: api-key

## Defaults
- timeout: 10s
- body: 1MB
````

> ⚠️ `mode: api-key` 是一項**假設**：規格只展示了 `mode: jwt`（§11.3）。既然有 `verify-api-key` op（§2A.4），這是順理成章的對應寫法，但確實的值並未規定。編譯器會在這裡附上 `assumption` 註記並發出 warning（§10.9）。

`api/partners/v1/list-orders/list-orders.lock.md`（僅列 section；`kind: api`、`scope: feature`）

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

### p2 rate-limit
- per-minute: 60

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| query.page | integer | min: 1 | no |
| query.page-size | integer | min: 1, max: 100 | no |

## Response
| status | schema | headers |
|---|---|---|
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
|---|---|---|
| 422 | VALIDATION_FAILED | platform |
````

## 安全考量

- API key **只以雜湊值儲存**，必須有 **scope**，可以**撤銷**，並且**絕不寫入日誌**（§4、§9）。
- 次序與預設拒絕規則和 JWT 相同：`auth` 最先執行（`must-run: first`），要移除它必須設定 `public: true`（§3、§4）。
- 認證成功後，`auth` 會注入 `ctx.tenant_id` 和 `ctx.user`；業務邏輯不能修改它們（§4）。
- 私隱刪除會撤銷被刪除對象的所有 API key（§12.10 第 5 步）。
- 認證失敗回傳 401（附 `WWW-Authenticate`），scope／權限不足回傳 403（§8）。

## 規格缺口

- 用來選擇 API key 模式的 `auth` 參數值，以及 `verify-api-key` op 的參數，**均未規定**。
- API key 的 **scope** 如何在 source／lock 中宣告、如何對應到端點，**未有規定**。
- API key 在哪裡簽發和管理（UI／CLI／model），**未有規定**。
- **雜湊演算法**：「只儲存雜湊值」（§4）沒有指明演算法；使用帶金鑰的快速雜湊（例如 HMAC-SHA256；高熵的 key 不需要 argon2id）是合理選擇，但**未有規定**。
- **傳輸方式**：header 名稱，以及是否**只**接受放在 header 的 key（絕不接受 query string，因為會洩漏到日誌），均未規定。
- **租戶來源**：API key 的 `ctx.tenant_id` 如何得出（是否在簽發時綁定？）未有規定。
- **每個 key 的限流**：規格只展示了以 `ip` 和 `tenant` 為 key 的限流；以 API key 為 key 的限流未有規定。
- **401 header**：§8 要求依 RFC 6750（Bearer）附上 `WWW-Authenticate`，但這個 scheme 對 API key 認證未必合適。
- API key 呼叫方的 `ctx.user` 包含甚麼內容，**未有規定**。
