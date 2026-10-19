# 公開寫入

決定 188：「任何人」或「公開」可直接授權 public，唔使寫 frontmatter flags。
公開 endpoint 移除 auth，public-writes 按呼叫方法有冇寫入推導；明示 frontmatter 優先。
GET 同 POST 共用 URL 時，用兩個 endpoint 檔明示同一條 route path。

來源句限制無法喺 lock 表達時，build warning 必須引述原句。
「每日最多 50 次」唔等於 per-minute rate-limit，warning 代表未執行該限制。
隱式 memory 只准決定 183 嘅 unique-key counter SQL；其他 SQL 係 build error。


## 用途

在嚴格控制的條件下，允許未經認證的訪客建立或更新資料（例如提交評論）。公開端點可以呼叫 `writes: true` 的 method（Decision 4），但 §7.2 的每一條規則都是強制的（Decisions 21–26、38、39、41、47、49、53）。

## 自然語言 source 範例

`api/catalog/v1/submit-review.md`（路由 `POST /catalog/v1/{slug}/reviews`）

```text
# Submit review
POST, path /catalog/v1/{slug}/reviews, public, no auth, tenant is determined by slug.
Visitors may leave reviews, so this writes data and requires captcha.
At most 5 per minute per IP and at most 100 per minute per tenant.
body: item_id (uuid), rating (1 to 5), comment (1 to 500 characters).
Create the review with review.submit; on success return 201 with id. If the item does not exist return 404 NOT_FOUND.
```

## 編譯後的 lock md 範例

`api/catalog/v1/submit-review.lock.md`（僅列 section；frontmatter 依 §2A.1，`kind: api`、`scope: feature`；**這個** lock 上的安全項目：`public`、`auth-removed`、`public-writes`（高風險），以及新回應欄位的 `output-new-field`（中風險））。新 INSERT 的 `write-sql` 項目（中風險）屬於 `review.submit` **method** 的 lock，而不是這個 api lock。每個 PR 最多 5 個安全項目的上限是**同一個 PR 內所有 lock 合計**計算（Decision 34），所以這個 api lock 加上 `review.submit` 合共 5 項。`review` model 中可公開寫入／`untrusted` 的欄位是另一個安全項目，必須在**更早的 PR** 中合併並部署。

````markdown
## Route
- method: POST
- path: /catalog/v1/{slug}/reviews

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_writer
- writable-columns: { reviews: [item_id, rating, comment] }
- challenge: { type: captcha, when: always }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 100
- key: tenant

### p4 audit-log
- event: public-write

## Limits
- provider: main
- timeout: 10s
- body: 16KB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | | yes |
| body.item_id | uuid | | yes |
| body.rating | integer | min: 1, max: 5 | yes |
| body.comment | string | min: 1, max: 500, untrusted | yes |

## Response
| status | schema | headers |
|---|---|---|
| 201 | { id } | Location |

## Transform
- in: { item_id: req.body.item_id, rating: req.body.rating, comment: req.body.comment }
- out: { id: r.id }

## Steps
### s1 method
- op: method
- target: review.submit
- args: transform.in
- out: r
- source-lines: 6-6

### s2 respond
- op: respond
- status: 201
- value: transform.out
- location: /catalog/v1/{req.path.slug}/reviews/{r.id}
- source-lines: 6-6

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | NOT_FOUND |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

`review.submit` 會透過 `item.get` 檢查貨品（跨物件的讀寫一律經該物件自己的 method），並執行一條參數化的 `INSERT INTO reviews (item_id, rating, comment) VALUES ($1, $2, $3)`；`comment` 在 `review` model 中標記為 `untrusted: true, max: 500`（§11.7）。

## 安全考量

