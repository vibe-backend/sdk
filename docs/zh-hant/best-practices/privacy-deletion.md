# 私隱刪除（crypto-shredding 加匿名化）

## 用途

在**完全不**啟用 `DELETE` 的前提下，處理資料當事人的刪除要求（例如《個人資料（私隱）條例》、GDPR）：敏感個人欄位以 crypto-shredding 處理，其他個人欄位予以匿名化，記錄則標記為已刪除並軟刪除（Decision 42、§12.10）。刪除流程**由平台執行，而不是由 lock 執行**；你在 source 中要做的，是正確分類個人欄位。

## 自然語言 source 範例

`models/customer/customer.md`

```text
# customer
Customer profiles, stored in the customers table of provider main.

Fields:
- id: uuid, primary key, auto-generated.
- user_id: uuid; identifies the data subject for privacy erasure.
- email: valid email, unique; standard personal data.
- name: at most 100 characters; standard personal data.
- phone: at most 20 characters; sensitive personal data.
- address: at most 300 characters; sensitive personal data.
- date_of_birth: timestamp; sensitive personal data.
- tier: string.

Provide get, list and update CRUD. list defaults to 20 per page, at most 100, sort by created_at.
```

刪除由租戶管理員在 source 以外觸發：

```text
subject erase --subject <id> --reason "..."
```

## 編譯後的 lock md 範例

`models/customer/customer.lock.md`（僅列 section；`kind: model`）

````markdown
## Table
- provider: main
- table: customers

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| user_id | uuid | subject-key | no |
| email | string | format: email, unique, personal: standard | no |
| name | string | max: 100, personal: standard | no |
| phone | string | max: 20, sensitive, personal: sensitive | no |
| address | string | max: 300, sensitive, personal: sensitive | no |
| date_of_birth | timestamp | sensitive, personal: sensitive | no |
| tier | string | | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at], filter: [] |
| update | partial: true, optimistic-lock: updated_at |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT]
````

欄位規則中的 `personal: standard | sensitive` 依照 v1.7 的 `user` 和 `email-outbox` lock（§12.8、§12.11.2）。`subject-key` 依照 Decision 65 的語法（§12.10）：`subject-key: {column}` 或 `subject-key: [{column}, ...]`（任一符合即算），其中 `{column}` 是指向 `users.id` 的 `uuid` 欄位（`users` 表用其本身的 `id`），或 `hash:{column}`，即存放 `HMAC-SHA-256(pepper, normalize(email))` 的 `bytea` 欄位，供沒有 `user_id` 的資料列使用（例如 `email_outbox.recipient_hash`）。有個人欄位但沒有 `subject-key` 的 model 屬 validator 錯誤。`erased_at` 在 v1.7 是平台系統欄位（F5）。

## 安全考量

- **加密**（`personal: sensitive`）：每筆記錄的每個敏感欄位都有自己的 DEK（AES-256-GCM）；DEK 由平台 KMS 中的租戶 KEK 包裝，保存在平台金鑰庫，與業務資料分開，也不會進入資料庫備份。加密／解密自動進行；lock 永遠看不到金鑰（§12.10）。
- **觸發**：由租戶管理員透過平台 UI 或 CLI `subject erase` 觸發；必須填寫原因及請求參考編號；由一位管理員提交、另一位確認（只有一位管理員的租戶：延遲 24 小時生效）（§12.10）。
- **流程**（由平台執行）：(1) 透過 `subject-key` 欄位找出當事人的所有記錄；(2) 銷毀敏感欄位的 DEK（crypto-shredding）；(3) 使用平台刪除角色，以不可逆的佔位值覆寫一般欄位（例如 `erased-<random>`，唯一欄位則用隨機唯一值），只用 UPDATE，不用 DELETE；(4) 設定 `erased_at` 並軟刪除，保留資料列及關聯；(5) 撤銷當事人所有 refresh token 家族及 API key（§12.10）。
- **期限**：在收到請求後 30 日內完成；平台目標為觸發後 24 小時內完成。
- **審計**：記錄觸發者、確認者、原因、參考編號、受影響的資料表／欄位、記錄數量及完成時間，當事人只以假名化 id 記錄（§12.10）。
- **備份**：經 shredding 的欄位在備份中無法讀取；一般欄位會在 35 日備份保留期屆滿後消失；還原備份時，會先重播刪除紀錄才重新開放（§12.10）。
- **日誌**：個人資料絕不可寫入日誌；日誌只保留假名化 id，保留 30 日（§12.10）。
- **不可重新寫入**：lock 不可再對已刪除記錄寫入個人欄位（§12.10）。
- **電郵 outbox**（Decision 68）：`email_outbox` 資料列帶有 `recipient_hash`，因此即使沒有 `user_id` 的資料列（「已有帳戶」通知）也能在刪除時找到並匿名化；無論如何，`sent_at` 後 30 日郵件 worker 都會遮蓋電郵並把 token 設為 null（§12.11.3）。
- **審計絕不儲存明文電郵**（只存 HMAC 雜湊或遮蓋值）（Decision 68）。
- 敏感欄位也應同時標記 `sensitive: true`，使其不會出現在 `list` 輸出、filter 和 sort 中（§12.9）。
- `DELETE` 在任何地方仍然禁止（§2A.4）。
- 規格 §13 要求就《個人資料（私隱）條例》／GDPR 方面進行法律審閱；本頁並非法律意見。

## 規格缺口

- `sensitive: true`（§12.9，令欄位不出現在 list／filter／sort）與 `personal: sensitive`（§12.10，crypto-shredding）是同一個標記還是互相獨立，**並不明確**；本頁兩者都設定。
- `subject-key` 的位置在各範例中不一致：在 migration 的 `## Data` section（§12.11.1）、在 model 的 `## Policies`（§12.11.2），以及作為欄位規則（`recipient_hash` 一列）。哪一個具權威性沒有說明。
- 已刪除記錄與 `unique` 約束之間如何互動（日後重用同一電郵）未有規定。
- §14 現在只把資料當事人**自助**提交入口列為未來工作（Decision 50）；§12.10 中租戶管理員的 `subject erase` CLI 流程是已定義的途徑。
