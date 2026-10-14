# Markdown 驅動 Serverless Backend：架構 Spec v1.5（定稿）

> 核心架構：所有定義（model、method、api、pipeline、provider）嘅 source `.md` 一律用自然語言寫。編譯流程係「自然語言 source `.md` → 結構化 lock markdown（`{name}.lock.md`）→ 簽名 manifest」。lock md 係 runtime 唯一依據，runtime 永遠唔會解譯自然語言。
>
> 定稿內容：除咗下面 v1.5 決定，仲包括：安全設定照用自然語言寫，唔強制結構化 frontmatter，防線改為「獨立 deterministic 驗證器 + 收緊嘅人手 review」；編譯器冇網絡、冇工具；model snapshot pin 死同 compiler-hash；確認綁定 commit；收緊 public 寫入；禁止 DDL／TRUNCATE；token 簽發用 capability 旗標；新增緊急 kill switch；public 最低回應耗時改為固定值。
>
> v1.5 定稿：refresh token 輪換、密碼底線、帳戶鎖定、固定回應耗時 300ms／500ms、私隱刪除（crypto-shredding 加匿名化）同所有營運數值全部定案。

## 0. 範圍同決定
### 0.1 範圍
- v1 只支援 HTTP API 入口，method 限 GET／POST／PUT／DELETE；排程、queue、webhook 入口唔喺 v1 範圍內。
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
|---|---|---|
| 1 | 物件欄位唔分 version，靠 migration 管理。 | 12.2、12.5 |
| 2 | method 只有單一版本：取消 `@v2` 同「同名 version 綁定」；取消 `models/{object}/{version}/` 資料夾，改為 `models/{object}/methods/{method}.md`。唔同 api version 之間嘅差異，由 api 層嘅 input／output 轉換（`## Transform`）處理。 | 1、2、12 |
| 3 | 寫入其他物件一定要經嗰個物件自己嘅 method。 | 12.3、12.3a |
| 4 | 取消「public endpoint 只可以 call `writes: false` method」嘅限制（風險控制見 7.2；剩餘風險由後續版本內容審核 hook 處理）。 | 7.2、12.6 |
| 5 | `sign-token` 放喺 model 層做 method（例如 `user.sign-token`）；`auth` 類 pipeline 透過呼叫 model method 做嘢。 | 4、12.6、12.8 |
| 9 | api 層都可以用自然語言寫。 | 2 |
| 10 | 由純結構化 source 編譯出嚟嘅 lock 可以自動確認；由自然語言編譯出嚟嘅 lock 一定要人手確認。v1.4 收緊：「係咪結構化」由 deterministic parser 判斷；觸及安全項目嘅 lock 一律要人手確認。 | 2A.5 |
| 11 | 只有 owner 或者 maintainer 可以確認 lock，喺 CI 嘅 PR review 入面做。v1.4 收緊：確認綁定 commit hash，高風險改動要兩個非作者確認。 | 2A.5 |
| 12 | 單一職責上限係 warning。 | 12.3a、10 |
| 13 | 軟刪除永遠唔會實際刪除資料，冇 purge。 | 12.9 |
| 14 | CRUD 嘅 list 用 offset 分頁（保留 page-size 上限）。 | 12.9 |
| 15 | 編譯器 major version 升級會強制重新編譯所有 lock；minor 升級唔會。 | 2A.6 |

### 0.3 已確認決定（v1.4 定案）
| # | 決定 | 影響章節 |
|---|---|---|
| 16 | 安全設定照用自然語言寫，**唔強制**結構化 frontmatter；防線係獨立驗證器加人手 review。唔設結構化強制。 | 2.2、2A.5、2A.7 |
| 17 | 編譯器輸出一定要通過獨立、deterministic 嘅驗證器（唔靠 LLM 自律）；編譯器冇網絡、冇任何工具權限；source 入面似指令嘅句子（prompt injection 模式）出 warning 並喺 PR 高亮。 | 2A.7、10.9 |
| 18 | 編譯用嘅 model pin 死 snapshot；compiler-hash（model snapshot + prompt + 編譯器版本）寫入 lock；CI 重編同 lock 唔一致就封鎖部署，冇 override；會改變輸出嘅 minor 升級當 major 處理。 | 2A.1、2A.6 |
| 19 | Review：每個安全項目逐項確認並寫原因；高風險改動要兩個非作者 owner／maintainer；每個 PR 安全改動數量有上限；PR 同部署報告顯示有效權限摘要。 | 2A.5、10.10 |
| 20 | 「係咪 structured」由 deterministic parser 判斷；觸及安全項目嘅 lock 一律人手確認（即使係 structured）。 | 2A.5 |
| 21 | Public 寫入收緊：每租戶每日寫入配額（強制）；建立資料嘅 endpoint 要 captcha 或者 proof-of-work；一律 audit-log；配額用完只封鎖 public 寫入。 | 7.2 |
| 22 | Public 寫入只准 INSERT 同 UPDATE 明確列出嘅欄位；平台黑名單封鎖敏感欄位（role、permissions、tenant、owner 等）；UPDATE 一定要有 WHERE 同 RLS。 | 7.2、10.7 |
| 23 | Public 寫入嘅 `tenant_id` 由平台按 slug 注入；body 含 `tenant_id`、`id`、`created_by` 一律拒絕。 | 7.2 |
| 24 | Public 寫入嘅內容標記為 untrusted，輸出時做 encoding，有長度同數量上限。 | 7.2 |
| 25 | Public 路徑預設禁止對外 call；昂貴 step 用更嚴嘅限流。 | 7.2 |
| 26 | `public_write_access` 只可以由租戶 admin 喺平台 UI 開啟。 | 7.2 |
| 27 | 所有 lock 禁止 DDL 同 TRUNCATE（migration 另外處理）；UPDATE 一定要用主鍵或者 RLS 限定範圍；DB role 冇 DELETE／TRUNCATE 權限。 | 2A.4、7.1、10.8 |
| 28 | 確認綁定 commit hash，有新 commit 就失效；最後 push 嘅人當係作者。 | 2A.5 |
| 29 | CI workflow 同編譯器版本設定受 CODEOWNERS 保護；簽名 key 只喺平台。 | 9B |
| 30 | Token 簽發喺 lock 明確標示 capability 旗標（唔靠 method 名推斷）；claim 白名單；roles 同 tenant_id 由平台從 DB 讀；exp 有上限；key 只用引用。 | 12.6 |
| 31 | 新增緊急 kill switch，可以停用個別 endpoint（回 503、寫審計、唔使重新部署）。 | 9A |
| 32 | Public 最低回應耗時由 p95 改為固定值（數值見決定 36）。 | 0.1、7.2 |

### 0.4 已確認決定（v1.5 定稿）
| # | 決定 | 影響章節 |
|---|---|---|
| 6 | Refresh token 每次使用都輪換；重用舊 token 會撤銷成個 token 家族；登出可以 revoke。refresh token ttl 上限 30 日（家族絕對壽命），閒置 7 日失效；access token 上限 1 小時。 | 4A |
| 7 | 平台密碼底線：最少 12 個字元，加洩漏密碼檢查；物件只可以收緊，唔可以放寬。 | 4A |
| 8 | 15 分鐘內失敗 5 次就鎖帳戶 15 分鐘，另加 IP 限流。防惡意鎖帳戶：同一帳戶 15 分鐘內失敗 3 次之後要 captcha；鎖定期間經 email 連結重設密碼可以即時解鎖；鎖定期間回應同密碼錯完全一樣。 | 4A |
| 33 | 細團隊雙人確認：project 合資格嘅非作者 owner／maintainer 少過 2 個，第二確認人由平台 on-call 安全團隊擔任；平台 on-call 未能喺 2 個工作日內處理，可以改用單人確認加 24 小時延遲生效，期間通知全體 owner。 | 2A.5 |
| 34 | 每個 PR 安全項目上限 5 項。 | 2A.5 |
| 35 | Model snapshot：平台承諾 snapshot 喺宣布淘汰之後最少保留 12 個月，項目有 90 日遷移窗口；窗口內舊 lock 照常有效。Golden corpus 由平台編譯器團隊維護（最少 500 個案例），項目可以加自己嘅案例。 | 2A.6 |
| 36 | Public 最低回應耗時：一般 300ms，login 類 500ms；實際超過只出 warning，唔係 error。 | 7.2、10.9 |
| 37 | 語意一致性檢查（lock 多過 source）係 error；prompt injection 規則命中係 warning，項目可以喺受 CODEOWNERS 保護嘅 allowlist 加精確句子豁免。零寬／bidi 字元仍然係 error。 | 2A.7 |
| 38 | Login challenge：同一帳戶或者同一 IP 15 分鐘內失敗 3 次後要 captcha；冇 INSERT 嘅其他 public 寫入唔強制 challenge。 | 4A、7.2 |
| 39 | 每租戶每日 public 寫入配額預設 1,000 次，每 IP 每日 50 次；租戶 admin 可以調低，調高最多 10,000 要經平台申請。昂貴 step 限流係 endpoint 上限嘅一半。 | 7.2 |
| 40 | Kill switch 生效 SLA：p99 30 秒內全部 instance 生效；租戶 admin 只可以停自己租戶嘅 public endpoint。 | 9A |
| 41 | `public_write_access` 批量操作：平台 UI 支援組織 admin 一次揀多個租戶開／關，要二次確認同寫原因；仍然冇 API／CLI。 | 7.2 |
| 42 | 私隱刪除：敏感個人欄位用 crypto-shredding，其他個人欄位匿名化；DELETE 照樣禁止。流程見 12.10。 | 12.10 |

