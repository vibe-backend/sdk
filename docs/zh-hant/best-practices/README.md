# 最佳實踐

本目錄收錄撰寫 Markdown 驅動後端定義的最佳實踐模式。每一頁都**嚴格**依據目前定稿的規格 [`docs/spec-v1.7.md`](../../spec-v1.7.md) 撰寫。文中「§7.2」、「Decision 48」等章節引用均指該份規格。舊版 [`docs/spec-v1.5.md`](../../spec-v1.5.md) 只作歷史參考。

> **規格版本說明：** 這些頁面已按**規格 v1.7** 修訂（issue #17）。與 v1.5 版本相比的主要改動：樂觀鎖改用 `ETag`／`If-Match`，缺少時回 428、不一致時回 412（Decision 48）；公開註冊不收密碼，並一律回 202（Decisions 56、67）；登入要求電郵已驗證（Decision 55）；admin 建立用戶改為邀請流程（Decision 69）；API key、JWT、rate-limit、refresh token 和 LLM provider 的設定改用 v1.7 已定案的數值（Decisions 43–45、57–59、61、62、65、72）。頁面與規格有出入時，以規格為準。

每一頁的結構相同：

1. **用途**：這個模式的作用，以及何時使用。
2. **自然語言 source 範例**：由人撰寫的 `.md` 檔（§2.2）。
3. **編譯後的 lock md 範例**：編譯器產生的 `{name}.lock.md`（§2A）。lock 檔由編譯器產生，絕不可手寫或手動修改；這裡的範例僅作示意。
4. **安全考量**：確定性驗證器強制執行的規則（§10），以及審核者必須檢查的事項（§2A.5）。
5. **規格缺口**：v1.7 未有規定或前後不一致的地方。凡規格沒有定義的，這些頁面會直接說明，而不會自行猜測。

> 規格本身以粵語撰寫；這裡的 source 範例則使用英文。自然語言 source 可以用任何語言撰寫，執行階段只以編譯後的 lock 為準（§2.1）。

## 範例慣例

- 範例中的名稱（`jack@example`、`mei@example`）和 PR 編號（`pr#121` …）均為**虛構**，風格仿照規格中的範例。v1.8 起 lock 不再有核准欄位或簽名（§2A.1）。
- 雜湊值為縮短的佔位符（`sha256:9a1c...e2`），與規格範例的寫法一致。
- 以節錄方式（「僅列出 section」）展示的 lock，其 frontmatter 完全依照 §2A.1；api lock 另外帶有 `scope:`（§2A.3）。
- lock 未使用的 section 以 `(none)` 表示（§2A.2）。

## 索引

| 主題 | 頁面 |
| --- | --- |
| Auth pipeline：JWT（以及經 create-user 的 admin 邀請） | [auth-jwt.md](auth-jwt.md) |
| Auth pipeline：API key | [auth-api-key.md](auth-api-key.md) |
| CRUD：offset 分頁 | [crud-offset-pagination.md](crud-offset-pagination.md) |
| CRUD：樂觀鎖（`ETag`／`If-Match`） | [crud-optimistic-locking.md](crud-optimistic-locking.md) |
| CRUD：軟刪除 | [crud-soft-delete.md](crud-soft-delete.md) |
| 註冊（`user.self-register`、`user.complete-verification`、admin `user.register`） | [user-register.md](user-register.md) |
| `user.login` | [user-login.md](user-login.md) |
| `sign-token`（token 簽發 capability） | [sign-token.md](sign-token.md) |
| Refresh token 輪換 | [refresh-token-rotation.md](refresh-token-rotation.md) |
| 限流 | [rate-limit.md](rate-limit.md) |
| 公開寫入 | [public-writes.md](public-writes.md) |
| 私隱刪除（crypto-shredding 加匿名化） | [privacy-deletion.md](privacy-deletion.md) |
| LLM provider 設定（編譯器 endpoint） | [llm-provider-setup.md](llm-provider-setup.md) |

## 通用規則（適用於每一頁）

- 執行階段永不解譯自然語言；只執行通過部署驗證、經已簽名的 manifest 交付的 lock（§2.1、§9）。
- 每個 endpoint 預設都經過 `auth`；移除 `auth`（`-auth`）必須同時 `public: true`（§3、§4）。
- 任何 lock 都不得有 `DELETE` 或 `TRUNCATE` SQL，`kind: migration` 以外的 lock 不得有 DDL；每個 `UPDATE` 都要有 `WHERE` 加 `scope: pk | rls`（§2A.4、§2B、Decisions 27、51）。
- SQL 一律參數化（`$1`、`$2` 加 `params`），且只有單一 statement（§2A.2、§2A.4）。
- lock 不記錄核准（v1.8，Decisions 77–80）。`security-items` 僅作資訊性標示；人手審核經 Git 平台的 pull request 進行（rulesets、CODEOWNERS；見 [pr-review-guide.md](../../pr-review-guide.md)）（§2A.5、§9C）。
- 租戶內的列層擁有權使用 `owner-key`（Decision 70、§12.6a）；RLS 本身只隔離租戶。
- Secret（DB 連線、JWT 金鑰、LLM 金鑰）只以引用出現，絕不出現在 source、lock、manifest 或 log（§2A.8.5、§9B）。審計永不記錄明文電郵（Decision 68）。