- **兩個開關**：租戶必須啟用 `public_access` **及** `public_write_access`。`public_write_access` 只能由租戶管理員在平台 UI 中啟用，沒有 API、CLI 或 source 途徑（Decisions 26、41）。未啟用 `public_access` 的租戶，以及已啟用 `public_access` 但未啟用 `public_write_access` 的租戶，都會收到**完全相同的 404 內容**，並補足到同一個固定最低耗時（300ms），使呼叫方無法分辨是哪個開關未開（§7.2）。
- **按端點啟用**：在 `## Access` 中設定 `public-writes: true`（高風險）（§7.2）。
- **角色與授權**：使用獨立的 `public_writer` 角色；授權由 lock 產生，只限於實際用到的資料表、**欄位**和操作（§7.2、§10.7）。
- **操作**：只允許 `INSERT` 和 `UPDATE`，而且只限 `writable-columns`。`UPDATE` 必須有以主鍵或 RLS 限定的 `WHERE`（§7.2）。
- **黑名單**：以下欄位永遠不可公開寫入：`role`、`roles`、`permissions`、`is_admin`、`tenant_id`、`owner`、`owner_id`、`created_by`、`approved`、`verified`、`password`（只有下面的註冊規則允許）、`deleted_at` 及所有系統欄位；物件可以再加入其他欄位（§7.2）。
- **密碼寫入**（Decision 47）：公開路徑只有在三項條件同時成立時才可寫入密碼欄位：method 具備 `capabilities: [set-password]`（高風險）、寫入值是同一 method 中 `hash-password` 的輸出，以及 api lock 設有 `password-write: allow` 並在 `writable-columns` 列出密碼欄位。v1.7 的公開註冊本身絕不寫入密碼；密碼在驗證步驟設定（Decision 67，見 [user-register.md](user-register.md)）。
- **電郵驗證寫入**（Decision 53）：`email_verified_at`（屬「verified」類黑名單欄位）只可由具備 `capabilities: [confirm-verification]` 的 method 寫入，並須在單一 UPDATE 中進行：WHERE 以 `hash-token` 輸出比對唯一的 token hash 欄位、檢查有效期和未驗證狀態，並清除 hash；api 需要 `verification-write: allow`（高風險）。
- **注入租戶**：`tenant_id` 由 slug 得出；body 含有 `tenant_id`、`id` 或 `created_by` 時以 422 拒絕，在 `## Request` 中宣告它們亦屬錯誤（Decision 23、§10.7）。
- **配額**：預設每個租戶每日 1,000 次公開寫入、每個 IP 每日 50 次；租戶管理員可以調低，調高至最多 10,000 次須向平台申請。配額用完時只會封鎖公開寫入（429 加 `Retry-After`）（Decision 39、Decision 21）。
- **限流**：IP 及租戶兩種限流都是必須的；`cost: high` 的步驟只有端點上限的一半（§7.2）。
- **防機械人**（Decision 49）：`challenge: { type: none | captcha | pow, when: always | after-failures, after-failures: <n> }`。含 INSERT 的公開端點必須使用 `type: captcha` 或 `pow` 配 `when: always`；登入類端點必須使用 `{ type: captcha, when: after-failures, after-failures: 3 }`；其他不含 INSERT 的公開寫入可用 `type: none`。不含 INSERT 的密碼寫入（例如 verify-email）不需要 captcha，但仍需要 IP 加租戶限流（§7.2）。
- **審計**：公開寫入端點不可移除 `audit-log`（§10.3）。
- **不可信內容**：公開寫入的欄位在 model 中標記為 `untrusted: true`；所有輸出（包括已認證的讀取）都按 Content-Type 進行輸出編碼；每個欄位都要有長度上限，陣列要有數量上限（Decision 24）。
- **對外呼叫**：公開路徑上禁止對外呼叫，除非設定 `external-calls: allow`（高風險）（§7.2）。
- **固定最低回應耗時**：一般公開端點為 300ms，上述 404 回應同樣適用，使回應時間不會透露租戶或開關狀態（§0.1、§7.2、Decision 36）。
- 部署報告會列出所有公開寫入端點及可寫入欄位，並附上 warning（§7.2、§10.9）。
- 剩餘風險（繞過 captcha、垃圾內容）將由日後的內容審核 hook 處理（§14）。

## 規格缺口

- `daily-write-quota: platform-default` 是唯一展示過的配額語法；租戶調低後的配額會否（以及如何）出現在 lock 中，未有規定（配額在平台 UI 中設定）。
- `untrusted` 在這裡以 `## Request` 規則出現，在 model 中則是 `untrusted: true`；model lock 的對應語法沒有展示。
- 物件如何宣告額外的黑名單欄位，未有規定。
- `challenge: { type: pow }` 的參數（難度）未有規定。
- model 層的 `public_writer` 授權是所有公開端點的聯集，而每個 api 只取得自己的欄位（§12.8 註解）；平台如何為同一個 role 實現按端點的授權沒有描述。