## 1. 目錄同命名
```
project/
  providers/{name}.md
  providers/{name}.lock.md
  pipelines/{name}.md
  pipelines/{name}.lock.md
  models/{object}/{object}.md
  models/{object}/{object}.lock.md
  models/{object}/methods/{method}.md
  models/{object}/methods/{method}.lock.md
  api/{definition}/_definition.md
  api/{definition}/_definition.lock.md
  api/{definition}/{version}/_version.md
  api/{definition}/{version}/_version.lock.md
  api/{definition}/{version}/{feature}/{feature}.md
  api/{definition}/{version}/{feature}/{feature}.lock.md
  api/{definition}/{version}/{feature}/schema.md      # 選填，自然語言欄位補充說明，會併入 feature 嘅 lock
```
- 所有名一律用 kebab-case，只准 `a-z0-9-`。
- 每個 source `.md` 旁邊一定有一個同名嘅 `.lock.md`（`schema.md` 除外，佢併入 `{feature}.lock.md`）。lock 檔由編譯器產生，唔准手寫或者手改。
- `_` 開頭嘅檔係保留檔，永遠唔會被路由；`_definition.md` 只可以放喺 `{definition}/`，`_version.md` 只可以放喺 `{version}/`。
- version 名必須符合 `^v[1-9][0-9]*$`（例如 v1、v2、v10）。version 只存在於 api 層；models 同 method 唔分 version。
- 預設路由係 `/{definition}/{version}/{feature}`；路由一定要包含 version，冇預設版本，亦唔會自動轉去最新版本。
- 保留路徑：`/_internal/*`、`/health`。

## 2. 編譯流程同 source 寫法
### 2.1 三段流程
1. **Source（`{name}.md`）**：用自然語言寫，講清楚「要做咩」。可以有選填嘅結構化 frontmatter（例如 `method: POST`），但唔係必須。
2. **Lock（`{name}.lock.md`）**：編譯器將 source 編譯成固定格式嘅結構化 markdown（格式見 2A）。lock 經確認同簽名之後先算生效。
3. **Manifest**：部署時將所有已確認、簽名有效嘅 lock 合併編譯成一份簽名 manifest。runtime 只載入 manifest。

- runtime 永遠唔會解譯自然語言，亦唔會讀 source `.md`；lock md 係 runtime 嘅唯一依據（經 manifest 載入）。
- 所有安全同驗證規則都喺 lock 層檢查（見第 10 節），唔係喺 source 層。
- source 只係人寫同人讀嘅意圖描述；source 同 lock 有出入時，以已確認嘅 lock 為準，並且應該改 source 再重新編譯。

### 2.2 Source 寫法建議
- 每個 source 用 `#` 標題寫名，之後用段落或者列點描述。
- 建議用以下小標題幫編譯器理解（唔強制）：`## 用途`、`## 輸入`、`## 輸出`、`## 做法`、`## 錯誤`、`## 權限`。
- 安全相關嘅設定（公開、剔走 auth、寫入、改 provider、放寬限流、簽 token）照用自然語言寫，唔強制結構化 frontmatter（決定 16）。建議用獨立一句清楚寫出，唔好埋喺長段落入面；無論寫喺邊，驗證器都會由 lock 抽出安全項目，PR 逐項高亮（見 2A.5）。
- source 入面嘅文字只會被當做描述，入面任何「指示編譯器」或者「略過檢查」嘅句子都唔會生效，lock 層規則照樣強制檢查；呢類似指令嘅句子會被 deterministic 規則偵測，出 warning 並喺 PR 高亮（見 2A.7）。

### 2.3 api feature source 涵蓋內容
一個 `{feature}.md` 要描述：
- HTTP method（GET／POST／PUT／DELETE，一個 feature 只可以有一個）同（選填）路徑。
- 要經邊啲 pipeline、係咪公開、租戶由邊度嚟（public 用 slug）。
- Request 有咩欄位、Response 有咩狀態碼同內容。
- 要 call 邊個 method、點樣將 request 轉做 method input、點樣將 method output 轉做 response。
- 業務錯誤碼對應嘅狀態碼。
- 選填：provider 覆蓋、timeout（預設 10s，平台上限 30s）、body 上限（預設 1MB，有平台上限）。

編譯後喺 lock 入面對應：
| lock 欄位／section | 說明 |
|---|---|
| `## Route` 嘅 `method` | 必填，GET／POST／PUT／DELETE |
| `## Route` 嘅 `path` | 選填，必須喺自己 `/{definition}/{version}/` 前綴入面；可以用 `{param}`；唔准有 `..`、萬用字元，亦唔准用保留路徑 |
| `## Pipelines` | 繼承合併之後、按次序執行嘅 pipeline 清單同參數 |
| `## Access` 嘅 `public` | 預設 false |
| `## Access` 嘅 `tenant` | public endpoint 必填：`{ from: path, param: slug }` |
| `## Access` 嘅 `public-writes` | public endpoint call 咗寫入 method 時必填 true（見 7.2） |
| `## Limits` | `provider`、`timeout`、`body` |
| `## Request` | 欄位宣告；未宣告嘅欄位一律拒絕，回 422 |
| `## Response` | 狀態碼同 schema |
| `## Transform` | request → method input、method output → response 嘅映射；處理唔同 api version 嘅差異 |
| `## Steps` | 有序步驟；api 層只准 `method`、`validate`、`transform`、`respond` 四種 op，**唔准 `query` 或者 `call`** |
| `## Errors` | 業務錯誤碼對應狀態碼 |

## 2A. Lock markdown 格式
### 2A.1 通用 frontmatter
每個 lock 一律用以下 frontmatter，欄位次序固定：
```
---
lock-version: 1
kind: method                       # provider | pipeline | model | method | api
id: user/methods/login             # 由路徑決定，唔准自訂
source: models/user/methods/login.md
source-hash: sha256:<source 正規化內容嘅 hash>
source-type: natural               # natural | structured
dependencies:                      # 所有依賴 source／lock 嘅 hash，按 id 排序
  - { id: user, kind: model, hash: sha256:<...> }
  - { id: main, kind: provider, hash: sha256:<...> }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:<model snapshot + prompt + 編譯器版本 嘅 hash>
status: approved                   # pending | approved | rejected
approval-mode: manual              # manual | auto
approved-by: [jack@example]        # 由 CI 根據 PR review 寫入；高風險要兩人
approved-at: 2026-10-14T08:00:00+08:00
approval-ref: pr#128               # 確認嘅 PR 參照
approved-commit: 3f9c2e1           # 確認時嘅 commit hash；有新 commit 即失效
security-items:                    # 驗證器由 lock 抽出嘅安全項目，每項逐個確認
  - { item: public, risk: high, reason: "公開登入入口", approved-by: [jack@example, mei@example] }
signature: <平台對成份 lock 正規化內容（除 signature 欄位外）嘅簽名>
---
```
- `source-hash`：source 先正規化（統一換行、去行尾空白、去 BOM、合併多餘空行），再計 sha256。淨係改排版而正規化內容唔變，hash 就唔變，**唔會重新編譯**。
- `dependencies`：lock 用到嘅所有其他定義（例如 method 依賴 model 同 provider、api 依賴 method、`_definition`、`_version` 同 pipeline）。任何一個依賴嘅 hash 變咗，lock 都當過時，要重新編譯。
- `source-type: structured` 只限 source 完全由結構化 frontmatter 同結構化列點組成、冇自由文字；判斷由平台嘅 deterministic parser 做，唔由 LLM 編譯器判斷（決定 20）。
- `compiler-hash`：model snapshot、prompt 版本同編譯器版本合埋計 hash（決定 18）。
- `approved-commit`：確認時 PR head 嘅 commit hash（決定 28）。
- `security-items`：驗證器 deterministic 抽出嘅安全項目清單；每項要有 `reason` 同確認人；冇安全項目就寫 `[]`。
- `signature` 由平台簽名 key 簽；簽名覆蓋 frontmatter（除 signature 本身）同全部 section。

### 2A.2 通用 section 規則
- section 用 `##` 標題，名稱同次序按 kind 固定（見 2A.3）；唔准有額外 section，缺 section 用 `(none)` 表示。
- 每個 step 用 `### s{n} {op}` 做標題，下面用 `- key: value` 列點，key 次序固定：`op`、（op 專用參數）、`out`、`source-lines`。
- 變數參照一律用 `input.x`、`ctx.x`、`req.x`、或者之前 step 嘅 `out` 名；唔准參照未定義變數。
- sql 一律寫喺 inline code 入面，用 `$1`、`$2` 參數，`params` 列出對應來源；唔准字串拼接。
- `source-lines` 指返 source `.md` 嘅行數，方便 review 對照。

