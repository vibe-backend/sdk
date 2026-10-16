# Sample Shop 核心 SDK 實作計畫與契約對照（草案）

- 狀態：Draft，待產品範圍與相關 PR 分流確認
- SDK 基線：`vibe-backend/sdk@413ccd6`（`main`，2026-10-15）
- 消費端基線：`vibe-backend/sample-shop` PR #62（`backend/markdown-v1`，Open Draft）
- 規格基線：`docs/spec-v1.8.md`

## 1. 目標與邊界

本計畫的目標是讓 SDK 能將 sample-shop PR #62 的核心 Markdown backend 編譯、驗證並以 HTTP runtime 執行，使 sample-shop 的 real adapter 可完成核心購物流程。

### 本階段範圍

1. 中文自然語言 source 可重現地產生合法 lock，並可逐檔診斷與重試，但 build／lock 輸出維持 all-or-nothing；任何檔案失敗都不留下部分輸出。
2. 核心驗證與 HTTP 能力：auth、CORS、跨 model transaction、冪等、cursor 分頁、關聯欄位／expand、錯誤細節、ETag／`If-Match`。
3. sample-shop PR #62 endpoint matrix 的全部 26 條核心 API：
   - 註冊、登入、目前使用者。
   - 公開／店員商品與 SKU。
   - 購物車。
   - 地址簿。
   - 下單、查單、付款、取消、確認收貨與店員狀態更新。
4. 以 sample-shop 契約 fixture 加入 compile、validate、serve 與 HTTP contract tests。

### 非目標

- 不把商品、訂單、庫存或付款等商店業務規則硬編碼至 SDK。
- 不在本階段承諾前端 `ShopFeatures` 的收藏、優惠券、錢包、評論、問答、售後、通知與其他未列於 PR #62 的功能。
- 不處理 UI 實作。
- 不合併 sample-shop PR #62。
- 不在 PR 尚未審查、合併及 release scope 未確認前發布版本。

## 2. 現況與風險

### 儲存庫狀態

盤點時 `sdk/main` 與 `sample-shop/main` 工作樹均乾淨。sample-shop PR #62 仍為 Open Draft，backend 只存在於 `backend/markdown-v1`，尚無生成的 lock／manifest，也尚未以 `vibe serve` 驗證。

SDK 目前有多個未合併且部分相互堆疊的 PR。直接在 `main` 重做會造成衝突，實作前須先決定合併／重建順序：

- #75：reference field／expand（Draft；2026-10-16 已合併 `main` 解衝突；產品決定延至 v1.1，見下方風險）
- #77、#95、#106：auth 三階段（#95 已合併其 base 解衝突；#77 與 `main` 仍有衝突）
- #88：method step validator（`main` 已有對應 commit `bcf1207`；PR 本身仍為 open，待確認是否可關閉）
- #92、#93、#96：自然語言編譯與中文 source（#92 已合併 `main` 解衝突；#93 仍需處理）
- #99：MemoryExec RLS／grant（Draft；已合併 `main` 解衝突；其 PostgreSQL 共用 runtime login 模型與 spec v1.8 決定 97 的租戶 login role 模型不一致，需重新對齊）
- #116：release signing（與核心 runtime 無直接依賴）

以上狀態於 2026-10-16 更新；實作前須以當時 PR 狀態重新確認。

### 主要風險

1. **規格與契約漂移**：PR #62 文件仍提及 v1.7 與舊 SDK commit，但 `main` 已合併 spec v1.8。
2. **堆疊 PR 衝突**：不可把「已有 open PR」視為 SDK 已具備能力；每項能力都須在整合基線重新測試。
3. **安全**：auth、refresh cookie、CORS credentials、tenant isolation、RLS、冪等資料與錯誤內容均涉及跨租戶或敏感資料風險。
4. **範圍衝突**：#75（ref／expand）已由產品決定延至 v1.1，但本計畫將 ref／expand 列入本階段；須先確認 sample-shop 購物車商品摘要改用其他方式，或由產品重新納入範圍。
5. **發布**：crate 仍為 `0.1.0`，目前沒有 GitHub Release；版本與 release scope 尚未核准。
6. **測試環境**：本機盤點時沒有 `cargo`，尚未取得本機基線結果；CI 使用 Rust 1.89 與 PostgreSQL 17。開始程式修改前須補齊工具鏈並跑完整基線。

