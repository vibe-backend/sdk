# Markdown 驅動 Serverless Backend：架構 Spec v1.8（草稿）

<!-- spec-status: draft -->
>
> 狀態：草稿，喺 v1.7 定稿基礎上修改，基準係 vibe-backend/sdk main `4239c95` 嘅 `docs/spec-v1.7.md`（相對 merge commit `df378ce`，只有第 1 節目錄改成樹狀圖，已經包含喺本稿）。新決定 73–119 已經由產品經理確認（2026-10-14；決定 80、116 已取代）；文件仍然係草稿（`spec-status: draft`），v1.8 正式定稿之前 v1.7 照樣係有效定稿。網站 `/latest/` 照樣指 v1.7（呢份文件有 `spec-status: draft` 標記，build 時唔會成為 latest，決定 94）。
>
> 核心架構：所有定義（model、method、api、pipeline、provider、migration）嘅 source `.md` 一律用自然語言寫。編譯流程係「自然語言 source `.md` → 結構化 lock markdown（`{name}.lock.md`）→ 簽名 manifest」。lock md 係 runtime 唯一依據，runtime 永遠唔會解譯自然語言。
>
> v1.4 起嘅內容：安全設定照用自然語言寫，唔強制結構化 frontmatter，防線改為「獨立 deterministic 驗證器 + 收緊嘅人手 review」（v1.8 起 SDK 唔再做人手 review，見決定 77、78 同 9C）；編譯器冇工具、網絡只准連編譯用 LLM endpoint；model snapshot pin 死，compiler-hash 只計編譯器版本同 prompt 版本（決定 174）；確認綁定 commit；收緊 public 寫入；禁止 DDL／TRUNCATE；token 簽發用 capability 旗標；新增緊急 kill switch；public 最低回應耗時改為固定值。
>
> v1.5 定稿：refresh token 輪換、密碼底線、帳戶鎖定、固定回應耗時 300ms／500ms、私隱刪除（crypto-shredding 加匿名化）同所有營運數值全部定案。
>
> v1.6：SDK maintainer 搵到嘅 9 個缺口已經補上同確認（見 0.5），並修正一批前後矛盾（見 0.6）。
>
> v1.7 定稿：新增完整 public 註冊同 email 驗證範例（12.11），並補齊範例需要嘅規則（見 0.7，決定 52–56 係已確認）。

## 變更紀錄

### v1.8（草稿，2026-10-14）

- 補編決定 150–174（見 0.14）：expression、LLM 錯誤同開發模式、auth、manifest、slug、validate repair、互動命名、Limits pins／structured 正規化同 prompt 版本規則。
- 新增 `vibe dev`：本地 web API server，唔使預先編譯；request 嚟到先將 URL 對應去 `.md`，第一次用先編譯成 lock，按 source hash 同 compiler-hash 輸入做 cache，檔案改動自動重編；驗證器每次都跑；預設用 in-memory 資料庫（模擬 RLS 同 grant），內嵌 PostgreSQL 做 opt-in（決定 73–75，見 2C.2、2C.4）。（決定 166：`vibe dev` 延至 vNext；v1 用互動 `vibe` → `vibe compile` → `vibe manifest --dev` → `vibe serve --memory`）
- 新增 `vibe build`：一個指令做晒編譯、驗證同 manifest（類似 `nest build`），冇 reviewer 步驟（審核由 Git 平台設定負責，決定 76，見 2C.3）。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext）
- SDK 移除人手確認：刪除 `vibe approve`；lock frontmatter 唔再有 `status`、`approval-*`、`approved-*` 欄位同 lock 簽名；SDK 忽略 reviewer 同確認簽名；`security-items` 改為資訊性清單；manifest 照樣簽名，release 用 Sigstore keyless（決定 77–80、119）。重寫決定 10、11、16、19、20、28、33、34 同 2A.5、2B.3、9B、10.1 嘅相關內容；決定 29（CODEOWNERS）同 37（allowlist 受 CODEOWNERS 保護）照舊必須。
- 新增產品經理六條基線做決定 89–94（release 簽名只限 `v*` tag 加 required-reviewer environment（臨時方案 (i) 下有寫權限嘅人都可以開 `v*` tag，tag 唔係閘門；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118）、高風險 fail closed（決定 90 已於 2026-10-16 撤回：高風險項目只列入部署報告）、production 拒絕 in-memory、冇 release 簽名就失敗（決定 119 改用 Sigstore keyless）、決定 29／37 恢復、`/latest/` 永遠唔係草稿），同決定 95（create-user 邀請失敗處理）。
- Lock 係 build 輸出，唔再 commit（`.gitignore`），由 CI／CD 喺部署時由 source build（決定 117，取代決定 80）。
- `sign-jwt` 簽名算法政策（決定 139，issue #198）：只支援 ES256、RS256，預設 ES256；RS256 key 最少 2048 bit，唔夠 fail closed；驗證端只接受設定嘅一個 alg；EdDSA 留到 vNext（見 4.1a）。
- Email 正規化（決定 120）：唔合併 plus addressing、保留點；local part NFC→trim→lowercase；domain IDNA UTS #46 punycode→lowercase；只儲存正規化值；遷移遇到衝突 fail closed，人手處理。
- 租戶 DB role 細節（決定 129）：`tenant_<id>_rw`／`_public_reader`／`_public_writer`、`tenant_<id>_owner` migration role、連線池上限滿即回 503 `DB_POOL_EXHAUSTED`、production 憑證經 secret manager、常數 policy 部署時比對。
- IP 限流網段 slot 上限（決定 130）：IPv4 /24、IPv6 /48 加 /32 嘅新計數器 slot 上限，跨租戶平台層（`VIBE_RATE_PLATFORM_FACTOR`），60 秒固定窗口 LRU 驅逐，slot 表滿 fail closed（429）。
- 撤回 `--release-approval`（產品經理 2026-10-16）：唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（2C.3a、9C）。
- CLI `--project` 預設係目前目錄；README 簡化做兩行（決定 81、82，見 2C.1）。
- 新增 9C「冇人手 review 嘅安全模型」：寫明未經人手 review 嘅 LLM 輸出而家可以直接部署、剩餘風險同仍然有效嘅防線；另附 PR review 指引草稿（[`pr-review-guide.md`](pr-review-guide.md)，決定 83）。
- 補 v1.7 reviewer 提出嘅缺口：11.4 create-user lock 補返 `user.invite` step；新增 `user.register`、`user.invite` 編譯 lock 範例；11.3／11.4 rate-limit 補 `key`；admin `EMAIL_TAKEN` 處理；create-user 改為可以重試（idempotent）並新增 re-invite endpoint（決定 84–88）。

- Challenge provider（issue #160，已確認（產品經理 2026-10-16））：決定 131–138，v1 用 Cloudflare Turnstile（server 端 siteverify、production 共用 Postgres replay store（#177）、`type: pow` build error、fail closed、錯誤碼、secret 處理、同限流組合、固定數值、已認證 endpoint opt-in、provider 抽象），見 0.12、7.2a。
- Challenge 綁定 request digest（決定 140，issue #156）：已確認（產品經理 2026-10-16），token 經 Turnstile `cdata` 綁定 request digest（完整長度，涵蓋 method、path、query、body、`Content-Type`、`If-Match`；包括 login route），session 綁定留到 vNext；`bind: none` v1 出 warning，下一個 minor 起新 public INSERT route 禁用（#245）；未實作（見 0.12、7.2b）；演算法修正（per-request nonce、query raw `+` 無效）已確認（產品經理 2026-10-16）。檢查次序同 nonce／token 單一交易（#258）已確認（產品經理 2026-10-16 12:44 HKT）。
- Refresh cookie 補充（issue #78，已確認（產品經理 2026-10-16））：決定 141–148（cookie 模式 login、對外錯誤一致、`Max-Age`、CSRF 次序、cookie logout、access token 預設 TTL、錯誤碼、向後相容），喺決定 6、45、60、61、65、72、101 之上補充，見 0.11、4A.1b；實作狀態見 4A.1b。
- Refresh 簽發同輪換一致性（決定 149，issue #270，產品經理已批准 2026-10-17）：`sign-method` 一律唯讀（validate、build、serve 靜態阻擋，serve 再用 `SET TRANSACTION READ ONLY` 兜底），解決 #270；見 0.13、4A.1c。

### v1.7（定稿）

- 見 0.7–0.9（決定 52–72）。

## 0. 範圍同決定

### 0.1 範圍

- v1 只支援 HTTP API 入口，method 限 GET／POST／PUT／PATCH／DELETE（PATCH 由決定 105 加入，已確認（產品經理 2026-10-14））；排程、queue、webhook 入口唔喺 v1 範圍內。
- `{definition}` 代表 domain；`{version}` 代表 API 版本。
- 主檔用 `{feature}/{feature}.md`，唔用 `index.md`。
- 只用路徑揀版本，唔支援 `Accept-Version` header；每個 version 可以有自己獨立嘅 feature。
- 狀態碼跟 RFC 9110（429 跟 RFC 6585）；已認證但冇權限回 403。
- v1 預設 provider type 係 PostgreSQL（`type: postgres`）。
- schema 升級去 database 只做離線遷移（期間凍結寫入）；舊 schema 保留 30 日。
- public 請求嘅最低回應耗時係固定值：一般 public endpoint 300ms，login 類 endpoint 500ms；唔再用 p95。
- 冷啟動 p95 目標係少過 1 秒。

### 0.2 已確認決定（v1.3 定案）

| # | 決定 | 影響章節 |
| --- | --- | --- |
| 1 | 物件欄位唔分 version，靠 migration 管理。 | 12.2、12.5 |
| 2 | method 只有單一版本：取消 `@v2` 同「同名 version 綁定」；取消 `models/{object}/{version}/` 資料夾，改為 `models/{object}/methods/{method}.md`。唔同 api version 之間嘅差異，由 api 層嘅 input／output 轉換（`## Transform`）處理。 | 1、2、12 |
| 3 | 寫入其他物件一定要經嗰個物件自己嘅 method。 | 12.3、12.3a |
| 4 | 取消「public endpoint 只可以 call `writes: false` method」嘅限制（風險控制見 7.2；剩餘風險由後續版本內容審核 hook 處理）。 | 7.2、12.6 |
| 5 | `sign-token` 放喺 model 層做 method（例如 `user.sign-token`）；`auth` 類 pipeline 透過呼叫 model method 做嘢。 | 4、12.6、12.8 |
| 9 | api 層都可以用自然語言寫。 | 2 |
| 10 | （v1.8 由決定 77 修訂，已確認（產品經理 2026-10-14））SDK 唔再分自動／人手確認；CI 由 source build 出嚟嘅所有 lock（structured 或者 natural）通過驗證器同決定 114 policy，而且 artifact 有決定 113 attestation，就可以部署（決定 117：lock 唔 commit，冇重編比對）。「係咪 structured」照樣由 deterministic parser 判斷，只影響使唔使 call LLM。 | 2A.5 |
| 11 | （v1.8 由決定 77 修訂，已確認（產品經理 2026-10-14））SDK 冇確認角色；邊個可以 merge 由 Git 平台 rulesets（分支規則） 決定（見 9C 同 [`pr-review-guide.md`](pr-review-guide.md)）。 | 2A.5 |
| 12 | 單一職責上限係 warning。 | 12.3a、10 |
| 13 | 軟刪除永遠唔會實際刪除資料，冇 purge。v1.8 澄清（已確認（產品經理 2026-10-14））：決定 13 只管用戶 API 同用戶資料；平台系統表（例如決定 104 嘅 `_platform_idempotency`）由 runtime 按 TTL 清理，唔受決定 13 限制。 | 12.9 |
| 14 | CRUD 嘅 list 用 offset 分頁（保留 page-size 上限）。 | 12.9 |
| 15 | 編譯器 major version 升級會強制重新編譯所有 lock；minor 升級唔會。 | 2A.6 |

### 0.3 已確認決定（v1.4 定案）

| # | 決定 | 影響章節 |
| --- | --- | --- |
| 16 | （v1.8 由決定 112 修訂：高風險欄位一定要喺 frontmatter 宣告）安全設定照用自然語言寫，**唔強制**結構化 frontmatter；防線係 frontmatter 高風險宣告（決定 112）、獨立驗證器、決定 114 deterministic policy、決定 113 attestation（build once, promote，決定 118；LLM 輸出唔決定性，所以冇重編比對）同 runtime 限制（v1.8 決定 77：SDK 唔再做人手 review，已確認（產品經理 2026-10-14））。 | 2.2、2A.5、2A.7 |
| 17 | 編譯器輸出一定要通過獨立、deterministic 嘅驗證器（唔靠 LLM 自律）；編譯器冇任何工具權限，網絡只准連編譯用 LLM endpoint 嘅 host（v1.7 決定 57 修訂）；source 入面似指令嘅句子（prompt injection 模式）出 warning 並喺 PR 高亮。 | 2A.7、10.9 |
| 18 | 編譯用嘅 model pin 死 snapshot；compiler-hash（決定 174：只計 prompt + 編譯器版本）寫入 lock；（v1.8 決定 113、117：唔再做重編比對，部署靠 attestation 同決定 114 policy）；會改變輸出嘅 minor 升級當 major 處理。 | 2A.1、2A.6 |
| 19 | （v1.8 由決定 78 修訂，已確認（產品經理 2026-10-14））安全項目由驗證器 deterministic 抽出，寫入 lock 同部署報告做資訊性高亮；唔再要求逐項確認人、兩個非作者或者 PR 數量上限；PR 同部署報告照樣顯示有效權限摘要。 | 2A.5、10.10 |
| 20 | 「係咪 structured」由 deterministic parser 判斷（v1.8 決定 77 修訂，已確認（產品經理 2026-10-14）：唔再影響確認，只決定編譯使唔使 call LLM）。 | 2A.5 |
| 21 | Public 寫入收緊：每租戶每日寫入配額（強制）；建立資料嘅 endpoint 要 captcha 或者 proof-of-work；一律 audit-log；配額用完只封鎖 public 寫入。 | 7.2 |
| 22 | Public 寫入只准 INSERT 同 UPDATE 明確列出嘅欄位；平台黑名單封鎖敏感欄位（role、permissions、tenant、owner 等）；UPDATE 一定要有 WHERE 同 RLS。 | 7.2、10.7 |
| 23 | Public 寫入嘅 `tenant_id` 由平台按 slug 注入；body 含 `tenant_id`、`id`、`created_by` 一律拒絕。 | 7.2 |
| 24 | Public 寫入嘅內容標記為 untrusted，輸出時做 encoding，有長度同數量上限。 | 7.2 |
| 25 | Public 路徑預設禁止對外 call；昂貴 step 用更嚴嘅限流。 | 7.2 |
| 26 | `public_write_access` 只可以由租戶 admin 喺平台 UI 開啟。 | 7.2 |
| 27 | 所有 lock 禁止 DDL 同 TRUNCATE（migration 另外處理）；UPDATE 一定要用主鍵或者 RLS 限定範圍；DB role 冇 DELETE／TRUNCATE 權限。 | 2A.4、7.1、10.8 |
| 28 | （v1.8 由決定 77 取代，已確認（產品經理 2026-10-14））SDK 冇 commit hash 綁定嘅確認；建議喺 Git 平台開「有新 commit 就撤銷 approval」（見 [`pr-review-guide.md`](pr-review-guide.md)）。 | 2A.5 |
| 29 | CODEOWNERS 必須（v1.8 照舊，唔係建議）：`api/`、`models/`、`migrations/`、`providers/`、`pipelines/`、`.github/`、`.compiler/`，同埋編譯器、驗證器版本設定，一定要有 CODEOWNERS 覆蓋，改動要 code owner review；作為 source PR 合併條件（決定 93、115），SDK 唔檢查（2C.3a）。Release manifest 用 Sigstore keyless（決定 119），只喺要 Jack 批准嘅 `release` environment 用 release workflow 嘅 GitHub OIDC 身份簽（決定 79、89、118）。 | 9B |
| 30 | Token 簽發喺 lock 明確標示 capability 旗標（唔靠 method 名推斷）；claim 白名單；roles 同 tenant_id 由平台從 DB 讀；exp 有上限；key 只用引用。 | 12.6 |
| 31 | 新增緊急 kill switch，可以停用個別 endpoint（回 503、寫審計、唔使重新部署）。 | 9A |
| 32 | Public 最低回應耗時由 p95 改為固定值（數值見決定 36）。 | 0.1、7.2 |

### 0.4 已確認決定（v1.5 定稿）

| # | 決定 | 影響章節 |
| --- | --- | --- |
| 6 | Refresh token 每次使用都輪換；重用舊 token 會撤銷成個 token 家族；登出可以 revoke。refresh token ttl 上限 30 日（家族絕對壽命），閒置 7 日失效；access token 上限 1 小時。 | 4A |
| 7 | 平台密碼底線：最少 12 個字元，加洩漏密碼檢查；物件只可以收緊，唔可以放寬。 | 4A |
| 8 | 15 分鐘內失敗 5 次就鎖帳戶 15 分鐘，另加 IP 限流。防惡意鎖帳戶：同一帳戶 15 分鐘內失敗 3 次之後要 captcha；鎖定期間經 email 連結重設密碼可以即時解鎖；鎖定期間回應同密碼錯完全一樣。 | 4A |
| 33 | （v1.8 由決定 77 取代，已確認（產品經理 2026-10-14））SDK 冇雙人確認，細團隊規則唔再適用；需要雙人 review 由 rulesets（分支規則） 設定。 | 2A.5 |
| 34 | （v1.8 由決定 78 取代，已確認（產品經理 2026-10-14））取消每個 PR 安全項目上限。 | 2A.5 |
| 35 | Model snapshot：平台承諾 snapshot 喺宣布淘汰之後最少保留 12 個月，項目有 90 日遷移窗口；窗口內舊 lock 照常有效。Golden corpus 由平台編譯器團隊維護（最少 500 個案例），項目可以加自己嘅案例。 | 2A.6 |
| 36 | Public 最低回應耗時：一般 300ms，login 類 500ms；實際超過只出 warning，唔係 error。 | 7.2、10.9 |
| 37 | 語意一致性檢查（lock 多過 source）係 error；prompt injection 規則命中係 warning，項目可以喺受 CODEOWNERS 保護嘅 allowlist 加精確句子豁免（v1.8 照舊：`.compiler/injection-allowlist` 一定要有 CODEOWNERS 覆蓋，release build 檢查，決定 93）。零寬／bidi 字元仍然係 error。 | 2A.7 |
| 38 | Login challenge：同一帳戶或者同一 IP 15 分鐘內失敗 3 次後要 captcha；冇 INSERT 嘅其他 public 寫入唔強制 challenge。 | 4A、7.2 |
| 39 | 每租戶每日 public 寫入配額預設 1,000 次，每 IP 每日 50 次；租戶 admin 可以調低，調高最多 10,000 要經平台申請。昂貴 step 限流係 endpoint 上限嘅一半。 | 7.2 |
| 40 | Kill switch 生效 SLA：p99 30 秒內全部 instance 生效；租戶 admin 只可以停自己租戶嘅 public endpoint。 | 9A |
| 41 | `public_write_access` 批量操作：平台 UI 支援組織 admin 一次揀多個租戶開／關，要二次確認同寫原因；仍然冇 API／CLI。 | 7.2 |
| 42 | 私隱刪除：敏感個人欄位用 crypto-shredding，其他個人欄位匿名化；DELETE 照樣禁止。流程見 12.10。 | 12.10 |

### 0.5 已確認決定（v1.6 SDK 補漏，已確認）

以下 9 項 Jack 已經確認，已經寫入相關章節，SDK 按呢啲值實作。

| # | 決定 | 影響章節 |
| --- | --- | --- |
| 43 | API key：header `X-API-Key`；格式 `mbk_{env}_{key_id}_{secret}`（env 係 `live`／`test`，key_id 12 字元 base32，secret 32 byte base62）；scope 語法 `{definition}:{read\|write\|admin}` 或者 `{definition}:*`；用 HMAC-SHA-256 加平台 pepper 存 hash；有效期預設 90 日、上限 365 日；每個 client 最多 2 條有效 key 做輪換，重疊期預設 7 日、上限 30 日；revoke 30 秒內生效。 | 4.2 |
| 44 | JWT：`iss` 預設 `https://auth.{platform-domain}/{project}`；`aud` 預設 `{project}`；`jwks-uri` 預設 `{iss}/.well-known/jwks.json`，一定要 https；JWKS cache 10 分鐘，遇到未知 `kid` 即刻重拉（最少相隔 60 秒），拉唔到最多用舊 cache 1 小時；clock skew 60 秒；alg 只准 ES256、RS256（RSA 最少 2048 bit），一定要有 `kid`。改 iss／aud／jwks-uri 屬高風險安全項目。 | 4.1 |
| 45 | `token-refresh` pipeline 參數：`sign-method`（必填）、`refresh-ttl: 30d`、`idle-ttl: 7d`、`reuse-grace: 10s`、`transport: body`（可選 `cookie`）。Refresh token 存喺平台管理嘅 `_platform_refresh_tokens` 表（每個租戶 schema 一張，有 RLS），只存 HMAC-SHA-256 hash，有 `family_id`；revoke 用 UPDATE 設 `revoked_at`，唔用 DELETE。 | 4A.1 |
| 46 | `hash-password`：argon2id，memory 64 MiB、iterations 3、parallelism 1、salt 16 byte（CSPRNG）、輸出 32 byte，用 PHC 字串格式儲存；登入成功時如果參數同而家唔同，平台自動 rehash。 | 4A.2 |
| 47 | Public 註冊：password 欄位只可以經 `hash-password` op 嘅輸出寫入，method 要標 `capabilities: [set-password]`，api lock `## Access` 要寫 `password-write: allow`（高風險安全項目）；黑名單照樣禁止 request 直接寫 password 欄位。 | 7.2、12.6 |
| 48 | 樂觀鎖用 HTTP `ETag`／`If-Match`：update 一定要帶 `If-Match`，缺少回 428，唔對應回 412；method input 欄位叫 `if_match`，只可以由 `req.header.if-match` 映射。 | 8、12.9 |
| 49 | challenge 語法統一：`challenge: { type: none \| captcha \| pow, when: always \| after-failures, after-failures: <n> }`；有 INSERT 嘅 public endpoint 一定要 `when: always`。Challenge token 單次使用狀態存喺每個租戶自己 schema（決定 97），runtime role 只有 SELECT／INSERT（決定 27）。 | 7.2、10.7 |
| 50 | 私隱刪除流程已經喺 12.10 定義（決定 42），後續版本改為「資料主體自助提交入口」，唔再列「正式流程」。 | 14 |
| 51 | 新增 `kind: migration`：source `migrations/{provider}/{seq}-{name}.md`，lock `.lock.md`；DDL 只准喺呢度；用 migration role：v1.8 起改為長期存在、只喺 migrate 期間可以登入嘅 `tenant_<id>_owner`（決定 129，已確認（產品經理 2026-10-16））。原因：決定 129 要求 runtime role 唔係 owner 同表要 `FORCE ROW LEVEL SECURITY`，表一定要有一個固定 owner；臨時 role 每次建立再刪除會令 ownership 轉移同授權變得複雜。剩餘風險：登入窗口期間外洩嘅 owner 憑證對表有完整權力，靠一次性密碼、parser 白名單、審計同定期檢查緩解；一律標高風險安全項目（v1.8 決定 78：資訊性，唔再要人手確認，已確認（產品經理 2026-10-14））；平台自動加 `tenant_id`、RLS、FORCE；DROP 租戶資料要行「棄用 30 日」流程。 | 2B |

### 0.6 v1.6 修正嘅前後矛盾

| # | 問題 | 修正 |
| --- | --- | --- |
| F1 | lock `kind` 列舉冇 `migration`，但 user model lock 依賴 `kind: migration`。 | 2A.1、2A.3 加入 migration，新增 2B。 |
| F2 | 目錄（第 1 節）冇 `migrations/`。 | 加入 `migrations/{provider}/`。 |
| F3 | 內置 pipeline 清單冇 `token-refresh`，pipeline op 白名單亦冇處理 refresh token 嘅 op。 | 加入 `token-refresh`、`token-revoke` 同平台專用 op。 |
| F4 | CRUD update 用 `updated_at` 做樂觀鎖輸入，但 `updated_at` 係唔准由 input 寫嘅系統欄位；衝突回 409 亦唔合 RFC 9110 條件請求語意。 | 改用 `If-Match`，回 412／428（決定 48）。 |
| F5 | 12.10 用 `erased_at`，但系統欄位清單冇佢。 | 系統欄位加入 `erased_at`。 |
| F6 | 7.1 話只有 migration 臨時 role 有特別權限，但 12.10 用 erasure role、rehash 要寫 password。 | 7.1 列出全部平台專用 role 同權限範圍。 |
| F7 | 7.2 黑名單寫「password（login 類由平台處理除外）」，同註冊需要寫 password 矛盾，而且 login 根本唔寫 password。 | 改為決定 47 嘅規則。 |
| F8 | `challenge: captcha-after-3-failures` 唔喺 `captcha \| pow` 列舉。 | 統一語法（決定 49）。 |
| F9 | 第 14 節列「私隱刪除正式流程」但決定 42 已經定義。 | 決定 50。 |
| F10 | 4A.1 講 refresh token，但 login 例子冇回 refresh token。 | `sign-jwt` 加 `issue-refresh`，login 例子回 `refresh_token`。 |
| F11 | 12.2 寫「argon2id 參數由平台管理」冇數值。 | 指向 4A.2（決定 46）。 |
| F12 | 2A.5 安全項目表冇涵蓋新加嘅高風險設定（set-password、JWT issuer、API key scope、migration）。 | 加入安全項目表。 |
| F13 | 狀態碼表冇 202、412、428，但註冊同樂觀鎖要用。 | 加入第 8 節。 |

### 0.7 v1.7 為註冊範例補齊嘅規則（已確認）

以下 5 項 Jack 已簽核確認。

| # | 規則（已確認） | 影響章節 |
| --- | --- | --- |
| 52 | 新增 method op `random-token`（產生 32 byte 隨機 token，回 `{ value, hash }`，hash 係 `HMAC-SHA-256(pepper, value)`）同 `hash-token`（將 request 入面嘅 token 計 hash）。`value` 只可以傳去標咗 `sensitive` 嘅 method input，唔可以出現喺 `respond`、log 或者 sql 明文欄位。 | 2A.4、10.8 |
| 53 | Email 驗證寫入：`email_verified_at` 屬黑名單「verified」類欄位；只有標 `capabilities: [confirm-verification]` 嘅 model method 可以喺 public 路徑寫佢，而且一定要喺一個 UPDATE 入面，WHERE 用 `hash-token` 輸出對比唯一 token hash 欄位、檢查未過期同未驗證，並同時清走 token hash。屬高風險安全項目，api lock 要寫 `verification-write: allow`。 | 7.2、10.7 |
| 54 | Email 發送唔用對外 `call`：method 經 `email-outbox.enqueue` 寫入 `email_outbox` 表，由平台 mail worker 非同步發送；payload 嘅 token 屬 `personal: sensitive`（crypto-shredding），發送後平台即時銷毀 DEK。 | 12.11 |
| 55 | Login 只接受已驗證 email 嘅用戶（`email_verified_at is not null`）；未驗證同密碼錯回同一個 401。 | 12.8 |
| 56 | 註冊唔分辨 email 係咪已經存在：一律做 `hash-password`、一律 enqueue 一封信（新用戶收驗證信，已存在用戶收「已經有帳戶」通知，由 mail worker 按 `user_id` 係咪空揀範本），一律回 202 同一個 body，耗時補到 300ms。註（決定 67 已確認，2026-10-14 起生效）：註冊唔再收密碼，`hash-password` 改為喺 `user.complete-verification`（設定密碼一步）每次都做；「一律做 `hash-password`」以呢個為準。 | 12.11 |

### 0.8 已確認決定（v1.7 編譯器 LLM 設定）

| # | 決定 | 影響章節 |
| --- | --- | --- |
| 57 | 初始化 project／SDK 時，用戶揀編譯用 LLM：`LLM_PROVIDER` 係 `openai`（預設，涵蓋所有 OpenAI-compatible endpoint 包括 Ollama、LM Studio、vLLM）、`anthropic` 或者 `gemini`；只喺部署／編譯時用。中立變數 `LLM_BASE_URL`、`LLM_API_KEY`（必填）、`LLM_MODEL`（必填，pin 死 snapshot）；`OPENAI_*` 只做 `openai` 嘅別名，`LLM_*` 優先，值衝突 error；選填 `LLM_TIMEOUT`、`LLM_MAX_RETRIES`、`LLM_SEED`。本地開發准 http loopback，其他內網要 `LLM_ALLOW_PRIVATE_HOST=true`；CI／production 一定 https 公開 host，本地 lock 唔可以部署。Key 永遠唔寫入 repo、lock、manifest、log、部署報告或者錯誤訊息；runtime 唔可以有任何 `LLM_*`／`OPENAI_*`。沙盒網絡只准連 endpoint host；決定 174：provider、host、model 寫入 build record 同 manifest provenance 嘅 compilers stamp，唔計入 compiler-hash。 | 2A.1、2A.6、2A.7、2A.8、9B、10.1、14 |

### 0.9 v1.7 review 修訂（決定 58 起）

呢節係 PR #5 code review 之後加嘅決定。純技術修正（58–66）由架構直接定為「已確認」；涉及產品、私隱或者安全政策取捨嘅 67–72 由 Jack 授權產品經理確認（2026-10-14），全部已確認，數值以下表為準。

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 58 | Adapter 按 model capability 送決定性參數：model 唔支援嘅參數（例如 `temperature`、`top_p`、`seed`）就唔送，唔可以強送；決定性只靠 CI 重編比對保證。Model pin 用 provider 回報嘅 model id／snapshot id，唔再要求 id 帶日期後綴。 | 已確認 | 2A.6、2A.8.2、2A.8.3、2A.8.6 |
| 59 | 三個 provider 嘅 request path 同 header 固定寫入 adapter：OpenAI `POST {base}/chat/completions`＋`Authorization: Bearer`；Anthropic `POST {base}/v1/messages`＋`x-api-key`＋`anthropic-version: 2023-06-01`；Gemini `POST {base}/models/{model}:generateContent`＋`x-goog-api-key`。 | 已確認 | 2A.8.2 |
| 60 | Refresh cookie 嘅 `Path` 一定包含 `{slug}`：`/{definition}/{version}/{slug}/refresh`（同 10.7 一致）。 | 已確認 | 4A.1 |
| 61 | Refresh 寬限期精確定義：DB 只存 hash；寬限期內用返舊 token，平台用 `used_at` 判斷，撤銷上一次發出嘅子 token，再喺同一家族發一組新 token；唔儲存、唔重發明文。並行請求喺第一個請求處理中出示同一 token，會收到可重試嘅 503 TX_CONFLICT（retryable: true，唔清 cookie）。第一個請求完成之後，喺寬限期（10 秒）內再出示，按寬限期規則處理；超過寬限期先當重用，撤銷整個 family。每個舊 token 最多重發一次（`used_at` 加 `grace_reissued_at`）；寬限期內第二次再用即當重用，撤銷成個家族。 | 已確認（產品經理 2026-10-17） | 4A.1 |
| 62 | `sign-token` 嘅 `tenant_id` 只有一個來源：平台由 slug（public）或者已驗證 token（auth）解析出嚟嘅 `ctx.tenant_id`；永遠唔由 client 或者 method 傳入，亦唔由 `users` 表讀。 | 已確認 | 12.6、12.8 |
| 63 | `mail_worker` 加入 7.1 平台專用 role 表。 | 已確認 | 7.1 |
| 64 | `compiler-hash` 只有一個標準定義（2A.1），決定 174：只包括 compiler.version 同 prompt-version，seed 同實際送出嘅參數移去 build record 同 manifest provenance。 | 已確認 | 2A.1、2A.6、10.1 |
| 65 | 補齊 grammar：`rate-limit` 參數 schema、`## Order` 准用值、`subject-key` 寫法、`token-revoke` 參數。 | 已確認 | 4.3、4.4、4A.1、12.10 |
| 66 | 非阻塞修正：CI 範例 pin `vibe` 版本同用 SHA pin action；Ollama 記 tag（唔係 digest）；token 類型統一；`email-outbox.enqueue` 嘅 `template` 寫入 Signature；註冊耗時用固定下限＋固定工作量（唔受 argon2 排隊影響）。 | 已確認 | 2A.8.3、2A.8.8、12.11 |
| 67 | 防預先註冊搶帳戶：公開註冊只收 email（同 name），唔收密碼；驗證連結帶用戶去「設定密碼」一步，一個 UPDATE 同時設 password 同 `email_verified_at`（同決定 69 邀請共用同一個 method `user.complete-verification`）。未驗證 email 再註冊只會輪換驗證 token（舊 token 即時失效），永遠唔改任何 credential。驗證成功即撤銷該用戶所有 session（refresh 家族）。未驗證帳戶喺 `email_verify_expires_at` 過咗之後由平台匿名化並 soft delete（唔 DELETE，決定 13）。決定 56 入面「註冊一律做 `hash-password`」改為喺設定密碼一步做（56 狀態唔變）。 | 已確認（產品經理 2026-10-14） | 12.11 |
| 68 | Audit 永遠唔記明文 email（記 HMAC hash 或者遮罩）；`email_outbox` 加 `recipient_hash`，令冇 `user_id` 嘅列都可以私隱刪除；outbox 列發送後 30 日由平台匿名化。 | 已確認（產品經理 2026-10-14） | 9、12.10、12.11 |
| 69 | Admin 建立用戶改為邀請流程：admin 建立時唔設 password、唔設 `email_verified_at`；用戶用邀請 token 經 `user.complete-verification`（同決定 67 共用）設 password，同時設 `email_verified_at`，令決定 55 成立。 | 已確認（產品經理 2026-10-14） | 11.4、12.11.9 |
| 70 | 列層擁有權預設規則：有 `owner-key` 嘅 model，非 admin 只可以讀寫 `owner-key = ctx.user.id` 嘅列；要放寬一定喺 source 明寫，屬高風險安全項目。 | 已確認（產品經理 2026-10-14） | 12.6a |
| 71 | `max-page-size` 平台上限 500（model 可以設低，唔可以設高）。 | 已確認（產品經理 2026-10-14） | 12.9 |
| 72 | Refresh endpoint 分類：public（冇 access token）但唔屬 public 寫入（只寫平台表，唔計 public 寫入配額）；限流 IP 30/min、家族 10/min；最低回應耗時 300ms。 | 已確認（產品經理 2026-10-14） | 4A.1 |

### 0.10 v1.8 新決定（決定 73 起，已確認（產品經理 2026-10-14））

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 73 | `vibe dev`：本地 web API server，唔使預先編譯。Request 嚟到先按路由搵 `.md`，第一次用先編譯成 lock，cache key 係 source hash 加全部依賴 hash 加 compiler-hash 輸入；file watch 偵測到改動就重編受影響嘅 lock；驗證器每次編譯同每次載入 cache 都跑，唔通過就回 500 並喺 terminal 報錯。 | 已確認（產品經理 2026-10-14） | 2C.2 |
| 74 | 正式支援 in-memory 資料庫（Jack 新要求，取代原本「預設內嵌 PostgreSQL」）：`vibe dev` 預設用 in-memory，`vibe serve --memory` 亦用同一個 in-memory 執行器。In-memory 只限開發同測試，server 一停所有資料即刻消失。喺非 dev build-mode 或者 production 環境一律拒絕 in-memory，啟動報錯退出，冇 override（決定 91）。因為 in-memory 係預設 dev 路徑，執行器一定要模擬租戶隔離（RLS＋FORCE 語意）、role grant 同 column grant，令 dev 行為同 production 一致；已知語意差異喺 2C.4 列明；CI 整合測試照樣要對真 PostgreSQL 跑。內嵌 PostgreSQL 保留做 opt-in `--db postgres`，用嚟做保真度測試。（決定 166：`vibe dev` 延至 vNext；v1 用互動 `vibe` → `vibe compile` → `vibe manifest --dev` → `vibe serve --memory`） | 已確認（產品經理 2026-10-14） | 2C.2、2C.4 |
| 75 | Dev 編譯嘅 lock 同 manifest 一律唔可以部署：lock 記 `build-mode: dev`，manifest 用本機 dev key 簽；production runtime 只接受 Sigstore keyless 簽、身份符合決定 119 嘅 release manifest。2A.8 嘅本地 LLM http 規則只喺 dev 適用。 | 已確認（產品經理 2026-10-14） | 2C.2、2A.8.4 |
| 76 | `vibe build`：一個指令做晒編譯（只編譯過時嘅 lock）、驗證、manifest 產生同簽名，冇 reviewer 步驟（審核由 Git 平台設定負責）；（決定 117）lock 係 build 輸出，冇 `--check-determinism`。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext） | 已確認（產品經理 2026-10-14） | 2C.3 |
| 77 | SDK 移除人手確認：刪除 `vibe approve`；lock frontmatter 刪除 `status`、`approval-mode`、`approved-by`、`approved-at`、`approval-ref`、`approved-commit`；SDK 讀到呢啲舊欄位（包括舊 lock 嘅 `approval-ref`，同已撤回嘅決定 90 build 旗標 `--release-approval` 係兩回事）會忽略並出 warning，唔再以佢哋做部署條件。取代決定 10、11、16（人手 review 部分）、20（人手確認部分）、28、33。 | 已確認（產品經理 2026-10-14） | 2A.1、2A.5、10.1 |
| 78 | `security-items` 保留，但改為資訊性：驗證器 deterministic 抽出、寫入 lock 同部署報告，方便 PR 高亮同 label；唔再要求逐項確認人、原因、兩個非作者，亦冇每個 PR 5 項上限。取代決定 19（確認部分）、34。 | 已確認（產品經理 2026-10-14） | 2A.5、10.10 |
| 79 | Lock 唔再有 `signature`（lock 完整性靠 `source-hash`、依賴 hash、compiler-hash；v1.8 部署靠決定 113 attestation，決定 117 之後 lock 唔 commit）；manifest 照樣簽名，由 `vibe build` 簽：release 用 Sigstore keyless（決定 119），以 `release` environment（要 Jack 批准；真正人手批准閘門係 environment，唔係 tag；自動閘門仍然係決定 113／114／118）入面 release workflow 嘅 GitHub OIDC 身份簽，本機用 dev key。Production runtime 只信任 pin 死嘅 Sigstore 身份（issuer `https://token.actions.githubusercontent.com`，certificate identity 完全等於 `https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v<部署緊嘅版本>`，trust root 見決定 119），dev key 永遠唔信任。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext） | 已確認（產品經理 2026-10-14） | 2A.1、9、9B、10.1 |
| 80 | （已由決定 117 取代：lock 唔再 commit）建議 `*.lock.md` 同 source 一齊 commit，令 PR diff 睇到編譯結果；CI 重編決定性檢查保留，唔一致照樣封鎖部署，冇 override。 | 已取代（決定 117） | 2A.6、2C.3 |
| 81 | CLI 所有指令嘅 `--project` 預設係目前目錄（cwd）。 | 已確認（產品經理 2026-10-14） | 2C.1 |
| 82 | README 快速開始簡化做兩行：`vibe dev` 加一個 `curl`。（決定 166：`vibe dev` 延至 vNext；v1 用互動 `vibe` → `vibe compile` → `vibe manifest --dev` → `vibe serve --memory`） | 已確認（產品經理 2026-10-14） | 2C.1 |
| 83 | 接受「未經人手 review 嘅 LLM 輸出可以直接部署」嘅剩餘風險（見 9C）；人手 review 由用戶喺 Git 平台用 rulesets（分支規則） 自行設定（指引見 [`pr-review-guide.md`](pr-review-guide.md)）；release 層面唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（決定 89、118、2C.3a）；CODEOWNERS 仍然係 source PR 合併條件（決定 93、115），唔係 SDK release 檢查。 | 已確認（產品經理 2026-10-14） | 9C |
| 84 | Admin `user.register` 保留 409 `EMAIL_TAKEN`：只限 admin、已認證、有限流同審計，admin 本身已經可以列出租戶用戶，所以唔當 enumeration 洩漏。 | 已確認（產品經理 2026-10-14） | 11.4、12.11 |
| 85 | `create-user` 可以安全重試：`user.register` 用「insert CTE + SELECT」一個 statement，以明確 `created` 欄位分辨新建立同重試（唔靠 `xmax`）；兩種都回 202 `{ user_id, created, invite_status }`（決定 95）；對「已存在、未驗證、冇密碼」嘅用戶唔報錯，回原有 id（`created: false`），再由 `user.invite` 重新寄邀請；只有已驗證或者有密碼先回 409 `EMAIL_TAKEN`。解決 7.1 每個 query 獨立 commit 時「建立咗用戶但邀請失敗」嘅狀況。 | 已確認（產品經理 2026-10-14） | 11.4、11.4a |
| 86 | 新增 re-invite endpoint `POST /users/v2/{id}/invite`（admin），用 `user.invite` 重新寄邀請，邀請入 outbox 之後回 202 `{ invite_status: "invite_pending" }`（決定 95）；用戶已驗證回 409 `ALREADY_ACTIVE`（決定 69 延伸）。 | 已確認（產品經理 2026-10-14） | 11.4b |
| 87 | 11.3 `_version` 同 11.4 create-user 嘅 rate-limit 補 `key`（分別係 `user` 同 `user`），合 4.3 schema；冇 `key` 嘅 rate-limit 係驗證器 error。 | 已確認（產品經理 2026-10-14） | 11.3、11.4、10.3 |
| 88 | Admin 建立用戶時遇到自己註冊緊（未驗證、冇密碼）嘅同一個 email：當同一個用戶處理，邀請 token 取代註冊 token（`email_verify_purpose` 變 `invite`），用戶只會收到邀請信。 | 已確認（產品經理 2026-10-14） | 11.4 |
| 89 | 產品經理基線 1：release manifest 只可以喺有專用 release ruleset 嘅 `v*` tag（臨時方案 (i) 下有寫權限嘅人都可以開 `v*` tag，tag 唔係閘門；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118）（v1.8 修訂：唔再由 `main` 直接出 release，見決定 96）、經有 required reviewers（Jack）、只准 `v*` tag嘅 GitHub environment `release`（Prevent self-review 關閉，見決定 96；admin bypass 保持開啟，Jack 已接受風險）、用 Sigstore keyless（決定 119） 簽；依賴 issue #28（release.yml 加固）完成先可以出第一個 release。 | 已確認（產品經理 2026-10-14） | 2C.3、9B、9C |
| 90 | （已撤回，產品經理 2026-10-16）原本要求高風險 release build 提供 `--release-approval`；呢個機制已撤回，唔會實作。高風險 `security-items` 只列入部署報告，唔阻 release；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118。剩餘風險（Jack 已接受）：admin 可以 bypass `main` ruleset 保護（OrganizationAdmin bypass）；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；同一個帳戶（`j113203`）可以自己批准；`release` environment admin bypass 保持開啟。 | 已撤回（產品經理 2026-10-16） | 2A.5、2C.3a、9C |
| 91 | 產品經理基線 3：production 一律拒絕 in-memory 資料庫，啟動直接報錯退出；冇任何 override 旗標。Production 定義只睇 build-mode 同環境：manifest 唔係 `build-mode: dev`，或者 `VIBE_ENV=production`。Listen 地址唔係 loopback（例如 Docker 入面 `vibe dev` 綁 `0.0.0.0`）只會警告，唔會拒絕；Docker 入面請明確設 `VIBE_ENV=development`。 | 已確認（產品經理 2026-10-14） | 2C.4、9C |
| 92 | 產品經理基線 4（v1.8 由決定 119 修訂）：CI release build 用 Sigstore keyless 加 Rekor 簽；OIDC、Fulcio 或者 Rekor 用唔到就失敗，永遠冇 fallback（唔 fallback 去 dev key、任何 secret 或者冇 Rekor 嘅簽名）；production runtime 只信任 pin 死嘅 Sigstore 身份，dev key 永遠唔信任。 | 已確認（產品經理 2026-10-14） | 2C.2、2C.3、9B |
| 93 | 產品經理基線 5：決定 29（CODEOWNERS 必須）同 37（injection allowlist 受 CODEOWNERS 保護）照舊有效，唔由決定 77 取代；CODEOWNERS required 係 source PR 合併條件（決定 115），SDK / `vibe build` 唔檢查 CODEOWNERS（2C.3a）。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext） | 已確認（產品經理 2026-10-14）：CODEOWNERS required 係 source PR 合併條件 | 2A.7、9B |
| 94 | 產品經理基線 6：網站 `/latest/` 永遠唔指向草稿 spec；spec 文件用 `<!-- spec-status: draft -->` 標記，網站 build（`vibe-backend/vibe-backend.github.io` 嘅 `build.mjs`）揀 latest 時跳過有呢個標記嘅版本。 | 已確認（產品經理 2026-10-14） | 文件標頭、網站 |
| 95 | create-user 邀請（v1.8 修訂）：邀請信寫入 transactional outbox（同其他寫入同一個 transaction）之後，create-user 同 re-invite 回 **202**，body 帶 `user_id` 同 `invite_status: invite_pending`。Mail worker 喺背景寄同重試；超過重試上限標 `delivery_failed`。邀請狀態（`invite_pending`／`sent`／`delivery_failed`）可以查詢，亦可以經 re-invite 重寄。只有 transaction 失敗（outbox 冇寫入，成個 request rollback，決定 99）先回 503 `INVITE_NOT_QUEUED`（帶 `Retry-After`）。`INVITE_DELIVERY_FAILED` 唔再係 503 錯誤碼，改為狀態值 `delivery_failed`。同 issue #26 相關。 | 已確認（產品經理 2026-10-14） | 11.4、11.4a、11.4b |
| 96 | （v1.8 修訂，issue #70；產品經理代 Jack 決定採用 (i) 做**臨時方案**）Release ruleset：專用 tag ruleset（target `tag`、`refs/tags/v*`），只有 `update`、`deletion`、`non_fast_forward` 三條 rule，**冇 `creation` rule**：任何有 write 權限嘅人都可以開 `v*` tag，但開咗就改唔到、刪唔到、force push 唔到。`bypass_actors` 一定要係空（冇 creation rule 之後同開 tag 冇衝突，保持空令 admin 都改唔到已發佈嘅 tag）。Release build 只可以由 `v*` tag 出。人手批准把關**而家只靠** `release` environment（reviewer `j113203`，Prevent self-review 關閉）；自動閘門仍然係決定 113／114／118；任何用 `j113203` 嘅 agent 都可以開 tag 再自己批准 release，呢個風險 Jack 已經接受。`--release-approval` 已撤回（產品經理 2026-10-16），冇額外把關。目標方案 (iii)（未生效）：專用 release GitHub App 係唯一可以開 `v*` tag 嘅 actor，同時開返 Prevent self-review。另外 live `release` environment `can_admins_bypass: true` 由 Jack 直接決定保持開啟，係已接受嘅風險（admin 可以跳過 environment 批准）。上面 ruleset 設定照舊要求，但 SDK 唔讀（原本嘅 ruleset 檢查屬 `--release-approval`，已撤回）。`main` ruleset 保留 OrganizationAdmin bypass。**已接受例外（Jack 決定 2026-10-16，issue #70；人手／文件層面）**：`main` ruleset `bypass_actors` 唔係空（例如 `[OrganizationAdmin, always]`）**唔會**阻 release。而家**冇**任何自動化檢查讀 ruleset 或者 `bypass_actors`（決定 90／#127：SDK 同 release workflow 唔讀 ruleset）；呢條例外係人手／文件層面接受。原因係而家得一個維護者、冇第二個 reviewer，冇 bypass 就冇嘢 merge 到。有第二個 reviewer 之後要重新檢討。如果將來 `release.yml` 加呢類步驟，只可以係唯讀、資訊性、永遠唔會擋 release（同決定 90、第 2C.3a 對齊）。呢條例外唔改 ruleset 設定，亦唔適用於 `v*` tag ruleset（嗰個 `bypass_actors` 照舊要求係空）。剩餘風險（Jack 已接受）：admin 可以 bypass `main` ruleset 保護（OrganizationAdmin bypass）；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；同一個帳戶（`j113203`）可以自己批准；`release` environment admin bypass 保持開啟。 | 已確認（產品經理 2026-10-14；Jack 2026-10-16, #70） | 2C.3、2C.3a、9C、[`pr-review-guide.md`](pr-review-guide.md) |
| 97 | 租戶隔離身份（v1 blocker）：runtime 為每個（租戶、role 類別）開獨立連線池，直接以 `LOGIN` 租戶 role 登入再執行 lock SQL；runtime 連線 role 同租戶 role 之間冇任何 membership，租戶連線入面冇辦法切返其他身份（唔用 `SET LOCAL ROLE`；除非之後證明 PostgreSQL 17 嘅 `SET LOCAL ROLE` 不可逆）。租戶 role `NOINHERIT`、只有自己 schema 嘅權限、唔係其他 role 嘅成員；憑證只存 runtime secret store。平台表（refresh token、idempotency、outbox）只喺租戶自己 schema，租戶 role 讀唔到任何跨租戶共用表。`app.tenant_id` 之類 GUC 只可以輔助（例如 log），永遠唔係身份來源。安全細節經私人 security advisory 處理。Role 命名、連線池上限、憑證、閒置關閉、owner role 同 policy 比對見決定 129。 | 已確認（產品經理 2026-10-14） | 7.1、7.1a、9C |
| 98 | Lock SQL parser 白名單：用 PostgreSQL 解析器（例如 `pg_query`）解析，只准單一 DML statement；禁止 transaction 控制、身份切換語句、會話設定修改同 schema-qualified 名稱；function 同 type 只准白名單；解析失敗即 fail closed。Conformance 要有跨租戶負面測試。 補充（已確認（產品經理 2026-10-14），Code Reviewer）：function 白名單係正面清單，冇列就禁止（動態 SQL／XML 查詢函數、introspection、取檔／外連函數、large object 全部唔喺入面）；lock SQL 唔可以有任何會話設定修改；唔可以引用平台表；migration 同樣用 parser 檢查（禁止提權 function、非白名單語言嘅 function／trigger、role 同權限操作、會話設定修改）；全部 fail closed，並有對應負面測試。 | 已確認（產品經理 2026-10-14） | 2A.4、7.1a、10.1 |
| 99 | 跨 model transaction：一個 request = 一個 runtime 管理嘅 transaction（`READ COMMITTED`）；用戶 SQL 控制唔到 transaction 邊界；庫存扣減用條件 UPDATE（`WHERE stock >= qty`）再檢查 affected rows，唔夠就 409 同 rollback；冇跨 request、跨 DB 或者分散式 transaction；寫入按固定次序、喺租戶 role 之下執行。取代 7.1「每個 query 獨立 transaction」。 | 已確認（產品經理 2026-10-14） | 7.1、7.4、12.6 |
| 100 | Transaction 超時同衝突：預設 `statement_timeout` 5s、transaction 總時間 10s、`idle_in_transaction_session_timeout` 5s；超時 rollback，回 504 `TX_TIMEOUT`。Deadlock（`40P01`）同 serialization failure（`40001`）回 503 `TX_CONFLICT`，body `retryable: true`，帶 `Retry-After: 1`；平台永遠唔會自動重試非冪等寫入。 | 已確認（產品經理 2026-10-14） | 7.4、8 |
| 101 | 瀏覽器 refresh cookie（issue #78）：refresh token 放 `__Host-vibe_rt_{slug}` cookie（`HttpOnly; Secure; Path=/`，冇 `Domain`）；產品經理確認防 subdomain 覆蓋比限定 Path 重要。兩個模式：預設 `same-site`（`SameSite=Strict`）；明確開嘅 `cross-site`（`SameSite=None`），一定要同時有嚴格 `Origin` 白名單（完全相等、唔准 `*`）同 `X-Vibe-CSRF` 自訂 header 檢查（強制 CORS preflight），缺任何一樣 build／部署失敗。每次 refresh 輪換，重用撤銷成個家族；access token 唔放 cookie；CORS credentials 只限 refresh 同 logout；logout 清 cookie 同撤銷家族。 | 已確認（產品經理 2026-10-14） | 4A.1、4A.1a |
| 102 | Build TOCTOU：`vibe build` 只讀一次 source 同 lock 入記憶體，驗證同之後所有 build 步驟（manifest、報告、簽名）都用同一份內容；之後檔案改動唔會影響今次 build。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext） | 已確認（產品經理 2026-10-14） | 2C.3 |
| 103 | CORS（issue #84）：`_definition.md` 加 `## Cors`（`allow-origins`、`allow-headers`、`expose-headers`、`max-age`）；origin 完全相等比對；`*` 只准喺冇 credentials 時用而且 release build 出 warning；`localhost` port 萬用只限 dev build；`## Cors` 唔准開 credentials（credentials 只限決定 101 嘅 refresh／logout）；永遠帶 `Vary: Origin`；`OPTIONS` preflight 由 runtime 處理。 | 已確認（產品經理 2026-10-14） | 2.3a、4A.1a、8 |
| 104 | `Idempotency-Key`（issue #85）：POST／PATCH api 可以喺 lock 開 `## Idempotency`；public endpoint 唔准開；key 範圍 `(tenant, subject, route, key)`；request hash 同成功回應喺同一個 request transaction（決定 99）寫入；同 key 唔同 body 回 422 `IDEMPOTENCY_KEY_REUSED`；處理中租約（`lease` 預設 30s，一定大過 transaction 上限 10s）有效時先回 409 `IDEMPOTENCY_IN_PROGRESS`，租約過期（例如 crash）當未開始、重試可以接手；過期紀錄由 runtime 按 TTL 清理（平台系統表，唔受決定 13 限制）；ttl 預設 24h；失敗回應唔儲存。平台照樣唔自動重試（決定 100）。 | 已確認（產品經理 2026-10-14） | 2.3b、8 |
| 105 | PATCH（issue #86）：route method 加 `PATCH`（修訂 0.1、2.3 嘅 GET／POST／PUT／DELETE）；語意係 JSON Merge Patch（RFC 7396），只限宣告欄位；`null` 只可以清 nullable 欄位；唔可以改嘅欄位回 422；內置 CRUD `update`（`partial: true`）用 PATCH、`partial: false` 用 PUT；CRUD update 照樣一定要 `If-Match`（決定 48），自訂 PATCH 可以選擇要唔要。 | 已確認（產品經理 2026-10-14） | 0.1、2.3、2.3c、12.9 |
| 106 | Cursor 分頁同區間篩選（issue #87）：CRUD `list` 可以揀 `pagination: cursor`（offset 照樣係預設，決定 14）；cursor 係簽名嘅不透明值，綁定 sort、filter 同租戶；排序一定加主鍵做 tiebreaker；`page-size` 預設 20，`max-page-size` 同 offset 一樣跟決定 71（平台上限 500），冇另外嘅 100 上限；`range` 篩選只准宣告咗而且有 index 嘅欄位，運算子 `gte`／`gt`／`lte`／`lt`；編譯時驗證；cursor 同 sort／filter 唔對應回 400 `INVALID_CURSOR`。 | 已確認（產品經理 2026-10-14） | 12.9 |
| 107 | 多欄唯一鍵：model lock 加 `## Constraints`（`- unique: [col_a, col_b]`，平台自動喺前面加 `tenant_id`）；一定要有對應 migration 建 unique index；衝突回 409 `UNIQUE_VIOLATION`，唔回顯欄位值。（決定 168：實作細節見 12.2） | 已確認（產品經理 2026-10-14） | 12.2、8 |
| 108 | 翻譯階段（issue #37）：非 ASCII source 按 `translate-1` 翻譯，只供語意一致性檢查；安全關鍵字一律檢查中文及英文原文，譯文不能補足。原文保留作 lock、hash、編譯 prompt 及 cache key。否定及 code 規則見 2.1a。此規則取代 #37 的 English-only／不處理中文否定決定。翻譯失敗即編譯失敗，沒有 fallback。翻譯 cache key 包含 source-hash、provider、model、prompt 版本及實際參數。 | 已確認（產品經理 2026-10-16） | 2.1a、2A.7 |
| 109 | Lock 記錄翻譯資料（審計用，唔計入 `source-hash`）：`translation: { prompt-version, llm-provider, model-snapshot, hash }`。（決定 165：translation stamp 改記喺 build record 同 manifest provenance，lock 唔再有） | 已確認（產品經理 2026-10-14） | 2A.1、2.1a |
| 110 | 高風險 release build 唔可以用翻譯 cache，一定重新翻譯。 | 已確認（產品經理 2026-10-14） | 2.1a、2C.3 |
| 111 | 翻譯 call 計入 #92 P5 每次編譯 LLM call 上限；超過即編譯失敗。 | 已確認（產品經理 2026-10-14） | 2.1a |
| 112 | 修訂決定 16：高風險欄位（寫入、公開、auth、secret）一定要喺 source frontmatter 宣告；宣告同譯文唔一致，或者偵測到未宣告嘅高風險行為，即 fail closed，錯誤訊息講明缺咗邊個宣告。關鍵字檢查係一致性檢查（縱深防禦），唔係唯一防線。api feature 唔再做寫入 sql 關鍵字檢查（由已檢查嘅 method lock 繼承）。 | 已確認（產品經理 2026-10-14） | 2.2、2.1a、2A.7 |
| 113 | 部署來源（issue #97，取代 lock 簽名做閘門）：runtime／部署只接受 CI 喺 `release` environment 由 source 重新 build 嘅 artifact，帶 GitHub artifact attestation（build provenance，SLSA），綁定 repo、workflow 路徑、ref（`refs/tags/v*`）同 commit SHA；再加 Sigstore keyless（決定 119） manifest 簽名。部署器驗證 attestation、manifest 簽名同 subject digest，任何一樣失敗即拒絕。Lock 係 build 輸出，唔 commit（決定 117）；部署輸入只有 CI 呢次 build 嘅 artifact；唔做重編比對（LLM 唔決定性）。CI 重編再比對唔再係部署條件（修訂決定 80 同 2A.6）。 | 已確認（產品經理 2026-10-14） | 2C.3b、9C、10.1 |
| 114 | 部署前 deterministic policy 檢查（對 artifact）：roles、寫入、public、auth 一定要等於決定 112 嘅 frontmatter 宣告；SQL 通過決定 98 parser 白名單；UPDATE 一定要有引用主鍵／唯一鍵或者宣告篩選欄位、而且綁參數嘅 WHERE；保守規則拒絕冇 WHERE、常數或者恆真條件；判斷唔到即 fail closed；任何錯誤即 fail closed。 | 已確認（產品經理 2026-10-14） | 2C.3b、10.1 |
| 115 | CODEOWNERS 唔係部署 gate，部署由決定 113／114 把關；CODEOWNERS required 照舊係 source PR 合併條件（決定 93）。 | 已確認（產品經理 2026-10-14） | 2C.3b、9C |
| 116 | （已由決定 117 取代）Artifact 同 repo lock 嘅差異：release job 產生 artifact 同 repo lock 嘅 deterministic diff（SQL 先經 parse tree 正規化，只係格式唔同唔計），放入 job summary 並 attest；高風險 release 嘅 diff 涉及 roles、寫入、public、auth 或者 SQL 就失敗，錯誤訊息講明 artifact 同 repo lock 喺邊度唔同（lock id、section、step），直到 repo lock 更新到一致。 | 已確認（產品經理 2026-10-14） | 2C.3b |
| 117 | Lock 係 build 輸出（Jack 方向，取代決定 80 同決定 116）：`*.lock.md` 唔再 commit（加入 `.gitignore`），由 CI／CD 喺部署時 build。Release job 將產生嘅 lock 存做審計 artifact 並納入 attestation，同上一個已部署版本比較，喺 release summary **列出**（唔阻擋）roles、寫入、public、auth 同 SQL 嘅變化（SQL 經 parse tree 正規化比較）。主要閘門照樣係決定 113 attestation 加決定 114 deterministic policy。`compile --check` 只喺本地／CI 驗證 build 結果同 policy，唔同已 commit 嘅 lock 比較。剩餘風險：產生嘅 SQL 唔再有人手 review；保護靠 source PR review frontmatter 宣告、決定 114 policy 同 release summary 列出變化。 | 已確認（產品經理 2026-10-14） | 2.1、2A.6、2C.3、2C.3b、9C |
| 118 | Build once, promote：release job 只喺 `v*` tag build 一次，同一個 artifact 原封不動 promote 去各環境，唔重新 build；部署器用 `gh attestation verify --source-ref refs/tags/v<version>` 驗證。PR CI build 只係非權威預覽，結果唔可以部署。LLM key：release 用 `release` environment secret `VIBE_LLM_API_KEY`，PR 預覽用 `VIBE_LLM_API_KEY_PREVIEW`，兩條分開。Artifact 入面要有審計戳記（tag、commit SHA、workflow run id、compiler-hash、build 時間），唔包含任何 key。閘門：臨時方案 (i) 下任何有寫權限嘅人都可以開 `v*` tag，所以 tag 唔係真正閘門；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（`--release-approval` 已撤回）。將來目標係方案 (iii)：專用 release App 係唯一可以開 `v*` tag 嘅 actor，並開啟 `prevent_self_review`。兩條 LLM secret 由 Jack 本人建立。 | 已確認（產品經理 2026-10-14） | 2A.8、2C.3b |
| 119 | Manifest 簽名改用 Sigstore keyless（Jack 決定，取代 KMS）：release manifest 由 release workflow 喺 `release` environment 用 GitHub OIDC 身份經 Sigstore（Fulcio 證書）簽，簽名一定要記錄喺 Rekor transparency log。驗證時證書身份一定要完全等於 `https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v<部署緊嘅版本>`（只接受完全相等，唔可以寬鬆配對 `refs/tags/v*`；如果工具一定要用 regex，只可以用完全錨定、正確轉義、寫死部署版本嘅式，例如 `^https://github\.com/vibe-backend/sdk/\.github/workflows/release\.yml@refs/tags/v1\.8\.0$`），commit SHA 同 GitHub-hosted runner 都要對，同決定 113 attestation 一致；issuer pin 死 `https://token.actions.githubusercontent.com`；冇 Rekor inclusion 證明即拒絕。驗證離線做：Rekor inclusion proof／SET 隨 Sigstore bundle 一齊交付；trust root（`trusted_root.json`：Fulcio CA、Rekor 公鑰、CT log key）由 Sigstore TUF repository 取得，release 時 pin 一份入 runtime binary，見 2C.3b。Keyless 身份只證明邊個 workflow 同 tag 產生呢個 build，唔證明邊個批准；佢唔會加多一個人，因為任何 `j113203` agent 都可以開 tag 再自己批准。冇任何長期簽名 key 或者 KMS，冇自管簽名 key 輪換（Sigstore trust root 輪換見 2C.3b）。Environment secret（例如簽名 secret）只可以用喺 dev／test，永遠唔用嚟簽 release。剩餘風險：臨時方案 (i) 下 tag 身份唔係閘門，唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；trust root 更新都係經同一個 `release` environment 出，同一個帳戶可以自己批准，冇額外嘅人把關。 | 已確認（產品經理 2026-10-14） | 2C.2、2C.3、2C.3b、9B、9C |
| 120 | Email 正規化（issue #106）：v1 唔合併 plus addressing，`a+x@` 同 `a@` 係兩個唔同地址；local part 入面嘅點（`.`）照樣保留，`a.b@` 同 `ab@` 係兩個唔同地址。Local part：先做 Unicode NFC（唔係 NFKC），再 trim，再 lowercase。Domain：用 IDNA UTS #46 轉做 punycode，再 lowercase；IDNA 轉換失敗回 422 `VALIDATION_FAILED`。唯一 index、查找同發送全部用正規化後嘅值；login、註冊、邀請、驗證、重發同 rate-limit 全部先正規化再儲存同查找（12.11）。v1 只儲存正規化後嘅值，唔另外保留原本地址做顯示。現有資料遷移：migration 先偵測正規化後衝突（只差大小寫或者 Unicode 形式嘅重複地址）；有任何衝突就 fail closed 停止，唔自動合併；衝突報告只列 user id，任何公開 log 都唔出現完整 email；由人手按 12.11 嘅處理程序解決之後先重新跑。 | 已確認（產品經理 2026-10-16） | 4.3、9、12.11 |
| 129 | 租戶 role 細節（補充決定 97，SDK #147）：(1) Runtime role 係 `tenant_<id>_rw`、`tenant_<id>_public_reader`、`tenant_<id>_public_writer`；`<id>` 係系統產生、只有 `[a-z0-9_]` 嘅不透明 id，永遠唔用用戶提供嘅名；role 名超過 63 bytes 即拒絕，永遠唔截斷。(2) 連線池每個（租戶、role）5 條，每個租戶最多 15 條；另有全 process 上限，預設 200，可以設定（設定名 `VIBE_DB_POOL_MAX_TOTAL`（已確認（產品經理 2026-10-16）））。三個上限（每個（租戶、role）5 條、每個租戶 15 條、全 process）任何一個滿咗，都即刻回 503 `DB_POOL_EXHAUSTED` 加 `Retry-After`（值 `1`（已確認（產品經理 2026-10-16））），並發 metric（名 `vibe_db_pool_exhausted_total{scope="role"\|"tenant"\|"process"}`（已確認（產品經理 2026-10-16）））；永遠唔排隊等 timeout，包括每個（租戶、role）同每個租戶上限。(3) `VIBE_TENANT_DB_SECRETS` JSON 檔只限開發同測試，權限一定 0600，永遠唔 commit、唔寫 log；production 憑證來源係 secret manager，經每個租戶 role 嘅環境變數參照注入（同 #147 實作一致），process 只讀參照、唔落地成檔；production 見到呢個檔就拒絕啟動（fail closed）。Owner role（`tenant_<id>_owner`）憑證：平台只喺每次 migrate 開始時由 secret manager 發一次性密碼，migrate 完成即作廢並將 role 設返 `NOLOGIN`；即係每次 migrate 都輪換，runtime 永遠攞唔到。(4) 閒置連線池 5 分鐘後關閉（v1 固定，冇環境變數設定）；呢點阻 GA，唔阻 v1 preview。(5) Migration role 係 `tenant_<id>_owner`，擁有該租戶 schema 同表，只畀 migrate 用。決定 27 嘅例外（已確認（產品經理 2026-10-16））：PostgreSQL owner 天生可以 TRUNCATE／DROP 同重新授權，冇辦法靠收權限禁止，所以 owner 係決定 27 嘅受控例外，條件見 2B「Owner role 條件」：migration parser 用正面白名單、migrate 單一 transaction、finally 由獨立平台 admin 連線收回登入、定期檢查、runtime 連線池永遠唔以 owner 登入；runtime role 唔係 owner，每張表都要 `FORCE ROW LEVEL SECURITY`；migrate 兩樣都檢查，缺任何一樣就失敗。(6) 編譯器輸出每個租戶常數 policy（`TO tenant_<id>_rw`），唔用 `current_setting('app.tenant_id')`；部署時比較編譯出嚟嘅 policy 同 DB 實際安裝嘅 policy，唔一致即 fail closed。 實作狀態（對照 #147 @8066b69）：#147 已包括三層連線池上限即時 503、`VIBE_DB_POOL_MAX_TOTAL`、`vibe_db_pool_exhausted_total{scope}`、production 拒絕 `VIBE_TENANT_DB_SECRETS`、一次性 owner 密碼、`pg_policies` 比對、migrate 拆做主 transaction 加 commit 後 concurrent index 步驟、finally 由獨立 admin 連線將 owner 設返 `NOLOGIN`、定期 owner 檢查（`vibe check-owners`）、連線池拒絕 owner role、parser 白名單（2B.3，#206）、INVALID index 只 drop 平台紀錄嘅（#207）、閒置連線池固定 5 分鐘關閉（v1 冇環境變數設定，#165）。受控 membership 例外（已確認（產品經理 2026-10-16））：每個租戶剛好一行 membership `tenant_<id>_rw` ← `tenant_<id>_owner`，`ADMIN FALSE`、`INHERIT FALSE`、`SET TRUE`（PostgreSQL 16+），反方向同其他任何 membership 令啟動失敗；owner migrate 以外 `NOLOGIN` 加定期偏差檢查；必須測試跨租戶、INHERIT、ADMIN OPTION 三種失敗情況——未實作，#246（#147 啟動檢查要調整）。另外嘅 lock 驗證器加固由 #147 進行中。未完成：backfill 同 replay 清理按租戶設定 RLS context、參數化同分批——未實作，#233。未完成：v1 要求 PostgreSQL 16 或以上（架構師決定 2026-10-16），16 以下 `vibe migrate`／`vibe serve` 啟動時 fail closed 並報明確錯誤，冇 fallback——未實作（code 仲未檢查版本），#254。 | 已確認（產品經理 2026-10-16）；owner `SET ROLE` 剩餘風險已接受（產品經理 2026-10-16） | 7.1、7.1a、9C |
| 130 | IP 限流計數鍵同驅逐（SDK #132，v1 blocker；修訂 #73 嘅限流設計）：(1) 網段 slot 上限（跟 #73）：上限係每個網段可以開嘅**新計數器 slot** 數目，唔係 request 次數。計數前 IPv4-mapped IPv6（`::ffff:a.b.c.d`）一律先轉做 IPv4（同 #182 一致）。IPv4 每個（`tenant_id`、/24）最多 10 個新 slot；IPv6 每個（`tenant_id`、/48）最多 10 個，另加每個（`tenant_id`、/32）最多 50 個。Request 次數限流照舊用 4.3 嘅滑動窗口同數字。(2) 平台層：另有一個跨租戶網段 slot 計數器，鍵係 /24 同 /48，預設上限係租戶 slot 上限 ×5（單位同樣係新計數器 slot），倍數用 `VIBE_RATE_PLATFORM_FACTOR` 設定（預設 5，最大 100（已確認（產品經理 2026-10-16）））；兩層都通過先放行；平台層亦保證每個租戶 64 slot 保底。拒絕回 429 加 `Retry-After`，回應永遠唔講係邊一層觸發。(3) 驅逐：固定窗口長度 60 秒，只用喺本決定嘅網段 slot 計數同佢嘅 LRU 驅逐（4.3 request 限流仍然係滑動窗口）；LRU 有上限，先驅逐已過期嘅項目；每個租戶有自己嘅計數器 slot 上限；slot 滿而冇過期項目就 fail closed，回 429 並記 metric；永遠唔驅逐仲喺窗口內嘅計數器。Metric：拒絕記 metric（名 `vibe_rate_limit_slot_refused_total{layer="tenant"\|"platform", reason="network_cap"\|"table_full"\|"tenant_budget_exhausted"\|"prefix_cap_exceeded"}`（已確認（產品經理 2026-10-16））；`layer` 只喺內部 metric 出現，回應唔講）。Slot 表（同 #182 新設計（已確認（產品經理 2026-10-16）））：租戶表：每個租戶有自己一個 4,096 slot 嘅表，滿咗只拒絕嗰個租戶；全部租戶總預算 = `VIBE_RATE_EXPECTED_TENANTS`（預設 256，範圍 1–4096）× 4,096；用緊少過 64 個 slot 嘅租戶一定放行。平台表：IPv4 按 /24、IPv6 按 /32 分開嘅表，每個最多 4,096 項。平台表保證每個租戶嘅 64 slot 保底：平台總量 = 租戶總預算 + 租戶數 × 64，預設 1,064,960（256 × 4,096 + 256 × 64），所以就算好多租戶填滿平台表，新租戶都唔會冇 slot 可用。多 instance 部署一定要用共用儲存（#157）。#73 嘅 shard 內 owner／reserve 上限（每個網段 100 個 bucket、較闊網段 1,000 個、最後 1,000 個保留 slot）照舊保留，喺本決定之外再疊加（已確認（產品經理 2026-10-16））。Test IP 覆寫由環境變數 `VIBE_HONOR_TEST_IP=true` 開啟，只喺 debug build 生效；release build 唔理 `x-vibe-test-ip` header，而且設定咗 `VIBE_HONOR_TEST_IP=true` 時 `vibe serve`／router 拒絕啟動（fail closed）。剩餘風險：(a) 固定窗口邊界前後，網段新 slot 數目最多可以去到上限 ×2（已接受）；(b) 平台層令一個租戶嘅流量可以用埋同一 /24 或者 /48 其他租戶嘅額度（已接受）；(c) 平台總量只有保底以上嘅部分係跨租戶共用；共用部分喺一個窗口內用晒嘅話，保底以上嘅新 slot 會被拒絕，保底照樣保證（已確認（產品經理 2026-10-16））；(d) 同時滿咗嘅租戶多過 `VIBE_RATE_EXPECTED_TENANTS`，就只有用緊少過 64 個 slot 嘅租戶可以開新 slot（已確認（產品經理 2026-10-16））；呢個情況一定記 metric reason `tenant_budget_exhausted`（已確認（產品經理 2026-10-16））並發 WARN 告警（每分鐘最多一次），唔可以靜默拒絕；(e) 最差情況每個表大約 300 MB 記憶體（估計，未實測）（已確認（產品經理 2026-10-16））；唔可以靠呢個估計：v1 一定要有硬記憶體上限 `VIBE_RATE_TABLE_MEMORY_MAX_BYTES`（預設 268435456，即 256 MiB，限 limiter 全部 slot 表合計；名同預設已確認（產品經理 2026-10-16）；預設刻意細過單一表最差估計（大約 300 MB），滿載時 limiter 會先到 `table_full`（fail closed），#195 實測後再調整預設），超過即以 reason `table_full` fail closed。記憶體上限預先保留保底份額：租戶數 × 64 × 3 KiB（每個保底 slot 一個租戶計數器、一個平台計數器同一個 bucket），256 個租戶即 48 MiB，最多佔上限一半。保底以內嘅增長對照完整上限檢查；其他增長對照「上限減保留」檢查，超過即 `table_full`。#195 實測：每個 slot 大約 481 B，上限按每個 slot 1 KiB 估算；**GA blocker：記憶體實測（#195）**；(f) 活躍租戶多過 `VIBE_RATE_EXPECTED_TENANTS` 時，就算喺保底以內嘅請求都可能撞到硬記憶體上限，以 `table_full` fail closed（已接受（產品經理 2026-10-16））；接受條件：活躍租戶超過 `VIBE_RATE_EXPECTED_TENANTS` 嘅 80% 就發告警 `rate_limit_active_tenants_high`；保底以內嘅 `table_full` 拒絕要喺 `vibe_rate_limit_slot_refused_total{reason="table_full"}` 用 `within_floor` label 分辨（或者只寫入內部 log），永遠唔出現喺任何回應；#195 實測要一併驗證呢個風險同保底記憶體預留嘅大小；營運調整見 4.3；(g) 一個 IPv6 /32 入面活躍 /48 太多、超過該 /32 嘅平台表上限時，嗰個 /32 自己嘅用戶會 fail closed（已確認（產品經理 2026-10-16））；呢個情況一定記 metric reason `prefix_cap_exceeded`（reason 名已確認（產品經理 2026-10-16））並發 WARN 告警（每分鐘最多一次），唔可以靜默拒絕。 實作狀態（對照 #182 @679e128（2026-10-16 核對））：#182 實作每租戶 4,096 slot 表、`VIBE_RATE_EXPECTED_TENANTS`、64 slot 保底（平台表同記憶體保留，保底以內增長對照完整上限）、平台 /24、/32 表（預設總量 1,064,960）、`VIBE_RATE_PLATFORM_FACTOR`、IPv4-mapped 轉 IPv4、四個 reason、每分鐘最多一次嘅 WARN 告警、`VIBE_RATE_TABLE_MEMORY_MAX_BYTES` 硬上限（#210、#211），同埋 release build 唔理 test IP header。未完成：告警 `rate_limit_active_tenants_high`——#182 未實作，#234；`within_floor` 分辨——#182 未實作，#235；release build 設定 `VIBE_HONOR_TEST_IP=true` 時拒絕啟動——#182 已實作（自 @c12736c 由環境變數讀取，有測試），#230 喺 #182 merge 之後由 maintainer 關閉。記憶體實測見 #195（GA blocker）。 | 已確認（產品經理 2026-10-16） | 4.3、4A.3、7.2 |
| 139 | `sign-jwt` 簽名算法政策（issue #198）：v1 只支援 ES256 同 RS256，預設 ES256；RS256 用嚟同第三方／JWKS 互通。RS256 key 最少 2048 bit，喺 `vibe validate --deploy`／部署同 `vibe serve` 啟動時檢查，唔夠就 fail closed（拒絕啟動，永遠唔會降級或者改用其他 alg）。驗證端只接受設定好嘅一個 alg；`alg` header 係其他值一律拒絕，包括 `none`、HS*，同埋 ES256／RS256 之中另一個（唔准混用）。EdDSA 留到 vNext。實作狀態：部分未實作，見 4.1a。 | 已確認（產品經理 2026-10-16） | 4.1、4.1a、12.6 |

### 0.11 v1.8 refresh cookie 補充（決定 141 起，issue #78，已確認（產品經理 2026-10-16））

決定 6、45、60、61、65、72 同 101 已經定咗 refresh 輪換、重用偵測、寬限期、pipeline 參數、限流、cookie 名同屬性、兩個模式同 CSRF／CORS 原則；以下只補返 issue #78 仲未寫清楚嘅部分。產品經理已確認（2026-10-16）。實作狀態見 4A.1b。

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 141 | Cookie 模式下 login 都用 cookie：`transport: cookie` 時，login（`sign-jwt` `issue-refresh: new-family`）同 refresh（決定 101）一樣，新 refresh token **只**喺 `Set-Cookie` 出現，response body 唔包含 `refresh_token`；body 只有 `access_token` 同 `expires_in`（login 而家已經用 `access_token` 欄位名）。`transport: body` 照舊（決定 6、45）。 | 已確認（產品經理 2026-10-16） | 4A.1b |
| 142 | 對外錯誤一致：輪換、重用撤銷家族同清 cookie 跟決定 6、61、101，唔重複定義。重用、過期、已撤銷、搵唔到，對外一律回同一個 401 錯誤碼同 body（決定 147），唔分開透露狀態；分別只寫入審計（`revoke_reason: reuse-detected` 等）。 | 已確認（產品經理 2026-10-16） | 4A.1、4A.1b |
| 143 | Cookie `Max-Age` = `min(家族剩餘絕對壽命, 閒置剩餘時間)`，每次輪換重新計（收緊決定 101「唔超過家族剩餘絕對壽命」；決定 143 只可以收緊決定 101，永遠唔可以放寬佢任何 cookie 屬性或者上限）；一定有 `Max-Age`（唔用 session cookie），唔用 `Expires`。`Path=/`、冇 `Domain` 照決定 101；決定 60 嘅 slug Path 只適用 `transport: body`。 | 已確認（產品經理 2026-10-16） | 4A.1a、4A.1b |
| 144 | Refresh／logout 檢查次序（全部喺讀 cookie 之前做，補充決定 101）：1. method 一定 `POST`（其他回 405）；2. `Origin` 一定存在而且完全等於 `allowed-origins` 其中一個；3. `cross-site` 模式一定有 `X-Vibe-CSRF: 1`（cookie logout 喺任何模式都一定要，決定 145）；2 或 3 失敗對外一律回 403 `CSRF_REJECTED`，詳細原因只寫 server log 同審計；呢類 403 唔清 cookie，cookie 只喺通過檢查之後先會被清；4. cookie 模式只由 cookie 讀 token，忽略 body 入面嘅 `refresh_token`。唔接受 `Referer` 代替 `Origin`。 | 已確認（產品經理 2026-10-16） | 4A.1a、4A.1b |
| 145 | Cookie 模式 logout 唔要求 access token：瀏覽器 reload 之後記憶體冇 access token，所以 `token-revoke` 喺 `token-source: cookie` 而且 `scope: family` 時，以 cookie 搵家族並撤銷，唔需要 `auth`（決定 65「一定要 `auth`」嘅例外，只限呢個組合）。條件（產品經理 2026-10-16 確認；架構師 2026-10-16 釐清）：(1) 三項檢查，依次：(a) `Origin` 一定存在而且完全等於嗰個 slug 嘅決定 101 `allowed-origins` 其中一個（同 refresh 用同一份設定，冇另外嘅 logout 清單）；(b) 一定帶 `X-Vibe-CSRF: 1`，`same-site` 模式都要；(c) token 只由 `__Host-vibe_rt_{slug}` cookie 讀，永遠唔由 body 或者其他 cookie 名讀。(a) 或 (b) 失敗即 fail closed：403 `CSRF_REJECTED`，唔讀 cookie、唔撤銷、唔清 cookie。(2) (a)、(b) 通過之後，有效 cookie 就撤銷佢對應嘅家族；冇 cookie、cookie 無效、過期或者已撤銷就乜都唔撤銷；兩種情況回應完全一樣：204 加清除 cookie，唔透露有冇 token 或者 token 係咪有效。(3) 只撤銷嗰個 cookie 對應嘅家族，而且家族一定要屬於 URL slug 嗰個租戶（RLS 照舊；租戶唔啱當無效 cookie 處理）；唔影響同一用戶其他家族。(4) 重複 logout 係 idempotent。(5) 同 refresh 一樣套用決定 72 嘅限流同最低回應耗時（300ms），令有效同無效 cookie 喺時間上分唔出。限流 key（issue #253，產品經理 2026-10-16 批准）：cookie logout 只用決定 72 嘅 IP 限流（每分鐘 30 次），計法同 refresh 一樣；**唔用**決定 72 嘅家族限流（每分鐘 10 次）。呢個係決定 72 嘅例外，只適用於 cookie logout，refresh 照舊用家族限流。有效、無效同冇 cookie 嘅 429 同回應耗時一定完全一樣；最低回應耗時 300ms 不變。見 4A.1b「Cookie logout 限流 key」。(6) 點解 `same-site` 都要 `X-Vibe-CSRF`：logout 冇 access token（本決定），所以除咗 `Origin` 檢查，CSRF header 係佢唯一嘅來源證明；無條件要求亦令兩個模式用同一條規則。(7) 必要測試見 4A.1b。`scope: all`（全部裝置登出）照舊一定要 `auth`。 | 已確認（產品經理 2026-10-16） | 4A.1a、4A.1b |
| 146 | Access token 預設 TTL 15 分鐘（上限 1 小時照決定 6）；refresh 家族、`idle-ttl`、`reuse-grace` 嘅預設同上限照決定 6、45、61，唔改。Cookie 模式建議前端喺 access token 到期前 60 秒內 refresh。 | 已確認（產品經理 2026-10-16） | 4A.1、4A.1b |
| 147 | Refresh／logout 錯誤碼（固定、平台提供，lock 唔使宣告；cookie 同 body transport 一律用同一套，`INVALID_REFRESH_TOKEN` 冇任何別名）：401 `INVALID_REFRESH_TOKEN`（只適用於 refresh：冇 token／冇 cookie、無效、過期、已撤銷、重用；同時清 cookie。Cookie logout 喺同樣情況唔回 401，按決定 145 回 204 加清 cookie；issue #253 釐清）；403 `CSRF_REJECTED`（決定 144；唔清 cookie）；405 `METHOD_NOT_ALLOWED`；429 `RATE_LIMITED`（只適用於決定 72 嘅 IP 限流，帶 `Retry-After`）；每個 refresh 家族嘅限流（決定 72）：超過上限時回 401 `INVALID_REFRESH_TOKEN`，同無效 token 嘅回應逐 byte 完全一樣（status、body 同所有 header，包括清除 refresh cookie 嘅同一個 `Set-Cookie`），唔帶 `Retry-After`，同樣套用最低回應耗時 300ms，唔撤銷家族。呢個事件只可以記錄喺營運人員／租戶可見嘅 server log，而且只可以用租戶 ID 同家族 ID 嘅 hash，永遠唔記錄 token 明文或者家族 ID 原文；除咗本規則之外，回應、API 錯誤訊息同公開文件都唔透露呢個事件；503 `TX_CONFLICT`／504 `TX_TIMEOUT`（決定 100）。錯誤 body 唔包含任何 token、hash 或者家族 id。 | 已確認（產品經理 2026-10-16） | 4A.1b、8 |
| 148 | 向後相容：`transport` 預設照舊 `body`，現有 lock 唔使改；改做 `cookie` 係 opt-in，要重新 build。每個 endpoint version 只接受一種 transport，永遠唔會同時收 body 同 cookie；要遷移就喺新 `{version}` 開 cookie endpoint，舊版 body endpoint 照舊運作直到 deprecate。已發出嘅 body refresh token 唔會自動轉做 cookie，用戶喺新版重新登入。SDK client 預設照舊。 | 已確認（產品經理 2026-10-16） | 4A.1b |

### 0.12 Challenge provider：Turnstile（決定 131 起，issue #160，已確認（產品經理 2026-10-16））

跟進 #139（#118）嘅實作（[`challenge.md`](challenge.md)）同 #177（共用 replay store）。產品經理已確認（2026-10-16）。只描述防禦設計。

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 131 | v1 challenge provider 係 Cloudflare Turnstile：`type: captcha` 一律由 server 端 siteverify 驗證，只有 header 存在唔算通過。`type: pow` v1 唔支援（修訂決定 49：v1 有 INSERT 嘅 public endpoint 一定要 `type: captcha`）；`type: pow` 係 **build 時驗證器 error**，`vibe build`、`vibe validate` 同部署一律失敗，唔會去到 runtime。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext） | 已確認（產品經理 2026-10-16） | 7.2a、10.7 |
| 132 | Server 端 siteverify：每個 request 帶 `x-challenge-proof` header；server 先拒絕空或者超過 2048 字元嘅 token，再喺 replay store 預留 token（只存 SHA-256），然後 call Turnstile siteverify（5s timeout、唔跟 redirect、只准公網地址），送 secret、token 同 client IP。一定要 `success: true`、`hostname` 喺 `VIBE_CHALLENGE_HOSTNAMES`、`action` 等於 endpoint 名、`challenge_ts` 存在而且唔超過 300s 前、唔超過 60s 後。每個 token 最多用一次。**Replay store（#177）**：production 一律用共用 Postgres replay store（所有 instance 共用），冇設定或者連唔到就拒絕啟動；per-process cache 只限 dev／test。Turnstile 本身嘅單次使用檢查（`timeout-or-duplicate`）只係額外一層，唔係主要防線。 | 已確認（產品經理 2026-10-16） | 7.2a |
| 133 | 邊啲 endpoint 要 challenge：跟決定 38、49：有 INSERT 嘅 public endpoint `{ type: captcha, when: always }`；login 類 `{ type: captcha, when: after-failures, after-failures: 3 }`；其他冇 INSERT 嘅 public 寫入可以 `type: none`。已認證 endpoint 預設**唔要**，但可以經 source frontmatter `challenge: captcha` opt-in（lock 寫 `{ type: captcha, when: always }`）；refresh endpoint（決定 72）唔要求。 | 已確認（產品經理 2026-10-16） | 7.2、7.2a |
| 134 | 失敗、超時同 fail closed：503 `CHALLENGE_UNAVAILABLE` **只限** provider 或者網絡失敗（provider 唔可達、超時、回應唔啱，同 replay store 暫時連唔到），永遠唔會當通過、冇 fallback；token 無效一律係 403，唔會變 503。**Replay store 滿**（已確認（產品經理 2026-10-16））：係流量造成嘅容量限制，唔係 provider 失敗，所以回 **429** `CHALLENGE_RATE_LIMITED`加 `Retry-After`，唔回 503；照樣 fail closed：未成功記錄 replay 就永遠唔准放行。`Retry-After` = 該租戶自己最舊紀錄嘅剩餘有效時間，上限 360 秒，計唔到就用 60 秒。同時發 metric `vibe_challenge_rejected_total{reason="replay_store_full"}`並觸發告警 `challenge_replay_store_full`（store 滿時觸發；metric reason `replay_store_full`）；名稱已確認（產品經理 2026-10-16）。**每個網段上限**（已確認（產品經理 2026-10-16））：每個租戶嘅 replay store 入面，每個網段（IPv4 /24、IPv6 /48）可以持有嘅記錄有上限，預設 16（同決定 137 每個 IP 16 個未驗證預留一致）；超過回 429 `CHALLENGE_RATE_LIMITED`（內部 reason `replay_prefix_cap`），`Retry-After` = 該租戶該網段最舊紀錄嘅剩餘有效時間，上限 360s，計唔到就 60，同 store 滿規則一樣（網段 `Retry-After` 已確認（產品經理 2026-10-16））。**429 回應一致**（已確認（產品經理 2026-10-16））：網段超過上限同 store 滿都用同一個錯誤碼 `CHALLENGE_RATE_LIMITED`；reason（`replay_store_full`、`replay_prefix_cap` 等）只寫入 metric 同內部 log，永遠唔出現喺 HTTP 回應（body、header、error detail 都冇）；所有呢類 429 回應除咗 `Retry-After` 之外完全一樣。Replay store 使用率超過 80% 觸發告警 `challenge_replay_store_high_usage`（已確認（產品經理 2026-10-16））。設定問題喺啟動時擋：production（`VIBE_ENV`）有任何 challenge route 而冇設定 verifier 或者共用 replay store，`vibe serve` 拒絕啟動、`vibe validate --deploy` 失敗。Dev（`vibe dev`）用假 verifier（一律通過，terminal 標示，決定 74），production 永遠唔准用。 | 已確認（產品經理 2026-10-16） | 7.2a、2C.4 |
| 135 | 錯誤碼：400 `CHALLENGE_REQUIRED`（冇 proof）；403 `CHALLENGE_FAILED`（偽造、重用、過期、時間錯、hostname／action 唔啱、每 IP／每租戶未驗證預留上限，即係所有 token 無效情況）；503 `CHALLENGE_UNAVAILABLE`（只限 provider／網絡失敗，固定帶 `Retry-After: 5`）；429 `CHALLENGE_RATE_LIMITED`（replay store 滿或者網段超過上限，決定 134；帶 `Retry-After`：store 滿按該租戶自己最舊紀錄、網段超過上限按該租戶喺嗰個網段最舊紀錄嘅剩餘有效時間，上限 360 秒，計唔到用 60 秒（網段 `Retry-After` 已確認（產品經理 2026-10-16））；回應唔包含 reason）。限流同配額係另一回事，回 429 `RATE_LIMITED`（帶 `Retry-After`），唔會用 503。錯誤 body 唔包含 token、provider 原始錯誤碼或者 secret；詳細原因只入 server log 同審計。 | 已確認（產品經理 2026-10-16） | 7.2a、8 |
| 136 | Secret 處理：`VIBE_TURNSTILE_SECRET` 只可以由環境變數或者 secret manager（例如 CI environment secret）注入；唔准寫入 source、lock、manifest、`.env` commit 或者 log；錯誤訊息同 debug 輸出只顯示「已設定／未設定」。驗證器見到 lock 或者 source 有疑似 secret 值即報 error。設定：`VIBE_CHALLENGE_PROVIDER`（預設 `turnstile`）、`VIBE_TURNSTILE_SECRET`、`VIBE_CHALLENGE_HOSTNAMES`（有 secret 就一定要有）、`VIBE_CHALLENGE_REPLAY_STORE`（production 一定 `postgres`，#177）；預設同 production 規則見 7.2a。 | 已確認（產品經理 2026-10-16） | 7.2a、2A.8.5 |
| 137 | 同限流組合：route 限流（決定 25、39）同 public 寫入每日配額喺 challenge 之前檢查，超過回 **429**（同 challenge 嘅 503 分開），被限流嘅 request 唔會去到 replay store 或者 provider；challenge 唔取代限流同配額。v1 用固定平台數值：token 壽命 300s、每 client IP 最多 16、每租戶最多 1,000 個未驗證預留（超過回 403 `CHALLENGE_FAILED`，唔 call provider）；**冇租戶 override**。將來如果加 override，只可以調低，唔可以調高。 | 已確認（產品經理 2026-10-16） | 7.2a、4.3 |
| 138 | Provider 抽象：runtime 經 `ChallengeVerifier` trait 驗證（輸入 token、client IP、預期 action；輸出 通過／失敗／不可用 三種，provider 細節唔外露；「失敗」對應 403，「不可用」對應 503）。之後加 hCaptcha 等 provider 只係新 implementation 同 `VIBE_CHALLENGE_PROVIDER` 新值，lock 語法、replay store 同錯誤碼唔變；未實作嘅值（例如 `hcaptcha`）啟動時拒絕。 | 已確認（產品經理 2026-10-16） | 7.2a |
| 140 | Challenge token 綁定 request digest（issue #156）：所有 `type: captcha` route（包括 login 類 `when: after-failures`）嘅 token 經 Turnstile `cdata` 綁定 request digest：`rd1.` 加完整 43 字元 base64url（冇 padding）SHA-256，唔截短。Client 每個 request 用 CSPRNG 產生 128 bit nonce，只經 `X-Vibe-Request-Nonce`（剛好 22 字元 base64url）送畀 server，冇或者重用即 fail closed，nonce key `(tenant_id, nonce)`、每個 request 一個紀錄，計入決定 134 同 137 上限：超過決定 137 即 403 `CHALLENGE_FAILED`，超過決定 134 即 429 `CHALLENGE_RATE_LIMITED`（同決定 134 一致）；驗證失敗（包括 nonce 重用）一律 403；`cdata` 只放 digest（私隱）。Digest 一律涵蓋 nonce、method、正規化 path（包括 tenant slug）、正規化 query 同 raw body 嘅 SHA-256；header 只涵蓋 `Content-Type` 同 `If-Match`；唔包括 client IP。Query raw `+` 無效（空格 `%20`、`+` 送 `%2B`，client helper 自動做）；冇 Unicode 正規化；任何無效 canonical 輸入回現有 403 `CHALLENGE_FAILED`，reason 只入內部 log；log 永遠唔記錄 nonce 或者 body。Server 喺 JSON 解析之前重新計算，同 siteverify `cdata` constant-time 比較，唔一致或者冇即 403 `CHALLENGE_FAILED`（唔講邊部分唔一致），fail closed。Proxy rewrite path 會 fail closed（部署要求）。v1 准 `bind: none` 但出 warning；下一個 minor release 起新 public INSERT route 唔准用 `none`（#245）。前端喺表格完成、送出之前先做 challenge。Session 綁定留到 vNext。實作狀態：未實作，見 7.2b。 檢查次序同交易（#258，已確認（產品經理 2026-10-16 12:44 HKT））：格式錯 403 → store 唔可用 503 → 重用 403 → 決定 137 403 → 決定 134 429；先 nonce 後 token，同一個交易，失敗全部 rollback；兩個紀錄都計入上限（數值喺 #256 調整）。 | 已確認（產品經理 2026-10-16；演算法修正 2026-10-16 11:58 HKT） | 7.2b |

### 0.13 v1.8 refresh 簽發一致性（決定 149，issue #270，產品經理已批准 2026-10-17）

源自 #95 review：現時 `token-refresh` 先用一個短 transaction commit reservation，再喺 transaction 外面經另一條 connection 執行 `sign-method`（佢每個寫入 step 各自 commit），最後先 finalize 輪換；finalize 失敗、超時或者 request 被取消嘅話，`sign-method` 嘅寫入已經生效但 token 冇輪換。決定 149 令 `sign-method` 一律唯讀，所以冇任何用戶寫入需要同輪換一齊 commit，解決 #270。只描述防禦設計。

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 149 | `sign-method` 一律唯讀：`token-refresh` 嘅 `sign-method`，連同佢經 `op: method`（任何深度）呼叫嘅 method，都唔可以有寫入 step。`vibe validate`、`vibe build` 同 `vibe serve` 一律阻擋（錯誤碼 `REFRESH_SIGN_METHOD_WRITES`）；refresh 路徑上 `sign-method` 嘅所有 DB statement 再以 READ ONLY 執行做 DB 層兜底。「最後 refresh 時間」以家族為單位，由 SDK 喺輪換確認 transaction 入面寫 `_platform_refresh_families.last_refreshed_at`（只可以向前，成個家族撤銷之後唔可以改；寬限期重發只撤銷單一 child，唔計）；其他業務寫入搬去 client 喺 refresh 成功之後另外呼叫嘅普通寫入 method。v1.0 前嘅 breaking change。唔改決定 6、45、61、147 嘅對外行為同錯誤碼。（決定 164：v1 用 `vibe manifest`，`vibe build` 延至 vNext） | 已確認（產品經理 2026-10-17） | 4A.1、4A.1c |

點解唔揀其他做法：選項 B（喺輪換 transaction 同一條 connection 入面執行有寫入嘅 method）要持有 family lock 更耐、要將 method executor 改做共用 transaction，實作面大，而且失敗模式（lock 等候、pool 用盡、部分 step 失敗）更複雜；延後到 vNext 再評估。選項 C（事後補償，反做 method 寫入）唔係真正 atomic，補償本身都可以失敗，而且業務寫入未必可以反做；唔採用。

### 0.14 v1.8 2026-10-17 決定（決定 150–174，產品經理已批准 2026-10-17）

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 150 | （#285）Expression 支援 JSON 字串／數字、`true`／`false`／`null` 字面值同加引號嘅 object key（未支援 array）；三個字係保留字。api `respond value` 會 evaluate，runtime 失敗回 500 `INTERNAL`，唔 fallback；validate／manifest 拒絕靜態可確定失敗嘅 expression。`## Transform` 可省略，等同 `(none)`（in = `{}`、out = `result`），`- in: (none)`／`- out: (none)` 都接受。 | 已批准（產品經理 2026-10-17） | 2.3、2A.2、2A.3 |
| 151 | （#287）LLM 連線錯誤顯示底層原因（地址／範圍被拒、DNS、TLS）同遮咗嘅 URL，永遠唔包含 key、request header 或 response body。`vibe validate --deploy --dev` 同 `vibe manifest --dev` 接受本地開發 private endpoint lock，CI／production 拒絕。開發環境 `vibe serve` 啟動 runtime 前警告並移除 `LLM_*`／`OPENAI_*`（包括 `.env` 值），其他環境照舊拒絕。 | 已批准（產品經理 2026-10-17） | 2A.8.4、2A.8.5、2C.3、10 |
| 152 | （#92）api source frontmatter `auth: none`（同 `removed`／`false`／`off`）一律編譯成 `-auth`，取代繼承或 model auth。固定清單嘅中英文移除 auth 句子當直接否定，唔出歧義 warning；login 要求照樣衝突，雙重否定保留嚴格檢查。 | 已批准（產品經理 2026-10-17） | 2.2、2.1a |
| 153 | （#289）LLM 錯誤只顯示寫死 allowlist 嘅值（已知 model 名、provider 錯誤碼、Responses API incomplete reason），其餘顯示 `unknown`；provider 訊息文字同 response body 永遠唔顯示。 | 已批准（產品經理 2026-10-17） | 2A.8.5 |
| 154 | （#97）移除 `vibe approve` 同 lock 簽名；lock 冇 `status`／`approval-*`／`approved-*`／`signature`，舊欄位出一次 warning 後忽略。`vibe manifest` 冇 `--dev` 就 build release manifest（同 `--release`）；release workflow 用 Sigstore keyless 簽名，非開發環境 `vibe serve` 只接受經 Sigstore 驗證嘅 release manifest。 | 已批准（產品經理 2026-10-17） | 2A.1、2A.5、2C.2、2C.3、2C.3b |
| 155 | （#290／#291）開發環境 `vibe serve --memory` 一定有 slug `dev`（tenant `devtenant`），即使 slug 檔設定咗其他 slug；檔案改動唔使重啟。非 memory 同非開發環境不變。 | 已批准（產品經理 2026-10-17） | 2C.1 |
| 156 | （#291）validate 按位置檢查 expression 變數根：Transform in 准 `req`、`ctx`；api step 同 Transform out 准 `input`、`req`、`ctx` 同之前 method step 嘅 out；method 准 `input`、`ctx` 同之前 step 嘅 out。錯誤報根名、位置同准用根。compile 每個 LLM lock 正規化後跑 validate，失敗經現有 repair loop 回饋 model，共用 `LLM_FORMAT_RETRIES` 上限；最後失敗就停止，成個 project 通過先寫 lock 同 build record。 | 已批准（產品經理 2026-10-17） | 2A.2、2A.8 |
| 157 | （#292）互動 `vibe` 取代 `vibe new`；首次問 LLM 設定，寫 project `.env`（0600），只補缺少嘅行，已有值優先；`.gitignore` 只加一次 `.env`。Endpoint 解析到 private／本地地址先問用戶再加 `LLM_ALLOW_PRIVATE_HOST=true`。產生 source 要簡單小寫 kebab-case 名，hello world 預設 `hello/v1/hello`。 | 已批准（產品經理 2026-10-17） | 2C.1 |
| 158 | （#293）移除冇 key 嘅 api step op `transform`（pre-1.0，冇 deprecation period）；validate 拒絕並提示刪除 step、改用 `## Transform`，見 2A.4 註。 | 已批准（產品經理 2026-10-17） | 2A.4 |
| 159 | （#293）api feature lock 嘅 `## Limits` 同 `## Request` `path.slug` row 由 code 按固定規則正規化，唔由 model 揀，同一 source 編譯出 byte-identical lock。 | 已批准（產品經理 2026-10-17） | 2A.2 |
| 160 | Frontmatter 可以 pin `timeout`、`body` 同 `provider`；timeout／body 按 pin → `_version` Defaults → `_definition` Defaults → `10s`／`1MB`；provider 按 pin → `_definition` Providers default → `main`。validate 檢查 pin 格式；Limits 只嚟自 frontmatter 同繼承預設，prose limits 唔會用，warning 留到 vNext。compile 同互動 prompt 都叫 model 同用戶將 limits 寫喺 frontmatter。 | 已批准（產品經理 2026-10-17） | 2.2、2A.2 |
| 161 | Structured source 用同一套 `path.slug` 同 Limits 規則；structured `## Limits` 有值同解析結果唔同就 compile error，要移去 frontmatter 或刪除。 | 已批准（產品經理 2026-10-17） | 2A.2 |
| 162 | 每次改 compile prompt（system prompt、guide、examples）一定要 bump `PROMPT_VERSION`，佢計入 compiler-hash 同 LLM cache key。 | 已批准（產品經理 2026-10-17） | 2A.6 |
| 163 | Structured source `## Limits` 有值同解析結果唔同就 compile error，錯誤講明欄位（provider／timeout／body）、source 寫嘅值、解析出嚟嘅值同修正方法（喺 frontmatter pin 或者刪除嗰行）。 | 已批准（產品經理 2026-10-17） | 2A.2 |
| 164 | `vibe build` 唔喺 1.0：`vibe manifest`（預設 release；`--dev`、`--release`）係官方 build 指令；`vibe build`（`--labels`、`-o`、部署報告）延至 vNext。 | 已批准（產品經理 2026-10-17） | 2C.1、2C.3 |
| 165 | 同一 source、同一 `PROMPT_VERSION` 同同一結構，一定產生 byte-identical lock；lock 唔可以包含任何每次 run 都可能改變嘅欄位。Translation stamp 改記喺 build record 同 manifest provenance，唔再寫入 lock。 | 已批准（產品經理 2026-10-17） | 2A.1、2.1a |
| 166 | `vibe dev` 唔喺 1.0，延至 vNext；1.0 本地開發流程係互動 `vibe` → `vibe compile` → `vibe manifest --dev` → `vibe serve --memory` → curl。 | 已批准（產品經理 2026-10-17） | 2C.1、2C.2 |
| 167 | `source-lines` 由 compiler 計，model 引用嘅範圍 snap 去所在 markdown block（段落、list、table、fenced code、heading、frontmatter）嘅 union；無效或者超出範圍用 body 全部 block 範圍做 fallback；validate 要求 block 對齊。已知問題：model 引用另一段落時 lock 仍然可能唔同；唔阻 rc。 | 已批准（產品經理 2026-10-17） | 2A.2、2A.4 |
| 168 | Fields 按 header 名（包括中文同必填欄）對應；全形 pipe 同 min/max 長度別名正規化，lock 唔准留別名；所有路徑（包括 method query 同 memory）嘅 SQLSTATE 23505 都回 409 `UNIQUE_VIOLATION`，永遠唔重試，唔影響 40001／40P01 嘅 `TX_CONFLICT`。PROMPT_VERSION 改為 prompt-8。 | 已批准（產品經理 2026-10-17） | 2A.2、12.2、8 |
| 169 | Feature 嘅 writes 由 method steps（CRUD、Signature、SQL 同遞迴呼叫）推導；唔使宣告 writes，false 同寫入 step 衝突就報錯，true 冇寫入 step 要 repair。Method 規則照舊。 | 已批准（產品經理 2026-10-17） | 2.2、2A.4 |
| 170 | 自然語言 POST／PUT／PATCH／DELETE 明確要求新增、更新或刪除記錄，英文動詞嘅常見屈折形式（例如 created／creating、updated／updating、deleted／deleting、listed／listing）都計；必須有對應 model.create／update／delete method step 同 respond；唔准 Steps (none)。排除否定、code、移除 auth 同 writes: false。自然語言 feature 冇 method step 又冇 literal constant response 就報錯；GET 肯定要求讀取時提示單一 model.list／帶 {id} 用 model.get。 | 已批准（產品經理 2026-10-17） | 2A.4、2A.6 |
| 171 | Errors 嘅 code 同 from 只准 runtime 公開錯誤碼、被呼叫 method Signature errors／model CRUD errors，同本 feature validate fail；未知碼列出排序後嘅允許清單並 repair，structured 都適用。CRUD 唯一衝突用 UNIQUE_VIOLATION。 | 已批准（產品經理 2026-10-17） | 2A.4、2A.6、8、12.2 |
| 172 | Transform in 平面 object 按 Request row 次序排 key（同名或 req.field 對應），其餘按 byte 字典序；冒號後一個空格、逗號後一個空格，expression 字串外空白正規化（包括 req.body.x），重複 key 報錯，唔能解析就保留原值。structured 同自然語言都適用；Transform out expression 同 respond value 相同就寫 transform.out。自然語言 model table 若等於 id_snake 或固定英文複數（不分大小寫），一律寫複數，其餘保留。 | 已批准（產品經理 2026-10-17） | 2A.4、2A.6 |
| 173 | 未知 capability model 同 LLM_SEND_SAMPLING=0 嘅 sampling warning 合計每個 process 最多一次，跨 compile、repair 同 keep-going 共用。 | 已批准（產品經理 2026-10-17） | 2A.8.2 |
| 174 | Lock 唔包含任何開發者環境資料；compiler 只留 name、version；LLM stamp 移去 build record 同 manifest provenance，compiler-hash 只計 compiler.version 同 prompt-version，PROMPT_VERSION 升到 prompt-10。決定 174：`scripts/vibe-install.sh` 接受 `vX.Y.Z` 同 `vX.Y.Z-rc.N` pre-release tags；checksum 同 attestation 驗證不變：signer workflow 係 `release.yml`，source ref 係 `refs/tags/v<version>`，issuer 係 `https://token.actions.githubusercontent.com`；決定 118 不變。 | 已批准（產品經理 2026-10-17） | 2A.1、2A.6、2A.8、10.1 |

## 1. 目錄同命名

```text
project/
├── providers/
│   ├── {name}.md
│   └── {name}.lock.md
├── pipelines/
│   ├── {name}.md
│   └── {name}.lock.md
├── migrations/
│   └── {provider}/
│       ├── {seq}-{name}.md
│       └── {seq}-{name}.lock.md
├── models/
│   └── {object}/
│       ├── {object}.md
│       ├── {object}.lock.md
│       └── methods/
│           ├── {method}.md
│           └── {method}.lock.md
└── api/
    └── {definition}/
        ├── _definition.md
        ├── _definition.lock.md
        └── {version}/
            ├── _version.md
            ├── _version.lock.md
            └── {feature}/
                ├── {feature}.md
                ├── {feature}.lock.md
                └── schema.md      # 選填，自然語言欄位補充說明，會併入 feature 嘅 lock
```

- 所有名一律用 kebab-case，只准 `a-z0-9-`。
- Build 時每個 source `.md` 旁邊會產生一個同名嘅 `.lock.md`（`schema.md` 除外，佢併入 `{feature}.lock.md`）。上面目錄樹入面嘅 `*.lock.md` 全部係 build 輸出，已 gitignore，唔 commit（決定 117）。
- migration 檔名 `{seq}-{name}`：`seq` 係 4 位數字（`0001` 起，連續唔可以跳號），例如 `0003-add-user-phone.md`。lock 檔由編譯器產生，唔准手寫或者手改。
- `_` 開頭嘅檔係保留檔，永遠唔會被路由；`_definition.md` 只可以放喺 `{definition}/`，`_version.md` 只可以放喺 `{version}/`。
- version 名必須符合 `^v[1-9][0-9]*$`（例如 v1、v2、v10）。version 只存在於 api 層；models 同 method 唔分 version。
- 預設路由係 `/{definition}/{version}/{feature}`；路由一定要包含 version，冇預設版本，亦唔會自動轉去最新版本。
- 保留路徑：`/_internal/*`、`/health`。

## 2. 編譯流程同 source 寫法

### 2.1 三段流程

1. **Source（`{name}.md`）**：用自然語言寫，講清楚「要做咩」。可以有選填嘅結構化 frontmatter（例如 `method: POST`），但唔係必須。
2. **Lock（`{name}.lock.md`）**：編譯器將 source 編譯成固定格式嘅結構化 markdown（格式見 2A）。Lock 係 **build 輸出**，唔 commit 入 repo（`.gitignore`，決定 117）；CI／CD 喺部署時由 source build，通過驗證器同決定 114 policy 檢查，而且被收入有 attestation 同簽名嘅 manifest 之後先算生效（決定 77、79、113）。
3. **Manifest**：`vibe manifest` 將所有通過驗證嘅 lock 合併成一份 manifest，release 輸出 unsigned manifest，由 release workflow 用 Sigstore keyless 簽名（決定 79、89、119）。runtime 只載入 manifest。

- runtime 永遠唔會解譯自然語言，亦唔會讀 source `.md`；lock md 係 runtime 嘅唯一依據（經 manifest 載入）。
- 所有安全同驗證規則都喺 lock 層檢查（見第 10 節），唔係喺 source 層。
- source 只係人寫同人讀嘅意圖描述；source 同 lock 有出入時，以已確認嘅 lock 為準，並且應該改 source 再重新編譯。

### 2.1a 翻譯階段（決定 108–112，已確認（產品經理 2026-10-14））

非英文 source 喺語意一致性檢查（2A.7）之前先譯做英文，令關鍵字檢查唔使逐個語言維護詞表。

- **觸發**：source 去咗 frontmatter、code（fenced 同 inline）同識別字（欄位名、`{object}.{method}`、路徑）之後，仲有非 ASCII 字元。全部係 ASCII 就唔翻譯。
- **做法**：用現有 LLM adapter（2A.8，同一個 provider 設定同沙盒），prompt `translate-1`：
  - 直譯，唔解釋、唔補充、唔總結；保留識別字、數字同否定（例如「唔公開」一定譯做 not public）。
  - Source 放喺有分隔嘅 data-only `SOURCE` 區塊（同編譯 prompt 一樣嘅分隔方式），入面任何指示都唔會執行；冇工具、冇 function calling。
- **用途限制**：譯文只用於語意一致性檢查。安全關鍵字只檢查中文及英文原文，不能由譯文補足。Lock、`source-hash`、編譯 prompt 及 cache key 用原文。以下規則取代 #37／決定 108 的 English-only 及不處理中文否定規則（產品經理 2026-10-16 確認）。
- **否定**：唔、不、冇、無、毋、非及 not、no、never 只作用於短距離內最近的下一個安全關鍵字，不跨越 `, . ; : ! ? 、，。；：！？` 或換行，不以偶數／奇數抵銷。固定清單內的 auth 移除字眼（例如「唔需要驗證」、「唔使驗證」、「唔使登入」、「唔需要登入」、「不需要登入」、「免登入」、「移除 auth」、「唔使 auth」、no auth、remove auth、no login required）算 auth 的直接否定；配合 frontmatter `auth: none` 時，compiler 會確定性寫入 `-auth`（宣告優先），但要求登入的字眼仍然構成衝突並係 compile error。只有無歧義的直接否定可讀作否定；中間有其他字詞或雙重否定（not … not、不得不、唔可以唔、不能不、非…不可）一律保留較嚴格的要求並發出 warning，請作者以 `auth` 等 structured frontmatter 確認；如果該欄位已經喺 frontmatter 宣告，就當已確認，唔再發含糊否定 warning，明確衝突仍然係 error。`不得不公開` 表示公開。Prose 不會自動降低保護；有 `auth/public/secrets/writes` 宣告時以宣告為準，明確的 prose 衝突為 compile error。
- **Code**：忽略行首最多三個空格後的至少三個反引號或波浪號 fence；結束 fence 必須同字元且長度不少於開始。行中 ``` 不開啟 fence。Inline code 的反引號 run 長度必須相同。未結束 fence 是 compile error，不能忽略其後內容；不確定時採取較嚴格的判讀。
- **失敗**：翻譯 call 失敗、回應格式唔啱、或者譯文缺咗原文嘅識別字／數字，成次編譯失敗（#93 全部或者冇，冇 lock 會寫入）；冇 fallback，唔會略過檢查。
- **Cache**：`.vibe/cache/translate/<sha256>`，key 係 `source-hash` + provider + model snapshot + translate prompt 版本 + 實際送出參數。**高風險 release build 唔可以用翻譯 cache**（決定 110），一定重新翻譯。
- **Call 上限**：翻譯 call 計入 #92 P5 嘅每次編譯 LLM call 上限；超過即編譯失敗（決定 111）。
- **審計**：translation stamp（prompt-version、llm-provider、model-snapshot、hash）按 lock path 記喺 `.vibe/build-record.json` 嘅 `translations` map 同 manifest provenance 嘅 optional `translations` map（決定 109、165）；lock 唔再有。譯文保留喺 `.vibe/audit/translations`，header 帶 hash；stamp 唔計入 `source-hash` 同 `compiler-hash`。
- **高風險宣告**（決定 112）：frontmatter 宣告咗嘅高風險欄位同譯文（同 lock）唔一致，或者偵測到未宣告嘅高風險行為，一律 fail closed，錯誤訊息講明缺咗邊個宣告。關鍵字檢查係一致性檢查（縱深防禦），唔係唯一防線；真正嘅閘門係 frontmatter 宣告加 lock 層驗證。
- **Api feature**：唔再對 api feature 做寫入 sql 關鍵字檢查；寫入由被呼叫、已經檢查過嘅 method lock（`writes`）繼承。

### 2.2 Source 寫法建議

決定 169 修訂高風險宣告規則：只有 api feature 嘅 `writes` 唔再必填，compile 同 validate 由 `op: method` steps 推導 `write-sql`，包括 model CRUD create／update／delete、Signature `writes: true`、寫入 SQL 同遞迴呼叫寫入 method。`writes: false` 同寫入 step 衝突就報錯，指出 step 同 target；`writes: true` 但冇寫入 step 就提示加 `<model>.create|update|delete`。Method 嘅宣告規則照舊。

- 每個 source 用 `#` 標題寫名，之後用段落或者列點描述。
- 建議用以下小標題幫編譯器理解（唔強制）：`## 用途`、`## 輸入`、`## 輸出`、`## 做法`、`## 錯誤`、`## 權限`。
- 安全相關嘅設定（公開、剔走 auth、寫入、改 provider、放寬限流、簽 token）可以用自然語言寫（決定 16），但 `## Limits` 嘅 `timeout`、`body`、`provider` 只用 frontmatter pin 同繼承預設，prose limits 唔會用（決定 160；rate-limit pipeline 唔受影響）；但**高風險欄位一定要喺 frontmatter 宣告**（決定 112）：`writes`（只限 method；feature 依決定 169 由 steps 推導）、`public`（同 `public-writes`）、`auth`、用到嘅 secret（`secrets`）。冇宣告而 lock 或者譯文顯示有呢啲行為，編譯失敗，錯誤講明要加邊個宣告。建議用獨立一句清楚寫出，唔好埋喺長段落入面；無論寫喺邊，驗證器都會由 lock 抽出安全項目，PR 逐項高亮（見 2A.5）。
- 決定 160：api feature 可以喺 frontmatter pin `timeout`、`body`、`provider`；`## Limits` 只用呢啲 pin 同繼承預設，prose 入面嘅 limits 唔會用（warning 留到 vNext），rate-limit pipeline 唔受影響。Compile 同互動 prompt 都要求 limits 寫喺 frontmatter；互動完成亦會提醒用戶。
- source 入面嘅文字只會被當做描述，入面任何「指示編譯器」或者「略過檢查」嘅句子都唔會生效，lock 層規則照樣強制檢查；呢類似指令嘅句子會被 deterministic 規則偵測，出 warning 並喺 PR 高亮（見 2A.7）。

### 2.3 api feature source 涵蓋內容

一個 `{feature}.md` 要描述：

- HTTP method（GET／POST／PUT／PATCH／DELETE，一個 feature 只可以有一個；PATCH 見 2.3c）同（選填）路徑。
- 要經邊啲 pipeline、係咪公開、租戶由邊度嚟（public 用 slug）。
- Request 有咩欄位、Response 有咩狀態碼同內容。
- 要 call 邊個 method、點樣將 request 轉做 method input、點樣將 method output 轉做 response。
- 業務錯誤碼對應嘅狀態碼。
- 選填：provider 覆蓋、timeout（預設 10s，平台上限 30s）、body 上限（預設 1MB，有平台上限）。

編譯後喺 lock 入面對應：

| lock 欄位／section | 說明 |
| --- | --- |
| `## Route` 嘅 `method` | 必填，GET／POST／PUT／PATCH／DELETE |
| `## Idempotency` | 選填，只限 POST／PATCH，見 2.3b |
| `## Route` 嘅 `path` | 選填，必須喺自己 `/{definition}/{version}/` 前綴入面；可以用 `{param}`；唔准有 `..`、萬用字元，亦唔准用保留路徑 |
| `## Pipelines` | 繼承合併之後、按次序執行嘅 pipeline 清單同參數 |
| `## Access` 嘅 `public` | 預設 false |
| `## Access` 嘅 `tenant` | public endpoint 必填：`{ from: path, param: slug }` |
| `## Access` 嘅 `public-writes` | public endpoint call 咗寫入 method 時必填 true（見 7.2） |
| `## Limits` | `provider`、`timeout`、`body` |
| `## Request` | 欄位宣告；未宣告嘅欄位一律拒絕，回 422 |
| `## Response` | 狀態碼同 schema |
| `## Transform` | 可省略（等同 `(none)`）；request → method input、method output → response 嘅映射；處理唔同 api version 嘅差異 |
| `## Steps` | 有序步驟；api 層只准 `method`、`validate`、`respond` 三種 op，**唔准 `query` 或者 `call`** |
| `## Errors` | 業務錯誤碼對應狀態碼 |

### 2.3a 跨來源請求 CORS（決定 103，已確認（產品經理 2026-10-14），issue #84）

`api/{definition}/_definition.md` 可以加：

```md
## Cors

- allow-origins: [https://shop.example.com, http://localhost:*]
- allow-headers: [authorization, content-type, idempotency-key, if-match]
- expose-headers: [etag, location, idempotent-replayed]
- max-age: 600
```

- 冇 `## Cors` 就唔回任何 CORS header（同源先用到）。
- **Origin 比對**：同 `Origin` header 完全相等（scheme、host、port 都要一樣，唔分大小寫只限 host）。
  - `*`：只准單獨使用，而且只限冇 credentials 嘅情況；release build 出 warning。
  - Port 萬用（`http://localhost:*`、`http://127.0.0.1:*`）：只准 `localhost`／`127.0.0.1`，只喺 dev build（`vibe manifest --dev`）生效；release build 遇到就 error。其他萬用（例如 `https://*.example.com`）一律唔准。
- **Credentials**：`## Cors` 唔可以開 credentials（驗證器 error）；access token 用 `Authorization` header，唔使 cookie。Credentials 只限決定 101 嘅 refresh／logout endpoint，由 `token-refresh` 嘅 `allowed-origins` 控制，而嗰個列表一定要係 `allow-origins` 嘅子集（唔准 `*`）。
- **回應**：origin 喺白名單就回 `Access-Control-Allow-Origin: <origin>`（`*` 模式回 `*`），同 `Access-Control-Expose-Headers`；所有回應（包括 origin 唔喺白名單）都帶 `Vary: Origin`。
- **Preflight**：runtime 處理所有 `OPTIONS` 請求，唔經 pipeline、唔計限流以外嘅配額：
  - Origin 喺白名單、`Access-Control-Request-Method` 係呢條 path 有嘅 method、`Access-Control-Request-Headers` 全部喺 `allow-headers`：回 204，帶 `Access-Control-Allow-Origin`、`-Allow-Methods`（只列呢條 path 有嘅 method）、`-Allow-Headers`、`-Max-Age`。
  - 否則回 204 但唔帶任何 `Access-Control-Allow-*` header（瀏覽器會擋），記 log。
- `max-age`：預設 600 秒，上限 7200。
- `allow-headers` 只可以包括平台認識嘅 header（`authorization`、`content-type`、`idempotency-key`、`if-match`、`x-vibe-csrf`）同 `## Request` 宣告咗嘅 header。

### 2.3b `Idempotency-Key`（決定 104，已確認（產品經理 2026-10-14），issue #85）

POST 或者 PATCH 嘅 api lock 可以加：

```md
## Idempotency

- header: Idempotency-Key
- required: false
- ttl: 24h
- lease: 30s
```

- 範圍：key 按 `(tenant, subject, route, key)` 分開；`subject` 係已認證用戶（或者 API key）嘅 id。**Public endpoint 唔准開**（冇 subject，唔同人用同一個 key 會攞到人哋嘅回應）。
- Key：1–255 個可見 ASCII 字元；DB 只存 `HMAC-SHA-256(pepper, key)`。`required: true` 而冇帶 header 回 400 `IDEMPOTENCY_KEY_REQUIRED`。
- Request hash：`SHA-256`（method、path、正規化之後嘅 JSON body）。
- 流程（全部喺決定 99 嘅 request transaction 入面）：
  1. 攞 key 嘅「處理中」租約（lease）：
     - `lease` 預設 30s，一定要大過決定 100 嘅 transaction 上限 10s（驗證器檢查）。
     - PostgreSQL 實作用 `pg_try_advisory_xact_lock`：租約綁住 transaction，commit、rollback、連線斷或者 process crash 都會即刻釋放，所以實際持有時間唔會超過 transaction 上限。冇 transaction 級 lock 嘅儲存（例如 in-memory 執行器或者之後嘅外部 store）用 `lease_expires_at` 欄位，處理期間每 10s heartbeat 延長。
     - 租約仲有效（另一個同 key 請求處理緊）即回 409 `IDEMPOTENCY_IN_PROGRESS`，帶 `Retry-After: 1`；**只有租約有效時先回 409**。
     - 租約過期（例如 crash、冇 heartbeat）：因為成個 request 喺同一個 transaction，冇任何嘢 commit 過，所以當「未開始」，重試可以接手重新執行。
  2. 搵未過期嘅紀錄：有而且 request hash 一樣，就重播儲存咗嘅狀態碼、body 同 `ETag`／`Location`，加 header `Idempotent-Replayed: true`，唔執行 steps；hash 唔一樣回 422 `IDEMPOTENCY_KEY_REUSED`。
  3. 冇紀錄就照常執行；成功（2xx）先喺同一個 transaction 寫入紀錄（request hash、狀態碼、body、`expires_at`），同業務寫入一齊 commit。
  4. 失敗回應（4xx／5xx、`TX_TIMEOUT`、`TX_CONFLICT`）會 rollback，唔儲存；用同一個 key 重試會重新執行。
- 儲存：平台表 `_platform_idempotency`（每個租戶 schema 一張，有 RLS），由平台 role `idempotency_store`（SELECT、INSERT、UPDATE）寫；body 用租戶資料 key 加密（同 12.10 crypto-shredding）。
- `ttl` 預設 24h，上限 7d；過期紀錄當唔存在，同一個 key 可以再用。`_platform_idempotency` 係平台系統表，由 runtime 按 TTL 清理過期紀錄（平台 role，唔經用戶 lock）；決定 13 只管用戶 API 同用戶資料，唔適用於平台系統表。
- 平台照樣唔會自動重試（決定 100）；`Idempotency-Key` 係畀 client 安全重試用。

### 2.3c PATCH（決定 105，已確認（產品經理 2026-10-14），issue #86）

- 語意：JSON Merge Patch（RFC 7396）。接受 `Content-Type: application/merge-patch+json` 同 `application/json`。
  - 只可以改 `## Request` 宣告咗嘅欄位；未宣告欄位照樣 422。
  - 冇出現嘅欄位維持原值。
  - `null` 代表清除，只限 nullable 欄位；非 nullable 欄位收到 `null` 回 422。
  - 唔可以改嘅欄位（系統欄位 `id`、`tenant_id`、`created_at`、`updated_at` 等，同埋 lock 標 `immutable` 嘅欄位）出現就回 422。
  - `json` 類型欄位整個取代，唔做深層合併。
  - 空 body `{}` 回 422 `EMPTY_PATCH`。
- `If-Match`／`ETag`：內置 CRUD `update` 照樣一定要（決定 48）；自訂 PATCH feature 可以喺 lock 揀 `if-match: required | optional`，預設 `required`。
- 內置 CRUD：`update` `partial: true`（預設）對應 PATCH；`partial: false` 對應 PUT（整個取代，所有可寫欄位都要有）。
- PATCH 可以配合 `## Idempotency`（2.3b）。

## 2A. Lock markdown 格式

### 2A.1 通用 frontmatter

每個 lock 一律用以下 frontmatter，欄位次序固定：

```markdown
---
lock-version: 1
kind: method                       # provider | pipeline | model | method | api | migration
id: user/methods/login             # 由路徑決定，唔准自訂
source: models/user/methods/login.md
source-hash: sha256:<source 正規化內容嘅 hash>
source-type: natural               # natural | structured
dependencies:                      # 所有依賴 source／lock 嘅 hash，按 id 排序
  - { id: user, kind: model, hash: sha256:<...> }
  - { id: main, kind: provider, hash: sha256:<...> }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:<見下面標準定義>
build-mode: release                # release | dev；dev lock 唔可以部署（決定 75）
security-items:                    # 驗證器 deterministic 抽出，資訊性（決定 78）
  - { item: public, risk: high, reason: "公開登入入口" }
---
```

- `source-hash`：source 先正規化（統一換行、去行尾空白、去 BOM、合併多餘空行），再計 sha256。淨係改排版而正規化內容唔變，hash 就唔變，**唔會重新編譯**。
- `dependencies`：lock 用到嘅所有其他定義（例如 method 依賴 model 同 provider、api 依賴 method、`_definition`、`_version` 同 pipeline）。任何一個依賴嘅 hash 變咗，lock 都當過時，要重新編譯。
- `source-type: structured` 只限 source 完全由結構化 frontmatter 同結構化列點組成、冇自由文字；判斷由平台嘅 deterministic parser 做，唔由 LLM 編譯器判斷（決定 20）。
- `compiler-hash`（標準定義，決定 174）：`sha256` of canonical JSON（key 排序、UTF-8、冇空白），只包含 `compiler.version` 同 `prompt-version`。唔受任何 LLM 設定影響。
- 決定 174：`compiler` 只寫 `name` 同 `version`。連 model id 都唔留喺 lock，因為 `LLM_MODEL` 係每位開發者嘅設定，同一 source 唔應該因為機器設定唔同而產生唔同 lock。Model、provider、endpoint、seed 同實際送出參數嘅重現證據，按 lock path 記喺 `.vibe/build-record.json` optional `compilers` map 同 manifest provenance 同名 map，欄位係 `llm-provider`、`model-snapshot`、`endpoint-host`、`endpoint-private`、`llm-seed`、`sent-params`；deterministic lock 冇 entry。唔記 key、URL path、username 或其他環境值。舊 compiler 環境欄位仍然 parse 得，但忽略、唔再 render；validate 每個 lock 出一次 decision 174 warning，要求執行 `vibe compile`。`PROMPT_VERSION` 係 `prompt-10`。
- 決定 165：同一 source、同一 `PROMPT_VERSION` 同同一結構，一定產生 byte-identical lock；lock 唔可以包含任何每次 run 都可能改變嘅欄位。`translation` stamp 只記喺 build record 同 manifest provenance，唔寫入 lock。舊 lock 嘅 `translation:` 仍然讀得，parse 後忽略，render 唔再寫；validate 每個舊 lock 警告一次："lock contains a `translation` field, which is no longer written (decision 165); run `vibe compile` to refresh it"。Lock format 改動會將 `PROMPT_VERSION` 升到 `prompt-6`，令舊 lock 過時並重新編譯。
- manifest JSON 用 snake_case：lock 嘅 `build-mode` 對應 manifest 嘅 `build_mode`（`dev`／`release`；冇呢個欄位當 `release`）。
- `build-mode`：`vibe manifest` 寫 `release`，`vibe manifest --dev` 寫 `dev`；`dev` lock 永遠唔可以入 release manifest（決定 75）。
- `security-items`：驗證器 deterministic 抽出嘅安全項目清單，`reason` 由驗證器按規則產生；資訊性，用嚟 PR 高亮同部署報告，唔需要確認人（決定 78）；冇安全項目就寫 `[]`。
- v1.8 起 lock 冇 `signature`，亦冇 `status`／`approval-*`／`approved-*`（決定 77、79）；SDK 讀到舊欄位會忽略並出 warning。

### 2A.2 通用 section 規則

決定 159、160、161、163：自然語言同 structured api feature 嘅以下 optional 部分由 code 用固定決定性規則正規化，永遠唔由 model 揀；忽略 model 喺呢啲部分嘅輸出，確保同一 source 編譯出 byte-identical lock。Structured source 用同一套規則；來源 `## Limits` 缺省、`(none)` 或者同解析值相同都接受；有值唔同就 compile error：`` `## Limits` <key>: source value `<value>` differs from the resolved value `<resolved>`; pin it in frontmatter (`<key>: <value>`) or delete the `<key>` line from `## Limits` ``。自然語言 model 輸出仍然靜默忽略。

- `## Limits` 一律丟棄 model 原有內容，固定依次寫三個 bullet：`provider` 用 source frontmatter 指定值，否則用 `_definition` 嘅 `## Providers default`，再否則 `main`；`timeout` 同 `body` 依次用 source frontmatter pin、`_version` 嘅 `## Defaults`、`_definition` 嘅 `## Defaults`，再否則分別用 `10s` 同 `1MB`。
- 決定 160：compile、validate 同互動生成一律檢查 pin；validate 亦檢查 lock Limits。`timeout` 要嚴格 duration（例如 `500ms`、`10s`），大過 0 而唔超過 30s；`body` 要嚴格 size（例如 `64kb`、`4KB`、`1MB`），大過 0 而唔超過 1MB；`provider` 要小寫 kebab-case。錯誤講明 key、壞值同有效例子。
- 正規化後嘅 Route path 有 `{slug}` segment，就先移除 Request 所有 model 寫嘅 `path.slug` row，再喺第一個 data row 加且只加 `| path.slug | string | - | yes |`；其他 row 保留次序。Request 係 `(none)` 或冇 table 時，建立 header `| field | type | rules | required |` 嘅 table。Route 冇 `{slug}` segment 就移除所有 `path.slug` row；移除後冇 row 就寫 `(none)`。

- B3（決定 165）：自然語言同 structured feature 嘅 `## Errors` 缺省、空白、只有 table header 同 separator 冇 data row、或者 `(none)`，一律正規化成 `(none)`；有真正 error row 就保留。冇自訂 error 唔發「feature has no Errors section」warning；手寫 lock 真係冇 Errors section，就由固定 section 結構檢查報 error，唔再加重複 warning。
- B4（決定 167）：自然語言同 structured source 嘅所有 section block（包括 steps 同 `-auth` 等 pipeline）嘅 `source-lines` 都由 compiler 喺 validate／repair 之前按下面規則 snap；缺少引用用固定 fallback，唔發 warning，亦唔係 compile error。

- section 用 `##` 標題，名稱同次序按 kind 固定（見 2A.3）；唔准有額外 section，缺 section 用 `(none)` 表示；api feature 可以省略 `## Transform`，等同 `(none)`，compile 會喺 lock 嘅固定位置補返。
- 每個 step 用 `### s{n} {op}` 做標題，下面用 `- key: value` 列點，key 次序固定：`op`、（op 專用參數）、`out`、`source-lines`。
- 變數參照一律用 `input.x`、`ctx.x`、`req.x`、或者之前 step 嘅 `out` 名；唔准參照未定義變數。Transform `in` 只可以用 `req`、`ctx`；step 同 Transform `out` 可以用 `input`、`req`、`ctx` 同之前 step 嘅 `out`；method 用 `input`、`ctx` 同之前 step 嘅 `out`。
- sql 一律寫喺 inline code 入面，用 `$1`、`$2` 參數，`params` 列出對應來源；唔准字串拼接。
- `source-lines` 係 compiler 計出嘅 enclosing markdown block 範圍，方便 review 對照；model 引用只係 hint。行數由 1 開始，以 compile prompt 送出嘅完整 `source.combined_text()` 為準（prompt 本身唔顯示行號），包括 frontmatter 同附加 schema 部分；自然語言、structured source 同 validate 都用同一份文字。
- Block 定義（決定 167）：第 1 行係 `---` 時，由第 1 行到 closing `---`（包括兩端）係一個 frontmatter block。其餘文字逐行分 block：trim start 後以至少三個連續 backtick 或 `~` 開頭就開 fenced code block，去到下一行 trim start 後以同一字元、至少同樣長度 marker 開頭嘅行為止（包括兩條 fence 同中間空行）；冇 closing fence 就去到文字結尾。Fence 即使緊貼其他非空行都獨立成 block。Fence／frontmatter 外，符合 `^#{1,6}(\s|$)` 嘅 ATX heading 每行獨立成 block。其餘連續非空行嘅最大範圍係一個 block，涵蓋段落、list 同 table；空行分隔 block，所以 loose list 項目之間有空行就係幾個 block。`<!-- schema.md -->` 等 HTML comment 當普通行。空行本身唔屬任何 block。
- Snap：引用接受 ASCII 數字 `N` 或 `A-B`（`-` 兩邊可有空格），要求 `1 <= A <= B`。結果由第一個同引用相交嘅 block 開始，到最後一個相交 block 結尾，包含中間空行。只引用空行就揀最近嘅下一個 block；後面冇 block 就揀前一個。開始仍然喺文字內、結尾超出最後一行，就先 clamp 結尾再 snap。缺少、無法解析、倒轉、0、整段超出最後一行，或者文字冇 body block，都用固定 fallback：由 body 第一個到最後一個 block（排除 frontmatter）；body 冇 block 就用 frontmatter；完全冇 block 就用 `1-1`。結果永遠寫 `A-B`，單行亦一樣；只係 provenance metadata，唔發 warning，亦唔係 compile error。
- `vibe validate` 對有 source 文字嘅 lock，逐個 `source-lines` 檢查值係咪等於同一 snap function 嘅結果，即必須已經 block 對齊同用 canonical `A-B`；唔一致就報 error，列明 lock、section／step、原值同預期值，提示行 `vibe compile`。

- expression 支援字面值：JSON 字串、數字、`true`／`false`／`null`；object key 可以加雙引號；暫時唔支援 array。
- `true`／`false`／`null` 係保留字，唔可以做變數名。object 入面明確寫嘅 `null` 會保留；path 解析成 `null` 嘅欄位會省略。
- api feature 嘅 `respond value` 留空或者係 `transform.out` 時，會 evaluate Transform out；其他值當 expression evaluate。runtime evaluation 失敗會回 `500 INTERNAL`，唔會 fallback；validate 會拒絕靜態可確定會失敗嘅 expression。
- 缺少 Transform、整個 section 係 `(none)`、或者 `- in: (none)`／`- out: (none)`，對應預設係 in = `{}`、out = `result`；空白 bullet（例如 `- in:`）會報 `empty expression`，validate 同 manifest 都會拒絕。

- B5（決定 168）：Fields canonical header 係 `| name | type | rules | system |`，按 trim、唔分英文大小寫嘅 header 對應，唔按位置。name 接受 name／欄位／欄位名／名稱／字段；type 接受 type／類型／型別／类型；rules 接受 rules／規則／约束／約束／规则；system 接受 system／系統／系统；required 接受 required／必填／必須／必需。name 同 type 必須有；system 缺省係 no。未知或者重複 header 報 error。required 真值 yes／true／y／是／必填／✓／✔／required 加現有 `required` rule（唔重複）；假值 no／false／n／否／唔係／-／空白唔加，其他值報 error。原本 canonical 表唔改。
- B6（決定 168）：自然語言模型輸出同 structured source 解析表之前，只喺 trim 後以 `|` 或者 `｜` 開頭嘅行將全形 `｜` 轉 ASCII `|`，包括 cell 入面嘅 literal `｜`。同一行逐字替換，唔改行號；source-hash 仍然 hash 原文。
- B7（決定 168）：Fields 同 API Request rules 嘅 max_length／maxLength／max-length／max_len／maxlen／max_chars 正規化做 `max:`；對應 min_length／minLength／min-length／min_len／minlen／min_chars 做 `min:`。唔包括 max_size／min_size。別名同 canonical 同值就去重，唔同值報 compile error；validate 拒絕 lock 留低任何呢啲別名，指出別名同替代 key。以上規則兩種 source 都適用。

### 2A.3 各 kind 嘅固定 section

| kind | section（按次序） |
| --- | --- |
| provider | `## Connection`、`## Isolation`、`## Access`、`## Limits`、`## Migrations` |
| pipeline | `## Parameters`、`## Order`、`## Steps` |
| model | `## Table`、`## Fields`、`## Constraints`（可省略）、`## CRUD`、`## Policies` |
| method | `## Signature`、`## Calls`、`## Steps` |
| migration | `## Target`、`## Up`、`## Verify`、`## Data` |
| api（scope: definition） | `## Providers`、`## Pipelines`、`## Defaults` |
| api（scope: version） | `## Pipelines`、`## Defaults`、`## Lifecycle` |
| api（scope: feature） | `## Route`、`## Access`、`## Pipelines`、`## Limits`、`## Request`、`## Response`、`## Transform`（可省略，等同 `(none)`）、`## Steps`、`## Errors` |

api 類 lock 嘅 frontmatter 要多一個 `scope: definition | version | feature`。

### 2A.4 op 白名單

決定 170：自然語言 feature 嘅 Route 係 POST／PUT／PATCH／DELETE，而且原文（唔包括 code，尊重否定）肯定要求 create／add／insert／update／edit／modify／delete／remove，或新增／建立／創建／创建／加入／更新／修改／編輯／刪除／删除／移除，就必須有寫入 method step。移除 auth 字眼唔計；GET、`writes: false` 同 structured source 唔用呢個 prose 規則。`writes: true` 就一定要有寫入 step。Canonical create steps 係 `### s1`、`op: method`、`target: <model>.create`、`args: transform.in`、`out: result`，之後有 respond；唔准 `Steps (none)`。

決定 170 讀取擴充：只對自然語言 feature，冇 `op: method`，又冇 respond value 係 literal JSON／string／number／bool constant，而且 Transform `out` 缺少、`(none)` 或唔係 literal constant，就喺 compile repair loop 報錯：`the feature has no method step and no constant response, so it always returns null; add` `op: method` `with target <model>.list (or get/create/update/delete) and a respond step`。Hello world 等 constant response 照常接受。原文肯定要求 list／get／show／fetch／read／query／search／列出／列表／查詢／查询／查看／顯示／显示／攞／讀取／搜尋，而且 Route 係 GET，提示 `<model>.list`；path 有 `{id}` 就提示 `<model>.get`。只有一個 model 就用佢個名；尊重否定同 code。Canonical list steps 係 `op: method`、`target: <model>.list`（或 get）、`args: transform.in`（冇 input 時 Transform `in: {}`，亦可以 explicit args）、`out: result`，跟住 respond `value: transform.out`。

決定 171：Errors table 嘅 `code` 同 `from` 只准 runtime 公開碼（`runtime::RUNTIME_ERROR_CODES`）、本 feature 呼叫嘅 method Signature errors／model CRUD errors，同本 feature validate steps 嘅 `fail:` 碼。未知碼會報錯，列出排序後嘅允許清單；structured 都適用。Callee 未編譯就只用已知碼，唔額外因為 callee 未知而 fail closed；target 解決檢查照舊。冇自訂 mapping 用 `(none)`；CRUD 唯一衝突用 `UNIQUE_VIOLATION`。

決定 172：Transform `in` 平面 object 按 Request row 次序排 key（同名 `body.k`／`query.k`／`path.k`／`header.k`，或者 expression 剛好係 `req.<field>`）；其餘 key 按 byte 字典序。輸出 `{k: expr, k2: expr2}`，冒號後一個空格、逗號後一個空格；expression 字串外空白正規化，包括 `req.body.x`。空 object 係 `{}`，重複 key 報錯；唔能解析就保留 trimmed 原值。自然語言同 structured 都適用，唔改語意。Transform `out` 本身亦用 canonical_expression 正規化；respond step `value` 同 Transform `out` 經 canonical_expression 後相等，就一律寫 `transform.out`，自然語言同 structured 都適用。

決定 172 model table 擴充：只對自然語言 model，`id_snake` 係 model id 將 `-` 換成 `_`。固定英文 plural 規則：已經以 `s` 結尾就保留（視為複數）；否則以 `x`、`z`、`ch`、`sh` 結尾加 `es`；consonant + `y` 換成 `ies`；其餘加 `s`。Model 答案嘅 `Table` `table` 若同 `id_snake` 或 plural(id_snake) 不分大小寫相等，compiler 一律寫 plural(id_snake)；其他 table 名保留（視為 source 指定）。Structured source 唔改 table，包括現有 `email_outbox` 範例。

決定 158：冇 key 嘅 `transform` step op 已移除；而家仲係 pre-1.0，唔設 deprecation period。validate 會拒絕佢，提示刪除呢個 step，改用 `## Transform`（`- in:`／`- out:`）寫 request／response 映射。

| 層 | 准用 op |
| --- | --- |
| api feature | `method`、`validate`、`respond` |
| method | `query`、`validate`、`call`、`method`、`respond`、`verify-password`、`hash-password`、`random-token`、`hash-token`、`sign-jwt`（`random-token` 同 `hash-token` 規則見決定 52，已確認；`sign-jwt` 只准喺 `## Signature` 標咗 `capabilities: [sign-token]` 嘅 method 入面用，唔靠 method 名推斷） |
| pipeline | `verify-jwt`、`verify-api-key`、`rate-limit-check`、`audit`、`validate`、`inject-ctx`、`method`；平台內置 pipeline 專用：`refresh-rotate`、`refresh-revoke`（用戶自訂 pipeline 唔准用） |
| migration | `ddl`、`backfill`、`verify`（只喺 migration lock，見 2B） |

- `method` op 係呼叫 `{object}.{method}`，部署時靜態綁定。
- `fail: CODE` 係 `validate` 嘅參數，CODE 一定要喺有宣告嘅錯誤碼入面。
- `DELETE` sql 喺任何 lock 都唔准出現（決定 13，所有刪除都係軟刪除）。
- 所有 DDL（`CREATE`、`ALTER`、`DROP`、`GRANT`、`REVOKE`、`COMMENT` 等）同 `TRUNCATE` 喺 migration 以外嘅任何 lock 都唔准出現；schema 改動只可以經 `kind: migration` lock（決定 27、51，見 2B）。`TRUNCATE`、`GRANT`、`REVOKE` 連 migration 都唔准。
- `query` 只准單一 statement，唔准 `;` 串多句、唔准 `SET`／`RESET`／`COPY`／`DO`／function 定義。驗證器同 runtime 都用 PostgreSQL 解析器檢查（決定 98，見 7.1a），唔靠字串比對。
- `UPDATE` 一定要有 `WHERE`，而且要用主鍵（或者唯一鍵）限定，或者明確依賴 RLS 範圍（lock 寫 `scope: pk` 或者 `scope: rls`）；冇 WHERE 嘅 UPDATE 一律 error。

### 2A.5 安全項目同 review（v1.8 改寫，決定 77、78，已確認（產品經理 2026-10-14））

- SDK 唔再有確認步驟：冇 `vibe approve`、冇 `pending`／`approved` 狀態、冇 commit hash 綁定、冇確認人資格檢查。CI 由 source build、通過驗證器（2A.7、第 10 節）同決定 114 policy、artifact 有決定 113 attestation，就可以部署（決定 117）。
- 「係咪 structured」照樣由平台 deterministic parser 判斷（決定 20），只決定編譯使唔使 call LLM。
- Lock 唔 commit（決定 117）；PR review 嘅對象係 source，尤其係 frontmatter 嘅高風險宣告（決定 112）；人手 review 由用戶喺 Git 平台設定 rulesets（分支規則）（見 9C 同 [`pr-review-guide.md`](pr-review-guide.md)）。

**安全項目**（驗證器由 lock deterministic 抽出，唔靠 source 文字或者 LLM）：

| 項目 | 風險級別 |
| --- | --- |
| `public` 變 true、剔走 `auth` | 高 |
| `public-writes: true`、public 可寫欄位改動 | 高 |
| `capabilities: [sign-token]`、claim 白名單、token ttl | 高 |
| `roles` 改動、放寬或者移除權限檢查 | 高 |
| 新增或者改 provider、shared provider 名單 | 高 |
| 新增 `call` 對外目標 | 高 |
| `capabilities: [confirm-verification]`、`verification-write: allow` | 高 |
| `capabilities: [set-password]`、`password-write: allow` | 高 |
| 改 JWT `iss`／`aud`／`jwks-uri`、API key scope | 高 |
| 任何 migration lock（`migration-ddl`），`destructive: true` | 高 |
| 新增寫入 sql、改寫入欄位 | 中 |
| 放寬 rate-limit／timeout／body／配額 | 中 |
| output 新增欄位 | 中 |
| source 出現似指令句子（見 2A.7） | 中 |

- 安全項目係資訊性（決定 78）：驗證器寫入 lock `security-items`、`vibe manifest` 輸出、部署報告；vNext `vibe build --labels`（決定 164）計劃輸出建議 PR label（例如 `security:high`），俾 rulesets（分支規則） 用（見 [`pr-review-guide.md`](pr-review-guide.md)）。
- 唔再要求逐項確認人、原因簽核、兩個非作者確認，亦冇每個 PR 上限（決定 19、34 由決定 78 修訂）。
- PR 同部署報告顯示**有效權限摘要**：每個 endpoint 最終係咪公開、用咩 role、可以讀寫邊啲表同欄位、會唔會簽 token、有冇對外 call、限流同配額。
- Lock 過時（source 或者依賴 hash 唔一致）或者驗證唔通過，就唔可以 build release manifest。

### 2A.6 重新編譯規則

決定 169–172、174：compile prompt 版本係 `prompt-10`；prompt 提供 canonical create feature 同 runtime 允許錯誤碼清單。Repair loop 喺 model 仲答緊時檢查 writes 宣告、缺少寫入／讀取 step、冇 constant response 同未知 Errors 碼，提示加返 method step 或改用已知碼。

- 決定 162：每次改 compile prompt 文字（system prompt、prompt guide、examples）一定要 bump `PROMPT_VERSION`；佢計入 compiler-hash 同 LLM cache key。今次係 `prompt-5`，舊 lock 要重新編譯；hash 測試記錄版本同所有 kind／scope 嘅 prompt SHA-256，改 prompt 後要一齊更新。

- 只有 `source-hash` 或者任何 `dependencies` hash 變咗先會重新編譯；source 改咗但正規化 hash 唔變就唔會重新編譯。
- 編譯用嘅 model 一定 pin 死 snapshot（唔准用會浮動嘅別名）；`compiler-hash` 按 2A.1 標準定義計，寫入每個 lock（決定 18、64）。
- 編譯器版本升級（包括 major、minor、patch）、或者 prompt 版本改咗（決定 174：compiler-hash 只計編譯器版本同 prompt 版本，model snapshot 改動唔會令 lock 過時）：所有 lock 強制重新編譯，即使 source hash 冇變；release job 由 source build、通過決定 114 policy 同 attestation 先可以部署（決定 77、113、117）。建議用獨立 PR 做升級；release summary 會列出同上一個已部署版本相比嘅高風險變化（決定 117）。
- 編譯器 minor／patch 升級（決定 174）：版本改動會改變 compiler-hash，舊 lock 要重新編譯；平台仍然會用 golden corpus 對比新舊輸出，**只要任何輸出唔同，該升級就當 major 處理**。
- Golden corpus 由平台編譯器團隊維護，最少 500 個案例，覆蓋所有 kind 同所有安全項目；項目可以喺 `.golden/` 加自己嘅案例，一樣會參與比對（決定 35）。
- Model snapshot 淘汰（決定 35）：平台承諾宣布淘汰後最少保留 12 個月；項目有 90 日遷移窗口，窗口內舊 lock 照常有效、照常可以部署；窗口完結前未遷移，部署會被封鎖（runtime 已部署嘅 manifest 照常運作）。
- 編譯一定要盡量 deterministic：固定 prompt、固定 snapshot；`temperature`、`top_p`、`seed` 等參數只喺 model capability 支援時先送固定值（決定 58，見 2A.8.6），唔支援就唔送，唔可以強送。決定性只係盡力而為，只用嚟減少 release summary 入面嘅無謂變化；唔做重編比對，部署安全靠決定 113 attestation 同決定 114 policy（決定 117）。
- 翻譯（2.1a，決定 108–111）：譯文同 build record／manifest provenance 嘅 translation stamp 唔計入 `source-hash` 同 `compiler-hash`，所以翻譯 prompt 或者翻譯 model 改咗唔會令 lock 過時；但編譯（包括 `vibe manifest` 同 CI 重編）一定會用當前設定重新做翻譯同一致性檢查，失敗即封鎖。
- **決定 113、117 修訂**：lock 係 build 輸出，唔 commit，所以冇「同 repo lock 比對」呢回事；`--check-determinism` 同重編比對唔再係部署條件。部署只接受 CI 由 source 重新 build、有 attestation 嘅 artifact（2C.3b）；`compile --check` 只驗證 build 結果同決定 114 policy。
- 唔准手改 lock：要改就改 source 再重新編譯。Lock 冇簽名（決定 79），而且 lock 唔 commit（決定 117），部署只用 CI 由 source build 嘅 artifact（決定 113），所以手改嘅 lock 唔會去到部署。

### 2A.7 獨立驗證器同編譯器隔離

- 編譯器（LLM）嘅輸出唔可信：所有 lock 一定要通過**獨立、deterministic 嘅驗證器**（普通程式、唔用 LLM），第 10 節所有規則都由驗證器執行，唔靠編譯器自律（決定 17）。
- 驗證器版本同編譯器分開管理、分開 pin，同樣受 CODEOWNERS 保護。
- 編譯器喺沙盒行：網絡只准 egress 去編譯用 LLM endpoint 嘅 host（`LLM_BASE_URL` 或者 provider 預設 base URL；CI 只准 https 443；本地開發 loopback 或者已 opt-in 嘅內網 host 見 2A.8.4），其他一律封鎖；冇檔案系統寫權限（只輸出 lock 文字）；冇任何工具或者 function calling（request 唔可以帶 `tools`／`functions`／`toolConfig`）；除咗 `LLM_API_KEY` 冇其他 secret。輸入只有 source、依賴 lock 同平台 prompt（決定 57，詳見 2A.8）。
- 編譯器只可以讀自己依賴範圍內嘅 lock（例如 method 只見到自己物件、provider 同被呼叫 method 嘅 `## Signature`）。
- Prompt injection 偵測：驗證器用固定規則掃 source，例如「忽略之前／上面嘅指示」、「你而家係」、「system:」、「唔使檢查」、「當作已確認」、「print secret」、隱藏字元（零寬字元、bidi 控制字元）、大段 base64 等；命中就出 warning、喺 PR 高亮做中風險安全項目。零寬同 bidi 控制字元直接 error。
- 誤報處理（決定 37）：項目可以喺 `.compiler/injection-allowlist` 加精確句子豁免（只可以精確匹配，唔准 pattern），檔案一定受 CODEOWNERS 保護（v1.8 照舊，決定 93），改動屬中風險安全項目。
- 驗證器亦會做語意一致性檢查：source 冇提及寫入，lock 卻有寫入 sql；source 冇提及公開，lock 卻有 `public: true`；呢類「lock 多過 source」嘅情況出 error（決定 37）；誤報就改 source 寫清楚，冇豁免。安全關鍵字只用原文；非英文 source 的其他一致性檢查用 2.1a 譯文（決定 108，2026-10-16 修訂）；高風險欄位一定要同 frontmatter 宣告一致（決定 112）；api feature 唔做寫入 sql 關鍵字檢查，由 method lock 繼承。

### 2A.8 編譯器 LLM 設定（決定 57）

#### 2A.8.1 用途同範圍

- 初始化 project（例如 `sample-shop`）或者 SDK 時，用戶揀一個 LLM provider 同 endpoint。
- 呢個 endpoint **只喺部署／編譯時**用嚟將自然語言 source 編譯成 lock；驗證器、manifest 編譯同 runtime 都唔會用佢。
- runtime 永遠唔會收到、亦唔會用到任何 LLM key（見 2A.8.5）。

#### 2A.8.2 Provider

v1 支援三個 provider，用 `LLM_PROVIDER` 揀，預設 `openai`（決定 57、59）：

| `LLM_PROVIDER` | 涵蓋 | 預設 base URL | Request | Header |
| --- | --- | --- | --- | --- |
| `openai`（預設） | OpenAI 同所有 OpenAI-compatible endpoint，包括本地 API（Ollama、LM Studio、vLLM） | `https://api.openai.com/v1` | `POST {base}/chat/completions` | `Authorization: Bearer {LLM_API_KEY}`、`Content-Type: application/json` |
| `anthropic` | Anthropic Messages API | `https://api.anthropic.com` | `POST {base}/v1/messages` | `x-api-key: {LLM_API_KEY}`、`anthropic-version: 2023-06-01`、`Content-Type: application/json` |
| `gemini` | Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` | `POST {base}/models/{LLM_MODEL}:generateContent` | `x-goog-api-key: {LLM_API_KEY}`、`Content-Type: application/json`（key 唔放 query string，避免入 log） |

- `{base}` 係 `LLM_BASE_URL`（冇設就用預設），去尾 `/`。`openai` 嘅 base 包含 `/v1`；`anthropic` 嘅 base 唔包含，path 自己帶 `/v1`；`gemini` 嘅 base 包含 `/v1beta`。
- `openai` 用 Chat Completions，因為所有 OpenAI-compatible 本地 API 都支援；Responses API 唔喺 v1 範圍（見第 14 節）。
- 決定性參數唔再按 provider 寫死，改由 adapter 按 model capability 決定（決定 58，見 2A.8.6）。已知限制（2026-10 查證）：
  - Anthropic：較新 model（Opus 4.6 之後）只接受 `temperature: 1`、拒絕 `top_k`、`top_p` 只接受 ≥ 0.99；就算 `temperature: 0` 都唔保證決定性；冇 seed。
  - OpenAI（`openai` adapter 規則）：
    - `temperature`／`top_p` 只喺 capability 表列明支援、而且 adapter 同時送 `reasoning_effort: "none"` 嘅 model 先送（例如 GPT-5.1、GPT-5.2）；唔支援 `none` 嘅 reasoning model（例如較舊 GPT-5、GPT-6 Astra、GPT-6.1 Sol）唔送任何 sampling 參數。
    - 決定 173：未知 capability model 同 `LLM_SEND_SAMPLING=0` 嘅 sampling warning 合計每個 process 最多一次；跨 compile、repair 同 keep-going 共用。
    - capability 表冇列出嘅 model（包括所有 OpenAI-compatible 本地 API 嘅 model，除非表有登記），一律唔送任何 sampling 參數（`temperature`、`top_p`、`seed`、`reasoning_effort` 都唔送）。
    - `seed`：OpenAI API reference（Create chat completion，<https://platform.openai.com/docs/api-reference/chat/create>）寫明 "This feature is in Beta"、系統只會 "make a best effort to sample deterministically"、"Determinism is not guaranteed"，並建議用 `system_fingerprint` 監察 backend 變動。所以 `seed` 只當 best-effort，冇決定性保證；adapter 將回應嘅 `system_fingerprint` 寫入編譯報告（唔計入 compiler-hash）。
  - Gemini：`generationConfig.temperature`、`topP`、`topK`、`seed` 都接受，但 `seed` 只係 best-effort。
- 所有 provider 一律唔帶 `tools`／`functions`／`tool_choice`／`toolConfig`，唔用 streaming。
- 決定性只係盡力而為；部署安全唔靠重編比對，靠決定 113 attestation 同決定 114 deterministic policy（決定 117）。
- Azure OpenAI 唔喺 v1 範圍（見第 14 節）。

#### 2A.8.3 環境變數同優先次序

| 變數 | 必填 | 規則 |
| --- | --- | --- |
| `LLM_PROVIDER` | 否 | `openai`、`anthropic`、`gemini`；預設 `openai` |
| `LLM_BASE_URL` | 否 | 預設用上表嘅 provider base URL；規則見 2A.8.4 |
| `LLM_API_KEY` | 是 | 只經環境變數、`.env`（已 gitignore）或者 CI secret 提供；本地 API 唔需要 key 時可以用任意占位值 |
| `LLM_MODEL` | 是 | 一定係 provider 承諾唔會浮動嘅 model id／snapshot id（決定 58）。判斷唔靠有冇日期後綴：provider 文件列明會浮動嘅別名（例如 `latest`、`default`、`*-latest`、OpenAI 冇日期嘅 alias）一律拒絕；provider 文件列明係固定 snapshot 嘅冇日期 id（例如 Anthropic 4.6 代之後嘅 id）可以用。Build record 同 manifest provenance 嘅 `compilers` stamp 記錄 provider 回報嘅 model id（`modelReported`）（決定 174），唔係用戶輸入。本地 API 用完整 tag（例如 `qwen2.5-coder:32b-instruct-q8_0`），唔准 `:latest` |
| `LLM_TIMEOUT` | 否 | 單次請求超時，預設 60s，上限 300s |
| `LLM_MAX_RETRIES` | 否 | 網絡錯誤重試次數，預設 2，上限 5；重試唔會改變輸入 |
| `LLM_SEED` | 否 | model capability 支援 seed 先送，預設 `0`；記喺 build record 同 manifest provenance 嘅 `compilers` stamp，唔計入 compiler-hash、唔會令 lock 過時（決定 174，2A.1） |
| `LLM_ALLOW_PRIVATE_HOST` | 否 | 預設 `false`；只喺本地開發用（見 2A.8.4） |

- 別名：`OPENAI_BASE_URL`、`OPENAI_API_KEY`、`OPENAI_MODEL` 只喺 `LLM_PROVIDER=openai` 時接受，分別等於 `LLM_BASE_URL`、`LLM_API_KEY`、`LLM_MODEL`。
- 優先次序：`LLM_*` 優先；只設咗別名就用別名。兩邊都有設而且值唔同，即刻 error（唔會靜靜揀一個）。
- `LLM_PROVIDER` 唔係 `openai` 但設咗任何 `OPENAI_*`，error。
- Ollama 嘅 API 只回報 tag，唔回報 model 檔 digest：build record 嘅 `compilers` stamp 記 tag（決定 174）。注意同一個 tag 可以被重新 pull 換成另一個檔，所以本地 lock 一律唔可以部署（2A.8.4）；可以部署嘅 lock 一定喺 CI 用公開 provider 編譯。

#### 2A.8.4 Endpoint 地址規則（包括本地 API）

| 情況 | 准用 | Build-record compilers stamp（決定 174） |
| --- | --- | --- |
| 公開 host | 一定 `https://` | `endpoint-private: false` |
| 本地 loopback（`localhost`、`127.0.0.1`、`::1`） | 只限本地開發；准 `http://` | `endpoint-private: true` |
| 其他內網地址（例如 `10.x`、`192.168.x`、`*.internal`） | 只限本地開發，而且要 `LLM_ALLOW_PRIVATE_HOST=true` | `endpoint-private: true` |

- CI 同 production 編譯一定要 `https://` 加公開 host；喺 CI 偵測到 loopback、內網地址或者 `LLM_ALLOW_PRIVATE_HOST=true`，即刻 error。
- 用本地或者內網 host 編譯出嚟嘅 lock（build-record stamp 嘅 `endpoint-private: true`，決定 174）**唔可以部署去 production**；一定要喺 CI 用公開 https endpoint 重新編譯，再經 CI 決定性比對同正常確認，先可以部署。
- 本地 lock 只可以用嚟本地試；佢係 gitignore 嘅 build 輸出（決定 117），部署只用 release job 用 release 設定 build 嘅 artifact（決定 113）。

- `LLM_ALLOW_PRIVATE_HOST=true` 亦適用於公開名稱但 DNS 解析到非公開地址嘅 host（例如 fake-IP proxy）；build record 記 `endpoint-private: true`（決定 174）；CI／production 照樣 error。

#### 2A.8.5 Key 保護

- Key 永遠唔可以出現喺 repo、source、lock、manifest、log、部署報告、PR 留言或者錯誤訊息；錯誤訊息可以講「`LLM_API_KEY` 無效或者缺少」，亦可以提供 network／DNS／TLS 原因、HTTP status 同 endpoint host／URL（secret query 值一定要遮蔽），但永遠唔可以包含 key、request headers 或者 response body。錯誤訊息可以顯示硬編碼允許清單入面嘅已知值（model 名、provider error code、Responses API incomplete reason）；唔喺清單入面嘅值一律顯示做 `unknown`，自由文字同 response body 永遠唔會顯示。
- 編譯器同 CLI 輸出 log 前會遮蔽 key（同第 9 節 token／secret 規則一致）。
- CI 用 secret scanning 掃 repo、lock、manifest、部署報告同 CI log；發現疑似 key（包括 `LLM_API_KEY`／`OPENAI_API_KEY` 嘅實際值）即刻 fail build。
- CI 用 GitHub Actions secrets 提供變數：release job 用 `release` environment secret `VIBE_LLM_API_KEY`；PR CI 用另一條 `VIBE_LLM_API_KEY_PREVIEW`（決定 118）。fork 嚟嘅 PR 攞唔到 secret，只跑驗證器，唔編譯。
- Runtime 環境變數唔可以有任何 `LLM_*` 或者 `OPENAI_*`；部署時平台檢查，發現就部署失敗。開發環境嘅 serve 會警告（只列變數名）並喺 handlers 啟動之前移除呢啲變數，runtime 仍然唔會收到 key（見第 10 節）。
- `.env` 一定要喺 `.gitignore`；repo 只准有 `.env.example`（冇真 key）。改 `.gitignore` 移除 `.env`、改 CI workflow 嘅 secret 用法，都受 CODEOWNERS 保護（9B）。

`.env.example`

```dotenv
# 編譯用 LLM，只喺部署／編譯時用；runtime 唔會讀
LLM_PROVIDER=openai
LLM_BASE_URL=https://api.openai.com/v1
LLM_API_KEY=replace-me
LLM_MODEL=vendor-model-2026-09-01
# 選填
LLM_TIMEOUT=60s
LLM_MAX_RETRIES=2
LLM_SEED=0
# 本地 API（Ollama）例子，只限本地開發：
# LLM_BASE_URL=http://localhost:11434/v1
# LLM_MODEL=qwen2.5-coder:32b-instruct-q8_0
# 內網 host 先要：LLM_ALLOW_PRIVATE_HOST=true
```

#### 2A.8.6 SDK provider adapter 介面

每個 provider 由一個 adapter 實作，SDK 只經呢個介面同 LLM 溝通：

```ts
interface LlmAdapter {
  readonly provider: "openai" | "anthropic" | "gemini";
  readonly capabilities: {
    tools: false;               // 永遠 false，adapter 唔可以送 tools／functions
    streaming: false;           // 永遠 false，只做單次非 streaming 請求
  };
  // 按 model id 回報該 model 支援邊啲決定性參數；資料來自 adapter 內置、隨 SDK 版本發佈嘅 capability 表
  modelCapabilities(model: string): {
    temperature: number | null; // 准送嘅固定值（例如 0）；null = 唔送
    topP: number | null;        // 例如 1；null = 唔送
    topK: number | null;        // 例如 1（只限 gemini）；null = 唔送
    seed: boolean;              // true 先送 LLM_SEED（唔代表保證決定性）
  };
  compile(request: {
    system: string;             // 平台 prompt（版本計入 compiler-hash）
    input: string;              // source 加依賴 lock
    model: string;              // LLM_MODEL
    seed?: number;              // LLM_SEED（capabilities.seed 為 true 先送）
    timeoutMs: number;
  }): Promise<{ text: string; modelReported?: string }>;
}
```

- adapter 只發一個非 streaming 請求，唔准送 `tools`、function calling 或者任何會令 provider 執行動作嘅參數；request 參數全部由 adapter 按 `modelCapabilities` 固定，用戶唔可以覆蓋（決定 58）。
- `modelCapabilities` 某參數回 `null`／`false` 就完全唔送嗰個參數，唔可以送 model 會拒絕嘅值；實際送出嘅參數集寫入 build record 同 manifest provenance 嘅 `compilers` stamp，唔計入 compiler-hash（決定 174）。
- capability 表冇登記嘅 model：只送 provider 必填參數，其他決定性參數一律唔送，並出 warning。
- `modelReported`（provider 回報嘅 model id）係 build record `compilers` stamp 嘅 `model-snapshot` 來源（決定 174）；佢同 `LLM_MODEL` 唔一致（例如 provider 將別名解析成另一個 id）就 error。
- 新增 provider 要改 SDK 同驗證器，屬平台改動，唔可以喺 project 內加。

#### 2A.8.7 同其他規則嘅關係

- 2A.7 沙盒：網絡只准 egress 去 `LLM_BASE_URL`（或者 provider 預設 base URL）嘅 host；冇工具規則不變。
- 2A.1／2A.6（決定 174）：LLM stamp 只記喺 build record 同 manifest provenance，唔寫入 lock，唔計入 compiler-hash；永遠唔記 key。
- 決定 174：改 provider、endpoint host 或者 model 唔會令 lock 過時，亦唔改 compiler-hash；改動記喺 build record 同 manifest provenance 嘅 `compilers` stamps。Release job 仍然喺 CI 由 source build、通過決定 114 policy 同 attestation 先可以部署（決定 77、113、117）。
- 只改 base URL 嘅 path（host 唔變）唔當 major，但 CI 決定性比對照樣要通過。
- 唔再做 CI 重編比對（決定 113、117）；2A.8.2 決定性設定照送，只為減少 release summary 入面嘅無謂變化。

#### 2A.8.8 GitHub Actions 範例

```yaml
name: compile-and-verify
on:
  pull_request:
  push:
    branches: [main]
jobs:
  compile:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683  # v4.2.2，用 SHA pin
      - name: Install vibe（pin 版本同 SHA256）
        run: |
          curl -fsSL -o vibe.tar.gz https://github.com/vibe-backend/sdk/releases/download/v0.1.0/vibe-x86_64-unknown-linux-gnu.tar.gz
          echo "<SHA256SUMS 入面嘅值>  vibe.tar.gz" | sha256sum --check
          tar -xzf vibe.tar.gz && install -m 755 vibe "$HOME/.local/bin/vibe"
      - name: Secret scan
        run: vibe secret-scan --fail-on-find
      - name: Recompile and compare locks
        if: github.event.pull_request.head.repo.fork != true
        env:
          LLM_PROVIDER: ${{ vars.LLM_PROVIDER }}
          LLM_BASE_URL: ${{ secrets.LLM_BASE_URL }}
          LLM_API_KEY: ${{ secrets.VIBE_LLM_API_KEY }}   # release environment；PR 預覽用 VIBE_LLM_API_KEY_PREVIEW（決定 118）
          LLM_MODEL: ${{ vars.LLM_MODEL }}
        run: vibe manifest                       # 編譯 + 驗證 + policy 檢查 + manifest（決定 76、114、117）
```

- 決定 174：`scripts/vibe-install.sh` 接受 `vX.Y.Z` 同 `vX.Y.Z-rc.N` pre-release tags；checksum 同 attestation 驗證不變：signer workflow 係 `release.yml`，source ref 係 `refs/tags/v<version>`，issuer 係 `https://token.actions.githubusercontent.com`；決定 118 不變。
- `v0.1.0` 係示範版本號；`vibe` 由 GitHub Releases 派發（唔係 npm），實際一定 pin 死確切版本同 SHA256，唔准 `latest`。所有第三方 action 一律用完整 commit SHA pin，tag 只寫喺註解。

- `vibe` 係 SDK CLI 名（暫定）；deploy job 唔設任何 `LLM_*` 或者 `OPENAI_*` 變數。

## 2C. SDK CLI：本地開發同 `vibe manifest`（決定 73–76、81、82、166，已確認（產品經理 2026-10-14））

### 2C.1 通用

- 所有指令嘅 `--project` 預設係目前目錄（cwd）；只有 project 唔喺 cwd 先要寫（決定 81）。
- 冇 `vibe approve`（決定 77）；舊 workflow 用到會報「指令已移除」並提示用 `vibe manifest`。
- 同現有 CLI（`4239c95`）嘅對照：`vibe compile`、`vibe validate` 保留做獨立指令，`vibe manifest` 係官方 build 指令（決定 164）；`vibe compile --check` 只喺本地／CI 驗證 build 結果同決定 114 policy，唔同 commit 咗嘅 lock 比較（決定 117）；`vibe validate --deploy` 唔再檢查確認同 lock 簽名（決定 77、79）；`vibe serve --memory` 保留，改用 2C.4 嘅 in-memory 執行器。
- `vibe`（冇參數、喺 terminal）進入互動模式（`vibe>`）：第一次用會問 LLM provider、base URL、model 同用戶自己嘅 API key（輸入時遮住，唔會顯示），寫入 project `.env`（確保喺 `.gitignore`，同決定 57、2A.8.3 一致）；之後用自然語言描述要做嘅 API，CLI 用設定好嘅 LLM 產生 feature source，`_definition.md`、`_version.md`、`providers/main.md` 用固定範本；寫入之前列出檔案同要用戶確認，唔會覆蓋現有檔案。非 terminal（例如 CI）執行 `vibe` 只顯示 help。
- `vibe serve --memory` 嘅預設 slug `dev`（tenant `devtenant`）喺 development `--memory` 一律可用，就算 slug 檔有其他 slug，slug 檔改咗唔使重啟就生效；production 同非 `--memory` 不變。
- README 快速開始（決定 82）：

  ```bash
  vibe            # first run asks for LLM settings, then: 幫我開一個 hello world
  vibe compile
  vibe manifest --dev
  vibe serve --memory
  curl http://127.0.0.1:8080/hello/v1/dev/hello
  ```

### 2C.2 本地開發流程（決定 166）

1. 喺 terminal 執行互動 `vibe`：首次問 LLM 設定，key 輸入時遮住；project `.env` 用 0600 權限，只補缺少嘅設定，已有值優先，`.gitignore` 只加一次 `.env`（決定 157）。描述 API，例如「幫我開一個 hello world」，預覽檔案再確認寫入；唔會覆蓋現有檔案。Hello world 預設係 `hello/v1/hello`。Timeout／body／provider 要喺 source frontmatter pin，唔靠 prose limits（決定 160）。
2. `vibe compile` 編譯 source，成個 project 驗證通過先寫 lock 同 `.vibe/build-record.json`；lock 係 gitignore 嘅 build 輸出，唔 commit（決定 117）。改 source 之後要再 compile。
3. `vibe manifest --dev` 用上一步嘅 lock 同 build record，跑同 `vibe validate --deploy --dev` 一樣嘅部署檢查，再用本機 `.vibe/signing.key` 簽 manifest；第一次自動建立 key（0600），manifest 標 `build-mode: dev`。Dev manifest 喺 development 以外一律拒絕，production 唔信任本機 dev key（決定 75、154、164）。
4. `vibe serve --memory` 載入 manifest，用 in-memory 執行器（2C.4），預設 listen `127.0.0.1:8080`。Development 一定提供 slug `dev`（tenant `devtenant`），即使設定咗其他 slug；slug 檔改動唔使重啟。只限開發同測試，server 一停資料就消失。
5. `curl http://127.0.0.1:8080/hello/v1/dev/hello` 測試 API。

2A.8.4 嘅本地 LLM 規則適用於開發環境 `vibe compile`：loopback 准 `http://`，其他內網地址要 `LLM_ALLOW_PRIVATE_HOST=true`；公開名稱解析到非公開地址（例如 fake-IP proxy）亦要呢個設定。互動設定會先問用戶先加呢個值。CI／非開發環境唔准呢個 override，編譯只准公開 HTTPS endpoint。`vibe validate --deploy --dev` 同 `vibe manifest --dev` 唔會呼叫 LLM，亦唔靠呢個環境變數放行；佢哋喺本地開發接受 build-record stamp 記咗 `endpoint-private: true` 嘅 lock（決定 174）。CI／非開發環境拒絕 `--dev`，release 部署檢查拒絕 private endpoint lock（決定 151）。

**vNext（決定 166）**：`vibe dev` 唔喺 1.0。保留設計係唔使預先編譯，按 URL／method 搵 `.md` 同依賴，首次 request 先編譯，cache 放 `.vibe/dev-cache/`；cache key 係 `sha256(source-hash + 依賴 hash 按 id 排序 + compiler-hash 輸入)`，每次編譯同載入 cache 都跑驗證器。File watch 令改動影響嘅 lock 同依賴失效，下次 request 重編，`--eager` 即時重編，migration 改動重建 dev DB。計劃 `--db memory|postgres|url`：memory 預設、postgres 用內嵌 PostgreSQL 做保真度測試、url 用 `VIBE_DATABASE_URL`；唔用 PGlite，因為 Rust 要額外嵌 WASM runtime，而且 role／連線行為保真度唔夠。Dev captcha（一律通過並標示）、kill-switch 同配額假實作亦只係 vNext 設計，唔代表 1.0 已提供。

### 2C.3 `vibe manifest`（官方 build 指令，決定 164）

- v1 官方 build 指令係 `vibe manifest`；預設係 release，同 `--release` 一樣。每次由 source 將所有 lock 重新編譯入記憶體，永遠唔讀磁碟上嘅 lock，亦唔寫返 lock（決定 117）；驗證通過先寫 manifest。
- Release 永遠唔讀 project `.env`，LLM key 只會送去 allowlist 入面嘅 host；唔載入任何 signing key。輸出係 unsigned manifest，帶 build-mode、version、commit、每個 lock 嘅內容同 hash、安全項目等 provenance；由 release workflow 用 Sigstore keyless 簽名（決定 119）。版本用 `--release-version` 或 `v*` tag，commit 用 `--commit` 或 CI／Git 資訊。
- 高風險項目會列出，唔阻 release（決定 90 撤回）；冇 reviewer 步驟，CODEOWNERS 覆蓋係 source PR 合併條件，SDK 唔檢查（決定 93、115）。
- `--dev` 用 `vibe compile` 產生嘅 lock，驗證 build record，再用本機 `.vibe/signing.key` 簽 dev manifest；第一次用會自動建立 key，權限 0600，標 `build-mode: dev`。CI／production 拒絕 `--dev`，dev manifest 只准喺 development serve。等同嘅本地檢查係 `vibe validate --deploy --dev`。
- 只讀一次／唔出部分輸出仍然係規則（決定 102）：驗證同 manifest 用同一份記憶體輸入，失敗唔應留下部分輸出。Release 高風險翻譯唔用 cache 仍然係規則（決定 110）；目前翻譯路徑仍會讀 cache，唔代表已實作 bypass。
- 選項包括 `--release`、`--dev`、`--release-version`、`--commit`、`--project` 同 `-o`／`--output`（預設 `manifest.json`，相對目前工作目錄）。Lock 唔 commit；`--check-determinism` 已移除（決定 117）。
- **vNext（決定 164）**：`vibe build` 一個指令組合 compile／validate／manifest、`--labels`、`-o` 同 10.10 部署報告，唔喺 1.0；v1 `vibe manifest` 已有上面列明嘅 output 選項。

### 2C.3a `--release-approval`（已撤回，產品經理 2026-10-16）

- 產品經理撤回 `--release-approval`（原決定 90），唔會實作。SDK 唔驗證 PR review、tree 相等、CODEOWNERS 設定、`main` 或者 tag ruleset，亦唔讀 environment 批准紀錄；高風險 `security-items` 只列入部署報告，唔阻 release。
- **SDK 同 release workflow 都唔讀 ruleset／`bypass_actors`**（決定 90／#127；決定 96）：`.github/workflows/release.yml` 而家冇任何 ruleset／`bypass_actors` 檢查。如果將來 `release.yml` 加呢類步驟，只可以係唯讀、資訊性、永遠唔會擋 release（決定 96 已接受例外；`main` ruleset `bypass_actors` 唔係空唔阻 release）。
- 唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118。
- `main` 同 `v*` tag ruleset 照舊按 [`pr-review-guide.md`](pr-review-guide.md) 設定，但 SDK 同 release workflow 都唔檢查。
- **剩餘風險（Jack 已接受）**：
  - Admin 可以 bypass `main` ruleset 保護（OrganizationAdmin bypass 保持開啟）。
  - 唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118。
  - 所有 agent 都用 `j113203`，同一個帳戶可以開 `v*` tag 再自己批准（`prevent_self_review: false`）。
  - `release` environment `can_admins_bypass: true` 保持開啟，admin 可以跳過 environment 批准。

### 2C.3b 部署來源同部署前檢查（決定 113–116，issue #97）

v1.8 冇 lock 簽名（決定 79），部署閘門改為「CI 由 source 重新 build 嘅 artifact」加 attestation 同 deterministic policy 檢查。

#### 1. 部署輸入（決定 113，已確認（產品經理 2026-10-14））

- Runtime 同部署器**只**接受 CI 喺 `release` environment 由 source 重新 build（`vibe manifest`，包括重新編譯）嘅 artifact（manifest 同佢包含嘅 lock 內容）。
- Artifact 一定要有 GitHub artifact attestation（build provenance，predicate `https://slsa.dev/provenance/v1`），綁定：repo、workflow 路徑（例如 `.github/workflows/release.yml`）、ref（`refs/tags/v*`，決定 96；呢個綁定只證明來源，唔係閘門，見第 5 節）、commit SHA；runner 一定係 GitHub-hosted。
- 另外要有 Sigstore keyless manifest 簽名（決定 119）：release workflow 喺 `release` environment 用 GitHub OIDC 身份簽，一定要有 Rekor transparency log inclusion。冇任何長期 key。
- 部署器全部驗證，任何一樣失敗即拒絕部署：
  - Attestation：例如 `gh attestation verify <artifact> --repo <owner>/<repo> --signer-workflow <owner>/<repo>/.github/workflows/release.yml --source-ref refs/tags/v<version> --source-digest <commit sha> --deny-self-hosted-runners`，或者用 sigstore 驗證同樣條件。
  - Manifest 簽名：Sigstore 驗證（例如 `cosign verify-blob --certificate-oidc-issuer https://token.actions.githubusercontent.com --certificate-identity https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v<部署緊嘅版本>`（只接受完全相等；如果工具一定要用 regex，只可以用完全錨定、正確轉義、寫死部署版本嘅式，例如 `^https://github\.com/vibe-backend/sdk/\.github/workflows/release\.yml@refs/tags/v1\.8\.0$`，唔可以用 `v.*` 之類寬鬆配對），加 `--certificate-github-workflow-sha <commit sha>`、`--certificate-github-workflow-repository vibe-backend/sdk`，同埋證書 extension 證明係 GitHub-hosted runner），一定要有 Rekor inclusion；身份同 attestation 一致（決定 119）。
  - **Sigstore trust root（決定 119）**：
    - 內容：`trusted_root.json`（Fulcio CA 證書鏈、Rekor 公鑰、CT log 公鑰），來源係 Sigstore 公共 TUF repository。
    - 取得同 pin：release build 經 TUF（有簽名同過期檢查）攞最新 `trusted_root.json`，embed 入 runtime binary；production runtime 只用 binary 入面嗰份，唔喺運行時上網攞。
    - 離線驗證：Sigstore bundle 一齊帶證書、簽名、Rekor inclusion proof 同 SET（signed entry timestamp），runtime 喺本地驗證，唔使連 Rekor 或者 Fulcio。
    - SET 同 inclusion proof 只用 binary 內嵌 `trusted_root.json` 入面嘅 Rekor 公鑰同 checkpoint 驗證，唔接受 bundle 自帶或者其他來源嘅 key。
    - Fulcio 證書有效期用 SET 已驗證嘅 Rekor `integratedTime` 檢查（簽名嗰刻證書要有效），唔用而家時間；冇已驗證嘅 `integratedTime` 即拒絕。
    - 輪換：Sigstore 輪換 Fulcio CA 或者 Rekor key 時，TUF metadata 會更新；平台出新 SDK／runtime release embed 新 trust root（新舊 key 喺 Sigstore 重疊期內都有效）。舊 binary 只信舊 root，過咗重疊期會 fail closed，拒絕新 manifest。Trust root 更新經正常 release 流程（`release` environment 批准）。
    - 告警同 runbook：runtime 因 trust root 驗證失敗拒絕 manifest 時要發告警（例如 `manifest_trust_root_rejected`）；Sigstore 公佈輪換時亦要提前告警。Runbook：確認原因係 trust root 過期定係簽名真係唔啱；係前者就部署 embed 新 trust root 嘅 SDK／runtime release，然後重新部署；永遠唔可以為咗過關關閉驗證或者 fallback 去 dev key。
  - **限制**：keyless 身份只證明邊個 workflow 同 tag 產生呢個 build，唔證明邊個批准；佢唔會加多一個人，因為任何 `j113203` agent 都可以開 tag 再自己批准。
  - **剩餘風險**：trust root 更新都係經同一個 `release` environment 出，同一個帳戶可以自己批准，冇額外嘅人把關。
  - Attestation 嘅 subject digest 一定等於實際部署嘅 artifact digest。
- Lock 係 build 輸出，唔 commit（決定 117）；部署輸入只有 CI 呢次 build 嘅 artifact。
- LLM 輸出唔決定性，所以唔做任何「重編再逐字比對」；安全靠第 2 節嘅 deterministic 檢查。

#### 2. 部署前 policy 檢查（決定 114，已確認（產品經理 2026-10-14））

對 artifact 入面每個 lock 做 deterministic 檢查（普通程式、唔用 LLM），任何錯誤即 fail closed：

- `roles`、寫入（`writes`）、`public`／`public-writes`、`auth` 一定要等於 source frontmatter 宣告（決定 112）；唔一致即失敗。
- 所有 SQL 通過決定 98 嘅 parser 白名單（7.1a）。
- `UPDATE`（`DELETE` 永遠唔准，決定 13）一定要有 `WHERE`，而且：
  - 常數摺疊之後，條件唔可以係常數（例如恆真或者恆假）。
  - 最少有一個「欄位 = 參數」或者「欄位同參數比較」，而嗰個欄位係主鍵、唯一鍵、或者 lock 宣告咗嘅篩選欄位。
  - `OR` 任何一邊摺疊之後係恆真，即拒絕。
  - 規則判斷唔到（例如太複雜嘅表達式），一律 fail closed。
- 呢個檢查喺 release job 同部署器都做；部署器發現唔通過就拒絕，唔信 CI 結果。

#### 3. CODEOWNERS（決定 115，已確認（產品經理 2026-10-14））

- 決定 77 已經移除人手確認，CODEOWNERS 唔再係主要閘門，只係建議（仍然建議保護 `migrations/`、`.github/`、`.compiler/` 等路徑，見 [`pr-review-guide.md`](pr-review-guide.md)）。

#### 4. Lock 審計同變化列表（決定 117，已確認（產品經理 2026-10-14），取代決定 116）

- Release job 將呢次產生嘅所有 lock 存做審計 artifact，佢嘅 digest 納入 attestation（同 manifest 一齊做 subject）。
- 同上一個已部署版本嘅審計 artifact 比較（按 lock id 排序，逐 section 比對；SQL 先用 PostgreSQL 解析器轉成 parse tree 再正規化，只係格式唔同唔算），喺 release summary **列出**以下變化，講明 lock id、section、step 同欄位：roles、寫入、public、auth、SQL。
- 呢個列表**唔阻擋**部署；閘門係第 1、2 節（決定 113、114）。
- 剩餘風險：產生嘅 SQL 唔再有人手 review。保護靠：source PR review frontmatter 高風險宣告（決定 112）、決定 114 deterministic policy、release summary 列出變化畀人睇。

#### 5. Build once, promote（決定 118，已確認（產品經理 2026-10-14））

- Release job 只喺 `v*` tag build 一次；同一個 artifact（同一個 digest）promote 去 staging 同 production，唔重新 build。
- 每次 promote，部署器都用 `gh attestation verify <artifact> --source-ref refs/tags/v<version>`（加第 1 節其他條件）驗證。
- PR CI 嘅 build 只係**非權威預覽**（例如畀 release summary 預覽同 label），永遠唔可以部署。
- LLM key 分開：release job 用 `release` environment secret `VIBE_LLM_API_KEY`；PR 預覽用 `VIBE_LLM_API_KEY_PREVIEW`。兩條 key 都唔可以出現喺 artifact、log 或者 summary。
- Artifact 入面有審計戳記：tag、commit SHA、workflow run id、compiler-hash、build 時間。
- `VIBE_LLM_API_KEY` 同 `VIBE_LLM_API_KEY_PREVIEW` 兩條 secret 由 Jack 本人建立。
- **閘門**：臨時方案 (i) 下，任何有寫權限嘅人都可以開 `v*` tag，所以 `v*` tag **唔係**真正閘門；`--source-ref` 驗證只證明 artifact 嚟自邊個 tag。真正人手批准閘門係 `release` environment 要 Jack 批准（`--release-approval` 已撤回，唔會加）；自動閘門仍然係決定 113／114／118。剩餘風險（Jack 已接受）：admin 可以 bypass `main` ruleset 保護（OrganizationAdmin bypass）；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；同一個帳戶（`j113203`）可以自己批准；`release` environment admin bypass 保持開啟。
- 將來目標：方案 (iii)，專用 release GitHub App 係唯一可以開 `v*` tag 嘅 actor，並開啟 `prevent_self_review`。

### 2C.4 In-memory 執行器（決定 74，已確認（產品經理 2026-10-14））

適用：`vibe serve --memory`（vNext `vibe dev`，決定 166）。

- **用途限制**：只限開發同測試。資料只存喺 process 記憶體，server 停止、重啟或者 crash 就全部消失；冇持久化、冇備份、冇跨 instance 共享。
- **Dev 提示**：dev 用 in-memory 時，啟動同 health 都要清楚標示資料唔會保存：
  - 喺 stderr 印顯眼警告（多行、紅色），內容包括「IN-MEMORY DATABASE：所有資料喺 server 停止時消失，唔可以用喺 production」；
  - health endpoint（`/_health`）回 `"db": "memory"` 同 `"warning": "in-memory database, data is lost on stop"`；
  - 每分鐘喺 log 寫一次 `warn` 級別嘅同一句。
  - Production（manifest 唔係 `build-mode: dev`，或者 `VIBE_ENV=production`）一律拒絕 in-memory：啟動直接報錯退出，冇任何 override 旗標（決定 91）。
  - 只係 listen 地址唔係 loopback（例如 Docker 入面 `vibe serve --memory --addr 0.0.0.0:8080`）唔算 production，只出警告；Docker 入面請明確設 `VIBE_ENV=development`，先用 `vibe manifest --dev` 產生 `build-mode: dev` manifest，所以照樣准用 in-memory。
- **一定要模擬嘅語意**（令 dev 行為同 production 一致）：
  - 租戶隔離（RLS＋FORCE）：每個 statement 一定帶 `tenant_id`；冇用租戶身份執行就報錯（同 production 冇權限一樣），唔係回空；租戶邊界係租戶 `LOGIN` role 同佢自己 schema，唔係 GUC；讀寫只見同一 tenant 嘅列；INSERT 嘅 `tenant_id` 由執行器注入，SQL 或者 input 帶 `tenant_id` 一律拒絕。
  - Role grant：每個 statement 用 lock 指定嘅 role（`app_rw`、`public_reader`、`public_writer`）執行，按 model lock `## Policies` 嘅 grant 檢查表級 SELECT／INSERT／UPDATE；冇權限回同 production 一樣嘅 `permission denied` 錯誤類別。
  - Column grant：INSERT／UPDATE 嘅每個欄位要喺該 role 嘅 column grant 入面；`writable-columns` 同 `effective-grant` 同 production 一樣檢查。
  - 所有 runtime role 冇 DELETE、TRUNCATE、DDL。
  - 唯一鍵（包括 `(tenant_id, email)`）、NOT NULL、CHECK（`char_length`、enum）、`ON CONFLICT ... DO UPDATE ... WHERE`、`RETURNING`、軟刪除、If-Match／ETag 版本檢查。
  - 每個 request 一個 transaction（同 7.4 一樣），失敗就成個 request rollback，令失敗恢復行為（例如 11.4 重試）一致；超時同衝突錯誤碼（`TX_TIMEOUT`、`TX_CONFLICT`）一樣。
- **已知語意差異**（喺 terminal 啟動訊息同文件列明）：
  - 只支援 lock 白名單用到嘅 SQL 子集；任意 SQL 函數、複雜 JOIN／subquery、window function 未必支援，唔支援就報 `MEMORY_UNSUPPORTED_SQL`，唔會靜靜錯。
  - 冇真正並發隔離（全部 statement 串行），所以 race condition（例如 refresh 寬限期兩個並發請求）喺 in-memory 測唔出。
  - `now()`、`interval` 用 process 時鐘；collation、排序同大小寫比較用 Unicode code point，可能同 PostgreSQL locale 唔同。
  - CTE 只支援 11.4a 嗰個「insert CTE + 同表 SELECT」固定模式。
  - 冇 `search_path`、schema／database 模式、PgBouncer 行為；冇 `CREATE INDEX CONCURRENTLY` 等 migration 細節（migration 只更新 in-memory schema）。
  - 冇 crypto-shredding 嘅 KMS；DEK 用記憶體假實作。
- **CI**：整合測試照樣一定要對真 PostgreSQL 跑（`--db postgres` 或者 `--db url`）；in-memory 通過唔代表 production 通過。

## 2B. Migration（`kind: migration`，決定 51）

### 2B.1 Source 同位置

- 位置：`migrations/{provider}/{seq}-{name}.md`，lock 喺旁邊 `{seq}-{name}.lock.md`；`provider` lock 嘅 `## Migrations` 指向呢個目錄。
- source 用自然語言寫改動，例如「users 表加一個 phone 欄位，可以冇，最多 20 字，屬敏感個人資料」。
- 只准向前（forward-only）；冇 down migration。要還原就寫一個新 migration。

### 2B.2 Lock 格式

- frontmatter 同 2A.1，`kind: migration`，`dependencies` 包括 provider lock 同上一個 migration lock。
- `## Target`：`provider`、`tables`（會改嘅表）、`seq`、`previous`（上一個 migration id）。
- `## Up`：`### s{n} ddl` 或者 `### s{n} backfill` step；`ddl` 每步一個 statement；`backfill` 係參數化 UPDATE，一定有 `WHERE` 同分批大小（`batch: 1000`）。
- `## Verify`：遷移後檢查，例如欄位存在、RLS ENABLE＋FORCE、policy 正確、行數唔變。
- `## Data`：`destructive: true | false`、`personal-fields`（新增或者改嘅個人欄位分類）、`deprecated-since`（DROP 時必填）。

### 2B.3 規則

- DDL 只准喺 migration lock 出現。本節係**白名單**：冇列出嘅 statement 一律拒絕（例如 `COMMENT`、`CREATE TYPE`、`CREATE SEQUENCE`、`INSERT`）。准用：`CREATE TABLE`、`ALTER TABLE ... ADD COLUMN`、`ALTER TABLE ... ADD` constraint、`ALTER TABLE ... ALTER COLUMN` 只限 `SET NOT NULL`／`DROP NOT NULL`／`SET DEFAULT`／`DROP DEFAULT`（`SET DATA TYPE` 一律拒絕，因為判斷唔到係放寬定收窄）、`CREATE INDEX`（非 UNIQUE、純效能 index 一律 `CONCURRENTLY` 而且一定要有名，因為平台要按名紀錄，冇名即拒絕；喺主 transaction commit 之後建；UNIQUE index 同 constraint 一律唔用 `CONCURRENTLY`，喺主 transaction 入面建，見「Owner role 條件」第 5 點）、`DROP INDEX`。`DROP TABLE` 同 `DROP COLUMN` 一律拒絕，因為 v1 冇棄用流程（見下）。（對照 #147 @8066b69）唔准：`TRUNCATE`、`DELETE`、`GRANT`、`REVOKE`、`ALTER ROLE`、`CREATE FUNCTION`、`CREATE EXTENSION`、`DISABLE ROW LEVEL SECURITY`、`NO FORCE`、改 policy。
- 補充（決定 98，已確認（產品經理 2026-10-14））：migration SQL 一樣用 PostgreSQL 解析器檢查，解析失敗或者遇到唔認識嘅 node 即 fail closed：
  - 唔准 `SECURITY DEFINER`；唔准 `CREATE FUNCTION`、`CREATE PROCEDURE`、`CREATE TRIGGER`（將來如果開放，只准平台白名單語言同 `SECURITY INVOKER`）。
  - 唔准任何 role 同權限操作（建立或者修改 role、授權同收回、預設權限），唔准任何會話設定修改（包括物件層級嘅設定）。
  - 唔准 schema-qualified 名稱，唔准觸及其他 schema、平台表或者 system catalog。
- `CREATE TABLE`：平台自動加 `tenant_id` 欄位、`ENABLE` 同 `FORCE ROW LEVEL SECURITY`、標準 policy、`created_at`／`updated_at`／`deleted_at`／`erased_at` 系統欄位，同埋按 model lock 產生 grant；source 自己寫呢啲會被忽略並出 warning。
- 執行：由平台用 migration role（決定 129 嘅 `tenant_<id>_owner`，長期存在，只喺 migrate 期間可以登入）按租戶逐個 schema 執行；全程審計（同 7.3 一致）。每個租戶一個 migration role，只擁有嗰個租戶 schema，對其他 schema、平台表同 role 管理冇任何權限，只以 `NOINHERIT` 做自己租戶嘅 `tenant_<id>_rw` 成員（2B backfill／決定 129 嘅受控例外，見 7.1），唔係任何其他 role 嘅成員（已確認（產品經理 2026-10-14））。
  - 點解由臨時 role 改做長期 owner（已確認（產品經理 2026-10-16））：決定 129 要求 runtime role 唔係 owner、每張表 `FORCE ROW LEVEL SECURITY`，表要有一個固定 owner；臨時 role 每次建立同刪除會令 ownership 轉移同授權複雜又易錯。改為長期 owner 但 migrate 以外 `NOLOGIN`。
- Owner role 條件（決定 27 受控例外，已確認（產品經理 2026-10-16））：
  1. Migration parser 用**正面白名單**（同決定 97、98 一致）：只有上面准用嘅 statement 先放行，其他一律拒絕。以下永遠拒絕：`DELETE`、`TRUNCATE`、`GRANT`、`REVOKE`、`ALTER ... OWNER`、`DISABLE` 或者 `NO FORCE ROW LEVEL SECURITY`、`DROP POLICY`、`SECURITY DEFINER`、任何 GUC 修改。
  2. 登入窗口：migrate 開始時平台由 secret manager 發一次性密碼並開 `LOGIN`；所有 `ddl` step 喺**單一 transaction** 入面執行（backfill 按租戶分批，見下面「Backfill 同 FORCE RLS」）。無論成功定失敗，都喺 finally 步驟由獨立嘅平台 admin 連線將 owner 設返 `NOLOGIN` 同作廢密碼（失敗咗嘅 transaction 入面做唔到）。
  3. 定期檢查：平台定期檢查每個 owner role；migrate 以外發現可以登入，即刻停用並發告警。
  4. Runtime 連線池永遠唔以 owner 登入：`vibe serve` 啟動時檢查每個連線池嘅登入 role 唔係任何 `tenant_<id>_owner`，唔係就拒絕啟動（fail closed）。
  5. `CREATE INDEX CONCURRENTLY`（已確認（產品經理 2026-10-16））：佢唔可以喺 transaction 入面跑。只有非 UNIQUE、純效能嘅 index 可以喺主 transaction commit 之後，喺**同一個 owner 登入窗口**入面逐條用獨立、有審計嘅 concurrent index 步驟建，之後先做 finally 收回登入；揀呢個做法係因為 concurrent index 唔會長時間鎖住大表。UNIQUE index 同 UNIQUE／其他 constraint 一定喺主 transaction 入面用非 concurrent 方式建，令唯一性同資料改動一齊 commit 或者 rollback。Index 步驟失敗會留低 INVALID index：下次 migrate 開始時平台先 drop 再重建。呢個系統 DROP 係白名單動作，唔使行 2B 棄用流程，寫審計；只可以針對同時符合三個條件嘅 index：(i) 平台自己嘅 concurrent index 步驟有紀錄係佢建立；(ii) 喺該租戶 schema 入面；(iii) database catalog 標記為無效（`pg_index.indisvalid = false`）。Index 名一律由平台紀錄攞，永遠唔由 migration source 攞。平台冇紀錄嘅 INVALID index：寫審計並發 ALERT，但唔令 migrate 失敗，亦永遠唔 drop；migration 報告標記失敗並發告警。
- Backfill 同 FORCE RLS（已確認（產品經理 2026-10-16），#233）：
  - Backfill 同 #193 replay 清理每個租戶各自跑；UPDATE 參數化同分批，固定批量（每個 transaction 1,000 行）；失敗按租戶處理：寫審計、可以重試、唔影響其他租戶。永遠唔用 `BYPASSRLS`、owner bypass、GUC 或者改 policy。
  - Context 機制（機制修正已確認（產品經理 2026-10-16））：每一批由 migrate runner 開 transaction，執行 `SET LOCAL ROLE tenant_<id>_rw`；`<id>` 由系統提供，role 係該租戶現有嘅 role，唔新建任何 role。Migration role 以 `NOINHERIT` 成為自己租戶嘅 `tenant_<id>_rw` 嘅成員，所以唔 `SET ROLE` 就用唔到租戶權限。咁現有嘅常數 policy（`TO tenant_<id>_rw`）只會令嗰個租戶嘅行可見。Transaction 完結時 role 自動還原。原因：決定 97 嘅 policy 唔讀 GUC，所以之前寫嘅「`SET LOCAL` 設定 GUC」冇效；已確認嘅方向唔變，只係修正機制。呢個做法同之前否決嘅「每個租戶專用 role」唔同：佢重用租戶現有 role，唔加新 role。
  - Migration role 就係 owner：spec（決定 129、7.1）同 #147 都只有一個 migration role，即係每個租戶嘅 `tenant_<id>_owner`；main 而家嘅 code 未有呢個 role（#147 未 merge）。佢以 `NOINHERIT` 成為自己租戶嘅 `tenant_<id>_rw` 成員，所以係高權限 role，照 owner 規則處理（已確認（產品經理 2026-10-16））：平時 `NOLOGIN`，只喺 migrate 窗口用一次性密碼登入；每次 `SET ROLE` 都寫審計；定期檢查 membership 同登入狀態有冇偏差；永遠唔用嚟做 runtime 登入（`vibe serve` 啟動檢查照舊拒絕以 owner 登入嘅連線池）。
  - 呢個 membership 係 7.1 migration role membership 限制同 `vibe serve`「租戶 role 唔可以有成員」檢查嘅受控例外（已確認（產品經理 2026-10-16）），只限同一租戶嘅 owner → `tenant_<id>_rw`（`NOINHERIT`）。條件：(1) 啟動檢查用正面條件寫：每個租戶剛好有一行 membership，`tenant_<id>_rw` ← `tenant_<id>_owner`（同一個 `<id>`），選項係 `ADMIN FALSE`、`INHERIT FALSE`、`SET TRUE`（PostgreSQL 16 或以上）；反方向同其他任何 membership（包括跨租戶、額外或者連鎖／間接）一律拒絕，啟動失敗。(2) Owner 喺 migrate 以外照舊 `NOLOGIN`，有定期偏差檢查（決定 129）。(3) 必須測試：owner 做其他租戶 rw 嘅成員、INHERIT、有 ADMIN OPTION，三種情況啟動都失敗。實作狀態：未實作，#147 嘅啟動檢查要調整，#246。決定 97（runtime 連線 role 同租戶 role 之間冇 membership）唔變。
  - 權限（已確認（產品經理 2026-10-16））：backfill 只可以做 `tenant_<id>_rw` 本身已經准做嘅嘢，永遠唔為 backfill 加 grant。權限唔夠就令該租戶 backfill 失敗（寫審計），改動交返產品經理決定。
  - Grant（PostgreSQL 16 或以上）：`GRANT tenant_<id>_rw TO tenant_<id>_owner WITH ADMIN FALSE, INHERIT FALSE, SET TRUE`。
  - PostgreSQL 版本（架構師決定 2026-10-16）：v1 要求 PostgreSQL 16 或以上。16 以下表達唔到上面嘅 grant 選項，所以 `vibe migrate` 同 `vibe serve` 啟動時一律 fail closed，報明確錯誤（要求 PostgreSQL 16+），冇任何 fallback。實作狀態：未實作（code 仲未檢查版本），#254 跟進（包括最低版本測試）。
  - Batch transaction：一定用明確嘅 `BEGIN` 開始；batch 入面禁止 savepoint；owner 連線永遠唔加入任何共用連線池。
  - 條件：(1) 只有 migrate runner 可以發 `SET ROLE`；用戶寫嘅 migration SQL 唔可以用 `SET ROLE`、`RESET ROLE`、`SET SESSION AUTHORIZATION` 或者改 GUC，決定 129 白名單 parser（2B.3、Owner role 條件第 1 點）一律拒絕。(2) 每個 batch UPDATE 之前同之後都核對 `current_user` 等於處理緊嗰個租戶嘅 `tenant_<id>_rw`、`session_user` 等於 `tenant_<id>_owner`，任何唔一致即中止（fail closed）；FORCE RLS 同部署時 policy 比對全程有效。(3) #193 replay 清理用同一個做法。(4) 必須測試：用租戶 A context 跑嘅 backfill 對租戶 B 改動零行。
  - 實作狀態：未實作，#233（#147 @8066b69 而家以 owner 跑一條冇參數化、冇分批嘅 `UPDATE … WHERE`，喺 FORCE RLS 下睇唔到任何行）。
  - 剩餘風險（已接受（產品經理 2026-10-16））：owner role 可以 `SET ROLE` 去自己租戶嘅 rw role，所以 owner 憑證如果喺登入窗口外洩，亦會有該租戶平台表（outbox、idempotency）嘅 rw 權限。緩解：只有 migrate runner 可以發 `SET ROLE`，用戶 migration SQL 一律被白名單拒絕；owner 平時 `NOLOGIN`、一次性密碼、每次 `SET ROLE` 寫審計、定期偏差檢查。接受理由：owner 本身已經擁有該租戶嘅表，owner 憑證外洩對該租戶已經係最壞情況，可以 `SET ROLE` 做 rw 唔會擴大影響範圍；而且決定 129 下 owner 平時 `NOLOGIN`、用一次性密碼（見上面「Owner role 條件」第 2 點），暴露窗口只限 migrate 執行期間。接受條件：每次 `SET ROLE` 寫審計（見上面「Migration role 就係 owner」）同定期偏差檢查（見上面受控例外條件 (2)、決定 129 `vibe check-owners`）一定要持續生效。
  - 剩餘風險（已接受）：登入窗口期間外洩嘅 owner 憑證對表有完整權力；靠一次性密碼、parser 白名單、審計同定期檢查緩解。
- 安全項目：migration lock 一律標 `migration-ddl`（高風險，資訊性，決定 78）；SDK 唔要求人手確認，建議 `migrations/` 受 CODEOWNERS 保護（見 [`pr-review-guide.md`](pr-review-guide.md)）。
- 棄用流程（DROP 租戶資料）：**v1 未提供**，所以 `DROP TABLE`／`DROP COLUMN` 一律拒絕；以下係將來版本嘅設計：
  1. 先有一個 migration 將欄位或者表標做 deprecated（`## Data` 寫 `deprecated-since`），同時所有 lock 唔可以再引用佢。
  2. 最少 30 日之後，先可以有 `destructive: true` 嘅 migration DROP 佢。
  3. DROP 之前平台自動做該租戶 schema 嘅快照備份（跟 35 日備份保留期）。
  4. 涉及 `personal` 欄位嘅 DROP，要先對受影響紀錄行 12.10 嘅 crypto-shredding／匿名化，確保備份入面都讀唔返敏感資料。
- 改咗 migration 會令依賴佢嘅 model lock hash 失效，要重新編譯同驗證。

### 2B.4 範例

`migrations/main/0003-add-user-phone.md`

```markdown
# 加用戶電話
users 表加 phone 欄位，可以冇，最多 20 字，屬敏感個人資料。
另外加 email 嘅細楷 index。
```

`migrations/main/0003-add-user-phone.lock.md`

````markdown
---
lock-version: 1
kind: migration
id: migrations/main/0003-add-user-phone
source: migrations/main/0003-add-user-phone.md
source-hash: sha256:6d1a...4e
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: migrations/main/0002-add-last-login, kind: migration, hash: sha256:2b8f...c3 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: migration-ddl, risk: high, reason: "加敏感電話欄位" }
---
## Target
- provider: main
- tables: [users]
- seq: 0003
- previous: migrations/main/0002-add-last-login

## Up
### s1 ddl
- op: ddl
- sql: `alter table users add column phone text null check (char_length(phone) <= 20)`
- source-lines: 2-3

### s2 ddl
- op: ddl
- sql: `create unique index users_email_key on users (tenant_id, email)`
- source-lines: 2-3

## Verify
### v1 verify
- op: verify
- check: column-exists users.phone
### v2 verify
- op: verify
- check: rls-enabled-forced users
### v3 verify
- op: verify
- check: row-count-unchanged users

## Data
- destructive: false
- personal-fields: { users.phone: sensitive }
- deprecated-since: (none)
````

## 3. 繼承

- 次序由外到內：`_definition` → `_version` → `{feature}`；繼承喺編譯 feature lock 時解析，結果寫死喺 `{feature}.lock.md` 嘅 `## Pipelines` 同 `## Limits`，`dependencies` 會列埋上層 lock 嘅 hash。
- `pipelines` 會合併；內層可以用 `-name` 剔走外層嘅 pipeline（自然語言寫「唔使 auth」會編譯成 `-auth`）。
- 剔走 `auth` 一定要同時 `public: true`。
- `timeout`、`limits`、`provider` 呢類單一設定由內層覆蓋外層，但唔可以超過平台上限。
- 唔同 version 之間唔會互相繼承。
- 任何一層 lock 改咗，底下所有 feature lock 嘅依賴 hash 都會失效，要重新編譯同驗證。
- `_version` 可以宣告 `deprecated: true` 同 `sunset: <ISO 日期>`（寫入 `## Lifecycle`）；設咗之後，回應會帶 `Deprecation` 同 `Sunset` header。

## 4. Pipeline 同認證

- 內置 pipeline：`auth`、`validate`、`rate-limit`、`audit-log`、`token-refresh`、`token-revoke`；佢哋嘅 lock 由平台提供同簽名。用戶可以喺 `pipelines/{name}.md` 用自然語言自訂，一樣編譯成 `pipelines/{name}.lock.md`。
- 每個 pipeline lock 都有 `## Parameters` schema；未知參數或者類型錯就報 error。
- 預設拒絕：所有 endpoint 預設都要經過 `auth`。
- `auth` 一定要排喺所有 `method` step 同其他 Logic 之前（`## Order` 宣告 `must-run: first`）。
- `roles-claim` 預設係 `roles`；`roles: []` 報 error。
- `auth` 成功之後注入 `ctx.tenant_id` 同 `ctx.user`；Logic 唔可以改呢兩個值。
- pipeline 可以用 `method` op 呼叫 model method（決定 5），例如 token 刷新 pipeline 呼叫 `user.sign-token`；pipeline 唔可以直接寫 `query` 或者 `call`。
- 帳戶安全（refresh token、密碼、鎖定）見 4A。
- token 簽發一律經 model 層標咗 `capabilities: [sign-token]` 嘅 method（例如 `user.sign-token`）；簽發規則見 12.6。

### 4.1 JWT 模式（決定 44）

寫法：`auth: { mode: jwt, roles: [admin], iss: ..., aud: ..., jwks-uri: ... }`（`iss`、`aud`、`jwks-uri` 選填，預設用平台值）。

| 參數 | 值 |
| --- | --- |
| `iss` | 預設 `https://auth.{platform-domain}/{project}`；一定要完全相等 |
| `aud` | 預設 `{project}`；token 嘅 `aud` 一定要包含呢個值 |
| `jwks-uri` | 預設 `{iss}/.well-known/jwks.json`；一定要 https |
| JWKS cache | 10 分鐘；遇到未知 `kid` 即刻重拉，但同一 issuer 最少相隔 60 秒；拉唔到時最多沿用舊 cache 1 小時，之後所有 token 回 401 |
| clock skew | `exp`、`nbf`、`iat` 容許 60 秒 |
| alg 白名單 | ES256、RS256（RSA key 最少 2048 bit），預設 ES256；每個部署只設定一個 alg，驗證只接受呢個 alg；禁止 `none`、HS*，亦唔准混用；header 一定要有 `kid`（決定 139，見 4.1a） |
| `roles-claim` | 預設 `roles` |

- 要驗 `iss`、`aud`、`exp`、`nbf`；`exp` 距離 `iat` 超過 1 小時（access token 上限）一律拒絕。
- 改 `iss`、`aud`、`jwks-uri`（例如接外部 IdP）屬高風險安全項目。

### 4.1a 簽名算法政策（決定 139，已確認（產品經理 2026-10-16），issue #198）

呢節係 `sign-jwt`（簽發）同 JWT 驗證（auth pipeline）共用嘅 alg 規則，優先於其他地方較簡略嘅寫法。

| 項目 | 規則 |
| --- | --- |
| 支援 alg | 只有 ES256（P-256）同 RS256；預設 ES256。RS256 只建議用嚟同第三方／JWKS 互通。 |
| RS256 key 長度 | 最少 2048 bit（按 modulus 實際長度計）。簽名私鑰、靜態驗證公鑰同 JWKS 入面嘅 RSA key 都適用。 |
| 唔符合 key 長度 | 簽名私鑰或者靜態驗證公鑰唔夠長：`vibe validate --deploy`、部署同 `vibe serve` 啟動 fail closed，拒絕啟動；唔會降級、唔會改用其他 alg、唔會只出 warning。JWKS 入面唔夠長嘅 key 當冇呢個 `kid`，用佢簽嘅 token 回 401。 |
| 驗證 alg | 每個部署（或者每個 issuer）只設定一個 alg。token header 嘅 `alg` 一定要等於設定值；`none`、HS*、ES256／RS256 之中另一個同任何未知值一律回 401。alg 由設定決定，唔由 token header 或者 key 類型推斷。 |
| key 同 alg 一致 | 設定 alg 同 key 類型唔夾（例如設定 ES256 但 key 係 RSA）：啟動 fail closed。JWKS key 嘅 `kty`／`alg` 同設定 alg 唔夾，嗰個 key 唔用。 |
| EdDSA | v1 唔支援，留到 vNext（見 14）。 |

錯誤訊息只講邊個設定唔啱同要求（例如「RS256 key 最少 2048 bit」），唔會印出 key 內容。

實作狀態（main，2026-10-16 檢查）：

- 已實作：`vibe validate` 拒絕 ES256／RS256 以外嘅 `sign-jwt` alg；`sign-jwt` 執行時 alg 要同已載入 key 一致；`VIBE_JWT_ALG` 只接受 ES256、RS256；驗證時 header `alg` 要等於 key 嘅 alg（`none`、HS* 會被拒）；JWKS 只載入 P-256 同 ≥ 2048 bit 嘅 RSA key。
- RS256 簽名私鑰最少 2048 bit；ES256 簽名私鑰一定要係 P-256，私鑰純量 d 要喺 `1..n-1` 範圍內，編碼入面唔可以有多餘資料：`vibe serve` 啟動時檢查，唔符合就拒絕啟動（#222）。`vibe validate --deploy` 讀唔到執行環境嘅私鑰，所以唔檢查 key 大細或者曲線；呢個檢查只喺 `vibe serve` 啟動時做。
- 靜態驗證公鑰（`VIBE_JWT_PUBLIC_KEY_PEM`）：啟動時檢查 key 類型同設定 alg 一致，RSA 最少 2048 bit，ES256 一定要係 P-256 同有效點（P-384 等其他曲線拒絕啟動）（#224）。
- JWKS 驗證只用設定 alg（`VIBE_JWT_ALG`，預設 ES256）嘅 key：解析 JWKS 時就按 kty／crv／alg 篩走其他 key；同一個 kid 喺文件出現多過一次就成個 kid 都唔用（記錄警告）；結果同 key 次序無關。route 層 issuer 嘅 JWKS 暫時用同一個部署設定 alg（#225）。
- `sign-jwt` 冇寫 `alg` 時預設 ES256（#226）。

### 4.2 API key 模式（決定 43）

寫法：`auth: { mode: api-key, scopes: [users:read] }`。

| 項目 | 值 |
| --- | --- |
| Header | `X-API-Key: <key>`；唔接受 query string 或者 body 傳 key（出現就 400，而且唔寫 log） |
| 格式 | `mbk_{env}_{key_id}_{secret}`；`env` 係 `live` 或者 `test`；`key_id` 12 字元 base32（細楷）；`secret` 32 byte 隨機值嘅 base62 |
| Scope 語法 | `{definition}:{action}`，`action` 係 `read`、`write`、`admin`；`{definition}:*` 代表全部；`admin` 包含 `write`，`write` 包含 `read`；唔准全域 `*` |
| 儲存 | 只存 `HMAC-SHA-256(pepper, secret)`，pepper 喺平台 KMS；用 `key_id` 搵紀錄，再做固定時間比對 |
| 有效期 | 預設 90 日，上限 365 日 |
| 輪換 | 每個 client 最多 2 條有效 key；新 key 建立後，舊 key 重疊期預設 7 日，上限 30 日，到期自動失效 |
| Revoke | 平台 UI／CLI `api-key revoke {key_id}`；30 秒內全部 instance 生效；紀錄保留（UPDATE `revoked_at`，唔 DELETE） |
| 其他 | 記錄 `last_used_at`；key 同 secret 永遠唔寫 log；`env: test` 嘅 key 唔可以喺 live 環境用 |

- `auth` 成功同樣注入 `ctx.tenant_id`（由 key 綁定嘅租戶決定）同 `ctx.user`（`type: api-key`、`key_id`、`scopes`）。

### 4.3 `rate-limit` 參數 schema（決定 65）

| 參數 | 類型 | 必填 | 說明 |
| --- | --- | --- | --- |
| `per-minute`／`per-hour`／`per-day` | 正整數 | 三揀一 | 時間窗內最多次數；一個 step 只可以寫一個 |
| `key` | `ip` \| `tenant` \| `user` \| `api-key` \| `family` \| `input.{field}` | 是 | 計數鍵；`user` 要 `auth`；`family` 只限 `token-refresh`；`input.{field}` 會先 normalize 再 HMAC，唔儲存明文；email 欄位用決定 120 嘅正規化形式做 rate-limit key，其他欄位做 trim、lowercase |
| `burst` | 正整數 | 否 | 預設等於窗口上限，唔可以大過佢 |
| `fail` | 錯誤碼 | 否 | 預設 `RATE_LIMITED`（429，帶 `Retry-After`） |

- 計數用滑動窗口，按 `tenant_id` 分開儲存；`key: ip` 嘅 IP 由平台按受信任 proxy 設定解析，唔讀 client 自報 header。
- `key: ip` 網段 slot 上限（決定 130，同 request 次數分開）：計數前 IPv4-mapped IPv6 先轉做 IPv4；每個網段可以開嘅新計數器 slot 數目有上限：IPv4 每個（`tenant_id`、/24）10 個；IPv6 每個（`tenant_id`、/48）10 個，另加每個（`tenant_id`、/32）50 個。平台層另有跨租戶網段 slot 計數器（/24、/48），預設上限係租戶 slot 上限 ×5（`VIBE_RATE_PLATFORM_FACTOR`，預設 5，最大 100（已確認（產品經理 2026-10-16）））；兩層都通過先放行；平台層保證每個租戶 64 slot 保底。拒絕一律回 429 加 `Retry-After`，唔講係邊一層觸發。
- Slot 計數同 LRU 驅逐用 60 秒固定窗口（只限決定 130 呢部分）：先驅逐已過期項目；每個租戶有自己嘅 slot 上限；slot 滿而冇過期項目就 fail closed（429 加 metric）；永遠唔驅逐仲喺窗口內嘅計數器；多 instance 一定要用共用儲存（#157）。拒絕記 `vibe_rate_limit_slot_refused_total`（`layer`；reason：`network_cap`、`table_full`、`tenant_budget_exhausted`、`prefix_cap_exceeded`）（已確認（產品經理 2026-10-16））。Slot 表（同 #182 新設計（已確認（產品經理 2026-10-16）））：租戶表：每個租戶有自己一個 4,096 slot 嘅表，滿咗只拒絕嗰個租戶；全部租戶總預算 = `VIBE_RATE_EXPECTED_TENANTS`（預設 256，範圍 1–4096）× 4,096；用緊少過 64 個 slot 嘅租戶一定放行。平台表：IPv4 按 /24、IPv6 按 /32 分開嘅表，每個最多 4,096 項。平台表保證每個租戶嘅 64 slot 保底：平台總量 = 租戶總預算 + 租戶數 × 64，預設 1,064,960（256 × 4,096 + 256 × 64），所以就算好多租戶填滿平台表，新租戶都唔會冇 slot 可用。#73 嘅 shard 內 owner／reserve 上限照舊保留（已確認（產品經理 2026-10-16））。剩餘風險：(a) 固定窗口邊界前後，網段新 slot 數目最多可以去到上限 ×2（已接受）；(b) 平台層令一個租戶嘅流量可以用埋同一 /24 或者 /48 其他租戶嘅額度（已接受）；(c) 平台總量只有保底以上嘅部分係跨租戶共用；共用部分喺一個窗口內用晒嘅話，保底以上嘅新 slot 會被拒絕，保底照樣保證（已確認（產品經理 2026-10-16））；(d) 同時滿咗嘅租戶多過 `VIBE_RATE_EXPECTED_TENANTS`，就只有用緊少過 64 個 slot 嘅租戶可以開新 slot（已確認（產品經理 2026-10-16））；呢個情況一定記 metric reason `tenant_budget_exhausted`（已確認（產品經理 2026-10-16））並發 WARN 告警（每分鐘最多一次），唔可以靜默拒絕；(e) 最差情況每個表大約 300 MB 記憶體（估計，未實測）（已確認（產品經理 2026-10-16））；唔可以靠呢個估計：v1 一定要有硬記憶體上限 `VIBE_RATE_TABLE_MEMORY_MAX_BYTES`（預設 268435456，即 256 MiB，限 limiter 全部 slot 表合計；名同預設已確認（產品經理 2026-10-16）；預設刻意細過單一表最差估計（大約 300 MB），滿載時 limiter 會先到 `table_full`（fail closed），#195 實測後再調整預設），超過即以 reason `table_full` fail closed。記憶體上限預先保留保底份額：租戶數 × 64 × 3 KiB（每個保底 slot 一個租戶計數器、一個平台計數器同一個 bucket），256 個租戶即 48 MiB，最多佔上限一半。保底以內嘅增長對照完整上限檢查；其他增長對照「上限減保留」檢查，超過即 `table_full`。#195 實測：每個 slot 大約 481 B，上限按每個 slot 1 KiB 估算；**GA blocker：記憶體實測（#195）**；(f) 活躍租戶多過 `VIBE_RATE_EXPECTED_TENANTS` 時，就算喺保底以內嘅請求都可能撞到硬記憶體上限，以 `table_full` fail closed（已接受（產品經理 2026-10-16））；接受條件：活躍租戶超過 `VIBE_RATE_EXPECTED_TENANTS` 嘅 80% 就發告警 `rate_limit_active_tenants_high`；保底以內嘅 `table_full` 拒絕要喺 `vibe_rate_limit_slot_refused_total{reason="table_full"}` 用 `within_floor` label 分辨（或者只寫入內部 log），永遠唔出現喺任何回應；#195 實測要一併驗證呢個風險同保底記憶體預留嘅大小；營運調整見 4.3；(g) 一個 IPv6 /32 入面活躍 /48 太多、超過該 /32 嘅平台表上限時，嗰個 /32 自己嘅用戶會 fail closed（已確認（產品經理 2026-10-16））；呢個情況一定記 metric reason `prefix_cap_exceeded`（reason 名已確認（產品經理 2026-10-16））並發 WARN 告警（每分鐘最多一次），唔可以靜默拒絕。
- Test IP 覆寫（決定 130）：`VIBE_HONOR_TEST_IP=true` 開啟 `x-vibe-test-ip` header（只限測試嘅 client IP 覆寫），只喺 debug build 生效；release build 唔理呢個 header，設定咗就拒絕啟動（fail closed）。#182 已實作（#230）。
- 營運調整（決定 130）：活躍租戶接近 `VIBE_RATE_EXPECTED_TENANTS`（告警 `rate_limit_active_tenants_high`，超過 80% 觸發）時，可以：(1) 調高 `VIBE_RATE_EXPECTED_TENANTS`（最大 4096）：租戶總預算同平台總量跟住增加，記憶體保留 = 租戶數 × 64 × 3 KiB（256 個租戶 48 MiB；4096 個租戶 768 MiB），保留最多佔上限一半，所以可能要同時調高記憶體上限；(2) 調高 `VIBE_RATE_TABLE_MEMORY_MAX_BYTES`：保底以上可用記憶體增加，保留部分唔變。改完要重新啟動，並對照 #195 實測數字。
- 同一 endpoint 可以有多個 `rate-limit` step；任何一個超過就拒絕。

### 4.4 `## Order` 准用值（決定 65）

pipeline lock 嘅 `## Order` 只准一行 `must-run: {值}`：

| 值 | 意思 |
| --- | --- |
| `first` | 一定係第一個 pipeline（只限 `auth`，同埋取代 `auth` 嘅平台 pipeline） |
| `before-steps` | 一定喺所有 Logic step 之前，但喺 `first` 之後 |
| `after-steps` | 喺 Logic step 之後、`respond` 之前 |
| `last` | 回應之後執行（例如 `audit-log`）；唔可以改回應 |
| `any` | 冇限制，按 lock `## Pipelines` 次序 |

其他值，或者一個 lock 有兩個 `first`，驗證器報 error。

## 4A. 帳戶安全：Token、密碼同鎖定

### 4A.1 Access token 同 refresh token（決定 6）

- For the per-refresh-family limit, when the limit is exceeded the response is 401, consistent with an invalid token. IP limits remain 429 `RATE_LIMITED` with `Retry-After`.
- Access token：JWT，ttl 上限 1 小時。
- Refresh token：不透明隨機值（最少 256 bit），DB 只存 hash；ttl 上限 30 日（由家族第一次簽發計，絕對壽命），閒置 7 日冇用就失效。
- 每次用 refresh token 都輪換：發新 refresh token 同新 access token，舊 refresh token 即時失效。
- 同一次登入產生嘅 refresh token 屬同一個家族（`family_id`）；偵測到重用已失效嘅 refresh token，即時撤銷成個家族，並寫審計記錄、通知用戶。
- 寬限期（決定 61）：為咗容忍網絡重試，輪換後 `reuse-grace`（預設同上限 10 秒）內再用同一個舊 token，唔當重用。DB 只存 hash，平台冇可能回返上一次嘅明文，所以做法係：
  1. 用 `HMAC-SHA-256(pepper, token)` 搵到嗰列；佢 `used_at` 唔係 null、`now() - used_at <= reuse-grace`、`grace_reissued_at` 係 null、家族未撤銷。
  2. 喺同一個 transaction 撤銷佢嘅 `replaced_by` 子 token（`revoke_reason: grace-reissue`），再由同一個舊 token 發一個新子 token（同一個 `family_id`，`parent_id` 指舊 token），更新舊 token 嘅 `replaced_by`，設 `grace_reissued_at = now()`；`used_at` 唔改（寬限期唔會延長）。用 `update ... where grace_reissued_at is null` 做條件更新，並行請求喺第一個請求處理中出示同一 token，會收到可重試嘅 503 TX_CONFLICT（retryable: true，唔清 cookie）。第一個請求完成之後，喺寬限期（10 秒）內再出示，按寬限期規則處理；超過寬限期先當重用，撤銷整個 family。
  3. 回新 access token 同新 refresh token。家族唔變，絕對壽命唔變。
  4. 每個舊 token 最多重發一次。超過寬限期、或者 `grace_reissued_at` 已經有值（寬限期內第二次再用）、或者家族已撤銷、或者子 token 已經再被輪換過，一律當重用：撤銷成個家族。
  5. 可選：平台可以額外比對 client 指紋（例如 User-Agent hash）先准重發；v1 唔要求。
- 登出：revoke 當前家族；「全部裝置登出」revoke 該用戶所有家族。
- 清理（#95）：revoke 一律用 UPDATE；成個家族絕對壽命完咗超過 1 日之後，`vibe migrate`（table owner）會分批 DELETE 嗰個家族嘅所有列（每批 1000 列，每次最多 100 批）。生效中嘅家族永遠唔會被清理，所以重用偵測唔受影響。
- refresh 流程由平台內置 `token-refresh` pipeline 實現，用 `method` op 呼叫標咗 `capabilities: [sign-token]` 嘅 model method 簽新 access token。
- 簽發：`sign-jwt` op 嘅 `issue-refresh` 參數決定係咪同時發 refresh token：`new-family`（登入）、`rotate`（refresh，由 `token-refresh` pipeline 注入）、`none`；由平台按呼叫來源決定，method 唔可以自己揀 `rotate`。

### 4A.1a 瀏覽器 refresh cookie（決定 101，issue #78）

瀏覽器 client：access token 只放記憶體（唔放 cookie、唔放 localStorage），refresh token 放 cookie；重新載入頁面時 call refresh endpoint 攞新 access token。

- **Endpoint**：`POST /{definition}/{version}/{slug}/refresh`（`token-refresh`，`transport: cookie`）同 `POST /{definition}/{version}/{slug}/logout`（`token-revoke`）。Refresh 回 body `{ "access_token", "expires_in" }`，新 refresh token 只喺 `Set-Cookie`。
- **Cookie 屬性**：`__Host-vibe_rt_{slug}=<token>; HttpOnly; Secure; SameSite=<模式>; Path=/`，冇 `Domain`，`Max-Age` 唔超過家族剩餘絕對壽命（決定 143 收緊做同時唔超過閒置剩餘時間）。
  - `__Host-` 前綴保證 cookie 只屬於呢個 host、一定 Secure，防止 subdomain 寫入或者覆蓋 cookie；但瀏覽器規定 `__Host-` cookie 一定要 `Path=/`，所以唔可以再限定 `Path` 去 refresh endpoint。我哋揀 `__Host-`：`Path` 唔係安全邊界，而 `HttpOnly` 已經令 JS 讀唔到；server 只喺 refresh 同 logout 讀呢個 cookie，其他 endpoint 一律忽略。Cookie 名包括 slug，同一個 host 多個租戶唔會撞。產品經理已確認：防止 subdomain 覆蓋 cookie 比限定 Path 重要（2026-10-14）。
- **兩個模式**（`token-refresh` 參數 `cookie-mode`）：
  - **`same-site`（預設）**：`SameSite=Strict`。前端同 API 一定要同 site（例如 `app.example.com` 同 `api.example.com`）。
  - **`cross-site`（要明確開）**：`SameSite=None`，畀唔同 site 嘅前端用。一定要**同時**有：
    1. 嚴格 `Origin` 白名單（完全相等，唔准 `*` 或者 wildcard，最少一個 origin）；同埋
    2. 自訂 header CSRF 檢查：refresh 同 logout 一定要帶 `X-Vibe-CSRF: 1`，冇就回 403。自訂 header 令瀏覽器一定先做 CORS preflight，跨 site 表單或者簡單請求帶唔到。
  - 開咗 `cross-site` 但缺少任何一樣，驗證器報 error，`vibe manifest` 同部署一律失敗。
  - `same-site` 模式都會檢查 `Origin` 白名單；refresh 嘅 `X-Vibe-CSRF` 可選，cookie logout 一定要（決定 145）。
- **輪換同重用偵測**：跟 4A.1，每次 refresh 輪換；偵測到重用已失效 token（寬限期外）撤銷成個家族，同時回 `Set-Cookie` 清除 cookie。
- **CSRF**：refresh 同 logout 一定要有 `Origin` header，而且喺租戶設定嘅前端 origin 白名單入面（完全相等，唔准 wildcard）；冇 `Origin` 或者唔喺白名單回 403，唔讀 cookie。`same-site` 模式再加 `SameSite=Strict`；`cross-site` 模式再加 `X-Vibe-CSRF` header 檢查。只接受 `POST`。
- **CORS**：只對白名單 origin 回 `Access-Control-Allow-Origin: <origin>`（唔准 `*`）、`Access-Control-Allow-Credentials: true`、`Vary: Origin`；`cross-site` 模式 preflight 回 `Access-Control-Allow-Headers` 包括 `X-Vibe-CSRF`。Credentials **只限** refresh 同 logout endpoint，其他 endpoint 一律唔回 `Access-Control-Allow-Credentials`。
- **Logout**：撤銷當前家族，回 `Set-Cookie: __Host-vibe_rt_{slug}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=<模式>` 清除 cookie；cookie 無效都照樣清除，回 204。
- Refresh 回應帶 `Cache-Control: no-store`。

#### `token-refresh` pipeline 參數（決定 45）

| 參數 | 類型 | 預設 | 上限／說明 |
| --- | --- | --- | --- |
| `sign-method` | `{object}.{method}` | 必填 | 一定要有 `capabilities: [sign-token]` |
| `refresh-ttl` | duration | 30d | 上限 30d（家族絕對壽命） |
| `idle-ttl` | duration | 7d | 上限 7d |
| `reuse-grace` | duration | 10s | 上限 10s |
| `transport` | `body` \| `cookie` | body | cookie 時跟 4A.1a（決定 101 修訂決定 60） |
| `cookie-mode` | `same-site` \| `cross-site` | same-site | 只限 `transport: cookie`；`cross-site` 要同時有 `allowed-origins` 同 `X-Vibe-CSRF` 檢查，否則 build 失敗（4A.1a） |
| `allowed-origins` | origin 列表 | 必填（cookie） | 完全相等，唔准 `*` 或者 wildcard |

- 對應 endpoint 寫法：api feature 寫「呢個係 refresh token endpoint」，lock 會有 `pipelines: [-auth, token-refresh: {...}]`，`public: true`（因為冇 access token），租戶由 slug 決定；`token-revoke` 用喺登出 endpoint，要 `auth`。

- Refresh endpoint 分類（決定 72，已確認（產品經理 2026-10-14））：
  - `public: true`（因為冇 access token），租戶由 slug 決定。
  - 唔屬 public 寫入：用戶 lock 冇寫入 step，只有平台 `refresh_store` 寫平台表，所以唔計 7.2 嘅 public 寫入每日配額，亦唔需要 captcha。
  - 平台固定限流：IP 每分鐘 30 次、同一家族每分鐘 10 次；用 4.3 嘅 schema 寫入 lock。
    超過家族上限嘅回應按決定 147。
  - 最低回應耗時 300ms（同一般 public endpoint）。

#### `token-revoke` pipeline 參數（決定 65）

| 參數 | 類型 | 預設 | 說明 |
| --- | --- | --- | --- |
| `scope` | `family` \| `all` | `family` | `family`：撤銷當前 refresh token 所屬家族（登出）；`all`：撤銷該用戶所有家族（全部裝置登出） |
| `token-source` | `body` \| `cookie` | 同 `token-refresh` 嘅 `transport` | `scope: family` 時用嚟搵家族；`scope: all` 唔需要。`cookie` 時冇自己嘅 `allowed-origins`：一律用同一個 slug `token-refresh` 嘅決定 101 `allowed-origins`（決定 145） |
| `clear-cookie` | boolean | `true`（cookie 時） | 回應帶 `Max-Age=0` 清除 refresh cookie，`Path` 同 `token-refresh` 一樣 |

- 一定要 `auth`，只可以撤銷 `sub` 自己嘅家族。例外：`token-source: cookie` 加 `scope: family`（決定 145）唔使 `auth`，只撤銷嗰個 cookie 對應、屬於 URL slug 租戶嘅家族，並套用決定 72 限流同最低回應耗時；`revoke_reason` 分別寫 `logout`／`logout-all`。
- 搵唔到 token 或者已撤銷都回 204（idempotent，唔洩漏狀態）。

#### Refresh token 儲存

平台喺每個租戶 schema 自動建立 `_platform_refresh_tokens`（有 `tenant_id`、ENABLE＋FORCE RLS），用戶 lock 唔可以 query：

| 欄位 | 類型 | 說明 |
| --- | --- | --- |
| `id` | uuid | 主鍵 |
| `tenant_id` | string | 平台注入 |
| `family_id` | uuid | 同一次登入嘅家族 |
| `user_id` | uuid | subject |
| `token_hash` | bytes | `HMAC-SHA-256(pepper, token)`，唯一 |
| `parent_id` | uuid | 上一條（輪換鏈） |
| `issued_at` | timestamp | |
| `family_expires_at` | timestamp | 家族第一次簽發 + `refresh-ttl` |
| `idle_expires_at` | timestamp | 每次輪換更新 |
| `used_at` | timestamp | 被用嚟輪換嘅時間 |
| `grace_reissued_at` | timestamp | 寬限期重發嘅時間；非 null 代表已經重發過一次 |
| `replaced_by` | uuid | 新 token id |
| `revoked_at` | timestamp | |
| `revoke_reason` | string | `logout`、`logout-all`、`reuse-detected`、`grace-reissue`、`email-verified`、`erasure`、`admin` |

- Revoke：一律 UPDATE `revoked_at` 同 `revoke_reason`（家族一次過；唯一例外係寬限期重發只撤銷遺失嘅舊 child，`revoke_reason: grace-reissue`，決定 61），唔用 DELETE；過期紀錄唔刪除，冇個人資料（只有 hash 同 id），12.10 刪除時一併 revoke。
- 只有平台專用 `refresh_store` role 可以讀寫呢張表（見 7.1）。
- 欄位層 UPDATE grant（runtime role）：`pending_rotation`、`pending_until`、`used_at`、`replaced_by`、`grace_reissued_at`、`revoked_at`、`revoke_reason`；其他欄位只由 INSERT 寫一次。Guard trigger：identity 欄位唔可以改、撤銷係最終狀態、`used_at` 只設一次。（呢張表冇「最後 refresh 時間」欄位：`used_at` 已經記低每個 token 幾時被輪換；家族層面嘅最後 refresh 時間喺下面 `_platform_refresh_families`，決定 149。）

平台喺同一個租戶 schema 再建立 `_platform_refresh_families`（決定 149；有 `tenant_id`、ENABLE＋FORCE RLS，用戶 lock 唔可以 query），每個家族一行：

| 欄位 | 類型 | 說明 |
| --- | --- | --- |
| `family_id` | uuid | 主鍵，等於 `_platform_refresh_tokens.family_id` |
| `tenant_id` | string | 平台注入 |
| `user_id` | uuid | subject |
| `created_at` | timestamp | login（`new-family`）時寫 |
| `last_refreshed_at` | timestamp | 家族最後一次 refresh 成功（輪換或者寬限期重發）嘅時間；login 時等於 `created_at` |
| `revoked_at` | timestamp | 成個家族被撤銷嘅時間（`logout`、`logout-all`、`reuse-detected`、`email-verified`、`erasure`、`admin`）；生效中係 null。寬限期重發（`grace-reissue`）只撤銷單一 child，唔設呢欄 |

- INSERT：login（`issue-refresh: new-family`）喺插入家族第一條 token 嘅同一個 transaction 入面插入（`revoked_at` = null）。
- 家族撤銷：每次成個家族撤銷（上面 `revoke_reason` 除咗 `grace-reissue` 之外嘅所有原因），SDK 喺 UPDATE `_platform_refresh_tokens.revoked_at` 嘅同一個 transaction 入面 `UPDATE _platform_refresh_families SET revoked_at = now() WHERE family_id = … AND revoked_at IS NULL`，所以兩張表一齊 commit 或者一齊 rollback。寬限期重發唔會寫呢欄。
- 欄位層 UPDATE grant（runtime role）：只有 `last_refreshed_at` 同 `revoked_at`；冇 DELETE（家族紀錄跟 `_platform_refresh_tokens` 一樣由 `vibe migrate` 嘅過期清理批次刪除）。
- Guard trigger `_platform_refresh_family_guard`（同 #95 單向規則一致）：identity 欄位（`family_id`、`tenant_id`、`user_id`、`created_at`）唔可以改；`last_refreshed_at` 唔可以改做 null 或者早過舊值；`revoked_at` 單向，只可以由 null 改做有值（已經有值之後唔可以改值、唔可以改返 null）；家族行嘅 `revoked_at`（`OLD.revoked_at`）有值之後，`last_refreshed_at` 唔可以再改（家族撤銷係最終狀態）。凍結只睇家族行自己嘅 `revoked_at`，唔睇個別 token：寬限期重發撤銷單一 child（`_platform_refresh_tokens.revoked_at` 有值、`revoke_reason: grace-reissue`）唔計，所以唔會凍結。
- 查法：`select last_refreshed_at from _platform_refresh_families where family_id = …`（平台內部／admin 用；用戶 lock 唔可以 query 平台表）。

### 4A.1b Refresh cookie 補充細節（決定 141–148，issue #78，已確認（產品經理 2026-10-16））

> 本節已確認（產品經理 2026-10-16）。只描述防禦設計。

#### 流程

1. Login（`transport: cookie`）：回 body `{ "access_token", "expires_in" }`，同 `Set-Cookie: __Host-vibe_rt_{slug}=…; HttpOnly; Secure; SameSite=<模式>; Path=/; Max-Age=<秒>`；body 冇 `refresh_token`（決定 141）。
2. 頁面載入／access token 就到期：前端 `POST /{definition}/{version}/{slug}/refresh`，`credentials: "include"`，`cross-site` 模式加 `X-Vibe-CSRF: 1`。
3. 平台按決定 144 次序檢查，之後先讀 cookie，跟 4A.1 輪換；成功回 200、新 access token 同新 cookie（`Max-Age` 跟決定 143）。
4. 重用或者無效：回 401 `INVALID_REFRESH_TOKEN` 加清除 cookie；重用時撤銷成個家族（決定 142、101）。
5. Logout：`POST …/logout`，cookie 模式唔使 access token。先檢查 `Origin`（同 refresh 同一份決定 101 白名單）同 `X-Vibe-CSRF: 1`（`same-site` 都要），任何一項失敗回 403 `CSRF_REJECTED`；之後 token 只由 `__Host-` cookie 讀。通過之後一律 204 加清除 cookie：有效 cookie 撤銷佢嘅家族，冇 cookie 或者無效就乜都唔撤銷，回應一樣；套用決定 72 限流同最低回應耗時（決定 145）。

#### Auth pipeline markdown 寫法

Source（`api/auth/refresh.md`）用自然語言寫，frontmatter 宣告高風險欄位（決定 112）：

```markdown
---
kind: api
public: true
auth: refresh-cookie
---

# Refresh token

呢個係 refresh token endpoint，用 httpOnly cookie，前端同 API 同 site。
簽 token 用 user.sign-access-token。
```

編譯後 lock（節錄）：

```markdown
## Route

- method: POST
- path: /{definition}/{version}/{slug}/refresh
- public: true

## Pipelines

- -auth
- token-refresh:
    sign-method: user.sign-access-token
    transport: cookie
    cookie-mode: same-site
    allowed-origins: [https://app.example.com]
    refresh-ttl: 30d
    idle-ttl: 7d
    reuse-grace: 10s
```

`cross-site` 例子只改 `cookie-mode: cross-site`，`allowed-origins` 一定最少一個完全相等嘅 origin；平台自動要求 `X-Vibe-CSRF`，唔使喺 lock 再寫。

Logout（節錄）：

```markdown
## Route

- method: POST
- path: /{definition}/{version}/{slug}/logout
- public: true

## Pipelines

- -auth
- token-revoke:
    scope: family
    token-source: cookie
    clear-cookie: true
```

驗證器 error：`transport: cookie` 冇 `allowed-origins`；`token-revoke` 自己宣告 `allowed-origins`（cookie logout 一律繼承同一個 slug `token-refresh` 嘅設定，決定 145）；`allowed-origins` 有 `*`、wildcard、path 或者唔係 `https`（dev build 除外）；`cross-site` 但冇 `allowed-origins`；`token-revoke` `scope: all` 但係 `public: true`；`token-revoke` `token-source: cookie` 但同一個 definition 嘅 `token-refresh` 唔係 `transport: cookie`。

#### 錯誤碼（決定 147）

| 狀態 | 錯誤碼 | 幾時 | 清 cookie |
| --- | --- | --- | --- |
| 401 | `INVALID_REFRESH_TOKEN` | 只適用於 refresh：冇 cookie、無效、過期、已撤銷、重用、超過每個 refresh 家族嘅限流（回應同無效 token 逐 byte 完全一樣，唔帶 `Retry-After`，最低回應耗時 300ms，唔撤銷家族；決定 147）。Cookie logout 喺同樣情況唔回 401，一律 204 加清 cookie（決定 145） | 係 |
| 403 | `CSRF_REJECTED` | 冇 `Origin`、唔喺白名單；refresh 喺 `cross-site` 冇 `X-Vibe-CSRF: 1`；cookie logout 喺任何模式（包括 `same-site`）冇 `X-Vibe-CSRF: 1`（決定 145）。冇 cookie 唔係 403（logout 回 204，refresh 回 401）。詳細原因只入 server log 同審計 | 否（只有通過檢查嘅請求先會清 cookie） |
| 405 | `METHOD_NOT_ALLOWED` | 唔係 `POST` | 否 |
| 429 | `RATE_LIMITED` | 只適用於 IP 限流（決定 72），帶 `Retry-After` | 否 |
| 503／504 | `TX_CONFLICT`／`TX_TIMEOUT` | 決定 100 | 否 |

所有回應帶 `Cache-Control: no-store`；body 同 log 永遠唔記錄 token 明文。

#### 向後相容（決定 148）

- `transport: body` 照舊係預設，現有 lock、SDK client 同 login 回應唔變。
- Cookie 係 opt-in，經新 `{version}` 遷移；每個 endpoint version 只接受一種 transport。
- Login 同 refresh 嘅 body 都用 `access_token`（現有 login 已經係咁）。
- 決定 60（slug Path）只喺 body 模式冇作用；cookie 模式跟決定 101 `Path=/`。

#### Cookie logout 必要測試（決定 145）

三項檢查各自單獨失敗（其餘兩項正確，有一個有效 cookie）各一個測試：

1. `Origin` 唔喺 `allowed-origins`（或者冇 `Origin`）：403 `CSRF_REJECTED`，家族冇被撤銷，回應冇 `Set-Cookie`。
2. 冇 `X-Vibe-CSRF: 1`（`same-site` 同 `cross-site` 各測一次）：403 `CSRF_REJECTED`，家族冇被撤銷，回應冇 `Set-Cookie`。
3. Token 唔係由 `__Host-vibe_rt_{slug}` cookie 嚟（issue #253 拆做兩個 case）：
   - 3a. 冇 `__Host-vibe_rt_{slug}` cookie，淨係 body 有 `refresh_token`（或者用其他 cookie 名）：204 加清除 cookie，同無效 cookie 回應完全一樣，body／其他 cookie 嘅家族冇被撤銷。
   - 3b. 有效 `__Host-vibe_rt_{slug}` cookie，加一個屬於另一個家族嘅 body `refresh_token`：204 加清除 cookie；只有 cookie 所屬嘅家族被撤銷，body token 嘅家族冇被撤銷、仍然可以 refresh。

另外：冇 cookie、無效、過期、已撤銷同有效 cookie 嘅 204 回應（status、header、body）完全一樣，同樣受最低回應耗時限制；重複 logout 兩次回應一樣；只有嗰個 cookie 嘅家族被撤銷；其他租戶嘅家族（slug 唔啱）唔會被撤銷；超過決定 72 IP 限流回 429；有效、無效同冇 cookie 喺相同請求次數下，觸發 IP 限流嘅時機、status、body、header 同回應耗時完全一樣，cookie logout 唔套用家族限流（決定 145 (5)）。

#### Cookie logout 限流 key（決定 145 (5)，產品經理 2026-10-16 批准）

> 本小節已由產品經理 2026-10-16 批准（issue #253）。

- Cookie logout 只套用決定 72 嘅 IP 限流（每分鐘 30 次），計法同 refresh 一樣，唔分 cookie 有效、無效定冇 cookie。
- Cookie logout **唔套用**決定 72 嘅家族限流（每分鐘 10 次）。呢個係決定 72 嘅例外，只適用於 cookie logout；refresh 照舊套用 IP 同家族兩個限流。
- 原因：冇 cookie 或者無效 cookie 冇家族 id；唔用家族限流，429 就唔會因為 cookie 有效與否而唔同。
- 最低回應耗時 300ms 不變（決定 72）；429 回應（status、body、header 包括 `Retry-After`）同回應耗時，喺有效、無效同冇 cookie 之間完全一樣。
- 必要測試：分別用有效、無效同冇 cookie 嘅請求，喺同一個 IP 連續送請求，觸發 IP 限流嘅次數、429 status、body、header 同回應耗時都一定完全一樣；有效 cookie 嘅請求唔會因為家族限流提早收到 429。

#### 實作狀態（對照 main，2026-10-17）

- **main 已實作**：`sign-jwt` 簽 access token，login body 回 `access_token`（`src/runtime/ops.rs`）；sqlguard 拒絕 app SQL 觸及 `_platform_refresh_tokens`（`src/sqlguard.rs`）。
- **main 已實作（#95，Refs #26）**：body transport 嘅 `token-refresh`／`token-revoke`、`_platform_refresh_tokens` 儲存、輪換、寬限期、重用偵測同 revoke；`sign-jwt issue-refresh`；`rate-limit key: family`；錯誤碼 `INVALID_REFRESH_TOKEN`（決定 147）。
- **main 未實作**：cookie transport（決定 101、141–148）；決定 149（`sign-method` 唯讀、READ ONLY 兜底、`_platform_refresh_families.last_refreshed_at`）。

### 4A.1c Refresh `sign-method` 唯讀（決定 149，issue #270，產品經理已批准 2026-10-17）

決定 149 解決 #270：`sign-method` 冇用戶寫入，一次 refresh 只會有兩種結果：(1) 輪換成功；(2) 輪換失敗，舊 token 照決定 61 仍然係 current。對外錯誤碼照決定 61、147，只有下面 DB 兜底一種情況用 500 `INTERNAL`。

**靜態檢查（validate、build、serve）：**

- 只計 step 同 `op: method` 嘅寫入：`sign-method` 所指嘅 method，`## Signature` 一定 `writes: false`，而且冇任何 `writes: true` step。
- 間接寫入一樣阻擋：跟 #166 嘅繼承規則（10.8），經 `op: method`（任何深度）呼叫到有寫入嘅 method，`sign-method` 就算有寫入；`op: method` target 未知或者格式錯，當寫入處理（fail closed）。
- `sign-jwt` 嘅 `issue-refresh` 由平台寫 `_platform_refresh_tokens`（login 時 `new-family`、refresh 時 `rotate`），唔係 step 寫入，唔計；所以同一個 method（例如 12.8 嘅 `user.sign-token`）可以照用喺 login。
- `vibe validate` 同 `vibe validate --deploy` 報 error，`vibe manifest` 失敗，`vibe serve` 拒絕載入帶住呢類 manifest 嘅部署（啟動失敗）。
- 錯誤碼 `REFRESH_SIGN_METHOD_WRITES`。訊息一定要講：邊個 lock 檔案、邊個 `sign-method`、邊個 method／step 寫入（間接寫入要列出 `op: method` 呼叫鏈），同埋點樣修正。範例：

```text
error[REFRESH_SIGN_METHOD_WRITES]: api/auth/v1/refresh/refresh.lock.md: token-refresh sign-method `user.sign-token` writes
  write: models/user/methods/touch-login.lock.md step s2 (writes: true), reached via user.sign-token -> op: method user.touch-login
  fix: sign-methods must be read-only (decision 149). Move the write into a separate write method that the client calls after the refresh succeeds, then set `writes: false` in ## Signature. The last refresh time is recorded by the platform per family (_platform_refresh_families.last_refreshed_at).
```

**DB 層兜底（serve，只限 refresh 路徑）：**

- `token-refresh` 執行 `sign-method` 期間，佢所有 DB statement（包括每個 `query` step 同 `platform-claims-source` 嘅 roles 查詢）一律以 READ ONLY transaction 執行，唔理 step 嘅 `writes` 標記；唔要求合併成單一 transaction，method executor 每個 step 各自開 transaction 嘅結構唔使改。Login 等其他路徑唔受影響。
- 違反 READ ONLY（PostgreSQL SQLSTATE `25006`）係設定錯誤，重試都唔會成功：對外回非重試嘅 500 `INTERNAL`（唔係 503 `TX_CONFLICT`），輪換唔 finalize，舊 token 仍然 current，server log 寫 error（唔寫 token 或者 secret 明文）。
- 唔可以為咗呢個兜底延長 family lock 持有時間超過現有 `statement_timeout`／`lock_timeout`；pool 細（例如 size 1）都唔可以 deadlock（#95 review）。

**最後 refresh 時間（家族層面 `last_refreshed_at`，平台寫入）：**

- 語意：「呢個 session（家族）最後幾時 refresh 成功」。以家族為單位，唔寫喺個別 token 行：舊（被出示嘅）token 行喺輪換時已經設 `used_at`，再加一個欄位幾乎重複；寫喺家族一行，查一個 session 只使讀一行。
- 新表 `_platform_refresh_families`（見 4A.1「Refresh token 儲存」）由 `vibe migrate` 用 `CREATE TABLE IF NOT EXISTS` 建立，連同 grant 同 guard trigger；現有家族由 migrate 補一行（`last_refreshed_at` = 家族入面最遲嘅 `used_at`，冇就用最早 `issued_at`；`revoked_at` = 家族入面 `revoke_reason` 唔係 `grace-reissue` 嘅 token 最早嘅 `revoked_at`，冇就 null；`ON CONFLICT DO NOTHING`，可以重複行）。
- SDK 喺輪換確認（finalize）嘅同一個 transaction 入面，`UPDATE _platform_refresh_families SET last_refreshed_at = greatest(last_refreshed_at, now()) WHERE family_id = …`；寬限期重發（決定 61）都喺佢嘅 finalize transaction 入面同樣更新。所以佢同輪換一齊 commit 或者一齊 rollback，用戶 method 唔使寫。
- 寬限期重發時，撤銷嘅只係遺失嘅舊 child（`_platform_refresh_tokens` 嗰行 `revoke_reason: grace-reissue`），唔係成個家族，唔會設 `_platform_refresh_families.revoked_at`；trigger 只睇家族行嘅 `revoked_at`，所以唔會撞到「家族撤銷之後唔可以改」；而且新值一定唔早過舊值（同決定 61 一致）。成個家族撤銷（例如重用偵測、`logout`）會喺同一個 transaction 設家族行 `revoked_at`，之後 `last_refreshed_at` 就凍結。
- 欄位層 UPDATE grant：`_platform_refresh_families` 只有 `last_refreshed_at` 同 `revoked_at`；`_platform_refresh_tokens` 嘅 grant 列表唔變（同 #95 一樣）。
- 同 #95 已批准嘅單向 guard trigger 一致：`last_refreshed_at` 只可以向前（唔可以係 null 或者早過舊值）；`revoked_at` 只可以由 null 改做有值；家族行 `revoked_at` 有值之後 `last_refreshed_at` 唔可以再改（單一 child 嘅 `grace-reissue` 撤銷唔計）；identity 欄位唔可以改。`vibe serve` 啟動時檢查兩張表嘅 grant 同 trigger，唔符合就拒絕啟動（fail closed）。
- 升級次序：因為新表、grant 同 trigger 都由 `vibe migrate` 建立，而 `vibe serve` 啟動檢查會要求佢哋存在，升級之後一定要先對每個租戶行 `vibe migrate`，之後先 `vibe serve`。

**範例：唯讀 sign-method 加獨立寫入 method：**

完整、可以照抄嘅版本就係 repo 入面嘅 fixture `tests/fixtures/refresh-session`，由 `tests/refresh_sign_method_example.rs` 嘅 `refresh_session_example_passes_validate_and_build` 測試（`vibe compile` → `vibe validate` → `vibe validate --deploy --dev` → `vibe manifest --dev`）。以下檔案全部係 structured source（`source-type: structured`），`vibe compile` 唔使 LLM 就會產生對應嘅 `.lock.md`（lock 嘅 frontmatter，例如 `source-hash`，係由 compile 計出嚟，唔好手寫）。之前喺 sign-method 入面做嘅業務寫入，搬去一個普通寫入 method，由 client 喺 refresh 成功之後用新 access token 另外呼叫。

`models/user/user.md`（model 要有 `last_device`；`id`、`tenant_id`、`created_at` 等 system 欄位由 compile 自動加）

```markdown
# user

## Table

- provider: main
- table: users

## Fields

| name | type | rules | system |
| --- | --- | --- | --- |
| email | string | required, unique, max: 254 | no |
| roles | string[] | - | no |
| last_device | string | max: 50 | no |

## CRUD

| method | options |
| --- | --- |
| get | - |

## Policies

- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
```

`migrations/main/0001-users-last-device.md`（現有 `users` 表加 `last_device`）

```markdown
# users last_device

## Target

- provider: main
- seq: 0001

## Up

### s1

- sql: ALTER TABLE users ADD COLUMN IF NOT EXISTS last_device text

## Verify

(none)

## Data

(none)
```

`models/user/methods/sign-token.md`（唯讀 sign-method：`writes: false`，只有讀取、`sign-jwt` 同 `respond`；`sub` 一定要嚟自 query 結果）

```markdown
# sign-token

## Signature

- input: {sub: uuid}
- output: {access_token: string, refresh_token: string}
- errors: []
- writes: false
- capabilities: [sign-token]

## Calls

(none)

## Steps

### s1

- op: query
- table: users
- sql: select id::text as id from users where id::text = $1
- params: [input.sub]
- writes: false
- out: u

### s2

- op: sign-jwt
- alg: ES256
- key-ref: JWT_SIGNING_KEY
- claims: {sub: u.id}
- ttl: 1h
- issue-refresh: platform-decided
- out: tok

### s3

- op: respond
- value: tok
```

`models/user/methods/record-session.md`（普通寫入 method；用戶 id 嚟自 runtime 嘅 `ctx.user.sub`，step 只可以用 `ctx.user.sub`／`ctx.user.roles`／`ctx.user.tenant`）

```markdown
# record-session

## Signature

- input: {device: string}
- output: (none)
- errors: []
- writes: true

## Calls

(none)

## Steps

### s1

- op: query
- table: users
- sql: update users set last_device = $1 where id::text = $2
- params: [input.device, ctx.user.sub]
- writes: true
- scope: pk
- out: r

### s2

- op: respond
- value: {}
```

`api/auth/v1/refresh/refresh.md`（`token-refresh` route，sign-method 係上面唯讀嘅 `user.sign-token`）

```markdown
---
body: 4KB
---
# refresh

## Route

- method: POST
- path: /auth/v1/{slug}/refresh

## Access

- public: true
- public-writes: false
- tenant: path.slug
- min-response: 300ms
- external-calls: forbidden
- challenge: { type: none, when: always }

## Pipelines

### -auth

- removed: true

### token-refresh

- sign-method: user.sign-token
- refresh-ttl: 30d
- idle-ttl: 7d
- reuse-grace: 10s
- transport: body

## Limits

- provider: main
- timeout: 10s
- body: 4KB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
| body.refresh_token | string | - | yes |

## Response

- status: 200

## Transform

- in: {}
- out: result

## Steps

(none)

## Errors

(none)
```

`api/auth/v1/session/session.md`（要 JWT；呼叫 `user.record-session`。標題要提到寫入：compile 嘅 semantic check 要求有 `write-sql` 嘅 source 提到 write／update 等字）

```markdown
---
body: 4KB
---
# session (writes users.last_device through user.record-session)

## Route

- method: POST
- path: /auth/v1/session

## Access

- public: false

## Pipelines

(none)

## Limits

- provider: main
- timeout: 10s
- body: 4KB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| body.device | string | max: 50 | yes |

## Response

- status: 204

## Transform

- in: {device: req.body.device}
- out: result

## Steps

### s1

- op: method
- target: user.record-session
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 204

## Errors

| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
```

Fixture 仲有 `providers/main.md`、`api/auth/_definition.md`（`auth` pipeline `mode: jwt`）、`api/auth/v1/_version.md` 同 `slugs.example.json`，同 `tests/fixtures/shop` 嘅寫法一樣。`vibe compile` 會為上面每個檔案產生 `.lock.md`（包括 `api/auth/v1/session/session.lock.md`），本地開發用 `vibe validate --deploy --dev` 驗證之後，再用 `vibe manifest --dev` build。

Client 流程：`POST …/refresh` 成功攞到新 access token 之後，再 `POST /auth/v1/session`。兩個 request 唔係 atomic：session 寫入失敗唔影響已經完成嘅輪換，client 可以重試；呢個寫入同 refresh token 狀態無關。

**必須有嘅測試：**

1. 直接寫入：`sign-method` 有 `writes: true` step 或者 `## Signature` `writes: true`，validate、build、serve 都報 `REFRESH_SIGN_METHOD_WRITES`，訊息有檔案、method、step 同修正方法。
2. 間接寫入：`sign-method` 經 `op: method`（包括兩層或以上）呼叫有寫入嘅 method，三者都報錯，訊息列出呼叫鏈；未知 target 都報錯。
3. 唯讀 `sign-method` 照常通過；同一個 method 用喺 login（`issue-refresh: new-family`）唔受影響。
4. 上面範例（fixture `tests/fixtures/refresh-session`：唯讀 `user.sign-token` 做 sign-method，加 `user.record-session` 同 `api/auth/v1/session`）通過 `vibe validate` 同 build（`vibe manifest`）：`tests/refresh_sign_method_example.rs` 嘅 `refresh_session_example_passes_validate_and_build`（已經喺 CI 行）。同一個檔案嘅 `refresh_session_example_writing_sign_method_is_rejected`（sign-method 加一個寫入 step，要報 `REFRESH_SIGN_METHOD_WRITES`）暫時 `#[ignore]`，由決定 149 嘅 code PR 啟用。
5. DB 兜底：繞過靜態檢查（測試專用 manifest）令 refresh 期間 `sign-method` 執行寫入 SQL，PostgreSQL 以 `25006` 拒絕，回 500 `INTERNAL`、輪換冇 finalize、舊 token 仍然 current、冇寫入殘留；login 路徑唔套用 READ ONLY。
6. `_platform_refresh_families.last_refreshed_at`／`revoked_at`：login 插入家族行（`revoked_at` null）；輪換同寬限期重發都喺 finalize transaction 更新 `last_refreshed_at`；finalize rollback 時唔變；寬限期重發（只撤銷舊 child，`grace-reissue`）唔設家族 `revoked_at`，而且照樣可以更新 `last_refreshed_at`（包括家族入面已經有 `grace-reissue` 撤銷嘅 token 之後）；成個家族撤銷（重用偵測、`logout`、`logout-all` 等）喺同一個 transaction 設家族 `revoked_at`，rollback 時兩張表都唔變；trigger 拒絕 `last_refreshed_at` 改細、改做 null、改 identity 欄位、`revoked_at` 改值或者改返 null，同家族 `revoked_at` 有值之後再改 `last_refreshed_at`；`_platform_refresh_tokens` 嘅 grant 列表冇變；serve 啟動檢查兩張表嘅 grant 同 trigger；`vibe migrate` 為現有家族補行而且可以重複行。
7. 現有 `tests/refresh_tokens.rs` 用緊會寫入嘅 sign-method（`app_with_issuance_control(_, true)`、`rejected_tokens_do_not_run_sign_method_writes`），實作決定 149 時要改寫做上面第 1、2 同 5 項嘅「靜態拒絕」同「READ ONLY 兜底」測試。

**遷移（breaking）：** 現有有寫入嘅 `sign-method` 要拆開：(1) 「最後 refresh 時間」之類嘅資料唔使再寫，改用平台嘅家族層面 `_platform_refresh_families.last_refreshed_at`；(2) 其他業務寫入搬去一個普通寫入 method，由 client 喺 refresh 成功之後另外呼叫（見上面範例）；`token-refresh` route 冇「refresh 之後」嘅 step，唔可以喺 refresh endpoint 入面做。`sign-method` 只保留讀取同 `sign-jwt`，`## Signature` 設 `writes: false`，再重新 build。升級 SDK 之後，一定要先對每個租戶行 `vibe migrate`（建立 `_platform_refresh_families`、grant 同 trigger），之後先 `vibe serve`；否則 serve 啟動檢查會拒絕啟動。

**#270：** 決定 149 實作 merge 之後，#270 可以 close。選項 B 延後到 vNext。

### 4A.2 密碼政策（決定 7）

- 平台底線：最少 12 個字元，最多 256 個字元；冇強制複雜度規則；一定做洩漏密碼檢查。
- 洩漏密碼檢查用 k-anonymity range query：只送密碼 SHA-1 嘅頭 5 個 hex 字元，開啟 padding（`Add-Padding: true`），令回應大小唔洩漏結果；比對喺本地做，明文同完整 hash 唔會離開平台。
- 洩漏密碼服務用唔到（timeout 或者錯誤）時 fail closed（已確認（產品經理 2026-10-14））：只影響註冊同改密碼，**登入唔受影響**（登入唔做洩漏檢查）；用短 timeout（例如 2 秒）；觸發告警；回 503，帶 `Retry-After`。
- `hash-password` op 喺 hash 之前強制檢查底線：長度唔喺 12–256 字元之內 fail `WEAK_PASSWORD`（422）；喺洩漏密碼清單搵到 fail `BREACHED_PASSWORD`（422，#90）。
- 錯誤回應、log 同錯誤訊息一律唔可以回顯密碼（明文、hash 或者 hash 前綴都唔得）。
- Argon2id 參數（決定 46）：memory 64 MiB（65536 KiB）、iterations 3、parallelism 1、salt 16 byte（CSPRNG，每個密碼唔同）、輸出 32 byte；用 PHC 字串格式儲存（`$argon2id$v=19$m=65536,t=3,p=1$<salt>$<hash>`），所以參數跟住 hash 走。
- 每個 instance 同時最多 4 個 argon2id 運算，超出排隊；`hash-password` 同 `verify-password` 都係 `cost: high`。
- 用戶唔存在時嘅假 hash 用同一組參數，令耗時一致。
- Rehash：登入成功後，如果儲存嘅 PHC 參數同而家平台參數唔同，平台用專用 `credential_rehash` role 自動 rehash 同 UPDATE password 欄位（只限該用戶、而且 WHERE 包括舊 hash 防競爭）；呢個寫入係平台內部動作，唔使 lock 宣告，唔計 public 寫入配額，寫審計記錄。
- 平台調整參數只可以加強，唔可以減弱。
- 物件只可以收緊（例如 `min: 16`），唔可以放寬；lock 寫低過底線就 error。

### 4A.3 帳戶鎖定同限流（決定 8、38）

- 同一帳戶 15 分鐘內登入失敗 5 次，鎖 15 分鐘。
- 同一帳戶或者同一 IP 15 分鐘內失敗 3 次之後，之後嘅登入一定要 captcha（防止被人用嚟惡意鎖死帳戶）。
- 鎖定期間，用戶可以經 email 重設密碼連結即時解鎖；鎖定同解鎖都寫審計記錄並通知用戶。
- 鎖定期間回應同密碼錯完全一樣（401 `INVALID_CREDENTIALS`），耗時都補到 500ms，唔洩露帳戶狀態。
- 另加 IP 限流：每分鐘 10 次登入（見 11.2 `login-guard`）；request 次數用滑動窗口；另外每個網段新計數器 slot 數目按決定 130 限制（IPv4 /24、IPv6 /48 加 /32，加平台層跨租戶網段 slot 上限，固定窗口）。

## 5. 資料庫提供者 `providers/{name}.md`

source 用自然語言描述，例如：

```markdown
# main
主資料庫，用 PostgreSQL，每個租戶一個 schema。
連線資料用 secret PG_MAIN。唔共用。
每個租戶連線池最多 5 條，每條 statement 最多行 5 秒。
migration 放喺 ./migrations/main/。
```

編譯後嘅 lock 欄位：

| 欄位 | 說明 |
| --- | --- |
| `type` | 平台登記嘅 driver；v1 預設 `postgres`，可以替換 |
| `isolation` | `schema` 或者 `database` |
| `connection.secret` | 只可以引用本 project namespace 嘅 secret |
| `shared` | 預設 false |
| `allowed-endpoints` | shared 時必填，唔可以留空 |
| `limits` | `pool-per-tenant`、`statement-timeout` |
| `migrations` | migration 目錄 |

- 連線資料唔可以寫明文；source 入面出現疑似密碼或者連線字串，編譯直接報 error。
- database 模式下，每個租戶用嘅憑證由平台維護嘅「租戶對 secret 對照表」決定，唔可以喺 secret 名入面嵌 `tenant_id`；用戶只可以透過 CLI 管理呢個對照表。
- `tenant_id` 格式必須符合 `^[a-z0-9]{8,32}$`；平台要先驗證租戶存在，先至去對照表攞憑證。
- 所有 provider 都強制有租戶隔離同 RLS，冇開關可以關；source 寫「唔使隔離」都唔會生效，仲會報 error。
- `shared: true` 嘅 provider：
  - 只可以讀，連線 role 只有 SELECT 權限。
  - 一定要寫 `allowed-endpoints`，而且唔可以留空。
  - 部署報告會列出所有用緊 shared provider 嘅 endpoint。

## 6. 引用 provider

- `_definition` 宣告 provider allowlist 同預設 provider（lock `## Providers`）。
- `_version` 同個別 feature 只可以喺 allowlist 入面揀 provider。
- method 入面嘅 `query` 用邊個 provider，由物件 lock 嘅 `## Table` 靜態決定，寫死喺 method lock 每個 query step 嘅 `provider`；唔可以用變數，亦唔可以由 request 決定。
- 有 query 嘅 endpoint（包括經 method 間接）一定要解析到一個 provider，而且要喺嗰個 endpoint 嘅 allowlist 入面。

## 7. 租戶隔離

### 7.1 通用規則

- 每個 request 喺一個 runtime 管理嘅 transaction 入面執行（決定 99，見 7.4）：由該租戶（同 role 類別）嘅連線池攞一條以租戶 role 登入嘅連線，`BEGIN`，按次序執行 lock SQL，最後 `COMMIT`（決定 97 補充，見下）。
- 每張表都一定要有 `tenant_id` 欄位，要 `ENABLE` 同 `FORCE ROW LEVEL SECURITY`。
- RLS policy 由編譯器按租戶產生常數 policy，`TO tenant_<id>_rw`（同其他租戶 role），租戶 id 係常數，唔用 `current_setting('app.tenant_id')`；部署時比較編譯出嚟嘅 policy 同 DB 實際安裝嘅 policy，唔一致即 fail closed（決定 129）；`app.tenant_id` GUC 只可以用嚟 log 同診斷，**永遠唔會出現喺任何 policy 或者權限判斷**；租戶邊界係租戶 `LOGIN` role 加佢自己 schema（決定 97）。冇切換到租戶 role 嘅話，query 會因為冇權限直接報錯，唔會回空結果。
- `search_path` 由平台固定產生：schema 模式係 `t_{tenant_id}`，唔包括 `public`；`public` schema 嘅 CREATE 權限要收走。
- 連線 role 唔可以係表嘅 owner，亦唔可以有 `BYPASSRLS`。
  - SDK 實作（決定 97）：冇共用 runtime role。每個租戶有自己嘅 migration role `tenant_<id>_owner`（擁有自己 schema，只畀 migrate 用，只以 `NOINHERIT` 做自己租戶嘅 `tenant_<id>_rw` 成員（2B backfill／決定 129 嘅受控例外，見 7.1），唔係任何其他 role 嘅成員；決定 129），`vibe migrate` 用佢喺該租戶 schema 跑 migration，再將 `SELECT`、`INSERT`、`UPDATE` 逐張表授權畀該租戶嘅 `LOGIN` role（`tenant_<id>_rw` 等），收走 `CREATE`；唔用 `ALTER DEFAULT PRIVILEGES` 跨租戶授權。跑完 migration 之後，schema 入面每張表都會 `ENABLE` + `FORCE ROW LEVEL SECURITY` 同埋加 `TO` 該租戶 role、寫死租戶 id 常數嘅 policy；冇 `tenant_id` 欄嘅表、view、materialized view 或 foreign table 會令 migrate 失敗；migrate 亦檢查 runtime role 唔係 owner 同每張表有 `FORCE ROW LEVEL SECURITY`，缺任何一樣即失敗（決定 129）。`vibe serve` 為每個（租戶、role 類別）用租戶 `LOGIN` role 開連線池，啟動時檢查每個租戶 role：係 superuser、有 `BYPASSRLS`、係任何其他 role 嘅成員或者有其他 role 係佢成員（唯一受控例外見下）、擁有任何 schema、relation、function 或 type、又或者可以存取其他租戶 schema 或者冇 `ENABLE` + `FORCE ROW LEVEL SECURITY` 嘅 relation，就拒絕啟動。
- 所有 runtime DB role 類別（`app_rw`、`public_reader`、`public_writer`；每個係**每個租戶各自一個** role 類別，例如 `tenant_<id>_rw`，見下面租戶身份）都冇 `DELETE`、`TRUNCATE`、DDL 權限（決定 27）。
- 平台專用 role（用戶 lock 唔可以用）：

  | role | 權限 | 用途 |
  | --- | --- | --- |
  | migration `tenant_<id>_owner`（每個租戶一個，決定 129） | 擁有自己租戶 schema 同表；只喺自己 schema 准用嘅 DDL（見 2B）；冇 BYPASSRLS，只以 `NOINHERIT` 做自己租戶嘅 `tenant_<id>_rw` 成員（2B backfill／決定 129 嘅受控例外，見 7.1），唔係任何其他 role 嘅成員。Owner 天生可以 TRUNCATE／DROP，係決定 27 嘅受控例外（已確認（產品經理 2026-10-16））：migrate 以外 `NOLOGIN`、一次性憑證、parser 正面白名單、單一 transaction、finally 收回登入、定期檢查、全程審計（見 2B「Owner role 條件」） | 只用嚟執行 migration；runtime 永遠唔用 |
  | `credential_rehash` | 只有 password 類型欄位嘅 UPDATE | 4A.2 rehash |
  | `refresh_store` | `_platform_refresh_tokens` 同 `_platform_refresh_families` 嘅 SELECT、INSERT、UPDATE（後者只可以 UPDATE `last_refreshed_at` 同 `revoked_at`，決定 149） | 4A.1 |
  | `erasure` | 個人欄位嘅 UPDATE | 12.10 |
  | `mail_worker` | `email_outbox` 嘅 SELECT、`UPDATE(sent_at)`；匿名化 job 用 `UPDATE(email, token, recipient_hash)` | 12.11 發送同 outbox 保留期（決定 54、63、68） |

  全部都冇 DELETE、TRUNCATE、BYPASSRLS，全部行為寫審計記錄。

- 租戶身份（決定 97；登入方式補充 已確認（產品經理 2026-10-14））：
  - 每個租戶、每個 role 類別（`app_rw`、`public_reader`、`public_writer`）一個 DB role：`tenant_<id>_rw`、`tenant_<id>_public_reader`、`tenant_<id>_public_writer`（決定 129；`<id>` 係系統產生、只有 `[a-z0-9_]` 嘅不透明 id，唔用用戶提供嘅名；role 名超過 63 bytes 即拒絕，唔截斷）：`LOGIN`、`NOINHERIT`、只有自己 schema（database 模式：自己 database）嘅權限，唔係任何其他 role 嘅成員，亦冇其他 role 係佢嘅成員（唯一受控例外：同一租戶 `tenant_<id>_owner` 以 `NOINHERIT` 做 `tenant_<id>_rw` 成員，見下）。
  - **揀直接登入，唔用 role 切換**：如果 runtime 連線 role 係租戶 role 嘅成員再切換過去，呢個做法冇辦法保證單向。產品經理決定：除非之後證明 PostgreSQL 17 嘅交易內 role 切換可以做到不可逆，否則一律用每個租戶 `LOGIN` role 嘅連線池（預設設計）。所以 runtime 為每個（租戶、role 類別）開獨立連線池，直接用租戶 role 登入；連線入面冇任何更高權限身份可以切返，身份切換語句一律冇效或者報錯。
  - 租戶 role 嘅登入憑證由平台產生同輪換，只存喺 runtime 嘅 secret store；唔寫入 repo、lock、manifest、log 或者錯誤訊息。Production 用 secret manager 參照；`VIBE_TENANT_DB_SECRETS` JSON 檔只限開發同測試（權限 0600、唔 commit、唔寫 log），production 見到呢個檔即拒絕啟動（決定 129）。
  - 連線池按需要開；每個（租戶、role）5 條，每個租戶最多 15 條，另有全 process 上限（預設 200，可以設定）。超過上限即刻回 503 `DB_POOL_EXHAUSTED` 加 `Retry-After` 並發 metric，唔排隊。閒置連線池 5 分鐘後關閉（阻 GA，唔阻 v1 preview）。PgBouncer 用 per-user pool（決定 129）。
  - 受控 membership 例外（已確認（產品經理 2026-10-16），決定 129、2B backfill）：`tenant_<id>_owner` 可以以 `NOINHERIT` 做同一租戶 `tenant_<id>_rw` 嘅成員，等 backfill 用 `SET LOCAL ROLE`。條件：(1) 啟動檢查用正面條件寫：每個租戶剛好有一行 membership，`tenant_<id>_rw` ← `tenant_<id>_owner`（同一個 `<id>`），選項係 `ADMIN FALSE`、`INHERIT FALSE`、`SET TRUE`（PostgreSQL 16 或以上）；反方向同其他任何 membership（包括跨租戶、額外或者連鎖／間接）一律拒絕，啟動失敗。(2) Owner 喺 migrate 以外照舊 `NOLOGIN`，有定期偏差檢查（決定 129）。(3) 必須測試：owner 做其他租戶 rw 嘅成員、INHERIT、有 ADMIN OPTION，三種情況啟動都失敗。實作狀態：未實作，#147 嘅啟動檢查要調整，#246。
  - Runtime 自己嘅管理連線（例如 mail worker、清理 job）用平台專用 role，唔執行 lock SQL，亦唔係租戶 role 嘅成員。
- **平台表**（`_platform_refresh_tokens`、`_platform_idempotency`、`email_outbox` 等）只喺租戶自己 schema；**唔准有任何跨租戶共用而租戶 role 讀得到嘅表**。租戶 role 對平台表只有同一個 transaction 必需嘅最少權限（例如 outbox 同 idempotency 嘅 INSERT），而 lock SQL 一律唔可以引用平台表名（parser 檢查，見 7.1a），只可以經平台 op 存取。
- 連線池同 cache 都按租戶分開；cache key 一定要包含 `tenant_id`。
- 每個租戶都有 CPU、記憶體、並發同 DB 連線上限。
- database 模式下同樣要加 RLS，做縱深防禦。
- 以上全部由平台產生，記錄喺 model lock 嘅 `## Policies`；source 唔可以改。

### 7.1a Lock SQL 白名單同租戶隔離測試（決定 97、98）

- 驗證器（部署前）同 runtime（載入 manifest 時）都用 PostgreSQL 解析器（例如 `pg_query`）解析每句 lock SQL，唔靠字串比對：
  - 只准單一 DML statement（`SELECT`、`INSERT`、`UPDATE`；`DELETE` 跟 2A.4 唔准）。
  - 禁止 transaction 控制、身份切換語句、會話設定修改、prepared statement 同通知類語句（parser 按 statement node 類型判斷，正面清單以外一律拒絕）。
  - 唔可以有任何會話設定修改，唔分邊個設定。
  - 關係引用用正面規則：parser 拒絕每一個唔係解析到租戶自己 schema 入面准用 relation 嘅 `RangeVar`／`TABLE` node（包括 set operation 入面嘅 `TABLE x` 寫法、子查詢同 CTE 入面嘅引用）；平台表同 system catalog 自然唔喺准用清單。
  - 禁止 schema-qualified 名稱（表、view、function 一律唔准寫 schema），名稱只經平台固定嘅 `search_path` 解析。
  - Function 白名單係**正面清單**：只准平台列明嘅 function（例如常用嘅聚合、字串、日期、數學 function），冇列就禁止。以下類別一定唔會入白名單：動態 SQL／XML 查詢函數、introspection 同讀設定、會話設定修改、取檔／外連函數、large object。
  - Type 同 cast 都係正面清單：只准平台列明嘅 type；所有 `reg*` object identifier type（例如 `regclass`、`regproc`、`regtype` 一族）嘅 cast 同字面值一律禁止，白名單以外嘅 type cast 一律拒絕。
  - 解析失敗、解析器版本同 PostgreSQL 版本唔對應、或者遇到唔認識嘅 node，一律失敗（fail closed）。
- Conformance 測試一定要包括跨租戶負面測試：用租戶 A 嘅身份執行，確認讀唔到、寫唔到、改唔到租戶 B 嘅資料同設定；呢啲測試唔可以跳過。負面測試最少覆蓋以下類別（已確認（產品經理 2026-10-14））：
  - 喺租戶連線入面嘗試切換身份或者改 session 設定，一定失敗。
  - Lock SQL 用唔喺白名單嘅 function（動態 SQL／XML 查詢函數、introspection、取檔／外連函數）、唔喺白名單嘅 type cast（包括 `reg*`）、或者引用准用清單以外嘅 relation（包括 `TABLE x` 寫法），驗證器同 runtime 都拒絕。
  - 讀其他租戶或者平台表（包括 refresh token、idempotency、outbox），一定冇權限。
  - Migration 嘗試建立提權 function 或者 trigger、改 role、改 GUC，驗證器拒絕；就算繞過驗證器，DB 權限都會擋。
- 呢節只寫要求；具體攻擊方式同修正細節經私人 security advisory 處理，唔會寫喺公開文件。

### 7.2 Public endpoint

- 租戶要自己開啟 `public_access`（opt-in）；冇開啟嘅租戶，所有 public 請求都回 404。
- 租戶來源寫成 `tenant: { from: path, param: slug }`。slug 係平台發出嘅不透明值，唔係 `tenant_id`，而且可以輪換。
- 租戶唔存在、冇開 `public_access`、或者冇資料，都一律回同一個 404 body；回應時間會補到固定最低值（一般 public endpoint 300ms，login 類 500ms；login 類即係有 `verify-password` step 嘅 endpoint），等人唔能夠靠耗時分辨；如果實際處理超過呢個值就照實際時間回，部署報告會列出 p95 超過固定值嘅 public endpoint，只出 warning（決定 36）。
- public 請求一律強制按 IP 限流，亦可以再加按租戶限流。
- 讀取：public 請求用 `public_reader` role，只有 SELECT 權限，配合 public RLS policy（只睇到 `is_public` 嘅資料，或者 method 明確宣告嘅公開範圍）。
- 寫入（決定 4 取消舊限制，v1.4 收緊）：public endpoint 可以 call `writes: true` 嘅 method，但全部以下規則都強制：
  - **開關**：`public_write_access` 同 `public_access` 分開，只可以由租戶 admin 喺平台 UI 開啟；冇 API、CLI 或者 source 可以開（決定 26）。組織 admin 可以喺 UI 一次揀多個租戶開／關，要二次確認同寫原因，每個租戶各自寫審計記錄（決定 41）。冇開嘅租戶，public 寫入回同一個 404。
  - **逐個 endpoint opt-in**：feature lock `## Access` 寫明 `public-writes: true`，係高風險安全項目。
  - **Role**：寫入用獨立 `public_writer` role，grant 由平台按 lock 自動產生，只限 lock 實際用到嘅表同**欄位**。
  - **操作**：只准 `INSERT` 同 `UPDATE`；兩者都只可以寫 lock `## Access` 嘅 `writable-columns` 明確列出嘅欄位（決定 22）。
  - **欄位黑名單**：平台維護敏感欄位黑名單，public 寫入一律唔准寫，例如 `role`、`roles`、`permissions`、`is_admin`、`tenant_id`、`owner`、`owner_id`、`created_by`、`approved`、`verified`、`password`（例外見下面「註冊」）、`deleted_at`、所有系統欄位；物件亦可以自己加黑名單。
  - **UPDATE**：一定要有 `WHERE`，用主鍵或者 RLS 限定，而且 RLS 生效。
  - **租戶**：`tenant_id` 由平台按 slug 解析後注入；request body 入面有 `tenant_id`、`id`、`created_by` 一律 422 拒絕（決定 23）。
  - **配額**：每租戶每日 public 寫入配額強制，預設 1,000 次，另每 IP 每日 50 次；租戶 admin 可以調低，調高最多到 10,000 要經平台申請（決定 39）；用完之後只封鎖 public 寫入（回 429 同 `Retry-After`），已認證請求同 public 讀取照常（決定 21）。
  - **限流**：IP 加租戶兩層限流強制（request 次數用 4.3 滑動窗口；網段新計數器 slot 上限、平台層同固定窗口驅逐見決定 130）；lock 標 `cost: high` 嘅昂貴 step（例如 `verify-password`、`hash-password`、大範圍 query）自動用更嚴嘅限流，即係 endpoint 上限嘅一半（決定 25、39）。
  - **防機器人**（決定 49）：lock `## Access` 用統一語法 `challenge: { type: none | captcha | pow, when: always | after-failures, after-failures: <n> }`。
    - 建立資料（有 INSERT）嘅 public endpoint：一定要 `type: captcha` 或者 `pow`，`when: always`（v1 只支援 `captcha`，`pow` 係 build error，見 7.2a 決定 131）。
    - login 類 endpoint：一定要 `{ type: captcha, when: after-failures, after-failures: 3 }`（跟 4A.3）。
    - 其他冇 INSERT 嘅 public 寫入：可以 `type: none`。
    - Challenge token 單次使用狀態（#157，產品決定）：production 有設定 verifier 時一律用 PostgreSQL replay store；表喺每個租戶自己 schema（`t_<tenant>.vibe_challenge_replay`，有 `tenant_id`、ENABLE + FORCE RLS，owner role 擁有，由該租戶嘅 `vibe migrate` 建立），冇跨租戶共用表（決定 97）。runtime role 只有 SELECT、INSERT，冇 DELETE／UPDATE（決定 27）：預留同結果都係追加記錄，讀取忽略過期記錄，過期記錄只由 owner role 喺 `vibe migrate` 刪除。Fail closed 唔依賴清理：清理冇做或者延遲，只會令 store 較早滿，滿咗按決定 134 回 429，永遠唔會放行未記錄 replay 嘅 request（見 #193）。容量上限同 quota 按租戶計；client IP 用每租戶 key 嘅 HMAC 儲存。表唔存在、用唔到或者 runtime role 權限多過 SELECT／INSERT 就拒絕啟動；production 設 `VIBE_CHALLENGE_REPLAY_STORE=memory` 亦拒絕啟動。dev／CI 一定要明確揀 `memory` 或者 `postgres`，冇預設值。v1 captcha provider 係 Cloudflare Turnstile。
  - **註冊（password 寫入）**（決定 47）：public 路徑寫 password 類型欄位只准一種方式：
    - method 標 `capabilities: [set-password]`（高風險安全項目），而且寫入 password 欄位嘅值一定係同一 method 內 `hash-password` op 嘅輸出；直接用 request 值寫 password 欄位一律 error。
    - api lock `## Access` 寫 `password-write: allow`，`writable-columns` 明確列出 password 欄位。
    - 三個條件缺一不可：method 有 `set-password`、值嚟自同一 method 嘅 `hash-password`、api 有 `password-write: allow`。captcha 唔係 password 寫入嘅條件：只有 public INSERT（例如註冊）先一定要 captcha（`when: always`）；冇 INSERT 嘅 password 寫入（例如 12.11.7 用 token 設定密碼）可以 `type: none`，但照樣要 IP 加租戶限流。
    - 照樣要每日配額同 audit-log；回應唔可以分辨 email 係咪已經註冊（一律 202 加統一訊息，經 email 完成驗證，見決定 56，已確認）。
  - **Email 驗證**（決定 53，已確認）：`email_verified_at` 只可以由標 `capabilities: [confirm-verification]` 嘅 method 寫，一個 UPDATE 入面用 token hash 做 WHERE、檢查未過期同未驗證、同時清走 token hash；api lock 寫 `verification-write: allow`。冇 INSERT，所以 challenge 可以 `type: none`，但照樣 IP 加租戶限流同 audit-log。
  - **審計**：public 寫入一律經 `audit-log`，唔可以剔走。
  - **Untrusted 內容**：public 寫入嘅欄位喺 model 標記 `untrusted: true`；所有輸出（包括已認證 endpoint 讀返出嚟）都按 Content-Type 做 output encoding；每個欄位要有長度上限，陣列要有數量上限，冇上限就 error（決定 24）。
  - **對外 call**：public 路徑（public endpoint 直接或者經 method 間接）預設禁止 `call`；要開要逐個 endpoint 喺 lock 寫 `external-calls: allow`，屬高風險安全項目。
  - 部署報告列出所有 public 寫入 endpoint 同佢哋嘅可寫欄位，並出 warning。

### 7.2a Challenge 驗證：Turnstile（決定 131–138，issue #160，已確認（產品經理 2026-10-16））

> 本節已確認（產品經理 2026-10-16），包括 replay store 滿回 429 同每個網段上限（決定 134，2026-10-16 補充）。實作細節見 [`challenge.md`](challenge.md)；共用 replay store 見 #177。

#### 宣告語法

Lock `## Access`（決定 49 語法，v1 `type` 只可以 `none` 或者 `captcha`；`pow` 係 build error）：

```markdown
## Access

- public: true
- challenge: { type: captcha, when: always }
```

Login 類：

```markdown
- challenge: { type: captcha, when: after-failures, after-failures: 3 }
```

已認證 endpoint opt-in（預設冇）：source frontmatter 加 `challenge: captcha`，lock 會有：

```markdown
## Access

- auth: required
- challenge: { type: captcha, when: always }
```

Source 用自然語言寫「要 captcha」，frontmatter 一樣要宣告 public 同寫入（決定 112）。前端 widget 嘅 `data-action` 一定等於 endpoint 名（feature id 最後一段，最多 32 個 `[A-Za-z0-9_-]` 字元）。

#### 處理次序

1. 限流同 public 寫入配額（決定 137）；超過即 **429** `RATE_LIMITED`（同 challenge 嘅 503 分開）。
2. 冇 `x-challenge-proof` → 400 `CHALLENGE_REQUIRED`。
3. Replay store 檢查（已確認（產品經理 2026-10-16 12:44 HKT），#258），次序固定，遇到第一個唔通過就停：
   1. 純輸入檢查（唔使 store）：token 格式（空或者超過 2048 字元）→ 403 `CHALLENGE_FAILED`。
   2. Replay store 連唔到或者出錯 → **503** `CHALLENGE_UNAVAILABLE` 加 `Retry-After: 5`（唔係 403）；fail closed，唔會放行。
   3. Token 重用 → 403 `CHALLENGE_FAILED`。
   4. 會超過每 IP／每租戶未驗證預留上限（決定 137）→ 403 `CHALLENGE_FAILED`。
   5. 會超過決定 134 容量上限：網段超過上限 → 429 `CHALLENGE_RATE_LIMITED` 加 `Retry-After`（該租戶喺嗰個網段最舊紀錄嘅剩餘有效時間，上限 360 秒，計唔到就 60 秒）；store 滿 → 429 `CHALLENGE_RATE_LIMITED` 加 `Retry-After`（該租戶自己最舊紀錄嘅剩餘有效時間，上限 360 秒，計唔到就 60 秒）；唔記錄 replay 就唔放行（決定 134）。
   6. 預留 token 紀錄。`bind: request` route 另外有 nonce 紀錄，次序同交易規則見 7.2b「Server 驗證次序」。
4. Siteverify（決定 132）→ token 無效 403；provider／網絡失敗（包括 replay store 暫時連唔到）503 加 `Retry-After: 5`（fail closed，決定 134）。
5. 通過之後先執行 lock steps。

#### 錯誤碼（決定 135）

| 狀態 | 錯誤碼 | 幾時 |
| --- | --- | --- |
| 400 | `CHALLENGE_REQUIRED` | 冇 proof |
| 403 | `CHALLENGE_FAILED` | token 無效：偽造、重用、過期、時間錯、hostname／action 唔啱、預留上限 |
| 429 | `RATE_LIMITED` | 限流或者配額（唔關 challenge 事），帶 `Retry-After` |
| 429 | `CHALLENGE_RATE_LIMITED` | Replay store 滿或者網段超過上限（決定 134）；回應除咗 `Retry-After` 之外完全一樣，唔包含 reason；`Retry-After` = 最舊紀錄剩餘有效時間（store 滿：該租戶自己最舊紀錄；網段：該租戶喺嗰個網段最舊紀錄），上限 360 秒，計唔到就 60 秒（網段 `Retry-After` 已確認（產品經理 2026-10-16））；fail closed；metric `vibe_challenge_rejected_total{reason="replay_store_full"}`、告警 `challenge_replay_store_full` |
| 503 | `CHALLENGE_UNAVAILABLE` | 只限 provider 或者網絡失敗：唔可達、超時、回應唔啱、replay store 暫時連唔到；固定 `Retry-After: 5` |

設定錯誤（冇 verifier、冇共用 replay store、`type: pow`、未實作 provider）喺 build 或者啟動時已經擋咗，production 唔會以 503 出現。

#### 固定數值（決定 137）

| 項目 | v1 值 | Override |
| --- | --- | --- |
| Token 壽命 | 300s | 冇；將來只可以調低 |
| 每 client IP 未驗證預留 | 16 | 冇；將來只可以調低 |
| 每租戶未驗證預留 | 1,000 | 冇；將來只可以調低 |
| 503 `Retry-After` | 5s | 固定 |

#### 設定（決定 136）

v1 challenge provider 係 **Cloudflare Turnstile**（決定 131），由 server 端 siteverify 驗證（決定 132）。設定只經環境變數：

| 變數 | 預設 | 意思 | Production 規則 |
| --- | --- | --- | --- |
| `VIBE_CHALLENGE_PROVIDER` | `turnstile` | v1 只接受 `turnstile`；其他值（例如 `hcaptcha`）未實作，啟動時拒絕 | 同 dev 一樣 |
| `VIBE_TURNSTILE_SECRET` | 冇（未設定 = verifier 未設定） | Turnstile secret key；只由環境變數／secret manager 注入，永遠唔 log、唔入 source、lock、manifest 或者 commit 嘅 `.env` | 有任何 challenge route 就一定要有，否則 `vibe serve` 拒絕啟動、`vibe validate --deploy` 失敗 |
| `VIBE_CHALLENGE_HOSTNAMES` | 冇 | 逗號分隔 widget hostname；siteverify 回應嘅 `hostname` 一定要喺入面 | 有 secret 就一定要有 |
| `VIBE_CHALLENGE_REPLAY_STORE` | Production：`postgres`；其他環境冇預設，一定要明確設定 | `postgres`（每租戶一份、所有 instance 共用，用 `VIBE_DATABASE_URL`）或者 `memory`（per process，只限 dev／CI） | 一定係 `postgres`；設 `memory` 或者冇 PostgreSQL 即拒絕啟動 |

`VIBE_ENV` 決定係咪 production（同 2C.4 一致）。固定數值（token 壽命 300s、每 IP 16、每租戶 1,000、503 `Retry-After: 5`）係平台常數，冇環境變數或者租戶 override（決定 137）。`type: pow` 喺 build 時已經係 error（決定 131）。

Production 一定用共用 Postgres replay store（#177），冇就拒絕啟動；Turnstile 本身嘅單次使用檢查（`timeout-or-duplicate`）只係額外一層。

#### Replay store 數值（同 main 嘅 code 一致）

| 項目 | 值 |
| --- | --- |
| 已驗證 token 保留時間（replay TTL） | 360s（token 壽命 300s + 60s clock skew 邊際） |
| 被拒 token 保留時間（rejected hold） | 10s |
| 未有答覆嘅預留（provider 唔可用，正常 settle） | 即時釋放 |
| Request 中斷、未 settle 嘅 pending 預留（pending hold） | 30s |
| Token 長度上限 | 2048 字元 |
| Siteverify timeout | 5s |
| 每個網段（IPv4 /24、IPv6 /48）每租戶持有上限 | 16（同決定 137 每個 IP 16 個未驗證預留一致）；超過回 429 `CHALLENGE_RATE_LIMITED`（內部 reason `replay_prefix_cap`，只入 metric／log）；`Retry-After` = 該租戶該網段最舊紀錄剩餘有效時間，上限 360s，計唔到就 60（網段 `Retry-After` 已確認（產品經理 2026-10-16）） |
| 告警 | 使用率超過 80%：`challenge_replay_store_high_usage`；store 滿：`challenge_replay_store_full`（metric reason `replay_store_full`） |
| Metric | `vibe_challenge_rejected_total{reason}`（例如 `reason="replay_store_full"`、`"replay_prefix_cap"`）；reason 只入 metric 同內部 log，永遠唔出現喺 HTTP 回應 |
| 容量上限 | Postgres store：每租戶 10,000；memory store（只限 dev／CI）：每 process 100,000；滿咗回 429（決定 134） |

#### 實作狀態（對比 `main` 4b85de5）

以下決定係產品經理已確認嘅 spec 目標，**唔會因為 code 未做而放寬**；未做嘅部分標明，跟進 issue 見下表（#200–#205）。

| 決定 | 狀態 | 差異 |
| --- | --- | --- |
| 134 啟動閘門 | **Implementation status: not yet implemented**（部分） | `production_gate` 仍然只檢查 `public` 而且 `when: always` 嘅 verifier 設定；未驗證嘅 `after-failures`／已認證 opt-in 改由 #199 `unverified_gate` 喺 production 拒絕（見下一列）。follow-up issue #200（verifier 覆蓋範圍）。 |
| 133／#199 未驗證 challenge | **Implemented**（production 拒絕；runtime 未驗證） | Runtime 而家只驗證 public route 嘅 `when: always` challenge。`when: after-failures`（login 類）同非 public（已認證）route 宣告嘅 challenge 未驗證，所以 production 下 `vibe serve` 同 `vibe validate --deploy` 會拒絕（就算已設定 verifier）；dev／CI 照樣接受。Runtime 驗證呢啲 mode 見 #201。 |
| 133 Runtime 執行範圍 | **Implementation status: not yet implemented**（部分） | `src/runtime/http.rs` 只對 `public` 而且 `when: always` 嘅 route 做 challenge；`after-failures`（login）同已認證 opt-in route 而家唔會驗證 challenge。follow-up issue #201。 |
| 135 `Retry-After: 5` | **Implementation status: not yet implemented** | `src/runtime/http.rs` `check_challenge` 回 503 `CHALLENGE_UNAVAILABLE` 時冇 `Retry-After` header。follow-up issue #202。 |
| 131 `type: pow` build error | **Implementation status: not yet implemented** | `src/validate.rs` 冇呢個檢查；而家係 runtime 回 503。follow-up issue #203。 |
| 134 503 範圍 | **Implementation status: not yet implemented**（部分） | Production 冇 verifier 嘅 `when: always` public route 已經喺啟動擋；但 runtime 對「冇 verifier」同 `type: pow` 仍然回 503（應該喺 build／啟動擋）。Replay store 容量滿而家回 503，決定 134（已確認（產品經理 2026-10-16））要求 429 加 `Retry-After`、metric 同告警；每個網段持有上限（IPv4 /24、IPv6 /48，預設 16，429 reason `replay_prefix_cap`）、metric `vibe_challenge_rejected_total{reason}`、告警 `challenge_replay_store_high_usage`（超過 80%）同 `challenge_replay_store_full`（滿）亦未做；所有呢類 429 一律 `CHALLENGE_RATE_LIMITED`、回應除 `Retry-After` 外一致、唔包含 reason，亦未做：not yet implemented，follow-up issue #204。 |
| 136 Turnstile secret 掃描 | **Implementation status: not yet implemented** | `src/secret.rs` 只比對已設定嘅 LLM key 同 `sk-`／`AKIA` 格式；冇比對 `VIBE_TURNSTILE_SECRET` 嘅值，亦冇 Turnstile secret 格式規則，所以而家捉唔到。follow-up issue #205。 |

### 7.2b Challenge 綁定 request digest（決定 140，issue #156，已確認（產品經理 2026-10-16））

> **狀態**：已確認（產品經理 2026-10-16）。方向（token 綁 request digest；session 綁定留到 vNext）同詳細 spec（#243 嘅 7 個 review 點）已確認。**演算法修正**（query raw `+` 無效；per-request nonce）亦已確認（產品經理 2026-10-16 11:58 HKT）。
>
> **Implementation status: not yet implemented**：`main` 而家未有 request digest 計算、nonce 檢查同 `cdata` 比對，code 未實作；實作跟 #156。

#### 目的

決定 132 驗證 token 嘅 `hostname`、`action`、時間同單次使用；本節再要求每個 token 綁定佢為咗嘅嗰一個 request。Client 喺送出前計好 request digest，經 widget `data-cdata`（Turnstile `cdata`）傳入；server 由收到嘅 request 獨立重新計算，同 siteverify 回應嘅 `cdata` 比較，唔一致即拒絕。Digest 唔經任何 client header 傳送：server 只信自己重新計算嘅值同 siteverify 回應。

#### Lock 語法

```markdown
- challenge: { type: captcha, when: always, bind: request }
```

`bind` 准用值：`request`（本節）、`none`（決定 132 現行行為）。新 lock 預設 `request`；過渡期安排見下面「推出」。

適用範圍：所有 `type: captcha` route，包括 login 類 `when: after-failures`（決定 133）：只要嗰個 request 要求 challenge，token 就一樣要綁定 request digest，規則同 `when: always` 完全一樣。

#### Request nonce

- Client 每個 request 用 CSPRNG（例如 `crypto.getRandomValues`）產生新嘅 128 bit（16 byte）隨機 nonce，經 header `X-Vibe-Request-Nonce` 送畀我哋嘅 server；協定唔會將 nonce 放入任何送畀 challenge provider 嘅欄位（`cdata` 只放 digest）。
- 格式嚴格：base64url（RFC 4648 §5）、冇 padding、**剛好 22 個字元** `[A-Za-z0-9_-]`，而且一定要係 canonical 編碼（decode 做 16 byte 再 encode 返要等於原值）。其他任何值（長度唔啱、有 padding、有其他字元、header 重複）即無效。
- 冇 nonce 或者 nonce 重用一律 fail closed（403 `CHALLENGE_FAILED`）。
- 單次使用：nonce 同 token 一齊喺現有 replay store（決定 132、#177）記錄（只存 SHA-256），有效期同 token 紀錄一樣（replay TTL 360s）；同一個 nonce 喺有效期內再出現即無效。
- **唯一性範圍**：每租戶。Replay store 嘅 nonce key 係 `(tenant_id, nonce)`（喺租戶自己嘅 schema，同決定 132 replay store 一樣），只喺同一個租戶入面檢查重用。128 bit 隨機值喺唔同租戶之間撞中嘅機會可以忽略；用每租戶範圍可以保持租戶之間嘅隔離，唔使跨租戶查詢。
- **計數**：每個 request 剛好佔**一個** nonce 紀錄（key `(tenant_id, nonce)`），另加決定 132 原有嘅 token 紀錄。Nonce 紀錄計入決定 134 嘅每租戶容量同每個網段（IPv4 /24、IPv6 /48）上限；因為佢喺驗證之前預留，亦計入決定 137 嘅未驗證預留上限（每 client IP 16、每租戶 1,000）。冇另外、冇上限嘅空間。
- **超過上限**（已確認（產品經理 2026-10-16 12:19 HKT））：
  - Nonce 紀錄會令決定 137 未驗證預留上限超過 → 403 `CHALLENGE_FAILED`。
  - Nonce 紀錄會令決定 134 容量上限（每租戶 store 滿、每網段上限）超過 → 429 `CHALLENGE_RATE_LIMITED` 加 `Retry-After`，規則同決定 134 完全一樣（`Retry-After`：store 滿就用該 tenant 自己最舊紀錄嘅剩餘有效時間；網段超額就用該 tenant 喺嗰個網段最舊紀錄嘅剩餘有效時間；上限 360 秒，計唔到就用 60 秒）。呢個 429 回應除咗 `Retry-After` 嘅值之外，同其他所有決定 134 嘅 429 完全一樣，唔透露內部原因係 nonce；reason 只入 metric 同內部 log。
  - 所有驗證失敗（包括 nonce 格式錯、同一個 nonce 重用）一律 403，永遠唔係 429。
- **檢查次序**（已確認（產品經理 2026-10-16 12:44 HKT），#258）（固定、deterministic）：① nonce 同 canonical 格式錯（403，唔使 store）→ ② replay store 唔可用（503 `CHALLENGE_UNAVAILABLE` 加 `Retry-After: 5`，fail closed，唔放行）→ ③ 重用（403）→ ④ 決定 137 未驗證預留上限（403）→ ⑤ 決定 134 容量上限（429）。遇到第一個唔通過就停。原因：純輸入檢查唔使 store，所以錯嘅輸入即刻 403；要用 store 而 store 唔可用，就做唔到重用檢查，所以回 503；容量 429 只會畀本身合法嘅 request。
- **兩個紀錄、一個交易**（已確認（產品經理 2026-10-16 12:44 HKT），#258）：先檢查 nonce，再預留 token 紀錄；兩個預留喺同一個交易入面做，一個失敗就成個 rollback，唔會留低任何半套紀錄（冇單獨 nonce 或者單獨 token 紀錄）。所以一個 request 要有 2 個空位先可以通過；只得 1 個空位就當超過上限。被拒嘅 nonce 唔會佔 token 位。
- **兩個紀錄都計入上限**（已確認（產品經理 2026-10-16 12:44 HKT），#258）：兩個紀錄都計入上限（每個 request 2 個）：決定 134 每租戶容量同每網段上限、決定 137 每 IP 同每租戶未驗證預留上限。如果數值太緊，喺 #256 調整數值；計數方法唔變。

#### 私隱考慮

`cdata` 會送畀 challenge provider。如果 digest 冇 salt，provider 可以對低熵 body（例如 login 嘅 email 同常見密碼）逐個試出內容。所以 canonical string 包括 client 產生、只送畀我哋 server 嘅 nonce：協定送畀 provider 嘅 `cdata` 只有 digest（`rd1.` 加 43 字元），唔包括 nonce、body、body hash 或者 canonical string。Nonce 防嘅係單憑 `cdata` 做離線估算：冇 nonce 就冇辦法由 digest 推返 body。佢唔係對所有可以睇到 request 或者頁面嘅一方嘅保護；邊啲地方睇得到 nonce，見下面「剩餘風險」。

#### 涵蓋範圍

Digest 一律涵蓋 nonce、method、path、正規化 query 同 body；涵蓋嘅 header **只有** `Content-Type` 同 `If-Match`（加 nonce header 本身），冇其他 header。Canonical string 由以下 8 行組成，順序固定，每行之間用一個 LF（`0x0A`）分隔，最後一行之後**冇** LF：

| 行 | 內容 |
| --- | --- |
| 1 | 固定版本標記 `vibe-request-digest-v1` |
| 2 | `nonce:` 加 `X-Vibe-Request-Nonce` 原值（22 字元） |
| 3 | HTTP method，原樣（一定係大寫 token，見下面） |
| 4 | 正規化 path（包括 tenant slug，所以 digest 已經綁定租戶） |
| 5 | 正規化 query；冇 query 就係空行 |
| 6 | `content-type:` 加正規化 `Content-Type` 值；冇呢個 header 就只有 `content-type:` |
| 7 | `if-match:` 加正規化 `If-Match` 值；冇呢個 header 就只有 `if-match:` |
| 8 | Body 嘅 SHA-256，64 個小寫 hex 字元 |

唔涵蓋：`Host`（由決定 132 `hostname` 檢查）、`x-challenge-proof` 本身、其他 header（包括 cookie、`Authorization`、`Idempotency-Key`）、client IP（**唔包括**，已確認（產品經理 2026-10-16）；流動網絡會轉 IP）、URL fragment。

#### Canonicalization 規則

- **冇 Unicode 正規化**：所有比較同 hash 都按收到嘅 byte；NFC 同 NFD 係唔同輸入（見 V8、V9）。
- **Byte 編碼**：canonical string 一律 ASCII（正規化之後所有非 ASCII byte 都係 percent-encoded，header 值唔准非 ASCII），以 UTF-8 bytes 做 hash 輸入。
- **Method**：一定要係 RFC 9110 token 而且冇細楷字母（例如 `POST`、`PATCH`）；唔會幫你轉大寫，`post` 即無效。
- **Path**：用 request target 嘅 path 部分（`?` 之前），唔包括 scheme、host、fragment。以 `/` 分段，每段：先將 `%XX` decode 做 byte（`%` 後面唔係兩個 hex 字元即無效），再將 unreserved 字元（`A–Z a–z 0–9 - . _ ~`）以原字元輸出，其他每個 byte 一律輸出成大寫 `%XX`。Raw 非 ASCII byte、控制字元、空格即無效。Decode 之後係 `.` 或者 `..` 嘅段（包括 `%2E`、`%2e%2E`）即無效（唔做 dot-segment 解析）。`%2F` 保持 `%2F`，唔會變成分段。空段同結尾 `/` 照樣保留（`/a//b/` 同 `/a/b` 係唔同 path）。
- **Query**：`?` 之後全部（唔包括 fragment）。Query 入面有 raw `+` 即**無效**：空格一定要送 `%20`，字面 `+` 一定要送 `%2B`，咁就唔使理 framework 點 decode `+`。以 `&` 分割，丟棄空項；每項以第一個 `=` 分做 name 同 value，冇 `=` 就當 value 係空字串（`?x` 同 `?x=` 一樣，輸出 `x=`）。Name 同 value 各自用同 path 段一樣嘅 percent 正規化。然後按 name、再按 value 以 byte 次序（升序）排序，重複 name 全部保留，用 `&` 接返。
- **Header 名**：大小寫唔分（按 RFC 9110），canonical string 入面一律寫小寫。涵蓋嘅 header（`Content-Type`、`If-Match`、`X-Vibe-Request-Nonce`）同 `Content-Encoding` 出現多過一次即無效。
- **Header 值**：只准可見 ASCII、空格同 tab（非 ASCII、CR、LF 或者其他控制字元即無效）；只剪頭尾空格同 tab，中間連續空格／tab 變一個空格。`Content-Type` 值再轉小寫 ASCII；`If-Match` 值保持大小寫（ETag 大小寫有意義）。
- **Body**：hash 輸入係 server 收到嘅 raw body bytes（transfer coding 已解開之後），喺 JSON 解析**之前**計；唔做 JSON 正規化，client 一定要原封不動送出計 digest 嗰份 bytes。冇 body 或者 body 長度 0（包括 GET、DELETE）用空字串嘅 SHA-256：`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`。綁定 route 唔接受 `Content-Encoding`（有就無效），避免壓縮前後 bytes 唔同。
- **Client helper**：前端文件同官方 client helper 一定要自動做 encoding（空格 `%20`、`+` 變 `%2B`），唔可以靠開發者手寫。

#### Hash、編碼同傳送

- 算法：`digest = "rd1." + base64url(SHA-256(UTF-8(canonical string)))`，base64url 用 RFC 4648 §5 字母表、**冇** padding；即係 `rd1.` 加 43 個字元，共 47 字元，喺 Turnstile `cdata` 255 字元上限之內。一律用完整長度（已確認（產品經理 2026-10-16）），唔截短（取代 #156 提案嘅 32 字元截短）；長度唔係 47 嘅 `cdata` 即無效。
- `rd1.` 前綴標示版本；之後改算法會用新前綴，server 只接受 lock 設定嘅版本，唔會自動降級。
- 傳送：client 將 digest 設做 widget 嘅 `data-cdata`（或者 `turnstile.render` 嘅 `cdata` 參數），nonce 放 `X-Vibe-Request-Nonce`；server 由 siteverify 回應嘅 `cdata` 欄位攞返 digest。Provider 只會見到 digest。參考實作同測試向量見 [`scripts/request-digest-vectors.mjs`](../scripts/request-digest-vectors.mjs)（Node 20）。
- Client 一定要喺表格填好之後、送出之前先產生 nonce、計 digest 同做 challenge；送出前改過 body 就一定要攞新 nonce 同新 token。

#### Server 驗證次序

接 7.2a「處理次序」，`bind: request` 嘅 route 改成：

1. 限流同配額（決定 137）→ 429 `RATE_LIMITED`。
2. Request 大小上限（見下面）→ body 超過即 413（同未綁定 route 一樣）。
3. 冇 `x-challenge-proof` → 400 `CHALLENGE_REQUIRED`。
4. 檢查 `X-Vibe-Request-Nonce`（存在、唔重複、剛好 22 字元 canonical base64url），再由收到嘅 request 計 digest（JSON 解析之前、未 call provider 之前）。Nonce 無效或者任何 canonicalization 無效（raw `+`、method 唔係大寫 token、壞 percent-encoding、dot segment、重複 header、非 ASCII header 值、有 `Content-Encoding`、超過上限等）→ 403 `CHALLENGE_FAILED`，唔 call provider。
5. Replay store 檢查同預留（已確認（產品經理 2026-10-16 12:44 HKT），#258），次序固定，遇到第一個唔通過就停：
   1. Token 格式（空或者超過 2048 字元）→ 403 `CHALLENGE_FAILED`（nonce 同 canonical 格式已喺第 4 步檢查，都唔使 store）。
   2. Replay store 唔可用 → 503 `CHALLENGE_UNAVAILABLE` 加 `Retry-After: 5`；fail closed，乜嘢都唔寫，唔放行。
   3. 開一個交易。先 nonce：nonce `(tenant_id, nonce)` 已經用過 → 403 `CHALLENGE_FAILED`；之後 token 已經用過 → 403 `CHALLENGE_FAILED`。
   4. 加上呢個 request 嘅 2 個紀錄會超過決定 137（每 IP 16、每租戶 1,000）→ 403 `CHALLENGE_FAILED`。
   5. 加上 2 個紀錄會超過決定 134（網段上限、每租戶 store 容量）→ 429 `CHALLENGE_RATE_LIMITED` 加 `Retry-After`（規則同決定 134）。
   6. 寫入 nonce 紀錄，再寫入 token 紀錄，commit。任何一步失敗（包括交易途中 store 出錯 → 503）成個 rollback，唔留低半套紀錄。
   任何一個 429，無論係 nonce 紀錄定 token 紀錄令到超過上限，回應都同其他決定 134 嘅 429 完全一樣，只係 `Retry-After` 值唔同；reason 只入 metric 同內部 log。
6. Siteverify（決定 132，所有現有檢查不變）。
7. 比較 siteverify `cdata` 同第 4 步嘅 digest：用 constant-time 比較；`cdata` 冇、空、長度唔啱或者唔相等 → 403 `CHALLENGE_FAILED`。Token 同 nonce 照樣記為已用（單次使用，唔會因為 digest 唔啱而釋放）。
8. 全部通過先做 JSON 解析同執行 lock steps；之後嘅處理只用第 4 步 hash 過嗰份 body bytes，唔會再讀第二次。

#### 錯誤碼同 HTTP 狀態

唔加新錯誤碼。任何無效嘅 canonical 輸入、nonce 問題同 digest 唔一致（第 4、5、7 步）一律回現有 403 `CHALLENGE_FAILED`，回應同其他 token 無效情況完全一樣（決定 135）：body、header、error detail 都唔講係邊一部分（nonce、method、path、query、header、body）有問題，亦唔回傳 server 計出嘅 digest。內部 reason（例如 `digest_mismatch`、`digest_missing`、`digest_invalid_request`、`nonce_invalid`、`nonce_reused`）只入內部 log 同 metric `vibe_challenge_rejected_total{reason}`。Log **永遠唔記錄** nonce、body、body hash、canonical string、digest、token 或者 secret。

#### 大小上限

| 項目 | 上限 | 超過 |
| --- | --- | --- |
| Body | 跟 route body 上限（預設 1MB，2.3），digest 涵蓋成個 body，冇另外截斷 | 413 |
| Path（正規化前） | 2,048 bytes | 403 `CHALLENGE_FAILED` |
| Query（正規化前） | 4,096 bytes，最多 64 項 | 403 `CHALLENGE_FAILED` |
| 每個涵蓋 header 值（`Content-Type`、`If-Match`） | 1,024 bytes | 403 `CHALLENGE_FAILED` |
| `X-Vibe-Request-Nonce` | 剛好 22 字元（唔多唔少） | 403 `CHALLENGE_FAILED` |
| `cdata` | 剛好 47 字元（`rd1.` + 43）；Turnstile 上限 255 | 403 `CHALLENGE_FAILED` |

Server 以 streaming 方式計 body hash，同時套用 body 上限；未讀完 body 唔會 call provider。

#### Proxy 同 rewrite

- Digest 按 vibe runtime 收到嘅 request 計。Client 同 runtime 之間嘅 proxy、CDN 或者 gateway **唔可以**改 method、path、query、`Content-Type`、`If-Match`、`X-Vibe-Request-Nonce` 或者 body，亦唔可以加 `Content-Encoding`；否則 request fail closed（403），唔會放行。
- **部署要求**：client 送出嘅 path 一定要同 vibe runtime 收到嘅 path 一致。Proxy 剝除 prefix 或者 rewrite path（例如外面 `/api/v1/...`，入面 `/v1/...`）會令 digest 唔一致，request 一律 fail closed（403 `CHALLENGE_FAILED`）；v1 冇任何設定可以話 runtime 用另一個 path 計 digest。
- Proxy 只可以做 percent-encoding 層面嘅等價改寫（例如 `%7e` 變 `~`），因為正規化之後結果一樣；proxy 重新排序 query 參數亦唔影響結果。Proxy 將 `%20` 改成 `+` 會令 request 無效。
- 決定 132 嘅 client IP 照舊只喺 trusted proxy 先讀 forwarded header（#54）；digest 唔涵蓋任何 forwarded header。

#### 推出

- 新 lock 預設 `bind: request`。現有冇寫 `bind` 嘅 lock 當 `none`，維持一個 release，期間 `vibe validate` 出 warning；下一個 release 預設變 `request`。
- v1：`bind: none` 准用，但 `vibe validate` 一定出 warning。
- **由下一個 minor release 起**：新增嘅 public INSERT route 唔准用 `bind: none`（驗證器 error）；追蹤 issue #245。

#### 測試向量

用 [`scripts/request-digest-vectors.mjs`](../scripts/request-digest-vectors.mjs)（Node 20，`node:crypto`）計；全部有效同無效向量都用另一個獨立 Python 實作（只照 spec 文字寫，`hashlib`）核對，結果一致。Nonce 係固定測試值，實際一定要用 CSPRNG。Canonical string 入面嘅換行係 LF。

##### V1

- Request：`POST /v1/t/acme-7f3k/register`，`Content-Type: application/json`，nonce `AAECAwQFBgcICQoLDA0ODw`（bytes `00`–`0f`），body（UTF-8）`{"email":"a@example.com","password":"correct horse battery staple"}`

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
POST
/v1/t/acme-7f3k/register

content-type:application/json
if-match:
55b5541b48ca3e041cb0eb9bd14e1cf2e61d8573de7751f018418285da65ecb9
```

- Digest：`rd1.T7Wu5sjIM6xgLlUzeI113wZhjMdkg5zo1h0BYNAqj1g`

##### V2

- Request：`GET /v1/t/acme-7f3k/products?sort=price&tag=b%20c&tag=a&q=caf%C3%A9&x=%7e&p=1%2B1`，nonce `8PHy8_T19vf4-fr7_P3-_w`（bytes `f0`–`ff`），冇 `Content-Type`，冇 body

```text
vibe-request-digest-v1
nonce:8PHy8_T19vf4-fr7_P3-_w
GET
/v1/t/acme-7f3k/products
p=1%2B1&q=caf%C3%A9&sort=price&tag=a&tag=b%20c&x=~
content-type:
if-match:
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

- Digest：`rd1.MPNyBkedB8X5C9DsjmeBiAC2PW8GSIB2qioPRimXe18`

##### V3

- Request：`PATCH /v1/t/acme-7f3k/items/%61bc`，`CONTENT-TYPE: Application/Merge-Patch+JSON`，`If-Match` 值係前後各一個空格嘅 `"v42"`，nonce 同 V1，body（UTF-8）`{"name":"新名"}`

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
PATCH
/v1/t/acme-7f3k/items/abc

content-type:application/merge-patch+json
if-match:"v42"
216e739739607d9cb0aa5a4f63cee52ec6fc705af18edbdad0b9fe81be38d4d0
```

- Digest：`rd1.rPxWUi275XKer2fs7nk8oxmDXtbDt7Mvv5ydEMntE6Q`

##### V4

- Request：同 V1，只係 body 最後一個字母改做 `staplf`

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
POST
/v1/t/acme-7f3k/register

content-type:application/json
if-match:
cc8acf6f7885b7f43746d2abe600b1899c2edb4666181522c40e13463b19b150
```

- Digest：`rd1.uX7NL5__6McKtLjzk_a1uUA-qvUXxyz_Ha8lASuKams`

##### V5

- Request：同 V1，只係 nonce 改做 V2 嗰個；同一個 body，digest 完全唔同

```text
vibe-request-digest-v1
nonce:8PHy8_T19vf4-fr7_P3-_w
POST
/v1/t/acme-7f3k/register

content-type:application/json
if-match:
55b5541b48ca3e041cb0eb9bd14e1cf2e61d8573de7751f018418285da65ecb9
```

- Digest：`rd1.6Wg9isE5oy598e_YuG4sHzXFWjKvYachx2lkd5x6buQ`

##### V6

- Request：`GET /v1/t/acme-7f3k/a%2fb//c/?x`，nonce 同 V1：`%2F` 唔會變分段，空段同結尾 `/` 保留，`?x` 變 `x=`

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
GET
/v1/t/acme-7f3k/a%2Fb//c/
x=
content-type:
if-match:
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

- Digest：`rd1.H9_aDPvpzR4l-L-m657_poVdmeKP2aI8ryHPIfEsEGY`

##### V7

- Request：`GET /v1/t/acme-7f3k/a%2Fb//c/?x=`：同 V6 digest 一樣

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
GET
/v1/t/acme-7f3k/a%2Fb//c/
x=
content-type:
if-match:
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

- Digest：`rd1.H9_aDPvpzR4l-L-m657_poVdmeKP2aI8ryHPIfEsEGY`

##### V8

- Request：`POST /v1/t/acme-7f3k/register`，nonce 同 V1，body `{"n":"é"}`，é 用 NFC（`C3 A9`）

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
POST
/v1/t/acme-7f3k/register

content-type:application/json
if-match:
cda516738eb07e9a1701275e8ba725b8604fc9d27185454a98f81478abb790cf
```

- Digest：`rd1.rQrth1qPMLQ6oKVKN49T_N-2_07GQF2a4Pg9TCEeETE`

##### V9

- Request：同 V8，但 é 用 NFD（`65 CC 81`）；冇 Unicode 正規化，所以 digest 同 V8 唔同

```text
vibe-request-digest-v1
nonce:AAECAwQFBgcICQoLDA0ODw
POST
/v1/t/acme-7f3k/register

content-type:application/json
if-match:
b6366f0d634afe549d3039313154f8e78529a9fd92396fc35a8e52cb6e9bcf09
```

- Digest：`rd1.24fttLUzKMXvEVrospLdcxoo_TcOtxHLiqEECmSruWg`

##### V10

- Request：`GET /v1/t/acme-7f3k/products?` 加 64 項 `k00=…`、`k01=v` … `k63=v`（`k00` 個 value 係 3714 個 `v`），query 剛好 4,096 bytes、64 項（上限邊界，有效）；nonce 同 V1。Canonical string 太長唔列出，由 script 產生
- Digest：`rd1.JdKv8b3ZVxHQLNosmVB-PnlhaLnS6cZMXbntHv-S31o`

##### 一定要當無效嘅輸入（全部 403 `CHALLENGE_FAILED`，唔 call provider）

| # | 輸入（其餘同 V1） |
| --- | --- |
| I1 | query 有 raw `+`：`?q=a+b` |
| I2 | method 細楷 `post` |
| I3 | `Content-Type` 出現兩次 |
| I4 | `If-Match` 有非 ASCII（`"vé"`） |
| I5 | nonce 21 字元 |
| I6 | nonce 23 字元 |
| I7 | 冇 nonce header |
| I8 | dot segment `%2E%2E` |
| I9 | dot segment `.` |
| I10 | 壞 percent-encoding `%2G` |
| I11 | 截斷 percent-encoding `%4` |
| I12 | 有 `Content-Encoding: gzip` |
| I13 | `If-Match` 有 CR |
| I14 | 非 canonical nonce `AAECAwQFBgcICQoLDA0ODx`（22 字元，但最後一個字元嘅 padding bit 唔係 0） |
| I15 | `X-Vibe-Request-Nonce` 出現兩次 |
| I16 | query 4,097 bytes（超過上限） |
| I17 | query 65 項（超過上限） |

必要測試（實作時）：上面有效向量逐個相等；無效向量全部 403 `CHALLENGE_FAILED` 而且唔 call provider；nonce、body、query、path、method、`Content-Type`、`If-Match` 任何一樣改咗即 403；`cdata` 冇或者空即 403；同一個 nonce 第二次用即 403；所有 403 回應 byte 一致；digest 唔一致之後 token 同 nonce 都唔可以再用；nonce 紀錄計入決定 134 同 137 上限；log 搵唔到 nonce 同 body；nonce 引起嘅 429 同其他決定 134 嘅 429 除 `Retry-After` 值外 byte 一致。

同時成立嘅條件（檢查次序測試，實作時逐個要有）。下表「格式錯」指第 4 步或者 token 格式檢查唔通過；「重用」指 nonce 或者 token 喺 replay store 已經有紀錄；「store 唔可用」指 replay store 連唔到或者出錯。

| # | 同時成立 | 結果 |
| --- | --- | --- |
| C1 | 只係決定 134 容量超過（nonce 同 token 合法、未用過、決定 137 未超過） | 429 `CHALLENGE_RATE_LIMITED` 加 `Retry-After` |
| C2 | Nonce 格式錯，而且同一個值之前出現過 | 403 `CHALLENGE_FAILED`（格式檢查已拒絕，唔會查 store） |
| C3 | 格式錯 + 決定 137 超過 | 403 `CHALLENGE_FAILED` |
| C4 | 格式錯 + 決定 134 超過（store 滿） | 403 `CHALLENGE_FAILED` |
| C5 | 重用 + 決定 137 超過 | 403 `CHALLENGE_FAILED` |
| C6 | 重用 + 決定 134 超過（store 滿或者網段上限） | 403 `CHALLENGE_FAILED` |
| C7 | 決定 137 超過 + 決定 134 超過 | 403 `CHALLENGE_FAILED` |
| C8 | 格式錯 + 重用 + 決定 137 超過 + 決定 134 超過（store 可用） | 403 `CHALLENGE_FAILED` |
| C9 | Canonical 輸入無效（例如 raw `+`）+ 決定 134 超過 | 403 `CHALLENGE_FAILED`（第 4 步已拒絕，未到 replay store） |
| C10 | 格式錯 + store 唔可用 | 403 `CHALLENGE_FAILED`（純輸入檢查喺 store 之前） |
| C11 | Store 唔可用 + 重用 | 503 `CHALLENGE_UNAVAILABLE` 加 `Retry-After: 5`（做唔到重用檢查，fail closed） |
| C12 | Store 唔可用 + 決定 137 或者決定 134 超過 | 503 `CHALLENGE_UNAVAILABLE` 加 `Retry-After: 5` |

Token 紀錄令到超過上限（兩個紀錄同一個交易，要 2 個空位）：

| # | 情況 | 結果 |
| --- | --- | --- |
| T1 | Nonce 冇問題；token 紀錄會超過決定 137（每 IP 16 或者每租戶 1,000） | 403 `CHALLENGE_FAILED` |
| T2 | Nonce 冇問題；加上今次 2 個紀錄會超過決定 134 網段上限（嗰個網段只剩 1 個空位，即係 token 紀錄超過） | 429 `CHALLENGE_RATE_LIMITED`，`Retry-After` = 該租戶喺嗰個網段最舊紀錄（上限 360 秒，計唔到 60 秒） |
| T3 | Nonce 冇問題；加上今次 2 個紀錄會超過決定 134 每租戶 store 容量（只剩 1 個空位，即係 token 紀錄超過） | 429 `CHALLENGE_RATE_LIMITED`，`Retry-After` = 該租戶自己最舊紀錄（上限 360 秒，計唔到 60 秒） |
| T4 | Token 紀錄會同時超過決定 137 同決定 134 | 403 `CHALLENGE_FAILED`（決定 137 喺決定 134 之前） |
| T5 | Token 重用 + 決定 134 滿 | 403 `CHALLENGE_FAILED`（重用喺容量之前） |
| T6 | Nonce 重用 + token 紀錄會超過決定 134 | 403 `CHALLENGE_FAILED`（先檢查 nonce） |
| T7 | Token 格式錯（空或者超過 2048 字元）+ store 唔可用 | 403 `CHALLENGE_FAILED`（純輸入檢查喺 503 之前） |
| T8 | 預留任何一個紀錄時 store 唔可用 | 503 `CHALLENGE_UNAVAILABLE` 加 `Retry-After: 5`；成個交易 rollback，乜嘢都唔寫，唔放行 |
| T9 | 上面任何一個 429 | 同其他決定 134 嘅 429 完全一樣，只係 `Retry-After` 值唔同；唔顯示係 token 定 nonce 紀錄超過上限 |

冇 proof（400 `CHALLENGE_REQUIRED`，7.2b 第 3 步）同其他條件一齊出現：

| # | 同時成立 | 結果 |
| --- | --- | --- |
| P1 | 冇 `x-challenge-proof` + nonce 或者 canonical 格式錯 | 400 `CHALLENGE_REQUIRED`（第 3 步喺第 4 步之前） |
| P2 | 冇 `x-challenge-proof` + replay store 唔可用 | 400 `CHALLENGE_REQUIRED`（未用 store） |
| P3 | 冇 `x-challenge-proof` + nonce 重用 + 決定 137 或者決定 134 超過 | 400 `CHALLENGE_REQUIRED`；唔寫入任何紀錄 |
| P4 | 冇 `x-challenge-proof` + route 限流或者配額超過（第 1 步） | 429 `RATE_LIMITED`（第 1 步喺第 3 步之前） |

所有 403 回應 byte 一致；所有 429（C1、T2、T3，同 store 滿、網段上限嘅 429）除 `Retry-After` 值外一致；所有 503 一致（`Retry-After: 5`）。每個被拒嘅 request 之後，store 入面唔會有呢個 request 嘅半套紀錄（一個交易）。

#### 剩餘風險

- **Provider 知道有 request**：provider 見唔到 request 內容，但仍然知道某個 widget 喺某個時間有一次 challenge（同冇綁定時一樣）。
- **Nonce 品質靠 client**：client 唔用 CSPRNG 或者重用 nonce 會削弱私隱保護；server 只可以檢查格式同單次使用，檢查唔到隨機程度。
- **同一個 request 喺 token 有效期內重送**：nonce 同 token 都係單次使用，所以要重新做 challenge；session 綁定留到 vNext。
- **Digest 涵蓋唔到嘅部分**：其他 header、cookie 同 client IP 唔受保護；呢啲唔應該影響綁定 route 嘅業務結果。
- **Client 端計算**：digest 證明 token 係為呢個 request 內容攞嘅，唔證明內容係真人輸入；challenge 本身嘅保證唔變。
- **部署相容性**：會改 body、path 或者 query encoding 嘅 proxy／CDN（壓縮、prefix rewrite、`%20` 改 `+`）會令所有綁定 request 被拒（fail closed），要喺部署文件講明。
- **Provider 依賴**：`cdata` 由 Turnstile 原樣回傳；如果 provider 改變 `cdata` 行為，比對會失敗（fail closed），唔會放行。
- **Replay store 容量**：每個 request 多一個 nonce 紀錄，同 token 共用決定 134 同 137 上限，高流量時會早啲到上限（決定 137 → 403，決定 134 → 429，都係 fail closed）。
- **網段上限較早觸發**（已確認（產品經理 2026-10-16 12:31 HKT））：每個 request 佔兩個紀錄（token 同 nonce），所以每個 IP 或者網段大約只可以同時有 8 個 request（決定 134 網段上限 16、決定 137 每 IP 16）；共用 carrier NAT 嘅用戶會較早撞到上限：超過決定 137 每 IP 上限回 403 `CHALLENGE_FAILED`（唔係 429），只有超過決定 134 網段上限先回 429。GA 前用 `vibe_challenge_rejected_total{reason="replay_prefix_cap"}` 檢視實際數字再決定要唔要調整，追蹤 #256。
- **拒絕回應時間**（已確認（產品經理 2026-10-16 12:31 HKT；涵蓋 token 重用，12:54 HKT））：v1 唔會拉平 nonce 同 token 重用相關拒絕回應嘅時間，接受為剩餘風險；vNext 評估同追蹤見 #255。
- **Nonce 喺邊度睇得到**：Operator 自己嘅 reverse proxy 或者 CDN 如果負責 TLS termination，睇得到 nonce 同 body；challenge script 喺我哋自己嘅頁面執行，原則上讀得到頁面內容（包括表格同 nonce）。Nonce 係單次使用、唔含任何 secret，睇到 nonce 唔可以用嚟偽造 request；呢度嘅保護範圍只係令 `cdata` 本身唔可以還原內容。
- **用戶體驗**（已接受，產品經理 2026-10-16）：用戶做完 challenge 之後再改表格，要重新做 challenge。前端文件教開發者喺表格填好之後、送出之前先做 challenge（[`challenge.md`](challenge.md)）。

### 7.3 schema 升級去 database（離線遷移，CLI `tenant migrate`）

1. 平台發出一個臨時遷移 role，全程寫入審計記錄。
2. 凍結該租戶寫入：寫入請求回 503 同 `Retry-After`，讀取照常。
3. 複製資料，然後喺新 DB 重新驗證 RLS、FORCE、權限同資料筆數。
4. 自動跑隔離測試，跨租戶讀寫一定要失敗；測試唔通過就回滾。
5. 按租戶做原子切換：更新 secret 對照表，清走該租戶嘅連線池同 manifest cache，再解凍寫入。
6. 即刻撤銷臨時遷移 role。
7. 舊 schema 改為唯讀，保留 30 日；保留期內可以回退，之後自動刪除，並且寫入審計記錄。（呢個係平台遷移流程，同業務資料嘅軟刪除無關。）

### 7.4 Request transaction（決定 99、100）

- 一個 request = 一個 runtime 管理嘅 transaction，isolation level `READ COMMITTED`。
- 用戶 SQL 控制唔到 transaction 邊界（7.1a 禁止 `BEGIN`／`COMMIT`／`SAVEPOINT` 等）；pipeline、steps 同 `method` 呼叫全部喺同一個 transaction。
- 所有寫入喺租戶 role 之下、按 lock 入面固定嘅 step 次序執行，減少 deadlock。
- 庫存類扣減用條件 UPDATE，例如 `UPDATE ... SET stock = stock - $qty WHERE id = $id AND stock >= $qty`，再檢查 affected rows；唔夠就成個 transaction rollback，回 409（錯誤碼由 lock 宣告，例如 `INSUFFICIENT_STOCK`）。
- 唔支援跨 request、跨 DB 或者分散式 transaction；需要跨 request 嘅流程用 outbox 之類非同步做法。
- 外部副作用（例如寄 email）一律用 transactional outbox：outbox 列喺同一個 transaction 寫入，commit 之後先處理；commit 之前永遠唔做；rollback 連 outbox 列一齊丟棄。
- 超時預設（決定 100）：`statement_timeout` 5s、transaction 總時間 10s、`idle_in_transaction_session_timeout` 5s。超時即 rollback，回 504 `TX_TIMEOUT`。
- 唯一鍵：SQLSTATE `23505` 一律回 409 `UNIQUE_VIOLATION`，唔回顯欄位值，永遠唔重試（12.2）。
- 衝突：deadlock（SQLSTATE `40P01`）同 serialization failure（`40001`）rollback，回 503 `TX_CONFLICT`，body 帶 `retryable: true`，header `Retry-After: 1`。
- 平台永遠唔會自動重試非冪等寫入；client 要自己決定重試（建議用 idempotency key）。

## 8. 狀態碼同錯誤

決定 171：runtime 公開錯誤碼清單係 `BAD_REQUEST`、`CHALLENGE_FAILED`、`CHALLENGE_REQUIRED`、`CHALLENGE_UNAVAILABLE`、`ENDPOINT_DISABLED`、`FORBIDDEN`、`INTERNAL`、`INVALID_REFRESH_TOKEN`、`METHOD_NOT_ALLOWED`、`NOT_FOUND`、`PAYLOAD_TOO_LARGE`、`PRECONDITION_FAILED`、`PRECONDITION_REQUIRED`、`RATE_LIMITED`、`SERVICE_UNAVAILABLE`、`TX_CONFLICT`、`TX_TIMEOUT`、`UNAUTHORIZED`、`UNIQUE_VIOLATION`、`UNSUPPORTED_MEDIA_TYPE`、`VALIDATION_FAILED`、`WEAK_PASSWORD`。Feature Errors 嘅 `code` 同 `from` 可以用呢個清單，加上被呼叫 method／CRUD 宣告同本 feature validate fail 碼；未知碼係 validate error。

| 狀態碼 | 用途 |
| --- | --- |
| 200 | 成功 |
| 201 | 建立咗資源，要帶 `Location` |
| 202 | 已接受，會非同步完成（例如註冊等 email 驗證） |
| 204 | 成功但冇內容 |
| 400 | body 解析唔到 |
| 401 | 未認證或者 token 無效，要帶 `WWW-Authenticate`（RFC 6750） |
| 403 | 已認證但冇權限 |
| 404 | 資源唔存在（public endpoint 規則見 7.2） |
| 405 | method 唔啱，要帶 `Allow` |
| 409 | 同現有狀態衝突（例如唯一欄位重複 `UNIQUE_VIOLATION`、庫存唔夠（7.4）、`IDEMPOTENCY_IN_PROGRESS`（2.3b）） |
| 412 | `If-Match` 同資源而家嘅 `ETag` 唔一致（樂觀鎖衝突） |
| 413 | body 太大 |
| 415 | Content-Type 唔支援 |
| 422 | 驗證唔通過，或者有未宣告欄位；`IDEMPOTENCY_KEY_REUSED`（2.3b）、`EMPTY_PATCH`（2.3c）、`WEAK_PASSWORD`（長度唔喺 12–256）、`BREACHED_PASSWORD`（洩漏密碼，#90；4A.2）；錯誤唔回顯密碼 |
| 428 | 需要 `If-Match` 但冇帶（RFC 6585） |
| 429 | 請求太多，要帶 `Retry-After`（`RATE_LIMITED`；決定 130 嘅網段 slot 上限同 slot 表滿都用呢個 code，唔講係邊一層） |
| 500 | 未宣告嘅錯誤，code 一律係 `INTERNAL`，唔會洩露內部資料 |
| 503 | 暫時唔可用（包括遷移期間凍結寫入、kill switch 停用咗嘅 endpoint、`TX_CONFLICT`（7.4，帶 `Retry-After: 1`）、`DB_POOL_EXHAUSTED`（決定 129，連線池任何上限滿咗，帶 `Retry-After`；值 `1`，已確認（產品經理 2026-10-16））），可以帶 `Retry-After` |
| 504 | 對外 call 超時，或者 transaction 超時 `TX_TIMEOUT`（7.4） |

- 錯誤 body 統一格式：`{ "error": { "code", "message", "request_id" } }`。
- 401、405、429 嘅必要 header 由平台自動加。
- 認證失敗統一回 401，唔會分開講係用戶唔存在定係憑證錯。
- 資源 ID 建議用 UUID，防止被逐個試出嚟。

## 9. Log 同 Manifest

- log 記錄 request id、狀態碼、耗時同 stack；stack 只會寫入 log，唔會放喺回應。token、key 同 secret 一律唔會記錄。
- `audit-log` pipeline 會記錄邊個做咗咩操作。Audit 永遠唔記明文 email（決定 68）：email 類欄位（`format: email`）預設用 `hash:` 記錄，或者寫 `mask:` 記遮罩（例如 `j***@e***.com`）；驗證器見到 audit 會記 email 明文就報 error。
- Audit `hash:` 值格式（決定 68）：記錄值一律係字串 `hash:v1:<64 字小寫 hex>`，hex 係 `HMAC-SHA-256(tenant_audit_key, normalize(value))`。
  - `normalize`：email 按決定 120 正規化（local part NFC、trim、lowercase；domain IDNA UTS #46 punycode 再 lowercase；唔合併 plus addressing，local part 嘅點保留）；其他欄位做 trim。
  - `tenant_audit_key` 係每個租戶獨立嘅 audit key，放喺平台 secret，lock 同 log 都唔會出現。同一租戶同一 email 永遠得出同一個值，方便查審計；唔同租戶冇辦法對比。
  - 同 `subject-key` 嘅 `hash:{column}` 唔同：嗰個係 DB 欄位，值係 `HMAC-SHA-256(pepper, normalize(email))` 嘅 raw bytes（`bytea`），用嚟做私隱刪除；audit 用另一條 key，所以 audit log 外洩唔可以直接對比 DB。
  - lock 入面 `hash: [req.body.email]`（例如 11.2、12.11.6）一律指呢個格式。
- 部署時將所有 lock 編譯成 manifest，入面有 routes、pipelines、steps、provider 對應、公開 endpoint 清單、public 寫入 endpoint 清單，同埋每個 lock 嘅 id、hash 同簽名。
- manifest 由 `vibe manifest --dev` 用 dev key 簽名；release 由 `vibe manifest` 輸出 unsigned manifest，再由 release workflow 用 Sigstore keyless 簽名（決定 79、119）；runtime 只會載入驗證成功而且簽名有效嘅 manifest，冷啟動唔會再解析任何 md（source 或者 lock 都唔會）。
- 編譯 manifest 前 `vibe manifest` 會逐個檢查 lock 嘅 hash（source、依賴、compiler-hash）同驗證器結果；lock 本身冇簽名（決定 79）。任何一個 lock 唔合格，成個部署失敗。

## 9A. 緊急 Kill Switch

- 用途：發現漏洞、濫用或者錯誤邏輯時，即時停用個別 endpoint，唔使重新部署（決定 31）。
- 邊個可以觸發：project owner、maintainer，同平台 on-call 安全團隊；租戶 admin 只可以停用自己租戶嘅 public endpoint（只影響自己租戶）。
- 點觸發：平台 UI 或者 CLI `endpoint disable {definition}/{version}/{feature} --reason "..."`；一定要寫原因。
- 範圍：可以按 endpoint（全部租戶）、按 endpoint + 租戶、或者按 definition 成批停用。
- 效果：
  - SLA：p99 30 秒內全部 runtime instance 生效（經平台控制面推送，唔改 manifest）；超過 SLA 會觸發平台告警（決定 40）。
  - 被停用嘅 endpoint 一律回 503，錯誤 code `ENDPOINT_DISABLED`，可以帶 `Retry-After`；唔會執行任何 pipeline 或者 method。
  - public endpoint 被停用時同樣回 503（唔會洩露租戶係咪存在，因為所有租戶都一樣）。
- 審計：觸發、恢復、觸發人、原因、範圍全部寫入審計記錄，並通知 owner 同 maintainer。
- 恢復：同樣權限嘅人喺 UI／CLI 恢復，寫原因；重新部署唔會自動恢復已停用嘅 endpoint。
- 部署報告列出所有停用緊嘅 endpoint。

## 9B. 供應鏈同設定保護

- CODEOWNERS 必須（決定 29、37、93、115）：`api/`、`models/`、`migrations/`、`providers/`、`pipelines/`、`.github/`、`.compiler/`（包括 `injection-allowlist`）、編譯器同驗證器版本設定，全部要有 CODEOWNERS 覆蓋，作為 source PR 合併條件；SDK / `vibe manifest` 唔檢查（2C.3a）。設定方法見 [`pr-review-guide.md`](pr-review-guide.md)。
- Release manifest 簽名（決定 79、119）：冇長期 production key；`vibe manifest` 輸出 unsigned manifest，由 CI `release` environment 嘅 release workflow 用 GitHub OIDC 身份做 Sigstore keyless 簽名，寫入 Rekor。Environment secret 只可以用喺 dev／test 簽名，永遠唔簽 release。本機 `vibe manifest --dev` 用本機 dev key，production runtime 拒絕。
- Manifest 簽名前 `vibe manifest` 一定再跑一次驗證器；runtime 載入時驗證 manifest 簽名同每個 lock 嘅 hash，唔需要任何人手確認簽名。
- 業務 secret（DB 連線、JWT 私鑰）只用引用名，永遠唔會出現喺 source、lock、manifest 或者 log。
- 編譯用 `LLM_API_KEY`（或者別名 `OPENAI_API_KEY`）只存在 CI secret 同開發者本機 `.env`（已 gitignore），runtime 冇（見 2A.8.5）。

## 9C. 冇人手 review 嘅安全模型（決定 77–80、83、89–94、96；本節已定稿，產品經理 2026-10-14）

**明確聲明：v1.8 起，未經人手 review 嘅 LLM 編譯輸出可以直接 build 同部署去 dev／staging；release manifest 就受決定 89–92、118–119 限制（`release` environment 批准、冇 Sigstore keyless 簽名就失敗；但同一個帳戶可以自己批准、admin 可以 bypass、`--release-approval` 已撤回）。CODEOWNERS 係 source PR 合併條件（決定 93、115），唔係 SDK release 閘門（2C.3a）。** SDK 唔再要求任何人確認 lock；係咪有人 review，完全取決於用戶喺 Git 平台嘅設定。

剩餘風險：

- `release` environment 嘅 admin bypass（`can_admins_bypass: true`）保持開啟：admin 可以跳過 Jack 嘅 environment 批准，令真正人手批准閘門失效；Jack 直接決定保留，已接受呢個風險。
- 臨時方案 (i) 下任何有寫權限嘅人都可以開 `v*` tag，tag 唔係閘門；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（`--release-approval` 已撤回）。將來改用方案 (iii)（專用 release App 唯一開 tag，開啟 `prevent_self_review`）。
- LLM 誤解自然語言，產生「合規但唔係作者原意」嘅 lock（例如 endpoint 意外 public、多咗一個可寫欄位、放寬咗限流），驗證器規則冇覆蓋嘅就會直接上線。
- Prompt injection：source 或者依賴入面嘅惡意句子影響編譯結果；驗證器只出 warning（決定 37）。
- 有 repo 寫權限嘅人（或者被盜嘅帳戶、惡意 dependency PR）可以直接改 source 部署，冇第二個人睇。
- 安全項目只係資訊性；冇人睇就冇人發現。

仍然有效嘅防線：

- 獨立 deterministic 驗證器（2A.7、第 10 節）：op 白名單、RLS、public 寫入規則、黑名單欄位、capability 規則、密碼同驗證寫入規則、語意一致性檢查；唔通過就唔可以 build。
- 部署來源（決定 113–115、117，取代 CI 重編決定性檢查做部署條件，見 2C.3b）：lock 係 build 輸出唔 commit；只接受 CI 由 source 重新 build、有 attestation 同 Sigstore keyless 簽名（決定 119）嘅 artifact，部署前做 deterministic policy 檢查，release summary 列出高風險變化。剩餘風險：產生嘅 SQL 冇人手 review。舊條（已取代）：CI 重編決定性檢查（2A.6）：lock 一定係 pin 死嘅編譯器喺 CI 重編得返嘅結果，冇人可以手改 lock 偷運邏輯；冇 override。
- 資料庫層：每個 request 一個 transaction 加租戶 role（決定 97、99）、parser 白名單（決定 98）、`FORCE ROW LEVEL SECURITY`、non-owner role、column grant、冇 DELETE／TRUNCATE／DDL（7.1）。
- Runtime 限制：只載入簽咗名嘅 manifest、唔解析 md、冇 `LLM_*`、對外 call 白名單、kill switch（9A）、固定最低回應耗時、配額同限流。
- Manifest 簽名（決定 79、89、92、96）：release 只由有專用 release ruleset（bypass 一定要空）嘅 `v*` tag 出（臨時方案 (i) 下有寫權限嘅人都可以開 `v*` tag，tag 唔係閘門；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118）；release 簽名只喺 `v*` tag 加有 required reviewers（Jack）嘅 `release` environment，用 release workflow 嘅 GitHub OIDC 身份做 Sigstore keyless 簽名（Rekor inclusion 必須，決定 119）；簽唔到就失敗，唔 fallback；production 只信 pin 死嘅 Sigstore 身份（issuer `https://token.actions.githubusercontent.com`，certificate identity 完全等於 `https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v<部署緊嘅版本>`，加 commit SHA；離線驗證，trust root embed 入 binary，見 2C.3b），dev key 永遠唔信任。剩餘風險：臨時方案 (i) 下 tag 身份唔係閘門，唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；keyless 身份只證明邊個 workflow 同 tag 產生 build，唔證明邊個批准，唔會加多一個人（任何 `j113203` agent 都可以開 tag 再自己批准）。
- 高風險 fail closed（決定 90）已撤回（產品經理 2026-10-16）：高風險安全項目只列入部署報告；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（見 2C.3a）。
- `main` ruleset 保留 admin bypass（決定 96 修訂）；`bypass_actors` 唔係空係人手／文件層面已接受例外（Jack 2026-10-16，issue #70）：而家 SDK 同 release workflow 都唔讀 ruleset／`bypass_actors`（決定 90／#127）；如果將來 `release.yml` 加檢查，只可以係唯讀、資訊性、永遠唔會擋 release（同決定 90、第 2C.3a 對齊）。剩餘風險（Jack 已接受）：admin 可以 bypass `main` ruleset 保護（OrganizationAdmin bypass）；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；同一個帳戶（`j113203`）可以自己批准；`release` environment admin bypass 保持開啟。
- CODEOWNERS 必須（決定 29、37、93、115）：source PR 合併條件；SDK / release build 唔檢查（2C.3a）。
- Production 一律拒絕 in-memory，冇 override（決定 91）。
- `/latest/` 永遠唔係草稿（決定 94）。

建議（SDK 外，見 [`pr-review-guide.md`](pr-review-guide.md)）：開 rulesets（分支規則）、required PR review、CODEOWNERS（必須，覆蓋 `api/`、`models/`、`migrations/`、`providers/`、`pipelines/`、`.github/`）、required CI status（`vibe manifest`／`vibe compile --check`，決定 117）、`security:high` label 要兩個 reviewer。

## 10. 部署驗證（全部喺 lock 層檢查；每個 error 都會報 lock 檔名、section、step id、對應 source 行數同原因）

### 10.1 Lock 完整性（error）

- 過時：`source-hash` 同而家 source 正規化 hash 唔一致；或者任何 `dependencies` hash 唔一致。Release 入面 lock hash 同 dependency hash 喺同一個 release build 入面計同驗，唔用之前 build 或者磁碟上嘅值。
- 缺失：有 source 冇 lock，或者有 lock 冇 source（孤兒 lock）。
- Dev lock：release build 入面有 `build-mode: dev` 嘅 lock（決定 75）。
- 舊確認欄位：lock 有 `status`、`approval-*`、`approved-*`、`signature`（v1.7 格式）係 warning，唔係 error；SDK 忽略佢哋（決定 77）。
- 安全項目：驗證器抽出嘅安全項目同 lock `security-items` 唔一致（lock 漏報或者多報）。
- structured 判斷：lock 寫 `source-type: structured` 但 deterministic parser 判斷係 natural。
- Manifest 簽名錯：runtime 驗證 manifest 簽名失敗，或者 manifest 入面 lock hash 同 lock 內容唔一致（決定 79）。
- 手改：lock 格式唔合 2A（frontmatter 欄位或者次序錯、多咗或者少咗 section、step 欄位次序錯）。
- （已取消，決定 113、117）重編唔一致唔再封鎖部署；改為 attestation 或者決定 114 policy 唔通過就封鎖。
- 編譯器（決定 174）：`compiler-hash` 按 2A.1 重算唔一致就要求 `vibe compile`；`compiler.version` major 唔一致；編譯器名唔係平台登記。部署驗證同 manifest 從 build record 檢查 LLM provider 同浮動 model alias。
- `kind` 或者 `id` 同檔案路徑唔一致；`lock-version` 唔支援。
- 編譯器 LLM 設定（決定 57）：
  - `LLM_PROVIDER` 唔係 `openai`、`anthropic`、`gemini`。
  - 缺少 `LLM_API_KEY` 或者 `LLM_MODEL`（包括別名都冇）。
  - `LLM_*` 同 `OPENAI_*` 別名都有設而且值唔同；`LLM_PROVIDER` 唔係 `openai` 但設咗 `OPENAI_*`。
  - `LLM_MODEL` 係別名（例如 `latest`、`:latest`），唔係 pin 死 snapshot id。
  - 公開 host 用 http；loopback 以外嘅內網 host 冇 `LLM_ALLOW_PRIVATE_HOST=true`。
  - CI 或者 production 編譯用咗 http、loopback、內網 host，或者設咗 `LLM_ALLOW_PRIVATE_HOST=true`。
  - 決定 174：部署去 production 嘅 build-record stamp 有 `endpoint-private: true`。
  - 決定 174：唔再將 lock 同而家嘅 LLM 設定比較；stale 檢查唔受 LLM 設定影響。
  - 決定 174：非 dev 部署嘅 natural lock 冇 build-record stamp，必須 error 並要求執行 `vibe compile`。
  - 編譯請求帶咗 `tools`／`functions`／`toolConfig`，或者唔合 2A.8.2 嘅決定性設定；provider 回報 model 同 `LLM_MODEL` 唔一致。
  - secret scanning 喺 repo、lock、manifest、部署報告或者 CI log 搵到 key。
  - `.env` 冇喺 `.gitignore`，或者 repo 有 `.env`。
  - runtime 環境變數有任何 `LLM_*` 或者 `OPENAI_*`，除咗開發環境（`vibe serve --memory`／VIBE_ENV development）：serve 會警告（只列變數名），並喺 handlers 啟動之前移除所有 `LLM_*`／`OPENAI_*`；runtime 仍然永遠唔會收到 key。
- source 有零寬字元或者 bidi 控制字元。
- 語意一致性：lock 有 source 冇提及嘅寫入、公開、剔走 auth、簽 token 或者對外 call。

### 10.2 結構同路由（error）

- 目錄深度唔係啱啱好 `api/{definition}/{version}/{feature}/{feature}.md`；models 唔合 `models/{object}/{object}.md` 或者 `models/{object}/methods/{method}.md`；檔名同資料夾名唔一致；version 名唔合規則；`_` 開頭檔放錯位置；出現已取消嘅 `models/{object}/{version}/` 資料夾。
- `## Route` 嘅 method 唔喺支援範圍；normalize 之後 method 加 path 重複（normalize 包括轉細楷、去尾斜線、解 % 編碼，同埋統一 `:id` 同 `{id}`）；path 越出自己前綴、用咗保留路徑，或者有 `..` 或萬用字元。

### 10.3 Pipeline 同 auth（error）

- `rate-limit` 冇 `key`，或者 `key` 唔係 4.3 准用值（決定 87，已確認（產品經理 2026-10-14））。
- 引用唔存在嘅 pipeline；pipeline 參數唔合 `## Parameters` schema；auth 次序錯；剔走 auth 但冇 `public: true`；`roles: []`。
- pipeline lock 用咗白名單以外嘅 op，或者直接寫 `query`／`call`。
- JWT alg 唔喺 RS256／ES256，或者冇驗 `iss`、`aud`、`exp`、`nbf`；`jwks-uri` 唔係 https；clock skew 超過 60 秒；JWKS cache 設定超出 4.1 範圍。
- API key：scope 唔合 `{definition}:{read|write|admin|*}`；scope 指向唔存在嘅 definition；有效期超過 365 日；重疊期超過 30 日。
- `token-refresh` 參數超出上限；`sign-method` 冇 `capabilities: [sign-token]`；用戶自訂 pipeline 用咗 `refresh-rotate`／`refresh-revoke`；任何 lock query `_platform_refresh_tokens` 或者 `_platform_refresh_families`。
- public 寫入 endpoint 剔走咗 `audit-log`。

### 10.4 api feature 內容（error）

- `## Steps` 用咗 `method`、`validate`、`respond` 以外嘅 op（包括 `query` 或者 `call`）。
- 缺少 `## Request` 或者 `## Response`；`## Transform` 參照未宣告嘅 request 欄位或者 method output 冇嘅欄位。
- timeout 或者 limits 超過平台上限。
- `## Errors` 嘅狀態碼同 RFC 語意唔一致。

### 10.5 Provider（error）

- type 未登記；isolation 唔係 schema 或者 database；connection 唔係 secret 引用；secret 唔喺本 project namespace；secret 名嵌咗 `{tenant_id}`；引用嘅 secret 唔存在；source 出現明文憑證。
- 用咗 allowlist 以外嘅 provider；query 嘅 provider 唔係靜態決定；有 query 但解析唔到 provider。
- shared provider 冇 `allowed-endpoints`；名單以外嘅 endpoint（直接或者經 method）用咗佢；喺佢上面有非 SELECT sql。

### 10.6 RLS 同 DB 權限（error）

- 有表冇 `tenant_id`、冇 ENABLE 或者 FORCE RLS；policy 唔係 `TO` 該租戶 role 加寫死嘅租戶 id 常數，或者任何 policy 引用 GUC（例如 `current_setting`）；`public` schema 仍然有 CREATE 權限；`## Policies` 同平台產生嘅唔一致。
- 任何 runtime role 有 `DELETE`、`TRUNCATE` 或者 DDL 權限。

### 10.7 Public（error）

- public endpoint 用到 query（包括經 method）但冇 `tenant: { from: path }`，或者 path 冇 `{slug}`。
- public endpoint 經 method 有寫入 sql，但冇 `public-writes: true`，或者冇 IP 加租戶兩層限流，或者冇每日配額。
- public 寫入用咗 `INSERT`／`UPDATE` 以外嘅操作；寫咗 `writable-columns` 以外嘅欄位；寫咗黑名單欄位。
- public 寫入嘅 `UPDATE` 冇 `WHERE`，或者唔係用主鍵／RLS 限定。
- public 寫入 endpoint 嘅 `## Request` 宣告咗 `tenant_id`、`id` 或者 `created_by`。
- `challenge` 用咗 `type: pow`（v1 唔支援，決定 131）。
- `challenge` 唔合統一語法；有 INSERT 嘅 public endpoint 唔係 `{ type: captcha | pow, when: always }`；login 類 endpoint 冇 `when: after-failures, after-failures: 3`。
- public 路徑寫 password 欄位但缺少以下任何一項：api 嘅 `password-write: allow`、method 嘅 `capabilities: [set-password]`、寫入值係同一 method 內 `hash-password` 輸出。
- 有 public INSERT 嘅 endpoint 冇 `challenge: { type: captcha | pow, when: always }`（冇 INSERT 嘅 password 或者驗證寫入唔要求 captcha，唔報 error）。
- public 路徑寫 `email_verified_at`（或者其他 verified 類欄位）但冇 `verification-write: allow`、method 冇 `capabilities: [confirm-verification]`，或者 UPDATE 唔係用 `hash-token` 輸出做 WHERE、冇檢查過期、冇同時清走 token hash。
- `random-token` 嘅 `value` 流去 `respond`、log、或者非 sensitive 嘅 input／欄位。
- public 寫入欄位冇 `untrusted: true`，或者冇長度／數量上限。
- public 路徑有 `call`，但冇 `external-calls: allow`。
- `public_writer` grant 超出 lock 實際用到嘅表、欄位同操作。

### 10.8 Model 同 method（error）

- 物件冇 provider 或者冇 table；provider 未登記；用 shared provider 嘅物件有寫入 method；物件自己宣告咗 `tenant_id`；欄位 type 未知。
- method 用咗白名單以外嘅 op。
- sql 唔係參數化（字串拼接、`$n` 冇對應 param、param 冇用到）；sql 有多個 statement。
- sql 用咗物件以外嘅表或者 provider（寫入其他物件一定要經佢自己嘅 method）。
- `writes: false` 但有 INSERT／UPDATE sql。
- Inherited writes (#166): a method that calls, via `op: method`, a method that writes (directly or through further `op: method` calls) is itself a writing method and must declare `writes: true` in `## Signature`; `writes: false` is an error. An `op: method` step may be marked `writes: true` only when its target writes, and must not be marked `writes: false` when it does. Call cycles are reported separately and add no writes. For security risk grading (`write-sql`), writes follow the same call graph; a target that is malformed or does not resolve to a method lock or a CRUD action on an existing model is graded as a write (it is also a validation error), so an unresolved target never lowers the risk. CRUD `create`, `update` and `delete` count as writes; `get` and `list` do not.
- migration 以外嘅 lock 出現 DDL；任何 lock（包括 migration）出現 `DELETE`、`TRUNCATE`、`GRANT`、`REVOKE`。
- Migration：用咗 2B.3 唔准嘅 DDL；`seq` 跳號或者重複；非 UNIQUE `CREATE INDEX` 冇 `CONCURRENTLY` 或者冇名，或者 UNIQUE index 用咗 `CONCURRENTLY`；`ALTER COLUMN ... SET DATA TYPE`；`DROP TABLE`／`DROP COLUMN`；`backfill` 冇 WHERE 或者冇 `batch`；`destructive: true` 但冇 `deprecated-since` 或者未夠 30 日；DROP 嘅欄位仲有 lock 引用；冇 `## Verify`。
- CRUD update 或者自訂 update method 用樂觀鎖但 input 唔係 `if_match`，或者 `if_match` 唔係由 `req.header.if-match` 映射。
- `UPDATE` 冇 `WHERE`，或者冇 `scope: pk | rls`，或者 `scope: pk` 但 WHERE 唔係主鍵／唯一鍵。
- `validate` 嘅 `fail` 用咗未宣告嘅錯誤碼；參照咗未定義嘅變數。
- output 包含 password 類型欄位；sql 揀咗 password 欄位但冇畀 `verify-password` 用。
- api 或者 pipeline 呼叫唔存在嘅 object 或者 method；呼叫寫咗版本（例如 `@v2`）；input 唔合 `## Signature`。
- 循環呼叫，或者呼叫深度超過 3 層（由 api 算起）。
- Token 簽發：
  - `sign-jwt` 出現喺冇 `capabilities: [sign-token]` 嘅 method。
  - 有 `capabilities: [sign-token]` 但唔係 model 層 method。
  - claim 唔喺平台 claim 白名單，或者 method 自己傳入 `roles`、`tenant_id`、`iss`、`aud`、`exp`、`nbf`。
  - `ttl` 超過平台上限（access token 最多 1h）。
  - alg 唔係 RS256／ES256，或者 key 唔係 `key-ref` 引用。

### 10.9 Warning

- 缺少 `## Errors`（內容係 `(none)`）。
- 有公開 endpoint；有 public 寫入 endpoint。
- deprecated 嘅 version 冇寫 sunset。
- method 錯誤碼喺 api 冇對應狀態碼（會變成 500）。
- 單一職責：method 多過 1 個寫入 sql，或者寫多過一張表（決定 12）。
- 自然語言 source 有含糊描述，編譯器喺 lock 附加 `assumption` 註記。
- source 有似指令句子（prompt injection 模式），同時喺 PR 高亮。
- public endpoint 嘅 p95 超過固定最低回應耗時。
- public 路徑有 `cost: high` step。

### 10.10 部署報告

- 列出公開 endpoint、public 寫入 endpoint 同可寫欄位、用緊 shared provider 嘅 endpoint、每個 endpoint 用緊邊個 provider、被覆蓋咗嘅 CRUD method、今次 build 產生嘅 lock 同佢哋嘅 hash、build 嘅 tag 同 commit SHA、同上一個已部署版本相比嘅高風險變化（決定 117）、停用緊嘅 endpoint。
- **有效權限摘要**：每個 endpoint 一行，列出公開與否、role、可讀表、可寫表同欄位、會唔會簽 token、對外 call、限流、配額、challenge；同上次部署比較，標出變咗嘅地方。

## 11. 完整範例（api、provider、pipeline）

### 11.1 Provider

`providers/main.md`

```markdown
# main
主資料庫，用 PostgreSQL，每個租戶一個 schema。
連線用 secret PG_MAIN。
每個租戶連線池最多 5 條，statement 最多 5 秒。
migration 放 ./migrations/main/。
```

`providers/main.lock.md`

````markdown
---
lock-version: 1
kind: provider
id: main
source: providers/main.md
source-hash: sha256:9a1c...e2
source-type: natural
dependencies: []
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items: [{ item: provider-new, risk: high, reason: "新增主資料庫" }]
---
## Connection
- type: postgres
- secret: PG_MAIN

## Isolation
- mode: schema
- rls: forced

## Access
- shared: false
- allowed-endpoints: (none)

## Limits
- pool-per-tenant: 5
- statement-timeout: 5s

## Migrations
- path: ./migrations/main/
````

`providers/enterprise.md`

```markdown
# enterprise
企業客戶用，每個租戶一個獨立 database，用 PostgreSQL。
連線用 secret PG_ENTERPRISE，每個租戶實際憑證由平台對照表決定。
```

（lock：`type: postgres`、`mode: database`、`secret: PG_ENTERPRISE`、`shared: false`，格式同上。）

`providers/geo.md`

```markdown
# geo
共用嘅地區資料庫，PostgreSQL，schema 隔離，只讀。
連線用 secret PG_GEO。
只准 catalog/v1/list-items 用。
```

`providers/geo.lock.md`（節錄 section，frontmatter 格式同上）

````markdown
## Connection
- type: postgres
- secret: PG_GEO

## Isolation
- mode: schema
- rls: forced

## Access
- shared: true
- role-grants: [SELECT]
- allowed-endpoints: [catalog/v1/list-items]

## Limits
- pool-per-tenant: 5
- statement-timeout: 5s

## Migrations
(none)
````

### 11.2 Pipeline

`pipelines/login-guard.md`

```markdown
# login-guard
登入用嘅防護：每個 IP 每分鐘最多 10 次，每個 email 每分鐘最多 5 次。
參數：ip-per-minute（整數，預設 10）、account-per-minute（整數，預設 5）。
要喺所有 method 之前行，並且記低審計記錄（唔記密碼）。
```

`pipelines/login-guard.lock.md`

````markdown
---
lock-version: 1
kind: pipeline
id: login-guard
source: pipelines/login-guard.md
source-hash: sha256:51bd...07
source-type: natural
dependencies: []
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items: [{ item: rate-limit-new, risk: medium, reason: "登入防爆破" }]
---
## Parameters
| name | type | default | min | max |
|---|---|---|---|---|
| ip-per-minute | integer | 10 | 1 | 600 |
| account-per-minute | integer | 5 | 1 | 60 |

## Order
- must-run: before-steps

## Steps
### s1 rate-limit-check
- op: rate-limit-check
- key: ip
- per-minute: params.ip-per-minute
- source-lines: 2-4

### s2 rate-limit-check
- op: rate-limit-check
- key: input.email                 # 4.3：normalize 之後 HMAC，唔儲存明文
- per-minute: params.account-per-minute
- source-lines: 2-4

### s3 audit
- op: audit
- event: login-attempt
- fields: [req.ip]
- hash: [req.body.email]           # 決定 68：記 HMAC，唔記明文 email
- redact: [req.body.password]
- source-lines: 2-4
````

### 11.3 api 定義同版本

`api/users/_definition.md`

```markdown
# users
用戶管理 API。可用 provider：main 同 enterprise，預設 main。
所有 endpoint 都要 JWT 認證。
```

`api/users/_definition.lock.md`（節錄 section；frontmatter 有 `kind: api`、`scope: definition`）

````markdown
## Providers
- allowlist: [main, enterprise]
- default: main

## Pipelines
### p1 auth
- mode: jwt

## Defaults
- timeout: 10s
- body: 1MB
````

`api/users/v1/_version.md`

```markdown
# users v1
每分鐘限 60 次。呢個版本已經 deprecated，2027-06-30 停用。
```

`api/users/v1/_version.lock.md`（節錄；`scope: version`）

````markdown
## Pipelines
### p1 rate-limit
- per-minute: 60
- key: user                        # 決定 87：已認證 endpoint 按用戶計

## Defaults
(none)

## Lifecycle
- deprecated: true
- sunset: 2027-06-30
````

### 11.4 `api/users/v2/create-user/create-user.md`（路由 `POST /users/v2/create-user`）

```markdown
# 建立用戶
POST。只有 admin 可以用，每分鐘最多 30 次。
請求 body 要有 email（合法 email）同 name（1 至 50 字）。
用 user.register 建立用戶（唔設密碼、未驗證），再用 user.invite 寄邀請信。
邀請信寫入 outbox 之後回 202（新建立同重試都一樣，決定 95），回 `{ user_id, created, invite_status: "invite_pending" }`（`created` 分辨新建立同重試），並帶 Location。
email 已經屬於已驗證或者有密碼嘅用戶就回 409 EMAIL_TAKEN；驗證唔通過回 422。
```

`api/users/v2/create-user/create-user.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: users/v2/create-user
source: api/users/v2/create-user/create-user.md
source-hash: sha256:7f02...aa
source-type: natural
dependencies:
  - { id: user/methods/invite, kind: method, hash: sha256:5b7d...e4 }
  - { id: user/methods/register, kind: method, hash: sha256:c3e1...90 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items: [{ item: roles, risk: high, reason: "只限 admin 建立用戶" }]
---
## Route
- method: POST
- path: /users/v2/create-user

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt
- roles: [admin]

### p2 rate-limit
- per-minute: 30
- key: user                        # 決定 87

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| body.email | string | format: email | yes |
| body.name | string | min: 1, max: 50 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 202 | { user_id, created, invite_status } | Location |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { user_id: u.id, created: u.created, invite_status: "invite_pending" }

## Steps
### s1 method
- op: method
- target: user.register
- args: transform.in
- out: u                           # u.created：true = 新建立，false = 重試（未接受邀請）
- source-lines: 2-6

### s2 method
- op: method
- target: user.invite
- args: { user_id: u.id }
- out: (none)
- source-lines: 2-6

### s3 respond
- op: respond
- status: 202   # 決定 95：邀請已入 outbox，invite_status = invite_pending
- value: transform.out
- location: /users/v2/{u.id}
- source-lines: 2-6

## Errors
| status | code | from |
|---|---|---|
| 409 | EMAIL_TAKEN | EMAIL_TAKEN |
| 409 | ALREADY_ACTIVE | ALREADY_ACTIVE |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 503 | INVITE_NOT_QUEUED | user.invite（outbox 冇寫入，transaction rollback） |
````

- 邀請（決定 95，已確認（產品經理 2026-10-14））：邀請信寫入 transactional outbox，`email_outbox` 列同其他寫入喺同一個 transaction 寫入；commit 之後 create-user 回 **202**，body `{ user_id, invite_status: "invite_pending" }`。Mail worker commit 之後先寄，commit 之前永遠唔寄；失敗按退避重試，超過重試上限將邀請狀態標 `delivery_failed`（狀態值，唔係錯誤碼）。Admin 可以查詢用戶嘅邀請狀態（`invite_pending`／`sent`／`delivery_failed`），亦可以 call re-invite（11.4b）重寄（發新 token 同新信）。只有 transaction 失敗（outbox 冇寫入）先回 503 `INVITE_NOT_QUEUED`（帶 `Retry-After`），成個 request rollback（決定 99），唔會留低半完成狀態。實作見 issue #26。
- 失敗恢復（決定 85，已確認（產品經理 2026-10-14））：v1.8 起一個 request 一個 transaction（決定 99），s2 失敗會 rollback s1；但 commit 之後先失敗（例如回應途中斷線）或者舊資料仍然可能有「已建立、未邀請」嘅用戶。Admin 用同一個 body 重試 create-user：`user.register` 對已存在、未驗證、冇密碼嘅用戶回原有 id，s2 重新寄邀請，回 202 `{ user_id, created: false, invite_status: "invite_pending" }`；只有已驗證或者有密碼先回 409 `EMAIL_TAKEN`；或者用 11.4b 嘅 re-invite endpoint。
- `EMAIL_TAKEN`（決定 84，已確認（產品經理 2026-10-14））：只限 admin（已認證、`roles: [admin]`、按用戶限流、audit-log），admin 本身可以列出租戶用戶，所以 409 唔當 enumeration 洩漏；public 註冊（12.11）照樣唔分辨。
- 同一個 email 自己註冊緊（未驗證、冇密碼）：當同一個用戶，邀請 token 取代註冊 token（決定 88，已確認（產品經理 2026-10-14））。
- 邀請流程（決定 69，已確認（產品經理 2026-10-14））：admin 建立嘅用戶冇 password、`email_verified_at` 係 null，所以未接受邀請之前唔可以 login（決定 55）。完整流程見 12.11.9。

### 11.4a Method 範例：`user.register` 同 `user.invite`（決定 84、85、88，已確認（產品經理 2026-10-14））

`models/user/methods/register.md`

```markdown
# register
admin 建立用戶，唔設密碼、未驗證。
輸入：email（合法 email，最多 254 字）、name（1 至 50 字）。
如果 email 已經存在，而且用戶未驗證、冇密碼，就當重試：唔改任何欄位，回原有用戶。
如果 email 屬於已驗證或者有密碼嘅用戶，就 fail EMAIL_TAKEN。
回傳 id、email、name，同埋係咪新建立。
```

`models/user/methods/register.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/register
source: models/user/methods/register.md
source-hash: sha256:c3e1...90
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
build-mode: release
security-items:
  - { item: write-sql, risk: medium, reason: "建立用戶" }
---
## Signature
- input: { email: { type: string, format: email, max: 254, required: true }, name: { type: string, min: 1, max: 50, required: true } }
- output: { id: uuid, email: string, name: string, created: boolean }
- errors: [EMAIL_TAKEN]
- writes: true

## Calls
(none)

## Steps
### s1 query
- op: query
- provider: main
- table: users
- sql: `with ins as (insert into users (email, name) values ($1, $2) on conflict (tenant_id, email) do nothing returning id, email, name, true as created) select id, email, name, created from ins union all select id, email, name, false as created from users where email = $1 and email_verified_at is null and password is null and deleted_at is null and not exists (select 1 from ins)`
- params: [input.email, input.name]
- writes: true
- scope: pk                     # conflict target (tenant_id, email)；冇 UPDATE
- columns: { insert: [email, name] }
- out: u
- source-lines: 2-6

### s2 validate
- op: validate
- when: `u.id == null`
- fail: EMAIL_TAKEN
- source-lines: 2-6

### s3 respond
- op: respond
- value: { id: u.id, email: u.email, name: u.name, created: u.created }
- source-lines: 2-6
````

- 一個 statement：CTE `ins` 做 `insert ... on conflict do nothing returning ..., true as created`；冇插入（email 已存在）就由第二段 SELECT 搵「未驗證、冇密碼、未刪除」嘅原有列，`created` 明確係 `false`；兩段都冇結果就 `EMAIL_TAKEN`（決定 85）。唔靠 `xmax` 呢類內部欄位，in-memory 執行器亦容易模擬。
- 冇 UPDATE，重試唔會改 `name`。驗證器接受「insert CTE + 同表 SELECT」呢個固定模式做 idempotent create。
- 並發：兩個同時 create 同一個 email，輸咗嗰個嘅 SELECT 可能睇唔到另一個未 commit 嘅列而回 `EMAIL_TAKEN`；admin 重試就會得到 200。

`models/user/methods/invite.md`

```markdown
# invite
admin 寄邀請信。
輸入：user_id（uuid）。
產生邀請 token，72 小時有效；只可以邀請未驗證嘅用戶，已驗證就 fail ALREADY_ACTIVE。
更新用戶嘅驗證 token、到期時間，用途係 invite（取代任何舊嘅註冊或者邀請 token）。
用 email-outbox.enqueue（purpose 係 invite）寄邀請信。
冇輸出內容。
```

`models/user/methods/invite.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/invite
source: models/user/methods/invite.md
source-hash: sha256:5b7d...e4
source-type: natural
dependencies:
  - { id: email-outbox/methods/enqueue, kind: method, hash: sha256:81aa...c2 }
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
build-mode: release
security-items:
  - { item: write-sql, risk: medium, reason: "寫邀請 token" }
---
## Signature
- input: { user_id: { type: uuid, required: true } }
- output: {}
- errors: [ALREADY_ACTIVE, INVITE_NOT_QUEUED]
- writes: true

## Calls
- methods: [email-outbox.enqueue]
- external: (none)

## Steps
### s1 random-token
- op: random-token
- bytes: 32
- out: it                       # it.value（sensitive）、it.hash
- source-lines: 2-7

### s2 query
- op: query
- provider: main
- table: users
- sql: `update users set email_verify_hash = $2, email_verify_expires_at = now() + interval '72 hours', email_verify_purpose = 'invite' where id = $1 and email_verified_at is null and deleted_at is null returning id, email`
- params: [input.user_id, it.hash]
- writes: true
- scope: pk
- columns: [email_verify_hash, email_verify_expires_at, email_verify_purpose]
- out: upd
- source-lines: 2-7

### s3 validate
- op: validate
- when: `upd.id == null`
- fail: ALREADY_ACTIVE
- source-lines: 2-7

### s4 method
- op: method
- target: email-outbox.enqueue
- args: { user_id: upd.id, email: upd.email, token: it.value, purpose: invite }
- out: (none)
- source-lines: 2-7

### s5 respond
- op: respond
- value: {}
- source-lines: 2-7
````

- 用 `app_rw`（已認證 admin 路徑），唔係 public role。
- s4 失敗（outbox insert 錯誤）：method fail `INVITE_NOT_QUEUED`（503，決定 95）；寄信失敗唔會令 method fail，只會喺背景重試，超過上限標 `delivery_failed`；成個 request rollback（決定 99），s2 嘅 token 輪換都唔會生效，舊邀請連結照樣有效（如果未過期）；重試或者 re-invite 會發新連結。
- 用戶唔存在同已驗證都回 `ALREADY_ACTIVE`（admin 路徑，404／409 分唔分冇私隱影響；揀一個錯誤碼令 lock 簡單）。

### 11.4b Re-invite：`api/users/v2/re-invite/re-invite.md`（路由 `POST /users/v2/{id}/invite`，決定 86，已確認（產品經理 2026-10-14））

```markdown
---
body: 1KB
---
# 重新寄邀請
POST，路徑 /users/v2/{id}/invite。只有 admin 可以用，每個 admin 每分鐘最多 10 次。
用 user.invite 重新寄邀請，舊邀請即時失效。邀請入 outbox 之後回 202，body `{ invite_status: "invite_pending" }`（決定 95）；亦可以用嚟重寄 `delivery_failed` 嘅邀請。
用戶已經驗證回 409 ALREADY_ACTIVE。
```

`api/users/v2/re-invite/re-invite.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: users/v2/re-invite
source: api/users/v2/re-invite/re-invite.md
source-hash: sha256:e4a0...19
source-type: natural
dependencies:
  - { id: user/methods/invite, kind: method, hash: sha256:5b7d...e4 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
build-mode: release
security-items: [{ item: roles, risk: high, reason: "只限 admin 寄邀請" }]
---
## Route
- method: POST
- path: /users/v2/{id}/invite

## Access
- public: false
- public-writes: false

## Pipelines
### p1 auth
- mode: jwt
- roles: [admin]

### p2 rate-limit
- per-minute: 10
- key: user

## Limits
- provider: main
- timeout: 10s
- body: 1KB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.id | uuid | | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 202 | { invite_status } | |

## Transform
- in: { user_id: req.path.id }
- out: (none)

## Steps
### s1 method
- op: method
- target: user.invite
- args: transform.in
- out: (none)
- source-lines: 1-3

### s2 respond
- op: respond
- status: 202   # 決定 95：invite_status = invite_pending
- source-lines: 1-3

## Errors
| status | code | from |
| --- | --- | --- |
| 409 | ALREADY_ACTIVE | ALREADY_ACTIVE |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 503 | INVITE_NOT_QUEUED | user.invite（outbox 冇寫入，transaction rollback） |
````

- 503 時可以直接重試，每次重試都發新 token（決定 95）。

### 11.5 `api/users/v2/get-user/get-user.md`（路由 `GET /users/v2/{id}`）

```markdown
# 攞用戶
GET，路徑 /users/v2/{id}，id 係 uuid。
用 user.get 攞用戶，回 200 同 id、email、name；搵唔到回 404 NOT_FOUND。
v2 嘅 name 欄位叫 display_name。
```

`get-user.lock.md`（節錄 section；frontmatter 格式同 11.4，`dependencies` 包括 `user/crud/get`）

````markdown
## Route
- method: GET
- path: /users/v2/{id}

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
| 200 | { id, email, display_name } | |

## Transform
- in: { id: req.path.id }
- out: { id: u.id, email: u.email, display_name: u.name }

## Steps
### s1 method
- op: method
- target: user.get
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
| 404 | NOT_FOUND | NOT_FOUND |
````

- 呢個例子示範決定 2：`user.get` 只有一個版本，v2 嘅 `display_name` 改名由 api 層 `## Transform` 處理。

### 11.6 Public 讀取：`api/catalog/v1/list-items/list-items.md`（路由 `GET /catalog/v1/{slug}/items`）

`api/catalog/_definition.md`

```markdown
# catalog
貨品目錄。可用 provider：main 同 geo，預設 main。
```

`list-items.md`

```markdown
# 公開貨品清單
GET，路徑 /catalog/v1/{slug}/items，公開，唔使 auth，租戶由 slug 決定。
每個 IP 每分鐘最多 120 次。
query 要有 region（2 個字元）。
用 item.list-public 攞公開貨品，再用 region.find-by-code 搵地區；
回 200 同 items（id、name、price）同 region。地區搵唔到回 404 NOT_FOUND。
```

`list-items.lock.md`（節錄 section）

````markdown
## Route
- method: GET
- path: /catalog/v1/{slug}/items

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: false
- role: public_reader
- external-calls: forbidden

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 120
- key: ip

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | - | yes |
| query.region | string | length: 2 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { items: [{ id, name, price }], region } | |

## Transform
- in: { code: req.query.region }
- out: { items: items, region: region.region }

## Steps
### s1 method
- op: method
- target: item.list-public
- args: {}
- out: items
- source-lines: 5-5

### s2 method
- op: method
- target: region.find-by-code
- args: transform.in
- out: region
- source-lines: 5-5

### s3 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 6-6

## Errors
| status | code | from |
|---|---|---|
| 404 | NOT_FOUND | NOT_FOUND |
````

### 11.7 Public 建立資料：`api/catalog/v1/submit-review/submit-review.md`（路由 `POST /catalog/v1/{slug}/reviews`）

```markdown
---
body: 16KB
---
# 提交評論
POST，路徑 /catalog/v1/{slug}/reviews，公開，唔使 auth，租戶由 slug 決定。
訪客可以留評論，所以會寫入資料，要 captcha。
每個 IP 每分鐘最多 5 次，每個租戶每分鐘最多 100 次。
body：item_id（uuid）、rating（1 至 5）、comment（1 至 500 字）。
用 review.submit 建立評論，成功回 201 同 id。貨品唔存在回 404 NOT_FOUND。
```

`submit-review.lock.md`（節錄 section）

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
| path.slug | string | - | yes |
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

- body 有 `tenant_id`、`id` 或者 `created_by` 會即刻 422；`tenant_id` 由平台按 slug 注入。
- `review.submit` 透過 `item.get` 檢查貨品存在（寫入其他物件／讀其他物件都經佢自己 method），然後一個參數化 `INSERT INTO reviews (item_id, rating, comment) VALUES ($1, $2, $3)`；`comment` 喺 `review` model 標 `untrusted: true, max: 500`，所有輸出做 encoding。

相關 method：

- `models/user/methods/register.md`：「如果 email 已經有人用就 fail EMAIL_TAKEN，否則建立用戶，回傳 id、email、name。」編譯成 1 個 SELECT 檢查、1 個參數化 INSERT（`writes: true`）。
- `models/item/item.md` 用 provider `main`、table `items`；`models/item/methods/list-public.md`（`writes: false`）負責揀 `is_public = true` 嘅項目。
- `models/region/region.md` 用 shared provider `geo`、table `regions`；`models/region/methods/find-by-code.md`（`writes: false`）負責按 code 搵地區，冇結果就 fail NOT_FOUND。

## 12. Models 層

### 12.1 目錄同命名

```text
models/{object}/{object}.md                   # 物件定義（自然語言）：欄位、provider、表、CRUD
models/{object}/{object}.lock.md
models/{object}/methods/{method}.md           # method 定義（自然語言），單一版本
models/{object}/methods/{method}.lock.md
```

- `{object}` 用單數 kebab-case（例如 `user`、`order-item`）；`{method}` 用 kebab-case 動詞（例如 `login`、`reset-password`、`sign-token`）。
- 檔名必須等於資料夾名。
- models 唔分 version（決定 1、2）；唔存在 `models/{object}/{version}/`。
- models 永遠唔會被路由，只可以由 api 或者 pipeline 嘅 `method` step、或者其他 method 呼叫。

### 12.2 物件 `{object}.md`

source 用自然語言描述欄位，例如「email 係唯一嘅合法 email」。編譯成 lock 嘅 `## Fields` 表：

| 欄位屬性 | 說明 |
| --- | --- |
| `type` | uuid、string、string[]、integer、number、boolean、timestamp、password、json |
| `primary`、`unique`、`default` | 約束同預設值（`gen`、`now`） |
| `required`、`nullable` | 必填同可為 NULL；必填 header 真值正規化成現有 `required` rule |
| `format`、`min`、`max` | 驗證規則 |
| `sensitive` | 敏感欄位（見 12.9） |

- 平台會自動加 `tenant_id` 欄位同 RLS policy（寫入 `## Policies`）；用戶唔可以自己宣告 `tenant_id`。
- 多欄唯一鍵（決定 107，已確認（產品經理 2026-10-14））：source 用自然語言講（例如「同一張訂單入面每件商品只可以出現一次」），編譯成 model lock 嘅 `## Constraints`：

  ```md
  ## Constraints

  - unique: [order_id, product_id]
  ```

  - 平台自動喺前面加 `tenant_id`（即係 `(tenant_id, order_id, product_id)`）；列出嘅欄位一定要喺 `## Fields`，唔可以係 `sensitive` 或者 `password`。
  - `## Constraints` 可省略；有就緊接 `## Fields`，喺 CRUD 之前。Structured source 可以直接寫。空 list、單欄（請用 Fields unique）、tenant_id、重複欄位、重複 entry（包括調轉次序）、未知 key、唔存在嘅欄位全部報 error。Entries 按 canonical list 字串排序，欄位保留宣告次序。
  - 一定要有同 provider 嘅 migration lock 建對應 unique index，欄位必須依序完全等於 `(tenant_id, col_a, col_b)`，冇就 error，指出 model、欄位同預期 index。驗證只接受 `CREATE UNIQUE INDEX [IF NOT EXISTS] name ON table [USING btree] (cols)`；table 必須無 schema qualification（用 model 自己 schema 嘅 search_path），cols 只准普通欄位，可用雙引號。WHERE 只准冇，或者剛好 `WHERE deleted_at IS NULL`。其他 predicate、AND、expression、INCLUDE、WITH、非 btree、NULLS NOT DISTINCT 同其他未支援語法（包括 ALTER TABLE／CREATE TABLE 入面嘅 UNIQUE）一律唔可以當證據，fail closed。
  - 所有路徑（CRUD create／update、method `query`、`serve --memory`）違反時回 409 `UNIQUE_VIOLATION`；message 只講邊個約束，唔回顯欄位值。單欄 `unique` 一樣用呢個錯誤碼（除非 lock 宣告咗業務錯誤碼，例如 `EMAIL_TAKEN`）。
  - Memory executor 按租戶原子檢查 Fields unique 同 Constraints；有 deleted_at 就忽略已 soft delete 嘅列，NULL 唔互相衝突。SQLSTATE 23505 用獨立 UniqueViolation，永遠唔進入 retry；40001／40P01 保留 Conflict／TX_CONFLICT。冇 DB constraint 名時用 generic message，唔包含重複值。
- `type: password` 嘅欄位：
  - 寫入時一律用 argon2id hash，參數見 4A.2（決定 46）。
  - 永遠唔可以喺 SELECT 結果、method 輸出、回應或者 log 出現，只可以用 `verify-password` step 比對。
- 物件只可以對應一個 provider 同一張表。用 `shared: true` provider 嘅物件只可以有 `writes: false` 嘅 method，而且呢個 provider 嘅 `allowed-endpoints` 一定要包括（直接或者間接）呼叫佢嘅 api endpoint。
- 欄位唔分 version（決定 1）；改欄位要經 migration，migration 改咗會令 model lock 嘅依賴 hash 失效。

### 12.3 Method `{method}.md`

source 用自然語言描述輸入、輸出、錯誤同做法。編譯成 lock 嘅：

- `## Signature`：`input`、`output`、`errors`、`writes`；部署時編譯成 schema，未宣告嘅 input 一律拒絕。
- `## Calls`：呢個 method 會呼叫嘅其他 method 同對外 call 目標（靜態清單）。
- `## Steps`：只准 2A.4 method 白名單 op。

規則：

- `query` 只可以用自己物件嘅 provider 同表。
- 寫入其他物件一定要用 `method` op 呼叫嗰個物件自己嘅 method（決定 3）；讀取其他物件都一樣要經佢嘅 method。
- 錯誤用 `validate` 嘅 `fail: CODE` 拋出，CODE 一定要喺 `errors` 入面。
- method 只有一個版本；唔准寫 `@v1`、`@v2`（決定 2）。要改 method 簽名而唔想影響舊 api version，應該喺舊 api 嘅 `## Transform` 做轉換；真係唔兼容就開一個新名嘅 method（例如 `register-with-phone`）。

### 12.3a 三層分工同單一職責

- model 係「係咩」：欄位、表、provider、CRUD 宣告。
- method 係「點做」：一個 method 只做一件事，所有 query 同對外 call 都喺呢度。
- api 係「對外」：路由、pipeline、Request、Response、Transform；Steps 只准 `method`、`validate`、`respond`。
- method 可以用 `method` op 呼叫其他 method 組合；部署時建立呼叫圖，有循環就報 error，深度上限 3 層（由 api 或者 pipeline 算起）。
- 單一職責：每個 method 最多 1 個寫入 sql，而且最多只寫一張表；超出出 warning，建議拆開（決定 12）。

### 12.4 喺 api 呼叫 method

api source 寫「用 user.login 驗證帳戶密碼」，編譯成：

````markdown
### s1 method
- op: method
- target: user.login
- args: transform.in
- out: result
````

- `target` 只可以係 `{object}.{method}`，部署時靜態綁定，唔可以由 request 決定，冇版本後綴。
- method 拋出嘅錯誤，喺 api lock 嘅 `## Errors` 對應狀態碼（例如 `| 401 | INVALID_CREDENTIALS | INVALID_CREDENTIALS |`）；冇對應嘅錯誤一律回 500 INTERNAL。
- 唔同 api version call 同一個 method，差異由各自 `## Transform` 處理。
- 呼叫深度上限 3 層，唔准循環呼叫。

### 12.5 版本同繼承

- 物件定義（欄位同表）唔分 version，改欄位要經 migration。
- method 只有單一版本，冇 version 資料夾，冇 method 層嘅 deprecated／sunset；deprecation 只喺 api `_version` 做。
- method 唔會繼承 api 嘅 pipelines。auth 只喺 api／pipeline 層執行，method 會收到 `ctx.tenant_id` 同 `ctx.user`，但係唔可以修改。

### 12.6 安全

- method 一樣行 7.1 嘅租戶隔離：喺 request 嘅同一個 transaction、以租戶 role 登入嘅連線上執行，RLS 強制生效（決定 97、99）。
- 由 public endpoint call 嘅 method：
  - tenant 由 slug 解析，平台注入。
  - 只讀 step 用 `public_reader` role；寫入 step 用 `public_writer` role，grant 只限 lock 用到嘅表、欄位同操作（見 7.2）。
  - 可以係 `writes: true`（決定 4），但要符合 7.2 全部 public 寫入規則。
  - 預設唔准 `call`。
- `verify-password` 一定以固定時間比對。用戶唔存在嘅時候，平台會用假 hash 比對一次，令耗時一致；兩種情況都回同一個錯誤碼，防止 enumeration。
- token 簽發放喺 model 層 method（例如 `user.sign-token`，決定 5、30）：
  - 只有 `## Signature` 明確寫 `capabilities: [sign-token]` 嘅 model method 可以用 `sign-jwt` op；唔靠 method 名推斷（叫 `sign-token` 但冇旗標一樣唔准）。
  - capability 由 source 自然語言描述（例如「呢個 method 負責簽 token」），編譯後屬高風險安全項目。
  - claim 白名單：method 只可以傳 `sub` 同平台允許嘅自訂 claim（例如 `sid`）；`roles` 由平台喺簽發時從 DB 讀（用 `sub` 喺當前租戶搵用戶）；`tenant_id` 只有一個來源：平台由 slug（public）或者已驗證 token（auth）解析出嚟嘅 `ctx.tenant_id`，唔由 client、method 或者 `users` 表提供（決定 62）。method 唔可以傳入或者覆蓋任何一個。
  - 平台自動加 `iss`、`aud`、`exp`、`nbf`、`iat`、`jti`；method 唔可以覆蓋。
  - `ttl` 有上限（access token 最多 1h）。
  - 只可以簽 JWT，alg 只准 ES256（預設）或者 RS256，RS256 key 最少 2048 bit，唔夠就啟動 fail closed（決定 139，見 4.1a）；私鑰喺平台 secret，lock 只寫 `key-ref`。
  - 其他 method（例如 `user.login`）同 pipeline 要簽 token，就用 `method` op 呼叫 `user.sign-token`。
  - refresh token 由 `sign-jwt` 嘅 `issue-refresh` 處理（見 4A.1）。
- `capabilities: [set-password]`：只有標咗呢個 capability 嘅 model method 可以喺 public 路徑寫 password 欄位，值一定要係 `hash-password` 輸出（見 7.2）；已認證路徑嘅改密碼 method 同樣建議標呢個 capability，方便 review。
- login 類 endpoint 一定同時按 IP 同帳戶限流（例如 11.2 嘅 `login-guard`），並跟 4A.3 嘅鎖定同 captcha 規則。

### 12.6a 列層擁有權（決定 70，已確認（產品經理 2026-10-14））

- RLS 只保證租戶隔離；租戶內「邊個可以掂邊列」由列層擁有權規則決定。
- model 可以喺 `## Table` 宣告 `owner-key: {欄位}`（例如 `user_id`，或者 `users` 表本身嘅 `id`）。
- 有 `owner-key` 嘅 model：非 admin 呼叫者嘅 read／update query，驗證器要求 SQL 帶 `{owner-key} = ctx.user.id`；insert 嘅 `{owner-key}` 一定由 `ctx.user.id` 填，唔可以由 input 提供。
- 放寬（例如「同租戶所有已登入用戶可以讀」或者「admin 可以掂任何列」）一定喺 source 明寫，lock 記 `ownership: tenant-wide` 或者 `ownership: role(admin)`，屬高風險安全項目。
- 冇 `owner-key` 嘅 model 視為租戶共享資料，驗證器出 warning。

### 12.7 驗證規則

models 層嘅所有檢查都喺 lock 層做，完整清單見 10.1 同 10.8。重點：

- 結構：路徑唔合 `models/{object}/{object}.md` 或者 `models/{object}/methods/{method}.md`；檔名唔等於資料夾名。
- 物件：冇 provider 或者 table；provider 未登記；shared provider 物件有寫入 method；自己宣告 `tenant_id`；欄位 type 未知。
- method：白名單以外 op；query 去咗其他表或者 provider；`writes: false` 有寫入 sql；有 `DELETE` sql；sql 唔參數化；`fail` 用咗未宣告錯誤碼；output 有 password 欄位；揀咗 password 欄位但唔係畀 `verify-password` 用；`sign-jwt` 喺冇 `capabilities: [sign-token]` 嘅 method 出現；有 DDL、TRUNCATE，或者 UPDATE 冇用主鍵／RLS 限定。
- 呼叫：唔存在嘅 object 或者 method；寫咗版本後綴；input 唔合簽名；循環或者超過 3 層。
- Warning：method 錯誤碼喺 api 冇對應狀態碼；單一職責超出。

### 12.8 完整範例（user model、user.sign-token、user.login、login api）

`models/user/user.md`

```markdown
# user
用戶資料，放喺 provider main 嘅 users 表。

欄位：
- id：uuid，主鍵，自動產生。
- email：合法 email，唯一。
- name：最多 50 字。
- password：密碼。
- roles：字串陣列。
- last_login_at：時間，可以冇。
- email_verified_at：email 驗證時間，可以冇。
- email_verify_hash：驗證 token 嘅 hash，唯一，可以冇。
- email_verify_expires_at：驗證 token 到期時間，可以冇。
- name 同 email 係訪客註冊時填，當 untrusted 內容。

提供 get、list、update 三個 CRUD。list 每頁預設 20，最多 100，可以按 created_at 或者 name 排序，可以按 email 篩選。
```

`models/user/user.lock.md`

````markdown
---
lock-version: 1
kind: model
id: user
source: models/user/user.md
source-hash: sha256:aa41...0f
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: migrations/main/0004-add-email-verification, kind: migration, hash: sha256:5e77...b1 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: public-writable-column, risk: high, reason: "last_login_at 畀 public 登入寫" }
---
## Table
- provider: main
- table: users

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| email | string | format: email, unique, max: 254, untrusted, personal: standard | no |
| name | string | max: 50, untrusted, personal: standard | no |
| password | password | argon2id, nullable（公開註冊同邀請未設定密碼之前係 null，決定 67、69） | no |
| roles | string[] | default: [], max-items: 20 | no |
| last_login_at | timestamp | nullable, public-writable | no |
| email_verified_at | timestamp | nullable, public-writable (confirm-verification) | no |
| email_verify_hash | bytes | nullable, unique, public-writable | no |
| email_verify_expires_at | timestamp | nullable, public-writable | no |
| email_verify_purpose | string | nullable, enum: [register, invite]（決定 67、69） | no |
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
- policy: TO tenant_<id>_*  USING／WITH CHECK (tenant_id = '<tenant_id 常數>')   # 每個租戶 build 時寫死常數；邊界係租戶 LOGIN role 同自己 schema，GUC 只用嚟 log（決定 97）
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer [INSERT(email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose), UPDATE(email_verify_hash, email_verify_expires_at, email_verify_purpose, password, email_verified_at, last_login_at)]   # model 層係所有 public endpoint 嘅聯集；每個 api 嘅實際 grant 只限佢 lock 用到嘅欄位（見 12.11.6、12.11.7）
````

`models/user/methods/sign-token.md`

```markdown
# sign-token
幫用戶簽一張 access token。
呢個 method 負責簽 token。
輸入：sub（用戶 id，uuid）。
輸出：token（access token）同 refresh_token。
token 有效 1 小時，用 ES256，key 用 JWT_SIGNING_KEY。
```

`models/user/methods/sign-token.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/sign-token
source: models/user/methods/sign-token.md
source-hash: sha256:3b90...d4
source-type: natural
dependencies:
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: sign-token, risk: high, reason: "唯一簽 token 入口" }
---
## Signature
- input: { sub: { type: uuid, required: true } }
- output: { token: string, refresh_token: string }
- errors: []
- writes: false
- capabilities: [sign-token]

## Calls
(none)

## Steps
### s1 sign-jwt
- op: sign-jwt
- alg: ES256
- key-ref: JWT_SIGNING_KEY
- claims: { sub: input.sub }
- platform-claims: [iss, aud, exp, nbf, iat, jti, roles, tenant_id]
- platform-claims-source: { roles: users.roles where id = sub, tenant_id: ctx.tenant_id }
- ttl: 1h
- ttl-cap: 1h
- issue-refresh: platform-decided   # 登入 new-family；token-refresh rotate
- out: token
- source-lines: 1-6

### s2 respond
- op: respond
- value: { token: token.access, refresh_token: token.refresh }
- source-lines: 2-6
````

`models/user/methods/login.md`

```markdown
# login
用 email 同密碼登入。
輸入：email（合法 email，必填）、password（1 至 256 字，必填）。
輸出：user（id、email、name、roles）同 token。

做法：
1. 用 email 搵用戶。
2. 比對密碼；搵唔到用戶或者密碼錯都 fail INVALID_CREDENTIALS。
3. 成功就記低最後登入時間。
4. 用 user.sign-token 簽 token，回傳 user 同 token。
```

`models/user/methods/login.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/login
source: models/user/methods/login.md
source-hash: sha256:e81f...2a
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
  - { id: user/methods/sign-token, kind: method, hash: sha256:3b90...d4 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: write-sql, risk: medium, reason: "記錄最後登入時間" }
---
## Signature
- input: { email: { type: string, format: email, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: { user: { id, email, name, roles }, token: string, refresh_token: string }
- errors: [INVALID_CREDENTIALS]
- writes: true

## Calls
- methods: [user.sign-token]
- external: (none)

## Steps
### s1 query
- op: query
- provider: main
- table: users
- sql: `select id, email, name, roles, password from users where email = $1 and deleted_at is null and email_verified_at is not null`
- params: [input.email]
- writes: false
- out: u
- source-lines: 6-10

### s2 verify-password
- op: verify-password
- cost: high
- hash: u.password
- plain: input.password
- missing-user: dummy-hash
- out: ok
- source-lines: 6-10

### s3 validate
- op: validate
- when: `ok != true`
- fail: INVALID_CREDENTIALS
- source-lines: 6-10

### s4 query
- op: query
- provider: main
- table: users
- sql: `update users set last_login_at = now() where id = $1`
- params: [u.id]
- writes: true
- scope: pk
- columns: [last_login_at]
- out: (none)
- source-lines: 6-10

### s5 method
- op: method
- target: user.sign-token
- args: { sub: u.id }
- out: t
- source-lines: 6-10

### s6 respond
- op: respond
- value: { user: { id: u.id, email: u.email, name: u.name, roles: u.roles }, token: t.token, refresh_token: t.refresh_token }
- source-lines: 6-10
````

- s4 係寫入，所以 `writes: true`；login 係 public endpoint，s4 用 `public_writer` 執行，grant 只有 `UPDATE(last_login_at)`（決定 4 令呢個情況變得可行）。

`api/auth/_definition.md`

```markdown
# auth
認證 API，只用 provider main。
```

`api/auth/v1/login/login.md`（路由 `POST /auth/v1/{slug}/login`）

```markdown
# 登入
POST，路徑 /auth/v1/{slug}/login。
公開，唔使 auth，租戶由 slug 決定。
呢個 endpoint 會寫入資料（記最後登入時間），准許公開寫入。
用 login-guard 防護：每個 IP 每分鐘 10 次，每個帳戶每分鐘 5 次；另外每個租戶每分鐘最多 300 次。

請求 body：email（合法 email，必填）、password（1 至 256 字，必填）。
用 user.login 登入，成功回 200 同 token、refresh_token 同 user（id、email、name、roles）。
帳戶密碼錯回 401 INVALID_CREDENTIALS；驗證唔通過回 422。
```

`api/auth/v1/login/login.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: auth/v1/login
source: api/auth/v1/login/login.md
source-hash: sha256:4c2d...19
source-type: natural
dependencies:
  - { id: auth/_definition, kind: api, hash: sha256:77e0...aa }
  - { id: login-guard, kind: pipeline, hash: sha256:51bd...07 }
  - { id: user/methods/login, kind: method, hash: sha256:e81f...2a }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: public, risk: high, reason: "登入入口一定要公開" }
  - { item: auth-removed, risk: high, reason: "未登入先要登入" }
  - { item: public-writes, risk: high, reason: "只寫 last_login_at" }
---
## Route
- method: POST
- path: /auth/v1/{slug}/login

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_reader + public_writer
- writable-columns: { users: [last_login_at] }
- challenge: { type: captcha, when: after-failures, after-failures: 3 }   # 4A.3
- min-response: 500ms
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required

## Pipelines
### p1 -auth
- removed: true

### p2 login-guard
- ip-per-minute: 10
- account-per-minute: 5

### p3 rate-limit
- per-minute: 300
- key: tenant

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | - | yes |
| body.email | string | format: email | yes |
| body.password | string | min: 1, max: 256 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { token, refresh_token, user: { id, email, name, roles } } | |

## Transform
- in: { email: req.body.email, password: req.body.password }
- out: { token: result.token, refresh_token: result.refresh_token, user: result.user }

## Steps
### s1 method
- op: method
- target: user.login
- args: transform.in
- out: result
- source-lines: 7-9

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 7-9

## Errors
| status | code | from |
|---|---|---|
| 401 | INVALID_CREDENTIALS | INVALID_CREDENTIALS |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
````

- PR review 會逐項高亮：`public: true`、`-auth`、`public-writes: true`（以上高風險；v1.8 起係資訊性高亮，review 要求由 rulesets（分支規則） 決定）、新增寫入 sql（`user.login` s4，中風險）。
- 有效權限摘要：`POST /auth/v1/{slug}/login` → public；role public_reader + public_writer；讀 users；寫 users.last_login_at；間接簽 token（經 `user.sign-token`）；冇對外 call；IP 10/min、帳戶 5/min、租戶 300/min；有每日配額。

### 12.9 CRUD

- 預設唔會自動生成任何 CRUD。model source 要明確講（例如「提供 get、list、update」或者「提供全部 CRUD」），lock 嘅 `## CRUD` 列咗邊樣就注入邊樣。
- CRUD 選項（喺 source 用自然語言講，編譯成 `## CRUD` 表）：
  - `list`：`pagination`（`offset` 預設，或者 `cursor`，決定 106）、`page-size`（預設 20）、`max-page-size`（預設 100，平台上限 500，決定 71，已確認（產品經理 2026-10-14）；超過上限驗證器報 error；offset 同 cursor 模式一樣跟決定 71）、`sort` 白名單、`filter` 白名單（欄位後面加 `: range` 開區間篩選）。
  - `update`：`partial`（預設 true）。
  - `delete`：一律軟刪除，冇 `soft: false` 選項（決定 13）。
- 注入嘅 method 名同簽名（每個都有平台產生同簽名嘅 lock，`source-type: structured`，`id` 例如 `user/crud/get`）：
  - `create`：input 係所有可寫欄位，output 係物件。
  - `get`：input 係 `id`，冇結果就 fail `NOT_FOUND`。
  - `list`：offset 分頁（決定 14），input 係 `page`（由 1 開始）、`page-size`（唔可以超過 `max-page-size`，超過回 422）、`sort`、`filter`；只准用白名單欄位排序同篩選；output 係 `{ items, page, page_size, has_more }`。
  - `list`（`pagination: cursor`，決定 106，已確認（產品經理 2026-10-14））：
    - 例：`| list | pagination: cursor, page-size: 20, max-page-size: 100, sort: [created_at, price_cents], filter: [category, price_cents: range] |`。
    - Input：`cursor`（選填）、`page-size`、`sort`、`filter`；output 係 `{ items, next_cursor }`，最後一頁 `next_cursor` 係 `null`。
    - 排序穩定：平台一定喺 sort 欄位後面加主鍵 `id` 做 tiebreaker；sort 欄位一定要 NOT NULL（驗證器檢查）。
    - Cursor：`base64url(payload) + "." + HMAC-SHA-256(租戶 cursor key, payload)`；payload 有版本、最後一列嘅 sort 值同 `id`、sort 同 filter 嘅 hash；唔加密，但唔可以改。簽名唔啱、版本唔啱、或者同今次 sort／filter 唔對應，回 400 `INVALID_CURSOR`。Cursor 唔跨租戶用。
    - 區間篩選：`filter` 入面標 `: range` 嘅欄位，query 參數係 `{field}[gte]`、`{field}[gt]`、`{field}[lte]`、`{field}[lt]`；值按欄位類型驗證，唔啱回 422。
    - 編譯時驗證：sort 同 range 欄位一定要喺 `## Fields`、唔係 `sensitive`／`password`，而且有包括 `tenant_id` 嘅 index（由 migration lock 比對），否則 error。
    - SQL 一律參數化，用 keyset 條件（`(sort..., id) > (...)`），唔用 OFFSET。
  - `update`：預設係部分更新（PATCH 語意），一定做樂觀鎖檢查（決定 48）：
    - `get` 同 `update` 回應都帶 `ETag`（由 `id` 加 `updated_at` 計出嘅強 ETag）。
    - 呼叫 update 嘅 api 一定要帶 `If-Match` header；api `## Transform` 將 `req.header.if-match` 映射做 method input `if_match`。
    - 冇 `If-Match` 回 428 `PRECONDITION_REQUIRED`；唔對應回 412 `PRECONDITION_FAILED`。
    - `updated_at` 照樣係系統欄位，唔可以由 input 寫。
  - `delete`：軟刪除，即係設 `deleted_at`；軟刪除咗嘅資料，`get` 同 `list` 會自動排除。
- 軟刪除永遠唔會實際刪除資料，冇 purge 機制，亦冇保留期到期刪除（決定 13）；任何 lock 都唔准有 `DELETE` sql，DB role 亦冇 DELETE 權限。
- 私隱法例嘅刪除要求用 12.10 嘅 crypto-shredding 同匿名化處理，唔使開放 DELETE。
- SDK 內置最佳寫法：一律參數化 query、有分頁上限、排序同篩選欄位用白名單、行 RLS 同租戶隔離。
- 敏感欄位：
  - `password` 欄位喺 create 同 update 時自動 hash，永遠唔會出現喺任何輸出。
  - 欄位可以標 `sensitive: true`（例如電話），呢啲欄位唔會出現喺 list 輸出，亦唔可以用嚟 filter 或者 sort；get 預設都唔會輸出，要喺 CRUD 選項明確開（`sensitive-output`）。
  - `id`、`tenant_id`、`created_at`、`updated_at`、`deleted_at`、`erased_at` 呢啲系統欄位唔可以由 input 寫入。
- 覆蓋：如果喺 `models/{object}/methods/` 自己寫一個同名 method（例如 `get.md`），就會取代注入嘅版本，部署報告會列出被覆蓋咗嘅 method。
- CRUD method 同其他 method 一樣只有單一版本；唔同 api version 嘅差異用 `## Transform` 處理。
- 範例：`models/user/user.md` 寫「提供 get、list、update」，api 就可以 call `user.get`、`user.list`、`user.update`。

### 12.10 私隱刪除（crypto-shredding 同匿名化）

- 欄位分類：model 欄位可以標 `personal: sensitive`（例如身份證號碼、電話、地址、出生日期）或者 `personal: standard`（例如 name、email、評論內容）。
- 加密（sensitive）：每筆紀錄每個 sensitive 欄位有自己嘅 data key（DEK），用 AES-256-GCM 加密；DEK 用租戶 key（KEK，喺平台 KMS）包住，存喺平台 key store，唔同業務資料放埋一齊，唔入 DB 備份。加解密由平台自動做，lock 唔會見到 key。
- 觸發：
  - 租戶 admin 喺平台 UI 或者 CLI `subject erase --subject <id> --reason "..."` 觸發；資料主體嘅要求由租戶接收再觸發。
  - 一定要寫原因同要求參照；由一個 admin 提交、另一個 admin 確認（細租戶只得一個 admin 就加 24 小時延遲生效）。
- 流程（平台執行，唔經 lock）：
  1. 平台按 model 宣告搵出該主體所有紀錄（用 `subject-key` 欄位，例如 `user_id`）。
     - `subject-key` grammar（決定 65）：`subject-key: {column}` 或者 `subject-key: [{column}, ...]`（任何一個符合就算）；`{column}` 只可以係 `uuid` 欄位（指向 `users.id`，`users` 表自己用 `id`），或者 `hash:{column}`（`bytea` 欄位，值係 `HMAC-SHA-256(pepper, normalize(email))`，用嚟搵冇 `user_id` 嘅列，例如 `email_outbox.recipient_hash`）。
     - 有 `personal-fields` 但冇 `subject-key` 嘅 model，驗證器報 error。
  2. sensitive 欄位：銷毀相關 DEK，資料即時永遠讀唔返（crypto-shredding）。
  3. standard 欄位：用平台專用 erasure role 做 UPDATE，換成不可逆占位值（例如 `erased-<隨機>`），email 等唯一欄位用隨機唯一值；只准 UPDATE，冇 DELETE。
  4. 紀錄標 `erased_at`，同時軟刪除；行同關聯保留，統計唔受影響。
  5. 撤銷該主體所有 refresh token 家族同 API key。
- 時限：收到要求後 30 日內完成；平台目標係觸發後 24 小時內完成。
- 審計：記錄觸發人、確認人、原因、要求參照、受影響表同欄位、紀錄數量、完成時間；審計只用假名化主體 id，唔記任何個人資料本身。
- 備份：sensitive 欄位因為 DEK 已銷毀，備份入面都讀唔返；standard 欄位喺備份保留期（35 日）屆滿後自然消失；從備份還原時，平台會重播 erasure 紀錄冊，再做一次匿名化先開放使用。
- Log：個人資料本身唔准寫入 log（同 token、secret 一樣）；log 只記假名化 id，log 保留 30 日。
- 匿名化欄位喺 public 寫入黑名單（7.2）之外另有保護：erased 紀錄唔准再由 lock 寫入個人欄位。

### 12.11 完整範例：public 註冊同 email 驗證

呢個範例用到決定 52–56（已確認）同決定 67–69（已確認（產品經理 2026-10-14））。範例示範全部 public 寫入規則點樣一齊運作：slug 租戶、`public-writes`、`writable-columns`、黑名單、`set-password`、`confirm-verification`、captcha、每日配額、audit-log、固定 300ms、untrusted 內容、IP 加租戶限流、冇對外 call。

Email 正規化（決定 120，已確認（產品經理 2026-10-16））：login、註冊（register）、邀請（invite）、驗證（verify）、重發（resend）同 rate-limit 全部先用決定 120 正規化 email，再儲存同查找；`email` 欄位同唯一 index 只存正規化值。

現有資料衝突處理程序（決定 120）：

1. Migration 先計每個現有 email 嘅正規化值，搵出正規化後相同嘅多個帳戶。
2. 有任何衝突就 fail closed 停止，唔寫入、唔自動合併。衝突報告只列 user id 分組，任何公開 log 都唔出現完整 email。
3. 由有權限嘅人喺受控環境逐組處理：決定保留邊個帳戶，其他帳戶由用戶確認後改 email 或者按私隱流程 soft delete（決定 13）；處理過程寫 audit-log。
4. 處理完重新跑 migration，冇衝突先繼續。

公開註冊用 `user.self-register`（同 11.4 admin 用嘅 `user.register` 分開：admin 版本會回 409 `EMAIL_TAKEN`（決定 84），public 版本一定唔可以分辨 email 係咪存在）。

#### 12.11.1 Migration：`migrations/main/0004-add-email-verification.md`

```markdown
# 加 email 驗證欄位
users 表嘅 password 改為可以冇；加 email_verify_purpose（register 或者 invite，可以冇）、email_verified_at（時間，可以冇）、email_verify_hash（bytes，唯一，可以冇）、email_verify_expires_at（時間，可以冇）。
另外開一張 email_outbox 表：id、user_id（可以冇）、email、recipient_hash（收件人 email 嘅 hash，用嚟私隱刪除）、template、token（敏感）、sent_at、created_at。
```

`0004-add-email-verification.lock.md`（節錄 section；frontmatter 同 2B.4 格式，`security-items` 只有 `migration-ddl` 一項）

````markdown
## Target
- provider: main
- tables: [users, email_outbox]
- seq: 0004
- previous: migrations/main/0003-add-user-phone

## Up
### s1 ddl
- op: ddl
- sql: `alter table users add column email_verified_at timestamptz null`
- source-lines: 2-2

### s2 ddl
- op: ddl
- sql: `alter table users add column email_verify_hash bytea null`
- source-lines: 2-2

### s3 ddl
- op: ddl
- sql: `alter table users add column email_verify_expires_at timestamptz null`
- source-lines: 2-2

### s4 ddl
- op: ddl
- sql: `create unique index users_email_verify_hash_uq on users (tenant_id, email_verify_hash)`
- source-lines: 2-2

### s4a ddl
- op: ddl
- sql: `alter table users add column email_verify_purpose text null check (email_verify_purpose in ('register', 'invite'))`
- source-lines: 2-2

### s4b ddl
- op: ddl
- sql: `alter table users alter column password drop not null`   # 放寬約束，非破壞性（決定 67、69）
- source-lines: 2-2

### s5 ddl
- op: ddl
- sql: `create table email_outbox (id uuid primary key, user_id uuid null, email text not null check (char_length(email) <= 254), recipient_hash bytea not null, template text not null, token bytea null, sent_at timestamptz null)`
- source-lines: 3-3

## Verify
### v1 verify
- op: verify
- check: column-exists users.email_verify_hash
### v2 verify
- op: verify
- check: rls-enabled-forced email_outbox
### v3 verify
- op: verify
- check: row-count-unchanged users

## Data
- destructive: false
- personal-fields: { email_outbox.email: standard, email_outbox.token: sensitive }
- subject-key: { email_outbox: [user_id, hash:recipient_hash] }
- deprecated-since: (none)
````

- `email_outbox` 嘅 `tenant_id`、RLS、FORCE、`created_at`／`updated_at`／`deleted_at`／`erased_at` 由平台自動加（2B.3）。

#### 12.11.2 Model：`models/email-outbox/email-outbox.md`

```markdown
# email-outbox
要發出嘅 email，放喺 provider main 嘅 email_outbox 表，由平台 mail worker 發送。
欄位：id（uuid 主鍵）、user_id（uuid，可以冇）、email（合法 email，最多 254 字）、template（verify-email 或者 already-registered）、token（敏感，發送後銷毀）、sent_at（時間，可以冇）。
冇 CRUD。
```

`email-outbox.lock.md`（節錄 section；frontmatter 同 12.8，`security-items` 有 `public-writable-column`）

````markdown
## Table
- provider: main
- table: email_outbox

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| user_id | uuid | nullable | no |
| email | string | format: email, max: 254, untrusted, personal: standard | no |
| recipient_hash | bytes | `HMAC-SHA-256(pepper, normalize(email))`，subject-key | no |
| template | string | enum: [verify-email, already-registered, invite] | no |
| token | bytes | nullable, personal: sensitive, shred-after-send；值係 DEK 加密後嘅 `random-token` 明文（43 字 base64url） | no |
| sent_at | timestamp | nullable, platform | yes |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
(none)

## Policies
- rls: enable, force
- policy: TO tenant_<id>_*  USING／WITH CHECK (tenant_id = '<tenant_id 常數>')   # 每個租戶 build 時寫死常數；邊界係租戶 LOGIN role 同自己 schema，GUC 只用嚟 log（決定 97）
- grants: app_rw [SELECT, INSERT], public_writer [INSERT(user_id, email, recipient_hash, template, token)], mail_worker(platform) [SELECT, UPDATE(sent_at, email, token, recipient_hash)]
- subject-key: [user_id, hash:recipient_hash]
- retention: { after: sent_at + 30d, action: anonymize, by: mail_worker }   # 決定 68，已確認（產品經理 2026-10-14）：發送後 30 日匿名化
````

#### 12.11.3 Method：`models/email-outbox/methods/enqueue.md`

```markdown
# enqueue
排一封 email 入 outbox。
輸入：user_id（uuid，可以冇）、email（合法 email）、token（敏感字串，43 字 base64url，可以冇）、purpose（register 或者 invite，由呼叫嘅 method 喺 lock 寫死）。
範本唔由終端用戶揀：冇 user_id 就用 already-registered 範本，唔放 token；purpose 係 invite 就用 invite 範本（連結去設定密碼）；否則用 verify-email 範本（連結去驗證並設定密碼）。
同時記低收件人 email 嘅 hash，方便私隱刪除。
冇輸出內容。
```

`models/email-outbox/methods/enqueue.lock.md`

````markdown
---
lock-version: 1
kind: method
id: email-outbox/methods/enqueue
source: models/email-outbox/methods/enqueue.md
source-hash: sha256:81aa...c2
source-type: natural
dependencies:
  - { id: email-outbox, kind: model, hash: sha256:4f10...9d }
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: write-sql, risk: medium, reason: "註冊信件排隊" }
---
## Signature
- input: { user_id: { type: uuid, required: false }, email: { type: string, format: email, max: 254, required: true }, token: { type: string, length: 43, pattern: base64url, sensitive: true, required: false }, purpose: { type: string, enum: [register, invite], required: true, const-from-caller: true } }
- derived: { template: { enum: [verify-email, already-registered, invite], rule: "user_id == null → already-registered（token 唔寫入）；purpose == invite → invite；否則 verify-email" } }
- output: {}
- errors: []
- writes: true

## Calls
(none)

## Steps
### s1 hash-token
- op: hash-token
- value: input.email
- normalize: email               # 決定 120 正規化，再 HMAC-SHA-256(pepper, ...)
- out: rh
- source-lines: 2-6

### s2 query
- op: query
- provider: main
- table: email_outbox
- sql: `insert into email_outbox (user_id, email, recipient_hash, template, token) values ($1, $2, $4, case when $1::uuid is null then 'already-registered' when $5 = 'invite' then 'invite' else 'verify-email' end, case when $1::uuid is null then null else $3 end)`
- params: [input.user_id, input.email, input.token, rh, input.purpose]
- writes: true
- columns: [user_id, email, recipient_hash, template, token]
- out: (none)
- source-lines: 2-6

### s3 respond
- op: respond
- value: {}
- source-lines: 2-6
````

- 範本選擇寫喺單一參數化 INSERT 入面，唔使條件分支 step；三個範本 `verify-email`、`already-registered`、`invite` 都由同一個 `case` 覆蓋，兩種連結（驗證、邀請）都去同一個設定密碼頁，最後都係 `user.complete-verification`；`template` 由 Signature 嘅 `derived` 宣告，驗證器對比 SQL 嘅 `case` 同 `derived.rule` 一致（決定 66）。
- Token 類型統一（決定 66）：`random-token` 產生 32 bytes，對外一律係 43 字 base64url 字串（`vt.value`、email 連結、verify-email 嘅 `body.token`）；DB 只存 `bytea`：`users.email_verify_hash` 係 `HMAC-SHA-256(pepper, token)`，`email_outbox.token` 係 DEK 加密嘅明文。
- 保留期（決定 68，已確認（產品經理 2026-10-14））：`sent_at` 之後 30 日，`mail_worker` 將 `email` 換成遮罩、`token` 設 null（DEK 已銷毀）、`recipient_hash` 保留到 12.10 私隱刪除；未發送嘅列唔受影響。12.10 刪除時按 `user_id` 或者 `recipient_hash` 搵出所有 outbox 列一併匿名化，包括 `user_id` 係 null 嘅「已經有帳戶」通知。
- token 屬 sensitive，DB 入面以 crypto-shredding 加密，mail worker 發送後即時銷毀 DEK。

#### 12.11.4 Method：`models/user/methods/self-register.md`（決定 67，已確認（產品經理 2026-10-14））

```markdown
# self-register
訪客自己註冊，只收 email 同 name，唔收密碼。
輸入：email（合法 email，最多 254 字）、name（1 至 50 字）。

做法：
1. 產生一個 email 驗證 token，24 小時有效。
2. 建立用戶，未驗證、冇密碼。如果 email 已經存在但未驗證，只換新 token 同到期時間（舊 token 即時失效），其他欄位一律唔改；如果已經驗證、或者有未過期嘅 admin 邀請，就乜都唔改。
3. 用 email-outbox.enqueue（purpose 係 register）排一封信：新用戶或者未驗證用戶收驗證信（帶 token），已驗證用戶或者有未過期邀請嘅用戶收「已經有帳戶」通知。
4. 唔好話畀人知 email 係咪已經存在，輸出一律空。
```

`models/user/methods/self-register.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/self-register
source: models/user/methods/self-register.md
source-hash: sha256:d3c9...10
source-type: natural
dependencies:
  - { id: email-outbox/methods/enqueue, kind: method, hash: sha256:81aa...c2 }
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: write-sql, risk: medium, reason: "建立未驗證用戶或者輪換驗證 token" }
---
## Signature
- input: { email: { type: string, format: email, max: 254, required: true }, name: { type: string, min: 1, max: 50, required: true } }
- output: {}
- errors: []
- writes: true

## Calls
- methods: [email-outbox.enqueue]
- external: (none)

## Steps
### s1 random-token
- op: random-token
- bytes: 32
- out: vt                       # vt.value（sensitive，43 字 base64url）、vt.hash（bytes）
- source-lines: 5-9

### s2 query
- op: query
- provider: main
- table: users
- sql: `insert into users (email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose) values ($1, $2, $3, now() + interval '24 hours', 'register') on conflict (tenant_id, email) do update set email_verify_hash = excluded.email_verify_hash, email_verify_expires_at = excluded.email_verify_expires_at, email_verify_purpose = 'register' where users.email_verified_at is null and users.deleted_at is null and not (users.email_verify_purpose = 'invite' and users.email_verify_expires_at > now()) returning id`
- params: [input.email, input.name, vt.hash]
- writes: true
- scope: pk                     # conflict target (tenant_id, email) 係唯一鍵，DO UPDATE 最多影響一列
- columns: { insert: [email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose], update: [email_verify_hash, email_verify_expires_at, email_verify_purpose] }
- out: ins                      # 新用戶或者未驗證用戶回 id；已驗證或者有未過期邀請 ins.id 係 null
- source-lines: 5-9

### s3 method
- op: method
- target: email-outbox.enqueue
- args: { user_id: ins.id, email: input.email, token: vt.value, purpose: register }
- out: (none)
- source-lines: 5-9

### s4 respond
- op: respond
- value: {}
- source-lines: 5-9
````

- 冇 `set-password` capability、冇 `hash-password` step：公開註冊永遠唔寫 password。
- Upsert 規則（決定 67）：
  - `INSERT` 只寫 `email`、`name`、`email_verify_hash`、`email_verify_expires_at`、`email_verify_purpose`（固定 `'register'`）；`ON CONFLICT DO UPDATE` 只准改 `email_verify_hash`、`email_verify_expires_at`、`email_verify_purpose`（設返 `'register'`），一定要有 `where users.email_verified_at is null and users.deleted_at is null and not (users.email_verify_purpose = 'invite' and users.email_verify_expires_at > now())`，即係未驗證、未刪除、而且冇未過期嘅邀請。驗證器檢查 `columns.update` 係 `columns.insert` 嘅子集，而且唔包括 password、`name`、`email` 或者任何黑名單欄位。
  - `scope: pk` 嘅意思：conflict target 係租戶內嘅唯一鍵 `(tenant_id, email)`（`tenant_id` 由 RLS 同平台注入，唔喺 SQL 參數入面），所以 DO UPDATE 最多只會影響同一租戶入面同一 email 嘅一列。
  - 有未過期 admin 邀請（`email_verify_purpose = 'invite'` 而且 `email_verify_expires_at > now()`）嘅 email：DO UPDATE 條件唔成立，邀請 token、到期時間同所有欄位都唔變，`ins.id` 係 null，所以 enqueue 寄「已經有帳戶」中性通知；回應同其他情況一樣係 202。邀請過期之後，註冊就當普通未驗證帳戶處理（輪換成 `register` token）。
  - 一個 upsert 只算一次 public 寫入（不論最後係 INSERT 定 UPDATE），計入每日配額一次。
- 攻擊者用受害人 email 先註冊：因為冇密碼，帳戶冇任何 credential；受害人再註冊只會輪換 token，攻擊者手上嘅舊 token 即時失效。設密碼只可以經 email 連結（12.11.5），所以冇受害人信箱就冇辦法搶帳戶。
- 未驗證帳戶清理（決定 67，已確認（產品經理 2026-10-14））：平台 job 定時搵 `email_verified_at is null and email_verify_expires_at < now() and deleted_at is null` 嘅用戶，用 `erasure` role 匿名化（email 換成 `erased+{id}@invalid`、token hash 清走），再 soft delete（`deleted_at`）；唔 DELETE（決定 13）。因為 key 係 `email_verify_expires_at`，再註冊輪換 token 會順延清理時間；邀請（72 小時）用同一條規則。
- 單一職責：自己物件只有 1 個寫入 sql；寫 email_outbox 經佢自己嘅 method（決定 3）。呼叫深度 2 層。

#### 12.11.5 Method：`models/user/methods/complete-verification.md`（決定 67、69 共用，已確認（產品經理 2026-10-14））

```markdown
# complete-verification
用 email 連結入面嘅 token 確認 email，同時設定密碼。註冊驗證同 admin 邀請都用呢個 method。
輸入：token（字串，43 字）、password（字串）。
呢個 method 負責確認驗證，亦會設定密碼。

做法：
1. 密碼一定先 hash（長度唔合 fail WEAK_PASSWORD，洩漏 fail BREACHED_PASSWORD）。
2. token 要啱、未過期、未驗證過，就喺同一個更新入面設密碼、記低驗證時間同清走 token；否則 fail INVALID_TOKEN。
```

`models/user/methods/complete-verification.lock.md`

````markdown
---
lock-version: 1
kind: method
id: user/methods/complete-verification
source: models/user/methods/complete-verification.md
source-hash: sha256:9e02...7b
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: set-password, risk: high, reason: "用 email token 設定密碼" }
  - { item: confirm-verification, risk: high, reason: "訪客用 token 確認 email" }
---
## Signature
- input: { token: { type: string, length: 43, pattern: base64url, sensitive: true, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: {}
- errors: [WEAK_PASSWORD, BREACHED_PASSWORD, INVALID_TOKEN]
- writes: true
- capabilities: [set-password, confirm-verification]

## Calls
(none)

## Steps
### s1 hash-password
- op: hash-password
- cost: high
- plain: input.password
- policy: platform-floor        # 最少 12 字元 + 長度唔合 fail WEAK_PASSWORD，洩漏 fail BREACHED_PASSWORD
- out: pw
- source-lines: 6-8

### s2 hash-token
- op: hash-token
- value: input.token
- out: th
- source-lines: 6-8

### s3 query
- op: query
- provider: main
- table: users
- sql: `update users set password = $1, email_verified_at = now(), email_verify_hash = null, email_verify_expires_at = null, email_verify_purpose = null where email_verify_hash = $2 and email_verify_expires_at > now() and email_verified_at is null and deleted_at is null returning id`
- params: [pw, th]
- writes: true
- scope: pk                     # (tenant_id, email_verify_hash) 係唯一鍵
- columns: [password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose]
- password-source: s1
- out: upd
- source-lines: 6-8

### s4 validate
- op: validate
- when: `upd.id == null`
- fail: INVALID_TOKEN
- source-lines: 6-8

### s5 respond
- op: respond
- value: {}
- source-lines: 6-8
````

- 一個 UPDATE 同時設 password 同 `email_verified_at`（決定 53、67、69）：冇「已驗證但冇密碼」或者「有密碼但未驗證」嘅中間狀態，所以決定 55 成立。
- s1 每次都做（token 錯都做），令耗時一致；`hash-password` 用平台固定大小嘅 argon2 worker pool。最低回應耗時 300ms 由收到 request 開始計，喺所有工作完成之後先補足；超過 300ms 就補到下一個 100ms 倍數（決定 66）。
- 成功 commit 之後，平台用 `refresh_store` 撤銷該用戶所有 refresh 家族（`revoke_reason: email-verified`）；method lock 唔需要寫，屬平台行為（決定 67）。
- 決定 56 嘅「一律做 `hash-password`」已移到呢一步（決定 67 已確認）。

#### 12.11.6 api：`api/auth/v1/register/register.md`（路由 `POST /auth/v1/{slug}/register`）

```markdown
---
body: 4KB
---
# 註冊
POST，路徑 /auth/v1/{slug}/register。
公開，唔使 auth，租戶由 slug 決定。
會建立未驗證用戶，准許公開寫入，唔寫密碼。一定要 captcha。
每個 IP 每分鐘最多 5 次，每個租戶每分鐘最多 60 次；用平台預設每日寫入配額；要記審計，email 只記 hash。

請求 body：email（合法 email，最多 254 字）、name（1 至 50 字）。
用 user.self-register 註冊。
無論 email 係咪已經存在，都回 202 同一句「如果資料正確，你會收到確認電郵」。
```

`api/auth/v1/register/register.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: auth/v1/register
source: api/auth/v1/register/register.md
source-hash: sha256:5a7e...31
source-type: natural
dependencies:
  - { id: auth/_definition, kind: api, hash: sha256:77e0...aa }
  - { id: user/methods/self-register, kind: method, hash: sha256:d3c9...10 }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: public, risk: high, reason: "訪客註冊入口" }
  - { item: auth-removed, risk: high, reason: "未有帳戶" }
  - { item: public-writes, risk: high, reason: "建立未驗證用戶同 outbox" }
---
## Route
- method: POST
- path: /auth/v1/{slug}/register

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_writer
- writable-columns: { users: { insert: [email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose], update: [email_verify_hash, email_verify_expires_at, email_verify_purpose] }, email_outbox: { insert: [user_id, email, recipient_hash, template, token] } }
- effective-grant: public_writer [users: INSERT(email, name, email_verify_hash, email_verify_expires_at, email_verify_purpose), UPDATE(email_verify_hash, email_verify_expires_at, email_verify_purpose); email_outbox: INSERT(user_id, email, recipient_hash, template, token)]
- challenge: { type: captcha, when: always }
- external-calls: forbidden
- daily-write-quota: platform-default     # 每租戶 1,000、每 IP 50；每次 upsert 算一次
- audit-log: required
- min-response: 300ms

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 5
- key: ip

### p3 rate-limit
- per-minute: 60
- key: tenant

### p4 audit-log
- event: public-register
- hash: [req.body.email]           # 記 HMAC，唔記明文 email（決定 68）

## Limits
- provider: main
- timeout: 10s
- body: 4KB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
| body.email | string | format: email, max: 254, untrusted | yes |
| body.name | string | min: 1, max: 50, untrusted | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 202 | { message: "如果資料正確，你會收到確認電郵" } | |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { message: "如果資料正確，你會收到確認電郵" }

## Steps
### s1 method
- op: method
- target: user.self-register
- args: transform.in
- out: (none)
- source-lines: 5-8

### s2 respond
- op: respond
- status: 202
- value: transform.out
- source-lines: 5-8

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

- 冇 `hash-password`，所以唔觸發 `cost: high` 減半：限流就係 IP 每分鐘 5 次、租戶每分鐘 60 次。
- body 有 `password`、`tenant_id`、`id`、`created_by`（或者任何未宣告欄位，例如 `roles`）一律 422。
- 耗時：新用戶、未驗證、已驗證三條路都係「1 個 upsert + 1 個 outbox insert」，工作量一樣；再補到 300ms。
- 有效權限摘要：public；role public_writer，grant 等於上面 `effective-grant`；唔寫 password；冇讀 output；唔簽 token；冇對外 call；captcha 每次；IP 5/min、租戶 60/min；每日配額 1,000／租戶、50／IP（每次 upsert 算一次）；audit 只記 email hash；最低 300ms。

#### 12.11.7 api：`api/auth/v1/verify-email/verify-email.md`（路由 `POST /auth/v1/{slug}/verify-email`）

```markdown
---
body: 2KB
---
# 驗證 email 同設定密碼
POST，路徑 /auth/v1/{slug}/verify-email。
公開，唔使 auth，租戶由 slug 決定。
email 連結會開一個設定密碼頁；用戶輸入密碼之後，頁面將 token 同密碼一齊送嚟呢度。
會設定密碼同確認 email，准許公開寫入、寫密碼同寫驗證欄位。唔使 captcha（冇 INSERT）。
每個 IP 每分鐘最多 10 次，每個租戶每分鐘最多 120 次；要記審計，token 同 email 唔記明文。

請求 body：token（43 字 base64url）、password（1 至 256 字）。
用 user.complete-verification。成功回 204。
token 錯、過期或者用過回 422 INVALID_TOKEN；密碼長度唔合回 422 WEAK_PASSWORD，喺洩漏清單搵到回 422 BREACHED_PASSWORD；錯誤唔回顯密碼。
```

`api/auth/v1/verify-email/verify-email.lock.md`

````markdown
---
lock-version: 1
kind: api
scope: feature
id: auth/v1/verify-email
source: api/auth/v1/verify-email/verify-email.md
source-hash: sha256:b81f...4d
source-type: natural
dependencies:
  - { id: auth/_definition, kind: api, hash: sha256:77e0...aa }
  - { id: user/methods/complete-verification, kind: method, hash: sha256:9e02...7b }
compiler: { name: md-logic, version: 2.0.0 }
compiler-hash: sha256:c0de...77
security-items:
  - { item: public, risk: high, reason: "email 連結入口" }
  - { item: auth-removed, risk: high, reason: "未有密碼" }
  - { item: public-writes, risk: high, reason: "更新用戶" }
  - { item: password-write, risk: high, reason: "設定密碼" }
  - { item: verification-write, risk: high, reason: "確認 email" }
---
## Route
- method: POST
- path: /auth/v1/{slug}/verify-email

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- password-write: allow
- verification-write: allow
- role: public_writer
- writable-columns: { users: { update: [password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose] } }
- effective-grant: public_writer [users: UPDATE(password, email_verified_at, email_verify_hash, email_verify_expires_at, email_verify_purpose)]
- challenge: { type: none }
- external-calls: forbidden
- daily-write-quota: platform-default
- audit-log: required
- min-response: 300ms

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 10
- key: ip

### p3 rate-limit
- per-minute: 120
- key: tenant

### p4 audit-log
- event: public-verify-email
- redact: [req.body.token, req.body.password]

## Limits
- provider: main
- timeout: 10s
- body: 2KB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
| body.token | string | length: 43, pattern: base64url | yes |
| body.password | string | min: 1, max: 256 | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 204 | (none) | |

## Transform
- in: { token: req.body.token, password: req.body.password }
- out: (none)

## Steps
### s1 method
- op: method
- target: user.complete-verification
- args: transform.in
- out: (none)
- source-lines: 5-9

### s2 respond
- op: respond
- status: 204
- source-lines: 5-9

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | WEAK_PASSWORD | WEAK_PASSWORD |
| 422 | BREACHED_PASSWORD | BREACHED_PASSWORD |
| 422 | INVALID_TOKEN | INVALID_TOKEN |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
| 429 | RATE_LIMITED | RATE_LIMITED |
````

- `cost: high`（`hash-password`）令實際限流減半：IP 每分鐘 5 次、租戶每分鐘 60 次（決定 25、39）。
- token 錯、過期、用過都回同一個 422 `INVALID_TOKEN`，耗時補到 300ms；token 只以 hash 比對，唔寫 log（audit redact）。
- 驗證成功之後先可以用 11／12.8 嘅 login（決定 55，已確認）；成功時平台撤銷該用戶所有 refresh 家族（決定 67）。
- 設定密碼頁由前端提供；連結格式 `https://{前端}/verify#token={token}`，token 放 fragment，唔會出現喺 server log 或者 Referer。

#### 12.11.8 驗證器對呢個範例檢查咗乜

- public 寫入：`public-writes: true`、IP 加租戶限流、每日配額、audit-log 都有；`writable-columns` 同 sql 實際寫入欄位一致（INSERT 同 UPDATE 分開對比），冇黑名單欄位；`public_writer` 嘅 `effective-grant` 等於 lock 用到嘅表、欄位同操作。
- 註冊：有 INSERT 所以 `challenge: { type: captcha, when: always }`；冇寫 password；upsert 嘅 DO UPDATE 欄位係 INSERT 欄位嘅子集，有 `email_verified_at is null` 條件，`scope: pk` 嘅 conflict target 係租戶內唯一鍵。
- 驗證：password 同 `email_verified_at` 只喺有 `set-password` 加 `confirm-verification` 嘅 method、用 `hash-token` 做 WHERE、有過期檢查、同時清 token hash 嘅一個 UPDATE 入面寫；password 值嚟自 `hash-password`；api 有 `password-write: allow` 同 `verification-write: allow`。
- `random-token` 嘅 `value` 只傳去 `email-outbox.enqueue` 嘅 sensitive input，冇出現喺 respond 或者 log。
- Audit 冇明文 email（只有 `hash:`），冇 token、password。
- 冇 DELETE、DDL、`call`；所有 sql 參數化、單一 statement；UPDATE 有 `scope: pk`。
- Request 冇 `tenant_id`、`id`、`created_by`；untrusted 欄位全部有長度上限。
- 安全項目每項有驗證器產生嘅原因（資訊性，決定 78）。v1.8 冇每個 PR 上限；下表係 v1.7 時代嘅分拆方式，而家只係建議（細 PR 較易 review）：

  | PR | 內容 | 安全項目 | 數量 |
  | --- | --- | --- | --- |
  | PR#149 | migration `0004-add-email-verification` | migration-ddl（高） | 1 |
  | PR#150 | `email-outbox` model、`email-outbox.enqueue`、`user.self-register`、`user.complete-verification`，同埋 user model 改動 | write-sql（中，enqueue）、write-sql（中，self-register）、set-password（高）、confirm-verification（高） | 4 |
  | PR#151 | `api/auth/v1/register` | public、auth-removed、public-writes（全部高） | 3 |
  | PR#152 | `api/auth/v1/verify-email` | public、auth-removed、public-writes、password-write、verification-write（全部高） | 5 |

  - 後面嘅 PR 依賴前面 PR 嘅 lock hash，要按次序合併。

#### 12.11.9 Admin 邀請流程（決定 69，已確認（產品經理 2026-10-14））

- `user.invite`（admin 路徑，要 `auth` 同 admin role；完整 lock 見 11.4a，重寄見 11.4b）：用 `random-token` 產生邀請 token，`users.email_verify_hash` 寫 token hash、`email_verify_expires_at` 設 72 小時後、`email_verify_purpose` 設 `invite`；經 `email-outbox.enqueue`（`purpose: invite`，lock 寫死）寄出，範本係 `invite`。
- 用戶開邀請連結，去同一個設定密碼頁，送去同一個 `POST /auth/v1/{slug}/verify-email`，由同一個 `user.complete-verification` 處理（決定 67、69 共用 code path）：一個 UPDATE 同時設 password 同 `email_verified_at`，成功後撤銷所有 refresh 家族。
- admin 建立嘅用戶喺接受邀請之前冇 password、未驗證，冇辦法 login，決定 55 成立。
- 有未過期邀請嘅 email 被人公開註冊：邀請完全唔受影響，註冊者只會收到「已經有帳戶」中性通知（見 12.11.4）。
- 重新邀請會覆寫 token hash 同到期時間，舊邀請即時失效；過期冇接受就按 12.11.4 同一條清理規則處理（key 係 `email_verify_expires_at`）。

## 13. 待確認

私隱法（PDPO、GDPR）部分嘅建議請法律意見覆核。

另外，0.10 嘅決定 73–119 已經由產品經理確認（2026-10-14；決定 80、116 已取代）；文件仍然係草稿，v1.8 正式定稿之前 v1.7 定稿照樣有效。

## 14. 後續版本

- 排程、queue、webhook 入口。
- 線上（唔凍結寫入）嘅 schema 升級去 database 遷移。
- 除咗 PostgreSQL 以外嘅 provider type。
- Steps 白名單 op 擴充，例如 transaction 區塊、條件分支。
- 本地模擬環境同 manifest 差異比較工具。
- 物件之間嘅關聯（has-many、belongs-to）。
- lock 語意差異工具（顯示 lock 改動對行為嘅影響，而唔止文字 diff）。
- 編譯器自動產生測試案例，同 source 一齊喺 PR review（lock 係 build 輸出，唔 commit，決定 117）。
- public 寫入內容審核 hook（spam／不當內容），處理 captcha 被破解同低質內容嘅剩餘風險。
- 私隱刪除嘅資料主體自助提交入口（12.10 流程已經定義，呢度只係加自助入口）。
- Kill switch 自動觸發（例如錯誤率或者寫入量異常時自動停用）。
- `sign-jwt` 支援 EdDSA（Ed25519）（決定 139 留到 vNext）。
- 編譯用 LLM provider 加入 `azure-openai`（要處理 deployment 名同 api-version 等設定）。
- `openai` adapter 支援 OpenAI Responses API（reasoning model 建議用），v1 只用 Chat Completions。