### 2A.3 各 kind 嘅固定 section
| kind | section（按次序） |
|---|---|
| provider | `## Connection`、`## Isolation`、`## Access`、`## Limits`、`## Migrations` |
| pipeline | `## Parameters`、`## Order`、`## Steps` |
| model | `## Table`、`## Fields`、`## CRUD`、`## Policies` |
| method | `## Signature`、`## Calls`、`## Steps` |
| api（scope: definition） | `## Providers`、`## Pipelines`、`## Defaults` |
| api（scope: version） | `## Pipelines`、`## Defaults`、`## Lifecycle` |
| api（scope: feature） | `## Route`、`## Access`、`## Pipelines`、`## Limits`、`## Request`、`## Response`、`## Transform`、`## Steps`、`## Errors` |

api 類 lock 嘅 frontmatter 要多一個 `scope: definition | version | feature`。

### 2A.4 op 白名單
| 層 | 准用 op |
|---|---|
| api feature | `method`、`validate`、`transform`、`respond` |
| method | `query`、`validate`、`call`、`method`、`respond`、`verify-password`、`hash-password`、`sign-jwt`（`sign-jwt` 只准喺 `## Signature` 標咗 `capabilities: [sign-token]` 嘅 method 入面用，唔靠 method 名推斷） |
| pipeline | `verify-jwt`、`verify-api-key`、`rate-limit-check`、`audit`、`validate`、`inject-ctx`、`method` |

- `method` op 係呼叫 `{object}.{method}`，部署時靜態綁定。
- `fail: CODE` 係 `validate` 嘅參數，CODE 一定要喺有宣告嘅錯誤碼入面。
- `DELETE` sql 喺任何 lock 都唔准出現（決定 13，所有刪除都係軟刪除）。
- 所有 DDL（`CREATE`、`ALTER`、`DROP`、`GRANT`、`REVOKE`、`COMMENT` 等）同 `TRUNCATE` 喺任何 lock 都唔准出現；schema 改動只可以經 migration（決定 27）。
- `query` 只准單一 statement，唔准 `;` 串多句、唔准 `SET`／`RESET`／`COPY`／`DO`／function 定義。
- `UPDATE` 一定要有 `WHERE`，而且要用主鍵（或者唯一鍵）限定，或者明確依賴 RLS 範圍（lock 寫 `scope: pk` 或者 `scope: rls`）；冇 WHERE 嘅 UPDATE 一律 error。

### 2A.5 確認（approval）
- 新編譯出嚟嘅 lock 一律係 `status: pending`。
- 「係咪 structured」由平台 deterministic parser 判斷（決定 20）：
  - `source-type: structured` 而且 `security-items: []` 嘅 lock 由 CI 自動確認，`approval-mode: auto`、`approved-by: [ci-auto]`。
  - 觸及任何安全項目嘅 lock，即使係 structured，都一定要人手確認。
- `source-type: natural` 嘅 lock 一定要人手確認：只有 project 嘅 owner 或者 maintainer 可以喺 CI 嘅 PR review 入面 approve；CI 根據 review 寫入 `approved-by`、`approved-at`、`approval-ref`、`approved-commit`，再由平台簽名。
- 作者定義（決定 28）：PR 開啟人、所有 commit 作者，同埋**最後 push 嘅人**都當係作者；作者唔可以確認。
- 確認綁定 commit hash：PR 有任何新 commit（包括 rebase、force push），所有已有確認即時失效，要重新確認。
- PR 一定要顯示 lock diff（唔係淨係 source diff）。

**安全項目**（驗證器由 lock deterministic 抽出，唔靠 source 文字或者 LLM）：
| 項目 | 風險級別 |
|---|---|
| `public` 變 true、剔走 `auth` | 高 |
| `public-writes: true`、public 可寫欄位改動 | 高 |
| `capabilities: [sign-token]`、claim 白名單、token ttl | 高 |
| `roles` 改動、放寬或者移除權限檢查 | 高 |
| 新增或者改 provider、shared provider 名單 | 高 |
| 新增 `call` 對外目標 | 高 |
| 新增寫入 sql、改寫入欄位 | 中 |
| 放寬 rate-limit／timeout／body／配額 | 中 |
| output 新增欄位 | 中 |
| source 出現似指令句子（見 2A.7） | 中 |

- 每個安全項目要**逐項**確認，而且要寫原因（寫入 `security-items[].reason`）；唔准一鍵全部 approve（決定 19）。
- 高風險項目要兩個唔係作者嘅 owner／maintainer 確認；中風險一個就得。
- 每個 PR 嘅安全項目上限係 5 項（決定 34）；超出 CI 直接 fail，要分拆 PR。
- 細團隊（決定 33）：合資格非作者 owner／maintainer 少過 2 個時，第二確認人由平台 on-call 安全團隊擔任；on-call 2 個工作日內未處理，可以改單人確認加 24 小時延遲生效，延遲期間通知全體 owner，任何 owner 可以喺期間否決。
- PR 同部署報告顯示**有效權限摘要**：每個 endpoint 最終係咪公開、用咩 role、可以讀寫邊啲表同欄位、會唔會簽 token、有冇對外 call、限流同配額。
- 新 lock 未確認之前，舊嘅已確認 lock（如果 hash 依賴仍然有效）繼續生效；新定義未確認就唔可以部署。

### 2A.6 重新編譯規則
- 只有 `source-hash` 或者任何 `dependencies` hash 變咗先會重新編譯；source 改咗但正規化 hash 唔變就唔會重新編譯。
- 編譯用嘅 model 一定 pin 死 snapshot（唔准用會浮動嘅別名）；model snapshot、prompt 版本、編譯器版本合埋計 `compiler-hash`，寫入每個 lock（決定 18）。
- 編譯器 major version 升級（例如 1.x → 2.0）、或者 model snapshot／prompt 改咗：所有 lock 強制重新編譯，即使 source hash 冇變；natural lock 同觸及安全項目嘅 lock 要重新人手確認，確認前唔可以部署。建議用獨立 PR 做升級。
- 編譯器 minor／patch 升級：唔會強制重新編譯；但平台會用 golden corpus 對比新舊輸出，**只要任何輸出唔同，該升級就當 major 處理**。
- Golden corpus 由平台編譯器團隊維護，最少 500 個案例，覆蓋所有 kind 同所有安全項目；項目可以喺 `.golden/` 加自己嘅案例，一樣會參與比對（決定 35）。
- Model snapshot 淘汰（決定 35）：平台承諾宣布淘汰後最少保留 12 個月；項目有 90 日遷移窗口，窗口內舊 lock 照常有效、照常可以部署；窗口完結前未遷移，部署會被封鎖（runtime 已部署嘅 manifest 照常運作）。
- 編譯一定要 deterministic（固定 temperature 0、固定 prompt、固定 seed、固定 snapshot）。CI 會用 lock 記錄嘅 `compiler-hash` 重編一次比對；唔一致就**封鎖部署，冇任何 override**。
- 唔准手改 lock：要改就改 source 再重新編譯同確認，手改會令簽名驗證失敗。

### 2A.7 獨立驗證器同編譯器隔離
- 編譯器（LLM）嘅輸出唔可信：所有 lock 一定要通過**獨立、deterministic 嘅驗證器**（普通程式、唔用 LLM），第 10 節所有規則都由驗證器執行，唔靠編譯器自律（決定 17）。
- 驗證器版本同編譯器分開管理、分開 pin，同樣受 CODEOWNERS 保護。
- 編譯器喺沙盒行：冇網絡、冇檔案系統寫權限（只輸出 lock 文字）、冇任何工具或者 function calling、冇 secret。輸入只有 source、依賴 lock 同平台 prompt。
- 編譯器只可以讀自己依賴範圍內嘅 lock（例如 method 只見到自己物件、provider 同被呼叫 method 嘅 `## Signature`）。
- Prompt injection 偵測：驗證器用固定規則掃 source，例如「忽略之前／上面嘅指示」、「你而家係」、「system:」、「唔使檢查」、「當作已確認」、「print secret」、隱藏字元（零寬字元、bidi 控制字元）、大段 base64 等；命中就出 warning、喺 PR 高亮做中風險安全項目。零寬同 bidi 控制字元直接 error。
- 誤報處理（決定 37）：項目可以喺 `.compiler/injection-allowlist` 加精確句子豁免（只可以精確匹配，唔准 pattern），檔案受 CODEOWNERS 保護，改動屬中風險安全項目。
- 驗證器亦會做語意一致性檢查：source 冇提及寫入，lock 卻有寫入 sql；source 冇提及公開，lock 卻有 `public: true`；呢類「lock 多過 source」嘅情況出 error（決定 37）；誤報就改 source 寫清楚，冇豁免。

