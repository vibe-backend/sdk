# CRUD：樂觀鎖（`ETag`／`If-Match`）

## 用途

防止兩個用戶端同時編輯同一筆記錄時發生更新遺失。v1.7 起，注入的 `update` CRUD method **一定**以 HTTP 條件請求執行樂觀鎖檢查（Decision 48、§12.9）：

- `get` 和 `update` 的回應都帶由 `id` 加 `updated_at` 計出的強 `ETag`。
- 每個呼叫 `update` 的 api 都必須要求 `If-Match` header；其 `## Transform` 把 `req.header.if-match` 映射為 method 輸入 `if_match`（唯一准許的來源）。
- 缺少 `If-Match` → **428** `PRECONDITION_REQUIRED`；不一致 → **412** `PRECONDITION_FAILED`（§8，RFC 6585／RFC 9110）。
- `updated_at` 仍是系統欄位，永不由輸入寫入。

這取代了 v1.5 的做法（用戶端送 `updated_at`，衝突回 409），v1.7 已將其列為前後矛盾並修正（F4）。

## 自然語言 source 範例

`models/user/user.md`（相關的一行）

```text
Provide get, list and update CRUD.
```

`api/users/v2/update-user/update-user.md`（路由 `PUT /users/v2/{id}`）

```text
# Update user
PUT, path /users/v2/{id}, id is a uuid. Requires JWT; only admin may use it.
The If-Match header is required and must carry the ETag the client last read.
Body: name (1 to 50 characters, optional).
Update the user with user.update and return 200 with id and name, plus the new ETag.
User not found returns 404 NOT_FOUND. Missing If-Match returns 428 PRECONDITION_REQUIRED; a stale ETag returns 412 PRECONDITION_FAILED.
```

## 編譯後的 lock md 範例

`models/user/user.lock.md`（只列 CRUD section，與 §12.8 相同）

````markdown
## CRUD
| method | options |
| --- | --- |
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: if-match |
````

`api/users/v2/update-user/update-user.lock.md`（僅列出 section；`kind: api`、`scope: feature`；`dependencies` 包含 `user/crud/update`）

````markdown
## Route
- method: PUT
- path: /users/v2/{id}

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt
- roles: [admin]

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.id | uuid | | yes |
| header.if-match | string | | yes |
| body.name | string | min: 1, max: 50 | no |

## Response
| status | schema | headers |
| --- | --- | --- |
| 200 | { id, name } | ETag |

## Transform
- in: { id: req.path.id, name: req.body.name, if_match: req.header.if-match }
- out: { id: u.id, name: u.name }

## Steps
### s1 method
- op: method
- target: user.update
- args: transform.in
- out: u
- source-lines: 5-5

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 5-5

## Errors
| status | code | from |
| --- | --- | --- |
| 404 | NOT_FOUND | NOT_FOUND |
| 412 | PRECONDITION_FAILED | PRECONDITION_FAILED |
| 428 | PRECONDITION_REQUIRED | PRECONDITION_REQUIRED |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

## 安全考量

- **同租戶存取**：RLS 只隔離租戶。本範例把 endpoint 限制為 `roles: [admin]`（高風險 `roles` 項目）。若要讓用戶自行編輯，請在 model 宣告 `owner-key`（`users` 表用其本身的 `id`）：非 admin 的 read／update SQL 就必須帶 `{owner-key} = ctx.user.id`；要放寬（`ownership: tenant-wide` 或 `ownership: role(admin)`）必須在 source 明寫，屬高風險（Decision 70、§12.6a）。
- **以 PUT 做部分更新**：注入的 `update` 是 PATCH 語意（§12.9），但 v1 只路由 GET／POST／PUT／DELETE（§0.1），因此以 PUT 承載部分更新。請向用戶端說明。
- `if_match` 只可由 `req.header.if-match` 映射；絕不從 body 或 query 接受版本（Decision 48）。
- 系統欄位 `id`、`tenant_id`、`created_at`、`updated_at`、`deleted_at`、`erased_at` 永不由輸入寫入（§12.9）。
- 在 `## Errors` 對應 412 和 428；未對應的 method 錯誤會變成 500 `INTERNAL` 並出 warning（§8、§10.9、§12.4）。
- 每個 `UPDATE` 都需要以主鍵／唯一鍵或 RLS 限定的 `WHERE`（`scope: pk | rls`）（§2A.4）。
- 若以 `models/user/methods/update.md` 覆蓋 `update`，就取代了注入的樂觀鎖邏輯，部署報告會列出該覆蓋（§12.9）。

## 規格缺口

- header 欄位在 `## Request` 的寫法（`header.if-match`）以及 `## Response` headers 中的 `ETag`，規格的 lock 範例都沒有展示；本頁沿用 Decision 48 的 `req.header.if-match` 名稱，僅作示意。
- 412／428 是由注入的 `update` method 拋出（因此對應 `from: user.update`），還是由平台在 method 執行前回應，**未有規定**。
- `NOT_FOUND` 是否為注入 `update` 的已宣告錯誤未有說明（`get` 則有說明）。
- `get` 的 api lock 是否也必須在 `## Response` headers 宣告 `ETag`，**未有規定**。