## 3. 前端需求與 SDK 能力對照

| sample-shop 契約／前端需求 | SDK `main` 現況 | 既有工作 | 本計畫處置 |
| --- | --- | --- | --- |
| 中文自然語言 source 穩定編譯 | 支援 LLM compile，但 sample-shop 9 次整批失敗 | #37、#83；PR #92/#93/#96 | 整合／重建後以固定 fixture 驗證；保留逐檔輸出、修復重試與可診斷輸出 |
| login／register／refresh | spec 已定義，runtime 尚未完整落地 | #26；PR #77/#95/#106 | 先完成與審查堆疊 PR；以 uniform auth errors、rotation/reuse tests 驗收 |
| cart item 關聯 product 摘要 | 缺通用 ref／expand | #21；PR #75 | 將通用 reference 與受限 expand 留在 SDK；業務投影留在 sample-shop source |
| 跨 model checkout transaction | spec v1.8 已定義語意；runtime 能力需補齊與驗證 | #34、PR #94 等相關工作 | 實作單 DB transaction、timeout/conflict mapping、rollback 與庫存競態測試 |
| `Idempotency-Key` | 規格已文件化；平台執行能力需補齊 | #85 | 實作 `(tenant, subject, route, key)` scope、request hash、response replay、TTL 與併發唯一性 |
| CORS／preflight | 規格已文件化；runtime 需補齊 | #84 | 僅允許明確 origin；驗證 `*` + credentials 禁用；支援 required/exposed headers |
| cursor pagination／range filter | 規格已文件化；內建 CRUD 尚缺 | #87 | 提供不透明且防竄改 cursor、穩定排序與 filter validation |
| PATCH partial update | sample-shop 核心目前採 PUT；spec 已定義 PATCH | #86 | 不阻塞 PR #62；保留相容性測試，避免 PUT 行為破壞 |
| ETag／`If-Match` | spec v1.8 已定義 | 相關 v1.8 工作 | 實作／驗證 428、412、成功後新 ETag |
| error `details`／`retryable` | 基本 error envelope 已有，擴充欄位不足 | 尚待去重確認 | 以向後相容的 optional fields 擴充，禁止洩漏 SQL、token 或跨租戶識別資訊 |
| tenant isolation／RLS | 有 PostgreSQL runtime 與 migration 防護 | PR #99 等 | PostgreSQL 與 memory executor 都測跨 tenant deny；memory mode 不得給錯誤安全保證 |
| SKU／庫存／狀態機 | 屬 sample-shop 業務 source | sample-shop #62 | SDK 僅提供原子操作、驗證與錯誤映射，不內建商店規則 |

### 行為真相與契約修正 gate

- SDK 通用 HTTP 行為以 `docs/spec-v1.8.md` 為唯一真相；sample-shop contract 可增加業務限制，但不得覆寫平台狀態碼或安全語意。
- 在 runtime 實作前，sample-shop PR #62 必須修正三項已確認漂移：同 key 不同 body 由 409 改為 422 `IDEMPOTENCY_KEY_REUSED`；invalid／mismatched cursor 明定為 400 `INVALID_CURSOR`；註冊密碼長度由 8–72 對齊 v1.8 `hash-password` 的 12–256，過短回 422 `WEAK_PASSWORD`。
- PR #62 的 endpoint matrix（目前 26 條，含地址簿 #21–#26）全部納入並須凍結成 contract test 清單；checkout 測試必須由同一登入使用者先建立並選取地址。refresh／logout 若納入本階段，需補入 matrix，否則明確列為依賴 auth phase、不可宣稱核心 auth 全部完成。

## 4. SDK 介面草案

最終語法以 `docs/spec-v1.8.md` 為準；以下只描述需穩定提供的通用介面。

### Compile CLI

```text
vibe compile [SOURCE ...] [--keep-going] [--dump-llm DIR]
```