## 3. 繼承
- 次序由外到內：`_definition` → `_version` → `{feature}`；繼承喺編譯 feature lock 時解析，結果寫死喺 `{feature}.lock.md` 嘅 `## Pipelines` 同 `## Limits`，`dependencies` 會列埋上層 lock 嘅 hash。
- `pipelines` 會合併；內層可以用 `-name` 剔走外層嘅 pipeline（自然語言寫「唔使 auth」會編譯成 `-auth`）。
- 剔走 `auth` 一定要同時 `public: true`。
- `timeout`、`limits`、`provider` 呢類單一設定由內層覆蓋外層，但唔可以超過平台上限。
- 唔同 version 之間唔會互相繼承。
- 任何一層 lock 改咗，底下所有 feature lock 嘅依賴 hash 都會失效，要重新編譯同驗證。
- `_version` 可以宣告 `deprecated: true` 同 `sunset: <ISO 日期>`（寫入 `## Lifecycle`）；設咗之後，回應會帶 `Deprecation` 同 `Sunset` header。

## 4. Pipeline 同認證
- 內置 pipeline：`auth`、`validate`、`rate-limit`、`audit-log`；佢哋嘅 lock 由平台提供同簽名。用戶可以喺 `pipelines/{name}.md` 用自然語言自訂，一樣編譯成 `pipelines/{name}.lock.md`。
- 每個 pipeline lock 都有 `## Parameters` schema；未知參數或者類型錯就報 error。
- 預設拒絕：所有 endpoint 預設都要經過 `auth`。
- `auth` 一定要排喺所有 `method` step 同其他 Logic 之前（`## Order` 宣告 `must-run: first`）。
- JWT：alg 白名單係 RS256 同 ES256（禁止 `none`，亦唔准同 HS 混用）；要驗 `iss`、`aud`、`exp`、`nbf`；支援 JWKS 輪換同 cache TTL。
- `roles-claim` 預設係 `roles`；`roles: []` 報 error。
- API key：只存 hash，要有 scope，可以 revoke，永遠唔會寫入 log。
- `auth` 成功之後注入 `ctx.tenant_id` 同 `ctx.user`；Logic 唔可以改呢兩個值。
- pipeline 可以用 `method` op 呼叫 model method（決定 5），例如 token 刷新 pipeline 呼叫 `user.sign-token`；pipeline 唔可以直接寫 `query` 或者 `call`。
- 帳戶安全（refresh token、密碼、鎖定）見 4A。
- token 簽發一律經 model 層標咗 `capabilities: [sign-token]` 嘅 method（例如 `user.sign-token`）；簽發規則見 12.6。

## 4A. 帳戶安全：Token、密碼同鎖定
### 4A.1 Access token 同 refresh token（決定 6）
- Access token：JWT，ttl 上限 1 小時。
- Refresh token：不透明隨機值（最少 256 bit），DB 只存 hash；ttl 上限 30 日（由家族第一次簽發計，絕對壽命），閒置 7 日冇用就失效。
- 每次用 refresh token 都輪換：發新 refresh token 同新 access token，舊 refresh token 即時失效。
- 同一次登入產生嘅 refresh token 屬同一個家族（`family_id`）；偵測到重用已失效嘅 refresh token，即時撤銷成個家族，並寫審計記錄、通知用戶。
- 為咗容忍網絡重試，輪換後 10 秒內用返同一個舊 token 會回同一組新 token，唔當重用。
- 登出：revoke 當前家族；「全部裝置登出」revoke 該用戶所有家族。
- refresh 流程由平台內置 `token-refresh` pipeline 實現，用 `method` op 呼叫標咗 `capabilities: [sign-token]` 嘅 model method 簽新 access token。

### 4A.2 密碼政策（決定 7）
- 平台底線：最少 12 個字元，最多 256 個字元；冇強制複雜度規則；一定做洩漏密碼檢查。
- 洩漏密碼檢查用平台維護嘅洩漏密碼清單，以 k-anonymity 方式查（只送 hash 前綴），明文同完整 hash 唔會離開平台。
- `hash-password` op 喺 hash 之前強制檢查底線；唔合就 fail `WEAK_PASSWORD`（422）。
- 物件只可以收緊（例如 `min: 16`），唔可以放寬；lock 寫低過底線就 error。

### 4A.3 帳戶鎖定同限流（決定 8、38）
- 同一帳戶 15 分鐘內登入失敗 5 次，鎖 15 分鐘。
- 同一帳戶或者同一 IP 15 分鐘內失敗 3 次之後，之後嘅登入一定要 captcha（防止被人用嚟惡意鎖死帳戶）。
- 鎖定期間，用戶可以經 email 重設密碼連結即時解鎖；鎖定同解鎖都寫審計記錄並通知用戶。
- 鎖定期間回應同密碼錯完全一樣（401 `INVALID_CREDENTIALS`），耗時都補到 500ms，唔洩露帳戶狀態。
- 另加 IP 限流：每個 IP 每分鐘 10 次登入（見 11.2 `login-guard`）。

## 5. 資料庫提供者 `providers/{name}.md`
source 用自然語言描述，例如：
```
# main
主資料庫，用 PostgreSQL，每個租戶一個 schema。
連線資料用 secret PG_MAIN。唔共用。
每個租戶連線池最多 5 條，每條 statement 最多行 5 秒。
migration 放喺 ./migrations/main/。
```
編譯後嘅 lock 欄位：
| 欄位 | 說明 |
|---|---|
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
- 每次 query 都喺自己嘅 transaction 入面執行：先 `BEGIN`，再 `SET LOCAL app.tenant_id`，然後執行 query，最後 `COMMIT`。就算用 PgBouncer transaction mode 都唔會漏。
- 每張表都一定要有 `tenant_id` 欄位，要 `ENABLE` 同 `FORCE ROW LEVEL SECURITY`。
- RLS policy 一律用 `tenant_id = current_setting('app.tenant_id', false)`；冇設定 tenant 嘅話，query 會直接報錯，唔會回空結果。
- `search_path` 由平台固定產生：schema 模式係 `t_{tenant_id}`，唔包括 `public`；`public` schema 嘅 CREATE 權限要收走。
- 連線 role 唔可以係表嘅 owner，亦唔可以有 `BYPASSRLS`。
- 所有 runtime DB role（`app_rw`、`public_reader`、`public_writer`）都冇 `DELETE`、`TRUNCATE`、DDL 權限；只有 migration 用嘅臨時 role 有 DDL 權限（決定 27）。
- 連線池同 cache 都按租戶分開；cache key 一定要包含 `tenant_id`。
- 每個租戶都有 CPU、記憶體、並發同 DB 連線上限。
- database 模式下同樣要加 RLS，做縱深防禦。
- 以上全部由平台產生，記錄喺 model lock 嘅 `## Policies`；source 唔可以改。

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
  - **欄位黑名單**：平台維護敏感欄位黑名單，public 寫入一律唔准寫，例如 `role`、`roles`、`permissions`、`is_admin`、`tenant_id`、`owner`、`owner_id`、`created_by`、`approved`、`verified`、`password`（login 類由平台 `hash-password` 處理除外）、`deleted_at`、所有系統欄位；物件亦可以自己加黑名單。
  - **UPDATE**：一定要有 `WHERE`，用主鍵或者 RLS 限定，而且 RLS 生效。
  - **租戶**：`tenant_id` 由平台按 slug 解析後注入；request body 入面有 `tenant_id`、`id`、`created_by` 一律 422 拒絕（決定 23）。
  - **配額**：每租戶每日 public 寫入配額強制，預設 1,000 次，另每 IP 每日 50 次；租戶 admin 可以調低，調高最多到 10,000 要經平台申請（決定 39）；用完之後只封鎖 public 寫入（回 429 同 `Retry-After`），已認證請求同 public 讀取照常（決定 21）。
  - **限流**：IP 加租戶兩層限流強制；lock 標 `cost: high` 嘅昂貴 step（例如 `verify-password`、`hash-password`、大範圍 query）自動用更嚴嘅限流，即係 endpoint 上限嘅一半（決定 25、39）。
  - **防機器人**：login 類 endpoint 按 4A.3 喺失敗 3 次後要 captcha；冇 INSERT 嘅其他 public 寫入唔強制；建立資料（有 INSERT）嘅 public endpoint 一定要 captcha 或者 proof-of-work，lock `## Access` 寫 `challenge: captcha | pow`；冇就 error。
  - **審計**：public 寫入一律經 `audit-log`，唔可以剔走。
  - **Untrusted 內容**：public 寫入嘅欄位喺 model 標記 `untrusted: true`；所有輸出（包括已認證 endpoint 讀返出嚟）都按 Content-Type 做 output encoding；每個欄位要有長度上限，陣列要有數量上限，冇上限就 error（決定 24）。
  - **對外 call**：public 路徑（public endpoint 直接或者經 method 間接）預設禁止 `call`；要開要逐個 endpoint 喺 lock 寫 `external-calls: allow`，屬高風險安全項目。
  - 部署報告列出所有 public 寫入 endpoint 同佢哋嘅可寫欄位，並出 warning。

