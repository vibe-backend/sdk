# CRUD：offset 分頁

## 用途

使用平台注入的 `list` CRUD method，提供分頁的記錄清單。CRUD **永遠不會隱式產生**：model source 必須列明需要哪些操作（§12.9）。`list` 使用 **offset 分頁**，並強制設有每頁筆數上限（Decision 14）。

## 自然語言 source 範例

`models/user/user.md`

```text
# user
User data, stored in the users table of provider main.

Fields:
- id: uuid, primary key, auto-generated.
- email: valid email, unique.
- name: at most 50 characters.
- password: password.
- roles: array of strings.
- last_login_at: timestamp, optional.
- email_verified_at: email verification time, optional.
- email_verify_hash: hash of the verification token, unique, optional.
- email_verify_expires_at: verification token expiry, optional.
- name and email are filled in by visitors at registration and are untrusted content.

Provide get, list and update CRUD. list defaults to 20 per page, at most 100, can sort by created_at or name, and can filter by email.
```

`api/users/v2/list-users/list-users.md`（路由 `GET /users/v2/list-users`）

```text
# List users
GET. Requires JWT; admin only.
Query: page (integer, starting at 1), page-size (integer, 1 to 100), sort (created_at or name), email (filter, optional).
List users with user.list and return 200 with items (id, email, name), page, page_size and has_more.
Validation failure returns 422.
```

## 編譯後的 lock md 範例

`models/user/user.lock.md`（僅列 section；完整 frontmatter 依 §2A.1，`kind: model`）

````markdown
## Table
- provider: main
- table: users

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| email | string | format: email, unique, max: 254, untrusted, personal: standard | no |
| name | string | max: 50, untrusted, personal: standard | no |
| password | password | argon2id, nullable | no |
| roles | string[] | default: [], max-items: 20 | no |
| last_login_at | timestamp | nullable, public-writable | no |
| email_verified_at | timestamp | nullable, public-writable (confirm-verification) | no |
| email_verify_hash | bytes | nullable, unique, public-writable | no |
| email_verify_expires_at | timestamp | nullable, public-writable | no |
| email_verify_purpose | string | nullable, enum: [register, invite] | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: if-match |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer [INSERT(email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose), UPDATE(email_verify_hash, email_verify_expires_at, email_verify_purpose, password, email_verified_at, last_login_at)]
````

這與 v1.7 §12.8 的 `user` lock 一致。model 層的 `public_writer` 授權是所有公開端點的**聯集**（登入寫入 `last_login_at`；註冊和 verify-email 寫入驗證欄位和密碼）；每個 api 的實際授權只限其本身 lock 用到的欄位（§12.8、§12.11.6、§12.11.7）。可公開寫入的欄位屬於高風險的 `public-writable-column` 安全項目。見 [user-login.md](user-login.md) 和 [user-register.md](user-register.md)。

注入的 `user.list` method 有自己的 lock，由平台產生並簽名（`id: user/crud/list`、`source-type: structured`）（§12.9）。

`api/users/v2/list-users/list-users.lock.md`（僅列 section；`kind: api`、`scope: feature`；`dependencies` 包含 `user/crud/list`）

````markdown
## Route
- method: GET
- path: /users/v2/list-users

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
| query.page | integer | min: 1 | no |
| query.page-size | integer | min: 1, max: 100 | no |
| query.sort | string | enum: [created_at, name] | no |
| query.email | string | format: email | no |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { items: [{ id, email, name }], page, page_size, has_more } | |

## Transform
- in: { page: req.query.page, page-size: req.query.page-size, sort: req.query.sort, filter: { email: req.query.email } }
- out: { items: r.items, page: r.page, page_size: r.page_size, has_more: r.has_more }

## Steps
### s1 method
- op: method
- target: user.list
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
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

## 安全考量

- `page` 從 1 開始；`page-size` 不可超過 `max-page-size`，超過時回傳 **422**（§12.9）。`max-page-size` 預設為 100；平台上限為 **500**：model 可設得更低，但不可更高，超過上限屬 validator 錯誤（Decision 71）。
- 只有白名單內的欄位可用於 `sort` 和 `filter`（§12.9）。
- `sensitive: true` 的欄位永遠不會出現在 `list` 的輸出中，也永遠不可用於 filter 或 sort（§12.9）。`password` 欄位不會出現在任何輸出中（§12.2）。
- 已軟刪除的資料列（已設定 `deleted_at`）會自動從 `get` 和 `list` 中排除（§12.9）。
- 所有查詢都在設有 `SET LOCAL app.tenant_id` 並強制 RLS 的交易中執行（§7.1、§12.6）。快取 key 包含 `tenant_id`（§7.1）。
- 未宣告的請求欄位一律以 422 拒絕（§2.3）。
- 大範圍查詢可以標記為 `cost: high`，在公開路徑上會觸發更嚴格的限流（§7.2）。

## 規格缺口

- 規格沒有展示 `## Request` 中列舉值的規則語法；上面的 `enum: [...]` 僅作示意。規格範例中只出現過 `format`、`min`、`max`、`length` 和 `untrusted`。
- `list` 的 `filter` 輸入的確實形式（物件還是平鋪欄位）**未有規定**。
- §12.9 中，輸入用 `page-size`（kebab-case），輸出用 `page_size`（snake_case），命名並不一致；本頁照規格原文書寫。
- `sort` 方向（asc／desc）的語法**未有規定**。
