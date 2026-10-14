# 最佳實踐

本目錄收錄撰寫 Markdown 驅動後端定義的最佳實踐模式。每一頁都**嚴格**依據 [`docs/spec-v1.5.md`](../../spec-v1.5.md)（v1.5 規格）撰寫。文中「§7.2」、「Decision 30」等章節引用均指該份規格。目前的規格為 [`docs/spec-v1.7.md`](../../spec-v1.7.md)。

> **規格版本說明：** 本目錄依據 **規格 v1.5** 撰寫。規格 v1.7 其後已定稿並合併，取代 v1.5。這些頁面**尚未按 v1.7 修訂**；內容有出入時請以 v1.7 規格為準，尤其是樂觀鎖頁面、登入頁面（v1.7 要求電郵驗證後才可登入）以及註冊流程頁面（v1.7 改為在驗證步驟設定密碼，而非註冊時設定）。

每一頁的結構相同：

1. **用途**：這個模式的作用，以及何時使用。
2. **自然語言 source 範例**：由人撰寫的 `.md` 檔（§2.2）。
3. **編譯後的 lock md 範例**：編譯器產生的 `{name}.lock.md`（§2A）。lock 檔由編譯器產生，絕不可手寫或手動修改；這裡的範例僅作示意。
4. **安全考量**：確定性驗證器強制執行的規則（§10），以及審核者必須檢查的事項（§2A.5）。
5. **規格缺口**：規格未有規定的地方。凡規格沒有定義的，這些頁面會直接說明，而不會自行猜測。

> 規格本身以粵語撰寫；這裡的 source 範例則使用英文。自然語言 source 可以用任何語言撰寫，執行階段只以編譯後的 lock 為準（§2.1）。

## 範例慣例

- lock frontmatter 中的審核者名稱（`jack@example`、`mei@example`）、PR 編號（`pr#121` …）和 commit id 均為**虛構**，風格仿照規格中的範例。
- 雜湊值、簽名和 commit id 為縮短的佔位符（`sha256:9a1c...e2`、`MEUCIQ...`），與規格範例的寫法一致。
- 以節錄形式（「僅列 section」）展示的 lock，frontmatter 完全依照 §2A.1；api lock 另外帶有 `scope:`（§2A.3）。
- lock 未使用的 section 一律寫作 `(none)`（§2A.2）。

## 索引

| 主題 | 頁面 |
| --- | --- |
| 認證 pipeline：JWT | [auth-jwt.md](auth-jwt.md) |
| 認證 pipeline：API key | [auth-api-key.md](auth-api-key.md) |
| CRUD：offset 分頁 | [crud-offset-pagination.md](crud-offset-pagination.md) |
| CRUD：樂觀鎖 | [crud-optimistic-locking.md](crud-optimistic-locking.md) |
| CRUD：軟刪除 | [crud-soft-delete.md](crud-soft-delete.md) |
| `user.register` | [user-register.md](user-register.md) |
| `user.login` | [user-login.md](user-login.md) |
| `sign-token`（token 簽發 capability） | [sign-token.md](sign-token.md) |
| Refresh token 輪換 | [refresh-token-rotation.md](refresh-token-rotation.md) |
| 限流 | [rate-limit.md](rate-limit.md) |
| 公開寫入 | [public-writes.md](public-writes.md) |
| 私隱刪除（crypto-shredding 加匿名化） | [privacy-deletion.md](privacy-deletion.md) |
| LLM provider 設定（編譯器 endpoint） | [llm-provider-setup.md](llm-provider-setup.md) |

## 通用規則（適用於每一頁）

- 執行階段永遠不會解讀自然語言；只會執行已確認並簽名的 lock（經由簽名 manifest 載入）（§2.1、§9）。
- 每個端點預設都經過 `auth`；要移除它（`-auth`）必須同時設定 `public: true`（§3、§4）。
- 任何 lock 都不可包含 `DELETE`、`TRUNCATE` 或 DDL SQL；每個 `UPDATE` 都必須有 `WHERE`，並加上 `scope: pk | rls`（§2A.4、Decision 27）。
- SQL 一律參數化（`$1`、`$2` 加 `params`），且每次只能有一條語句（§2A.2、§2A.4）。
- 由自然語言編譯的 lock，以及任何涉及安全項目的 lock，都需要人工確認；高風險項目需要兩位非作者的 owner／maintainer 確認；每個 PR 最多 5 個安全項目（§2A.5、Decisions 19、33、34）。
- 機密資料（資料庫連線、JWT 金鑰）只能以引用形式出現，絕不可寫入 source、lock、manifest 或日誌（§9B）。