### 7.3 schema 升級去 database（離線遷移，CLI `tenant migrate`）
1. 平台發出一個臨時遷移 role，全程寫入審計記錄。
2. 凍結該租戶寫入：寫入請求回 503 同 `Retry-After`，讀取照常。
3. 複製資料，然後喺新 DB 重新驗證 RLS、FORCE、權限同資料筆數。
4. 自動跑隔離測試，跨租戶讀寫一定要失敗；測試唔通過就回滾。
5. 按租戶做原子切換：更新 secret 對照表，清走該租戶嘅連線池同 manifest cache，再解凍寫入。
6. 即刻撤銷臨時遷移 role。
7. 舊 schema 改為唯讀，保留 30 日；保留期內可以回退，之後自動刪除，並且寫入審計記錄。（呢個係平台遷移流程，同業務資料嘅軟刪除無關。）

## 8. 狀態碼同錯誤
| 狀態碼 | 用途 |
|---|---|
| 200 | 成功 |
| 201 | 建立咗資源，要帶 `Location` |
| 204 | 成功但冇內容 |
| 400 | body 解析唔到 |
| 401 | 未認證或者 token 無效，要帶 `WWW-Authenticate`（RFC 6750） |
| 403 | 已認證但冇權限 |
| 404 | 資源唔存在（public endpoint 規則見 7.2） |
| 405 | method 唔啱，要帶 `Allow` |
| 409 | 同現有狀態衝突 |
| 413 | body 太大 |
| 415 | Content-Type 唔支援 |
| 422 | 驗證唔通過，或者有未宣告欄位 |
| 429 | 請求太多，要帶 `Retry-After` |
| 500 | 未宣告嘅錯誤，code 一律係 `INTERNAL`，唔會洩露內部資料 |
| 503 | 暫時唔可用（包括遷移期間凍結寫入、kill switch 停用咗嘅 endpoint），可以帶 `Retry-After` |
| 504 | 對外 call 超時 |

- 錯誤 body 統一格式：`{ "error": { "code", "message", "request_id" } }`。
- 401、405、429 嘅必要 header 由平台自動加。
- 認證失敗統一回 401，唔會分開講係用戶唔存在定係憑證錯。
- 資源 ID 建議用 UUID，防止被逐個試出嚟。

## 9. Log 同 Manifest
- log 記錄 request id、狀態碼、耗時同 stack；stack 只會寫入 log，唔會放喺回應。token、key 同 secret 一律唔會記錄。
- `audit-log` pipeline 會記錄邊個做咗咩操作。
- 部署時將所有 lock 編譯成 manifest，入面有 routes、pipelines、steps、provider 對應、公開 endpoint 清單、public 寫入 endpoint 清單，同埋每個 lock 嘅 id、hash 同簽名。
- manifest 會簽名；runtime 只會載入驗證成功而且簽名有效嘅 manifest，冷啟動唔會再解析任何 md（source 或者 lock 都唔會）。
- 編譯 manifest 前會逐個驗證 lock 嘅簽名同 hash；任何一個 lock 唔合格，成個部署失敗。

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
- CI workflow 檔、編譯器版本設定（包括 model snapshot、prompt 版本）、驗證器版本設定、安全項目上限設定，全部受 CODEOWNERS 保護，改動要 owner 確認，而且屬高風險（要兩人）（決定 29）。
- 簽名 key 只存在平台（KMS／HSM），CI 同開發者都攞唔到；CI 只係將通過驗證嘅 lock 交平台簽名。
- 平台簽名前會自己再跑一次驗證器同確認檢查，唔信 CI 報告。
- 業務 secret（DB 連線、JWT 私鑰）只用引用名，永遠唔會出現喺 source、lock、manifest 或者 log。

## 10. 部署驗證（全部喺 lock 層檢查；每個 error 都會報 lock 檔名、section、step id、對應 source 行數同原因）
### 10.1 Lock 完整性（error）
- 過時：`source-hash` 同而家 source 正規化 hash 唔一致；或者任何 `dependencies` hash 唔一致。
- 缺失：有 source 冇 lock，或者有 lock 冇 source（孤兒 lock）。
- 未確認：`status` 唔係 `approved`；冇 `approved-by`、`approved-at`、`approval-ref` 或者 `approved-commit`。
- 確認失效：`approved-commit` 唔係 PR 最新 commit。
- 確認人無效：`source-type: natural` 或者 `security-items` 唔係空，但 `approval-mode: auto`；確認人唔係 owner／maintainer；確認人係作者（包括最後 push 嘅人）。
- 安全項目：驗證器抽出嘅安全項目同 lock `security-items` 唔一致；有項目冇 `reason`；高風險項目少過兩個非作者確認；PR 安全項目超過上限。
- structured 判斷：lock 寫 `source-type: structured` 但 deterministic parser 判斷係 natural。
- 簽名錯：`signature` 驗證失敗（即係有人手改過 lock）。
- 手改：lock 格式唔合 2A（frontmatter 欄位或者次序錯、多咗或者少咗 section、step 欄位次序錯）。
- 重編唔一致：CI 用 lock 嘅 `compiler-hash` 重編，結果同 lock 唔一致，封鎖部署，冇 override。
- 編譯器：`compiler-hash` 同而家 pin 嘅 model snapshot、prompt、編譯器版本唔一致；`compiler.version` major 唔一致；冇 `model-snapshot` 或者用咗浮動別名；編譯器名唔係平台登記。
- `kind` 或者 `id` 同檔案路徑唔一致；`lock-version` 唔支援。
- source 有零寬字元或者 bidi 控制字元。
- 語意一致性：lock 有 source 冇提及嘅寫入、公開、剔走 auth、簽 token 或者對外 call。

### 10.2 結構同路由（error）
- 目錄深度唔係啱啱好 `api/{definition}/{version}/{feature}/{feature}.md`；models 唔合 `models/{object}/{object}.md` 或者 `models/{object}/methods/{method}.md`；檔名同資料夾名唔一致；version 名唔合規則；`_` 開頭檔放錯位置；出現已取消嘅 `models/{object}/{version}/` 資料夾。
- `## Route` 嘅 method 唔喺支援範圍；normalize 之後 method 加 path 重複（normalize 包括轉細楷、去尾斜線、解 % 編碼，同埋統一 `:id` 同 `{id}`）；path 越出自己前綴、用咗保留路徑，或者有 `..` 或萬用字元。

### 10.3 Pipeline 同 auth（error）
- 引用唔存在嘅 pipeline；pipeline 參數唔合 `## Parameters` schema；auth 次序錯；剔走 auth 但冇 `public: true`；`roles: []`。
- pipeline lock 用咗白名單以外嘅 op，或者直接寫 `query`／`call`。
- JWT alg 唔喺 RS256／ES256，或者冇驗 `iss`、`aud`、`exp`、`nbf`。
- public 寫入 endpoint 剔走咗 `audit-log`。

### 10.4 api feature 內容（error）
- `## Steps` 用咗 `method`、`validate`、`transform`、`respond` 以外嘅 op（包括 `query` 或者 `call`）。
- 缺少 `## Request` 或者 `## Response`；`## Transform` 參照未宣告嘅 request 欄位或者 method output 冇嘅欄位。
- timeout 或者 limits 超過平台上限。
- `## Errors` 嘅狀態碼同 RFC 語意唔一致。

### 10.5 Provider（error）
- type 未登記；isolation 唔係 schema 或者 database；connection 唔係 secret 引用；secret 唔喺本 project namespace；secret 名嵌咗 `{tenant_id}`；引用嘅 secret 唔存在；source 出現明文憑證。
- 用咗 allowlist 以外嘅 provider；query 嘅 provider 唔係靜態決定；有 query 但解析唔到 provider。
- shared provider 冇 `allowed-endpoints`；名單以外嘅 endpoint（直接或者經 method）用咗佢；喺佢上面有非 SELECT sql。

### 10.6 RLS 同 DB 權限（error）
- 有表冇 `tenant_id`、冇 ENABLE 或者 FORCE RLS；policy 唔係用 `current_setting(..., false)`；`public` schema 仍然有 CREATE 權限；`## Policies` 同平台產生嘅唔一致。
- 任何 runtime role 有 `DELETE`、`TRUNCATE` 或者 DDL 權限。

### 10.7 Public（error）
- public endpoint 用到 query（包括經 method）但冇 `tenant: { from: path }`，或者 path 冇 `{slug}`。
- public endpoint 經 method 有寫入 sql，但冇 `public-writes: true`，或者冇 IP 加租戶兩層限流，或者冇每日配額。
- public 寫入用咗 `INSERT`／`UPDATE` 以外嘅操作；寫咗 `writable-columns` 以外嘅欄位；寫咗黑名單欄位。
- public 寫入嘅 `UPDATE` 冇 `WHERE`，或者唔係用主鍵／RLS 限定。
- public 寫入 endpoint 嘅 `## Request` 宣告咗 `tenant_id`、`id` 或者 `created_by`。
- 有 INSERT 嘅 public endpoint 冇 `challenge: captcha | pow`。
- public 寫入欄位冇 `untrusted: true`，或者冇長度／數量上限。
- public 路徑有 `call`，但冇 `external-calls: allow`。
- `public_writer` grant 超出 lock 實際用到嘅表、欄位同操作。