- 輸入、依賴摘要及 prompt 順序必須穩定。
- 格式修復有明確上限，不可無限重試。
- diagnostic 不輸出 API key、token、cookie 或完整敏感 source。
- 可保留翻譯／LLM cache 供重試，但 lock／build 輸出是原子性的；任何失敗皆不落下部分 lock，並回非零 exit code 及可機器解析的摘要。

### HTTP runtime

- CORS：從 API definition allowlist 產生 preflight 與實際回應標頭。
- Idempotency：以 tenant、subject、route、key 與 canonical request hash 辨識；同 key 不同 request 回 422 `IDEMPOTENCY_KEY_REUSED`；409 僅用於有效租約仍在處理中的 `IDEMPOTENCY_IN_PROGRESS`；相同 request 重播首次狀態碼、必要標頭與 body。
- Transaction：跨 model method steps 在同一資料庫 transaction 執行；serialization/deadlock 映射為可重試 `TX_CONFLICT`，逾時映射 `TX_TIMEOUT`。
- Pagination：cursor 封裝排序鍵、id、filter identity 與版本；簽章、版本或 sort／filter 驗證失敗回 400 `INVALID_CURSOR`，不接受任意 SQL sort。
- Concurrency：需要版本保護的更新缺 `If-Match` 回 428、不符回 412。
- Error envelope：

```json
{
  "error": {
    "code": "TX_CONFLICT",
    "message": "request could not be completed",
    "request_id": "opaque-id",
    "retryable": true,
    "details": {}
  }
}
```

`retryable`、`details` 為 optional，既有只讀取 `code`／`message`／`request_id` 的 client 必須維持相容。

### Rust 公開 API

本工作優先使用現有 `compile`、`validate`、`runtime` façade，不新增 sample-shop 專用 public type。若必須增加 public API：

- 先提供最小 façade，不暴露儲存層細節。
- 為序列化格式與 error enum 加相容性測試。
- `0.1.x` 仍須在 PR 記錄 breaking changes；不得以 pre-1.0 為由無提示破壞 CLI、lock 或 manifest。

## 5. 測試與驗收

### SDK 層

1. `cargo check --locked --all-targets --all-features`
2. `cargo fmt --all -- --check`
3. `cargo clippy --locked --all-targets --all-features -- -D warnings`
4. `cargo test --locked --all-features`
5. Markdown lint、offline link check、繁中／英文 code block parity。

### Contract fixture

從 sample-shop PR #62 建立固定、可審查的 fixture，不在測試時呼叫不確定性 LLM。至少涵蓋：

- 全部 source 可 compile、validate、build manifest；任一檔失敗時無部分 lock／build 輸出。
- public catalog 與 staff-only route 權限。
- register/login 的相同 401 行為、密碼 12–256 規則與 422 `WEAK_PASSWORD`，以及 refresh rotation/reuse。
- unknown request fields、型別與 boundary validation 回 422。
- CORS allow／deny 與 preflight。
- cursor 首頁、末頁、非法／竄改或 sort／filter 不符時回 400 `INVALID_CURSOR`，以及資料插入後穩定性。
- 冪等相同 key／相同 body 重播、相同 key／不同 body 回 422 `IDEMPOTENCY_KEY_REUSED`、有效處理租約回 409 `IDEMPOTENCY_IN_PROGRESS`、兩個併發相同 key 只提交一次。
- checkout 成功、缺貨 rollback、serialization conflict、timeout、跨 tenant 資料不可見。
- `If-Match` 缺少 428、不符 412、正確版本成功。
- error optional fields 與敏感資訊遮蔽。

### sample-shop 端到端驗收

- 在 PR #62 branch 用整合後 SDK 重新 compile，不提交未審查的模型輸出。
- 以同一登入使用者建立地址、加入購物車並以 `address_id` 完成下單，覆蓋 browse → cart → address → checkout。
- 對生成 lock 做人工 diff 與 `vibe validate`。
- `vibe serve` 下執行 frontend real adapter 核心流程，不以 mock 成功取代驗收。
- PostgreSQL 17 為權威整合測試；memory mode 僅供快速測試。

