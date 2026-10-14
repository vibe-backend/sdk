# CRUD：樂觀鎖

## 用途

防止兩個用戶端同時編輯同一筆記錄時發生更新遺失。注入的 `update` CRUD method 會執行部分更新（PATCH 語意），**並以 `updated_at` 進行樂觀鎖檢查**；發生衝突時以 `CONFLICT` 失敗（§12.9）。

## 自然語言 source 範例

`models/user/user.md`（相關的一行）

```text
Provide get, list and update CRUD. update is a partial update.
```

`api/users/v2/update-user/update-user.md`（路由 `PUT /users/v2/{id}`）

```text
# Update user
PUT, path /users/v2/{id}, id is a uuid. Requires JWT; only admin may use it.
Body: name (1 to 50 characters, optional) and updated_at (timestamp, required — the version the client last read).
Update the user with user.update and return 200 with id, name and updated_at.
User not found returns 404 NOT_FOUND. If someone else changed the user in the meantime return 409 CONFLICT.
```

## 編譯後的 lock md 範例

`models/user/user.lock.md`（只列 CRUD section）

````markdown
## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: updated_at |
````

`api/users/v2/update-user/update-user.lock.md`（僅列 section；`kind: api`、`scope: feature`；`dependencies` 包含 `user/crud/update`）

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
|---|---|---|---|
| path.id | uuid | | yes |
| body.name | string | min: 1, max: 50 | no |
| body.updated_at | timestamp | | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { id, name, updated_at } | |

## Transform
- in: { id: req.path.id, name: req.body.name, updated_at: req.body.updated_at }
- out: { id: u.id, name: u.name, updated_at: u.updated_at }

## Steps
### s1 method
- op: method
- target: user.update
- args: transform.in
- out: u
- source-lines: 3-3

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 3-3

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | user.update |
| 409 | CONFLICT | user.update |
| 422 | VALIDATION_FAILED | platform |
````

## 安全考量

- **同租戶 IDOR**：RLS 只會隔離租戶。如果沒有角色或擁有權檢查，同一租戶內任何已認證的用戶都可以按 id 更新任何用戶。這個範例把端點限制為 `roles: [admin]`（屬高風險的 `roles` 安全項目）。規格**沒有定義資料列擁有權機制**（例如「只有擁有者可以更新」），見規格缺口。
- **以 PUT 做部分更新**：注入的 `update` 是 PATCH 語意（§12.9），但 v1 只支援 GET／POST／PUT／DELETE（§0.1），所以部分更新使用 PUT。請在給用戶端的文件中註明。
- 在 `## Errors` 中把 `CONFLICT` 對應到 **409**；未對應的 method 錯誤會變成 500 `INTERNAL`，並觸發 warning（§8、§10.9、§12.4）。
- 系統欄位 `id`、`tenant_id`、`created_at`、`updated_at`、`deleted_at` 不可由輸入寫入（§12.9）。用戶端提供的 `updated_at` 只是用來比較的預期版本，並不會被寫入。
- 更新時 `password` 欄位會自動雜湊（§12.9）。
- 每個 `UPDATE` 都必須有以主鍵／唯一鍵或 RLS 限定範圍的 `WHERE`（`scope: pk | rls`）（§2A.4）。
- 修改可寫入欄位或新增寫入 SQL，屬於**中風險**安全項目（§2A.5）。
- 如果你撰寫 `models/user/methods/update.md` 覆寫 `update`，覆寫版本會取代注入版本，並列在部署報告中；此後樂觀鎖邏輯由你自行負責（§12.9）。

## 規格缺口

- 攜帶樂觀鎖預期版本的**輸入欄位名稱****未有規定**（§12.9 只說檢查使用 `updated_at`）。傳入 `updated_at` 是最直接的理解，但仍屬假設。
- `NOT_FOUND` 是否為 `update` 已宣告的錯誤並未說明（`get` 有說明）。`update` 只規定了 `CONFLICT`。
- 規格的 `## CRUD` 選項 `optimistic-lock: updated_at` 出現在 §12.8 的 lock 中，但 §12.9 只把 `partial` 列為 update 選項；樂觀鎖能否停用**未有規定**。
- **資料列擁有權**（「用戶只可以編輯自己的記錄」，例如比較 `ctx.user` 與該資料列）沒有規定的 CRUD 選項或 lock 語法；注入的 CRUD 沒有擁有者檢查。請使用角色，或以自訂 method 覆寫 `update` 並送交審核。
- `timestamp` 請求欄位類型列於 model 欄位（§12.2）；規格範例中沒有展示它在 `## Request` 中的用法。