### 10.8 Model 同 method（error）
- 物件冇 provider 或者冇 table；provider 未登記；用 shared provider 嘅物件有寫入 method；物件自己宣告咗 `tenant_id`；欄位 type 未知。
- method 用咗白名單以外嘅 op。
- sql 唔係參數化（字串拼接、`$n` 冇對應 param、param 冇用到）；sql 有多個 statement。
- sql 用咗物件以外嘅表或者 provider（寫入其他物件一定要經佢自己嘅 method）。
- `writes: false` 但有 INSERT／UPDATE sql。
- 任何 lock 出現 `DELETE`、`TRUNCATE` 或者 DDL sql。
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
- 列出公開 endpoint、public 寫入 endpoint 同可寫欄位、用緊 shared provider 嘅 endpoint、每個 endpoint 用緊邊個 provider、被覆蓋咗嘅 CRUD method、今次重新編譯同等緊確認（pending）嘅 lock、每個 lock 嘅確認人同 commit、停用緊嘅 endpoint。
- **有效權限摘要**：每個 endpoint 一行，列出公開與否、role、可讀表、可寫表同欄位、會唔會簽 token、對外 call、限流、配額、challenge；同上次部署比較，標出變咗嘅地方。

## 11. 完整範例（api、provider、pipeline）
### 11.1 Provider
`providers/main.md`
```
# main
主資料庫，用 PostgreSQL，每個租戶一個 schema。
連線用 secret PG_MAIN。
每個租戶連線池最多 5 條，statement 最多 5 秒。
migration 放 ./migrations/main/。
```
`providers/main.lock.md`
````
---
lock-version: 1
kind: provider
id: main
source: providers/main.md
source-hash: sha256:9a1c...e2
source-type: natural
dependencies: []
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:00:00+08:00
approval-ref: pr#120
approved-commit: a1b2c120
security-items: [{ item: provider-new, risk: high, reason: "新增主資料庫", approved-by: [jack@example, mei@example] }]
signature: MEUCIQ...
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
```
# enterprise
企業客戶用，每個租戶一個獨立 database，用 PostgreSQL。
連線用 secret PG_ENTERPRISE，每個租戶實際憑證由平台對照表決定。
```
（lock：`type: postgres`、`mode: database`、`secret: PG_ENTERPRISE`、`shared: false`，格式同上。）

`providers/geo.md`
```
# geo
共用嘅地區資料庫，PostgreSQL，schema 隔離，只讀。
連線用 secret PG_GEO。
只准 catalog/v1/list-items 用。
```
`providers/geo.lock.md`（節錄 section，frontmatter 格式同上）
````
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
```
# login-guard
登入用嘅防護：每個 IP 每分鐘最多 10 次，每個 email 每分鐘最多 5 次。
參數：ip-per-minute（整數，預設 10）、account-per-minute（整數，預設 5）。
要喺所有 method 之前行，並且記低審計記錄（唔記密碼）。
```
`pipelines/login-guard.lock.md`
````
---
lock-version: 1
kind: pipeline
id: login-guard
source: pipelines/login-guard.md
source-hash: sha256:51bd...07
source-type: natural
dependencies: []
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example]
approved-at: 2026-10-14T08:05:00+08:00
approval-ref: pr#121
approved-commit: a1b2c121
security-items: [{ item: rate-limit-new, risk: medium, reason: "登入防爆破", approved-by: [jack@example] }]
signature: MEYCIQ...
---
## Parameters
| name | type | default | min | max |
|---|---|---|---|---|
| ip-per-minute | integer | 10 | 1 | 600 |
| account-per-minute | integer | 5 | 1 | 60 |

## Order
- must-run: before-steps
- after: [auth]

## Steps
### s1 rate-limit-check
- op: rate-limit-check
- key: req.ip
- per-minute: params.ip-per-minute
- source-lines: 2-2

### s2 rate-limit-check
- op: rate-limit-check
- key: req.body.email
- per-minute: params.account-per-minute
- source-lines: 2-2

### s3 audit
- op: audit
- event: login-attempt
- fields: [req.ip, req.body.email]
- redact: [req.body.password]
- source-lines: 4-4
````

### 11.3 api 定義同版本
`api/users/_definition.md`
```
# users
用戶管理 API。可用 provider：main 同 enterprise，預設 main。
所有 endpoint 都要 JWT 認證。
```
`api/users/_definition.lock.md`（節錄 section；frontmatter 有 `kind: api`、`scope: definition`）
````
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
```
# users v1
每分鐘限 60 次。呢個版本已經 deprecated，2027-06-30 停用。
```
`api/users/v1/_version.lock.md`（節錄；`scope: version`）
````
## Pipelines
### p1 rate-limit
- per-minute: 60

## Defaults
(none)

## Lifecycle
- deprecated: true
- sunset: 2027-06-30
````

### 11.4 `api/users/v2/create-user/create-user.md`（路由 `POST /users/v2/create-user`）
```
# 建立用戶
POST。只有 admin 可以用，每分鐘最多 30 次。
請求 body 要有 email（合法 email）同 name（1 至 50 字）。
用 user.register 建立用戶，成功回 201 同新用戶嘅 id、email、name，並帶 Location。
email 已經有人用就回 409 EMAIL_TAKEN；驗證唔通過回 422。
```
`api/users/v2/create-user/create-user.lock.md`
````
---
lock-version: 1
kind: api
scope: feature
id: users/v2/create-user
source: api/users/v2/create-user/create-user.md
source-hash: sha256:7f02...aa
source-type: natural
dependencies:
  - { id: user/methods/register, kind: method, hash: sha256:c3e1...90 }
  - { id: users/_definition, kind: api, hash: sha256:11ab...3c }
  - { id: users/v2/_version, kind: api, hash: sha256:0d9e...51 }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:10:00+08:00
approval-ref: pr#122
approved-commit: a1b2c122
security-items: [{ item: roles, risk: high, reason: "只限 admin 建立用戶", approved-by: [jack@example, mei@example] }]
signature: MEUCIG...
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
| 201 | { id, email, name } | Location |

## Transform
- in: { email: req.body.email, name: req.body.name }
- out: { id: u.id, email: u.email, name: u.name }

## Steps
### s1 method
- op: method
- target: user.register
- args: transform.in
- out: u
- source-lines: 4-4

### s2 respond
- op: respond
- status: 201
- value: transform.out
- location: /users/v2/{u.id}
- source-lines: 4-4

## Errors
| status | code | from |
|---|---|---|
| 409 | EMAIL_TAKEN | user.register |
| 422 | VALIDATION_FAILED | platform |
````

### 11.5 `api/users/v2/get-user/get-user.md`（路由 `GET /users/v2/{id}`）
```
# 攞用戶
GET，路徑 /users/v2/{id}，id 係 uuid。
用 user.get 攞用戶，回 200 同 id、email、name；搵唔到回 404 NOT_FOUND。
v2 嘅 name 欄位叫 display_name。
```
`get-user.lock.md`（節錄 section；frontmatter 格式同 11.4，`dependencies` 包括 `user/crud/get`）
````
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
| 404 | NOT_FOUND | user.get |
````
- 呢個例子示範決定 2：`user.get` 只有一個版本，v2 嘅 `display_name` 改名由 api 層 `## Transform` 處理。

### 11.6 Public 讀取：`api/catalog/v1/list-items/list-items.md`（路由 `GET /catalog/v1/{slug}/items`）
`api/catalog/_definition.md`
```
# catalog
貨品目錄。可用 provider：main 同 geo，預設 main。
```
`list-items.md`
```
# 公開貨品清單
GET，路徑 /catalog/v1/{slug}/items，公開，唔使 auth，租戶由 slug 決定。
每個 IP 每分鐘最多 120 次。
query 要有 region（2 個字元）。
用 item.list-public 攞公開貨品，再用 region.find-by-code 搵地區；
回 200 同 items（id、name、price）同 region。地區搵唔到回 404 NOT_FOUND。
```
`list-items.lock.md`（節錄 section）
````
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
| path.slug | string | | yes |
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
| 404 | NOT_FOUND | region.find-by-code |
````

### 11.7 Public 建立資料：`api/catalog/v1/submit-review/submit-review.md`（路由 `POST /catalog/v1/{slug}/reviews`）
```
# 提交評論
POST，路徑 /catalog/v1/{slug}/reviews，公開，唔使 auth，租戶由 slug 決定。
訪客可以留評論，所以會寫入資料，要 captcha。
每個 IP 每分鐘最多 5 次，每個租戶每分鐘最多 100 次。
body：item_id（uuid）、rating（1 至 5）、comment（1 至 500 字）。
用 review.submit 建立評論，成功回 201 同 id。貨品唔存在回 404 NOT_FOUND。
```
`submit-review.lock.md`（節錄 section）
````
## Route
- method: POST
- path: /catalog/v1/{slug}/reviews

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: true
- role: public_writer
- writable-columns: { reviews: [item_id, rating, comment] }
- challenge: captcha
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
| 404 | NOT_FOUND | review.submit |
| 422 | VALIDATION_FAILED | platform |
| 429 | QUOTA_EXCEEDED | platform |
````
- body 有 `tenant_id`、`id` 或者 `created_by` 會即刻 422；`tenant_id` 由平台按 slug 注入。
- `review.submit` 透過 `item.get` 檢查貨品存在（寫入其他物件／讀其他物件都經佢自己 method），然後一個參數化 `INSERT INTO reviews (item_id, rating, comment) VALUES ($1, $2, $3)`；`comment` 喺 `review` model 標 `untrusted: true, max: 500`，所有輸出做 encoding。