## 6. 相容性與安全

- 所有 tenant-owned query 與 idempotency record 都必須綁 tenant；subject-scoped record 不得跨使用者重播。
- refresh token 只存雜湊；cookie 使用 `__Host-vibe_rt_{slug}`、HttpOnly、Secure、Path=/ 且無 Domain。same-site 使用 SameSite=Strict；cross-site 使用 SameSite=None，且強制精確 Origin allowlist、`X-Vibe-CSRF: 1`、preflight 與 `Vary: Origin`。credentials 只允許 refresh／logout。
- auth failure 維持一致時間與回應，避免帳號枚舉。
- cursor 必須簽章或 MAC，並綁 filter／sort；不可直接接受 client SQL 片段。
- `details` 只允許 schema 驗證後的公開欄位，不回傳 SQL、stack trace、內部 table 名或憑證。
- lock／manifest schema 新欄位採可選或版本化；舊產物的接受／拒絕策略須有明確錯誤與 migration 說明。
- release artifact 必須由正式 workflow 建立；不得用長期私鑰或本機未追蹤 artifact 發布。

### Release gate

以下任一未完成即不得發布：#28 release hardening、#29 PostgreSQL／coverage、#114 audit attestation、#115 high-risk reason，以及 #118 challenge proof（若 release scope 暴露 challenge 路徑）。另須確認所有核心 PR 已合併、main CI 全綠、版本與 changelog 核准。

## 7. 分階段交付

### Phase 0：基線與分流

- 安裝／確認 Rust 1.89 與 PostgreSQL 17 測試環境。
- 跑 `main` 完整基線。
- 審查並決定 #75、#77/#95/#106、#88、#92/#93/#96、#99 的合併、重建或關閉順序。
- 將 PR #62 契約更新至 spec v1.8，凍結核心 endpoint 清單。

### Phase 1：編譯與驗證閉環

- 整合自然語言 compile 修補與 method step validation。
- 建立 sample-shop deterministic contract fixture。
- 驗收 compile → validate → manifest，不先追求完整 HTTP 業務流程。

### Phase 2：核心 runtime primitives

- 依序交付 auth、CORS、transaction／error mapping、idempotency、cursor、ref／expand、ETag。
- 每個 primitive 使用獨立 PR 或可審查 commit，避免單一巨型 PR。

### Phase 3：sample-shop 整合

- 重新編譯 PR #62 source，人工審查 lock。
- 跑 PostgreSQL contract tests 與 frontend real adapter 核心流程。
- 將非核心 ShopFeatures 去重後分別建立具體 issue。

### Phase 4：穩定化與發布候選

- 全量 CI、相容性、安全與 release workflow 檢查。
- 盤點所有 blocker、changelog、版本號與 rollback 說明。
- 僅在 PR 已審查合併、CI 全綠、release scope 與版本由負責人確認後發布；否則停止於 release candidate。

## 8. Issue 與 PR 策略

- 先搜尋 open／closed Issue 與 PR，已有 #21、#26、#37、#78、#83–#87 等工作不得重複建立。
- 新 issue 必須包含需求、使用者影響、建議通用方案、非目標、相容性／安全風險與可驗收條件。
- 核心實作 PR 必須列出範圍、非目標、設計決策、關聯 SDK issue 與 sample-shop #62、實際測試結果、未完成項目與 rollout／rollback。
- 本計畫文件 PR 只供設計審查，不代表核准實作、合併或發布。

## 9. 待確認事項

1. 產品範圍是否採「PR #62 核心契約」而非 frontend 全部 `ShopFeatures`。
2. #75、#77、#92、#99 等 DIRTY／Draft PR 是由原作者合併 base 更新（不 rebase），還是由本工作以新整合 PR 重建。
3. refresh cookie 的實際部署 topology、SameSite 與 credentials 策略。
4. `details` 的公開 schema，以及 `PRODUCT_UNAVAILABLE` 可揭露欄位。
5. 第一個可發布版本是維持 `0.1.0`、升 `0.1.1`，或在 scope 完成後定義其他版本；未確認前不發布。
