# CRUD：軟刪除

## 用途

刪除記錄而不銷毀資料。在這個平台上，**所有刪除都是軟刪除**：注入的 `delete` CRUD method 會設定 `deleted_at`，已軟刪除的資料列會自動從 `get` 和 `list` 中排除（§12.9、Decision 13）。平台沒有硬刪除、沒有 purge，也沒有按保留期限自動過期。

## 自然語言 source 範例

`models/note/note.md`

```text
# note
User notes, stored in the notes table of provider main.

Fields:
- id: uuid, primary key, auto-generated.
- title: 1 to 200 characters.
- body: at most 10000 characters.
- user_id: uuid, the owner of the note; only the owner may read, change or delete it.

Provide all CRUD. list defaults to 20 per page, at most 100, can sort by created_at.
```

`api/notes/v1/delete-note.md`（路由 `DELETE /notes/v1/{id}`）

```text
# Delete note
DELETE, path /notes/v1/{id}, id is a uuid. Requires JWT; users may delete only their own notes.
Delete the note with note.delete and return 204.
Note not found returns 404 NOT_FOUND.
```

## 編譯後的 lock md 範例

`models/note/note.lock.md`（僅列 section；`kind: model`）

````markdown
## Table
- provider: main
- table: notes
- owner-key: user_id

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| title | string | min: 1, max: 200 | no |
| body | string | max: 10000 | no |
| user_id | uuid | | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| create | |
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at], filter: [] |
| update | partial: true, optimistic-lock: if-match |
| delete | soft |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT]
````

留意授權設定：任何執行階段角色都沒有 `DELETE` 權限（§7.1）。

`api/notes/v1/delete-note.lock.md`（僅列 section；`kind: api`、`scope: feature`；`dependencies` 包含 `note/crud/delete`）

````markdown
## Route
- method: DELETE
- path: /notes/v1/{id}

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.id | uuid | | yes |

## Response
| status | schema | headers |
|---|---|---|
| 204 | (none) | |

## Transform
- in: { id: req.path.id }
- out: (none)

## Steps
### s1 method
- op: method
- target: note.delete
- args: transform.in
- out: (none)
- source-lines: 3-3

### s2 respond
- op: respond
- status: 204
- source-lines: 3-3

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | NOT_FOUND |
````

如果你撰寫自訂的刪除 method，它必須是同時強制擁有權的 `UPDATE`，例如：

````markdown
### s1 query
- op: query
- provider: main
- table: notes
- sql: `update notes set deleted_at = now() where id = $1 and user_id = $2 and deleted_at is null`
- params: [input.id, ctx.user.id]
- writes: true
- scope: pk
- columns: [deleted_at]
- out: (none)
- source-lines: 3-3
````

## 安全考量

- **資料列擁有權**（Decision 70、§12.6a）：RLS 只會隔離租戶。`owner-key: user_id` 令 validator 要求非 admin 的 read／update SQL 帶 `user_id = ctx.user.id`（軟刪除屬 UPDATE），insert 則由 `ctx.user.id` 填入 `user_id`，絕不取自輸入。要讓 admin 或整個租戶存取任何筆記，必須在 source 明寫一句，編譯為 `ownership: role(admin)` 或 `ownership: tenant-wide`，屬高風險項目。沒有 `owner-key` 的 model 視為租戶共用資料，並會出 warning。
- 任何 lock 都絕不可出現 `DELETE` SQL；資料庫角色沒有 `DELETE`／`TRUNCATE` 權限（§2A.4、§7.1、§10.8）。沒有 `soft: false` 選項（§12.9）。
- `deleted_at`（及 `erased_at`）是系統欄位：不可由請求輸入寫入（§12.9），並且在公開寫入黑名單上（§7.2）。
- 注入的 `get`／`list` 會自動排除已軟刪除的資料列。**自訂 method 必須自行排除**（例如 `deleted_at is null`，如 §12.8 的 `user.login` 所做）。
- 已軟刪除的資料會永久保留。如收到法律上的刪除要求，請使用[私隱刪除](privacy-deletion.md)（crypto-shredding 加匿名化），而不是 DELETE（§12.9、§12.10）。
- 每個 `UPDATE` 都需要帶 `scope: pk | rls` 的 `WHERE`（§2A.4）。

## 規格缺口

- 任何規格 lock 範例都**沒有展示** `create` 和 `delete` 的 `## CRUD` 選項語法（§12.8 只有 get／list／update）。上面的 `delete | soft` 及空白的 `create` 一行僅作示意。
- 注入的 `delete` 的輸出及錯誤集合（例如對已刪除的 id 是否以 `NOT_FOUND` 失敗）**未有規定**。
- **注入的** CRUD method 如何套用 `owner-key`（例如 `note.delete` 會否自動加上 `user_id = ctx.user.id`）沒有明確說明；§12.6a 只規定了 SQL 的 validator 規則，而非注入 method 的 lock。
- 執行軟刪除的 api 是否也必須傳送 `If-Match`（Decision 48 只要求 `update`）**未有規定**。
- 是否有「取消刪除／還原」的途徑**未有規定**。
- 回傳 204 的 `respond` 是否接受 `value` 並未展示；這裡予以省略。