相關 method：
- `models/user/methods/register.md`：「如果 email 已經有人用就 fail EMAIL_TAKEN，否則建立用戶，回傳 id、email、name。」編譯成 1 個 SELECT 檢查、1 個參數化 INSERT（`writes: true`）。
- `models/item/item.md` 用 provider `main`、table `items`；`models/item/methods/list-public.md`（`writes: false`）負責揀 `is_public = true` 嘅項目。
- `models/region/region.md` 用 shared provider `geo`、table `regions`；`models/region/methods/find-by-code.md`（`writes: false`）負責按 code 搵地區，冇結果就 fail NOT_FOUND。

## 12. Models 層
### 12.1 目錄同命名
```
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
|---|---|
| `type` | uuid、string、string[]、integer、number、boolean、timestamp、password、json |
| `primary`、`unique`、`default` | 約束同預設值（`gen`、`now`） |
| `format`、`min`、`max` | 驗證規則 |
| `sensitive` | 敏感欄位（見 12.9） |

- 平台會自動加 `tenant_id` 欄位同 RLS policy（寫入 `## Policies`）；用戶唔可以自己宣告 `tenant_id`。
- `type: password` 嘅欄位：
  - 寫入時一律用 argon2id hash，參數由平台管理。
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
- api 係「對外」：路由、pipeline、Request、Response、Transform；Steps 只准 `method`、`validate`、`transform`、`respond`。
- method 可以用 `method` op 呼叫其他 method 組合；部署時建立呼叫圖，有循環就報 error，深度上限 3 層（由 api 或者 pipeline 算起）。
- 單一職責：每個 method 最多 1 個寫入 sql，而且最多只寫一張表；超出出 warning，建議拆開（決定 12）。

### 12.4 喺 api 呼叫 method
api source 寫「用 user.login 驗證帳戶密碼」，編譯成：
````
### s1 method
- op: method
- target: user.login
- args: transform.in
- out: result
````
- `target` 只可以係 `{object}.{method}`，部署時靜態綁定，唔可以由 request 決定，冇版本後綴。
- method 拋出嘅錯誤，喺 api lock 嘅 `## Errors` 對應狀態碼（例如 `| 401 | INVALID_CREDENTIALS | user.login |`）；冇對應嘅錯誤一律回 500 INTERNAL。
- 唔同 api version call 同一個 method，差異由各自 `## Transform` 處理。
- 呼叫深度上限 3 層，唔准循環呼叫。

### 12.5 版本同繼承
- 物件定義（欄位同表）唔分 version，改欄位要經 migration。
- method 只有單一版本，冇 version 資料夾，冇 method 層嘅 deprecated／sunset；deprecation 只喺 api `_version` 做。
- method 唔會繼承 api 嘅 pipelines。auth 只喺 api／pipeline 層執行，method 會收到 `ctx.tenant_id` 同 `ctx.user`，但係唔可以修改。

### 12.6 安全
- method 一樣行 7.1 嘅租戶隔離：每個 query 都喺有 `SET LOCAL app.tenant_id` 嘅 transaction 入面執行，RLS 強制生效。
- 由 public endpoint call 嘅 method：
  - tenant 由 slug 解析，平台注入。
  - 只讀 step 用 `public_reader` role；寫入 step 用 `public_writer` role，grant 只限 lock 用到嘅表、欄位同操作（見 7.2）。
  - 可以係 `writes: true`（決定 4），但要符合 7.2 全部 public 寫入規則。
  - 預設唔准 `call`。
- `verify-password` 一定以固定時間比對。用戶唔存在嘅時候，平台會用假 hash 比對一次，令耗時一致；兩種情況都回同一個錯誤碼，防止 enumeration。
- token 簽發放喺 model 層 method（例如 `user.sign-token`，決定 5、30）：
  - 只有 `## Signature` 明確寫 `capabilities: [sign-token]` 嘅 model method 可以用 `sign-jwt` op；唔靠 method 名推斷（叫 `sign-token` 但冇旗標一樣唔准）。
  - capability 由 source 自然語言描述（例如「呢個 method 負責簽 token」），編譯後屬高風險安全項目。
  - claim 白名單：method 只可以傳 `sub` 同平台允許嘅自訂 claim（例如 `sid`）；`roles` 同 `tenant_id` 由平台喺簽發時從 DB 讀（用 `sub` 搵用戶），method 唔可以傳入或者覆蓋。
  - 平台自動加 `iss`、`aud`、`exp`、`nbf`、`iat`、`jti`；method 唔可以覆蓋。
  - `ttl` 有上限（access token 最多 1h）。
  - 只可以簽 JWT，alg 只准 RS256 或者 ES256；私鑰喺平台 secret，lock 只寫 `key-ref`。
  - 其他 method（例如 `user.login`）同 pipeline 要簽 token，就用 `method` op 呼叫 `user.sign-token`。
- login 類 endpoint 一定同時按 IP 同帳戶限流（例如 11.2 嘅 `login-guard`），並跟 4A.3 嘅鎖定同 captcha 規則。

### 12.7 驗證規則
models 層嘅所有檢查都喺 lock 層做，完整清單見 10.1 同 10.8。重點：
- 結構：路徑唔合 `models/{object}/{object}.md` 或者 `models/{object}/methods/{method}.md`；檔名唔等於資料夾名。
- 物件：冇 provider 或者 table；provider 未登記；shared provider 物件有寫入 method；自己宣告 `tenant_id`；欄位 type 未知。
- method：白名單以外 op；query 去咗其他表或者 provider；`writes: false` 有寫入 sql；有 `DELETE` sql；sql 唔參數化；`fail` 用咗未宣告錯誤碼；output 有 password 欄位；揀咗 password 欄位但唔係畀 `verify-password` 用；`sign-jwt` 喺冇 `capabilities: [sign-token]` 嘅 method 出現；有 DDL、TRUNCATE，或者 UPDATE 冇用主鍵／RLS 限定。
- 呼叫：唔存在嘅 object 或者 method；寫咗版本後綴；input 唔合簽名；循環或者超過 3 層。
- Warning：method 錯誤碼喺 api 冇對應狀態碼；單一職責超出。

### 12.8 完整範例（user model、user.sign-token、user.login、login api）
`models/user/user.md`
```
# user
用戶資料，放喺 provider main 嘅 users 表。

欄位：
- id：uuid，主鍵，自動產生。
- email：合法 email，唯一。
- name：最多 50 字。
- password：密碼。
- roles：字串陣列。
- last_login_at：時間，可以冇。

提供 get、list、update 三個 CRUD。list 每頁預設 20，最多 100，可以按 created_at 或者 name 排序，可以按 email 篩選。
```
`models/user/user.lock.md`
````
---
lock-version: 1
kind: model
id: user
source: models/user/user.md
source-hash: sha256:aa41...0f
source-type: natural
dependencies:
  - { id: main, kind: provider, hash: sha256:9a1c...e2 }
  - { id: migrations/main, kind: migration, hash: sha256:5e77...b1 }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:20:00+08:00
approval-ref: pr#125
approved-commit: a1b2c125
security-items:
  - { item: public-writable-column, risk: high, reason: "last_login_at 畀 public 登入寫", approved-by: [jack@example, mei@example] }
signature: MEUCIQ...
---
## Table
- provider: main
- table: users

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| email | string | format: email, unique | no |
| name | string | max: 50 | no |
| password | password | argon2id | no |
| roles | string[] | | no |
| last_login_at | timestamp | nullable, public-writable | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [email] |
| update | partial: true, optimistic-lock: updated_at |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer [UPDATE(last_login_at)]
````

`models/user/methods/sign-token.md`
```
# sign-token
幫用戶簽一張 access token。
呢個 method 負責簽 token。
輸入：sub（用戶 id，uuid）。
輸出：token（字串）。
token 有效 1 小時，用 ES256，key 用 JWT_SIGNING_KEY。
```
`models/user/methods/sign-token.lock.md`
````
---
lock-version: 1
kind: method
id: user/methods/sign-token
source: models/user/methods/sign-token.md
source-hash: sha256:3b90...d4
source-type: natural
dependencies:
  - { id: user, kind: model, hash: sha256:aa41...0f }
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:22:00+08:00
approval-ref: pr#125
approved-commit: a1b2c125
security-items:
  - { item: sign-token, risk: high, reason: "唯一簽 token 入口", approved-by: [jack@example, mei@example] }
signature: MEQCIA...
---
## Signature
- input: { sub: { type: uuid, required: true } }
- output: { token: string }
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
- out: token
- source-lines: 1-5

### s2 respond
- op: respond
- value: { token: token }
- source-lines: 4-4
````

`models/user/methods/login.md`
```
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
````
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
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:25:00+08:00
approval-ref: pr#125
approved-commit: a1b2c125
security-items:
  - { item: write-sql, risk: medium, reason: "記錄最後登入時間", approved-by: [jack@example] }
signature: MEUCIQ...
---
## Signature
- input: { email: { type: string, format: email, required: true }, password: { type: string, min: 1, max: 256, required: true } }
- output: { user: { id, email, name, roles }, token: string }
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
- sql: `select id, email, name, roles, password from users where email = $1 and deleted_at is null`
- params: [input.email]
- writes: false
- out: u
- source-lines: 7-7

### s2 verify-password
- op: verify-password
- cost: high
- hash: u.password
- plain: input.password
- missing-user: dummy-hash
- out: ok
- source-lines: 8-8

### s3 validate
- op: validate
- when: `ok != true`
- fail: INVALID_CREDENTIALS
- source-lines: 8-8

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
- source-lines: 9-9

### s5 method
- op: method
- target: user.sign-token
- args: { sub: u.id }
- out: t
- source-lines: 10-10

### s6 respond
- op: respond
- value: { user: { id: u.id, email: u.email, name: u.name, roles: u.roles }, token: t.token }
- source-lines: 10-10
````
- s4 係寫入，所以 `writes: true`；login 係 public endpoint，s4 用 `public_writer` 執行，grant 只有 `UPDATE(last_login_at)`（決定 4 令呢個情況變得可行）。

`api/auth/_definition.md`
```
# auth
認證 API，只用 provider main。
```
`api/auth/v1/login/login.md`（路由 `POST /auth/v1/{slug}/login`）
```
# 登入
POST，路徑 /auth/v1/{slug}/login。
公開，唔使 auth，租戶由 slug 決定。
呢個 endpoint 會寫入資料（記最後登入時間），准許公開寫入。
用 login-guard 防護：每個 IP 每分鐘 10 次，每個帳戶每分鐘 5 次；另外每個租戶每分鐘最多 300 次。

請求 body：email（合法 email，必填）、password（1 至 256 字，必填）。
用 user.login 登入，成功回 200 同 token 同 user（id、email、name、roles）。
帳戶密碼錯回 401 INVALID_CREDENTIALS；驗證唔通過回 422。
```
`api/auth/v1/login/login.lock.md`
````
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
compiler: { name: md-logic, version: 2.0.0, model-snapshot: llm-2026-09-01 }
compiler-hash: sha256:c0de...77
status: approved
approval-mode: manual
approved-by: [jack@example, mei@example]
approved-at: 2026-10-14T08:30:00+08:00
approval-ref: pr#126
approved-commit: a1b2c126
security-items:
  - { item: public, risk: high, reason: "登入入口一定要公開", approved-by: [jack@example, mei@example] }
  - { item: auth-removed, risk: high, reason: "未登入先要登入", approved-by: [jack@example, mei@example] }
  - { item: public-writes, risk: high, reason: "只寫 last_login_at", approved-by: [jack@example, mei@example] }
signature: MEYCIQ...
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
- challenge: captcha-after-3-failures   # 4A.3
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
| path.slug | string | | yes |
| body.email | string | format: email | yes |
| body.password | string | min: 1, max: 256 | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { token, user: { id, email, name, roles } } | |

## Transform
- in: { email: req.body.email, password: req.body.password }
- out: { token: result.token, user: result.user }

## Steps
### s1 method
- op: method
- target: user.login
- args: transform.in
- out: result
- source-lines: 7-7

### s2 respond
- op: respond
- status: 200
- value: transform.out
- source-lines: 7-7

## Errors
| status | code | from |
|---|---|---|
| 401 | INVALID_CREDENTIALS | user.login |
| 422 | VALIDATION_FAILED | platform |
````
- PR review 會逐項高亮：`public: true`、`-auth`、`public-writes: true`（以上高風險，要兩個非作者確認並寫原因）、新增寫入 sql（`user.login` s4，中風險）。
- 有效權限摘要：`POST /auth/v1/{slug}/login` → public；role public_reader + public_writer；讀 users；寫 users.last_login_at；間接簽 token（經 `user.sign-token`）；冇對外 call；IP 10/min、帳戶 5/min、租戶 300/min；有每日配額。

### 12.9 CRUD
- 預設唔會自動生成任何 CRUD。model source 要明確講（例如「提供 get、list、update」或者「提供全部 CRUD」），lock 嘅 `## CRUD` 列咗邊樣就注入邊樣。
- CRUD 選項（喺 source 用自然語言講，編譯成 `## CRUD` 表）：
  - `list`：`page-size`（預設 20）、`max-page-size`（預設 100，平台有上限）、`sort` 白名單、`filter` 白名單。
  - `update`：`partial`（預設 true）。
  - `delete`：一律軟刪除，冇 `soft: false` 選項（決定 13）。
- 注入嘅 method 名同簽名（每個都有平台產生同簽名嘅 lock，`source-type: structured`，`id` 例如 `user/crud/get`）：
  - `create`：input 係所有可寫欄位，output 係物件。
  - `get`：input 係 `id`，冇結果就 fail `NOT_FOUND`。
  - `list`：offset 分頁（決定 14），input 係 `page`（由 1 開始）、`page-size`（唔可以超過 `max-page-size`，超過回 422）、`sort`、`filter`；只准用白名單欄位排序同篩選；output 係 `{ items, page, page_size, has_more }`。
  - `update`：預設係部分更新（PATCH 語意），會做樂觀鎖檢查（`updated_at`），衝突就 fail `CONFLICT`。
  - `delete`：軟刪除，即係設 `deleted_at`；軟刪除咗嘅資料，`get` 同 `list` 會自動排除。
- 軟刪除永遠唔會實際刪除資料，冇 purge 機制，亦冇保留期到期刪除（決定 13）；任何 lock 都唔准有 `DELETE` sql，DB role 亦冇 DELETE 權限。
- 私隱法例嘅刪除要求用 12.10 嘅 crypto-shredding 同匿名化處理，唔使開放 DELETE。
- SDK 內置最佳寫法：一律參數化 query、有分頁上限、排序同篩選欄位用白名單、行 RLS 同租戶隔離。
- 敏感欄位：
  - `password` 欄位喺 create 同 update 時自動 hash，永遠唔會出現喺任何輸出。
  - 欄位可以標 `sensitive: true`（例如電話），呢啲欄位唔會出現喺 list 輸出，亦唔可以用嚟 filter 或者 sort；get 預設都唔會輸出，要喺 CRUD 選項明確開（`sensitive-output`）。
  - `id`、`tenant_id`、`created_at`、`updated_at`、`deleted_at` 呢啲系統欄位唔可以由 input 寫入。
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
  2. sensitive 欄位：銷毀相關 DEK，資料即時永遠讀唔返（crypto-shredding）。
  3. standard 欄位：用平台專用 erasure role 做 UPDATE，換成不可逆占位值（例如 `erased-<隨機>`），email 等唯一欄位用隨機唯一值；只准 UPDATE，冇 DELETE。
  4. 紀錄標 `erased_at`，同時軟刪除；行同關聯保留，統計唔受影響。
  5. 撤銷該主體所有 refresh token 家族同 API key。
- 時限：收到要求後 30 日內完成；平台目標係觸發後 24 小時內完成。
- 審計：記錄觸發人、確認人、原因、要求參照、受影響表同欄位、紀錄數量、完成時間；審計只用假名化主體 id，唔記任何個人資料本身。
- 備份：sensitive 欄位因為 DEK 已銷毀，備份入面都讀唔返；standard 欄位喺備份保留期（35 日）屆滿後自然消失；從備份還原時，平台會重播 erasure 紀錄冊，再做一次匿名化先開放使用。
- Log：個人資料本身唔准寫入 log（同 token、secret 一樣）；log 只記假名化 id，log 保留 30 日。
- 匿名化欄位喺 public 寫入黑名單（7.2）之外另有保護：erased 紀錄唔准再由 lock 寫入個人欄位。

## 13. 待確認
私隱法（PDPO、GDPR）部分嘅建議請法律意見覆核。

## 14. 後續版本
- 排程、queue、webhook 入口。
- 線上（唔凍結寫入）嘅 schema 升級去 database 遷移。
- 除咗 PostgreSQL 以外嘅 provider type。
- Steps 白名單 op 擴充，例如 transaction 區塊、條件分支。
- 本地模擬環境同 manifest 差異比較工具。
- 物件之間嘅關聯（has-many、belongs-to）。
- lock 語意差異工具（顯示 lock 改動對行為嘅影響，而唔止文字 diff）。
- 編譯器自動產生測試案例，連同 lock 一齊 review。
- public 寫入內容審核 hook（spam／不當內容），處理 captcha 被破解同低質內容嘅剩餘風險。
- 私隱刪除（crypto-shredding／匿名化）嘅正式流程同 CLI。
- Kill switch 自動觸發（例如錯誤率或者寫入量異常時自動停用）。
