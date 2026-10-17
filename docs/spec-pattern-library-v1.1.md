# Best-practice pattern library：Spec v1.1（草稿）

<!-- spec-status: draft -->

> 狀態：草稿，未 review。基準係 vibe-backend/sdk main `9674786` 嘅 [`spec-v1.8.md`](spec-v1.8.md)。ADR 見 [`adr/0001-pattern-library.md`](adr/0001-pattern-library.md)。產品經理硬性規則（2026-10-17 09:59 HKT）同 §9 嘅決定（2026-10-17 10:08 HKT）寫成「一定要（MUST）」。只描述防禦設計。
>
> 範圍：本文件屬 v1.1 spec，merge 之後併入 v1.1；唔阻擋任何 v1 blocker，v1 嘅實作次序照決定 157。

## 0. 決定

| # | 決定 | 狀態 | 位置 |
| --- | --- | --- | --- |
| 150 | Pattern 分兩類：`security`（auth、rate limiting、challenge、RLS、refresh token）留喺 core，同 SDK 一齊 release 同 review；`business`（CRUD、pagination、search、upload、email）v1.1 拆做獨立版本嘅 package，`vibe compile` 只下載需要嗰啲。v1 全部內置，但內部用同一個介面寫。 | 草稿 | 1、2 |
| 151 | Pattern manifest：`id`、semver `version`、`kind: security \| business`、`provides`（capability）、`requires`（DB 權限、對外網絡（預設冇，用白名單）、secret 名、依賴嘅 security pipeline）。 | 草稿 | 3 |
| 152 | Pattern 只可以輸出 IR，一定唔可以喺 runtime 執行自己嘅 code。 | 已確認（產品經理 2026-10-17） | 4 |
| 153 | `kind: security` 只接受 `source: builtin`；pattern 嘅 `requires` 一定要係 endpoint 宣告嘅子集。 | 已確認（產品經理 2026-10-17） | 3、5 |
| 154 | 揀 pattern：多過一個 business pattern 符合就係 compile error，一定要用 `use: <id>@<range>` 明確揀；冇隱含評分。 | 已確認（產品經理 2026-10-17） | 5 |
| 155 | 來源驗證：signer 身份固定到一個 repo 同 workflow（issuer + SAN）；支援離線驗證（bundle + cache 嘅 trust root），Sigstore／Rekor 用唔到都可以部署；驗證失敗 fail closed；`vibe serve` cold start 唔可以下載任何嘢。 | 已確認（產品經理 2026-10-17） | 6 |
| 156 | Lock 新欄位：core SDK 版本同 hash、compiler 版本、IR schema 版本、`patterns[]`、每個 endpoint 嘅 capability → pattern `id@version`。v1 一律 `source: builtin`，digest 係內嵌內容嘅 hash。 | 草稿 | 7 |
| 157 | v1 內部改用同一介面：要等 #147、#95 同決定 149 實作 merge 之後先做；只可以加，唔可以改行為；放喺 GA 計劃最後。 | 已確認（產品經理 2026-10-17） | 8 |
| 158 | Registry 發布者同 signer 身份（P1、P2）：v1.1 registry 只接受一個官方 repo 加一個 release workflow 簽嘅 package；SAN 只接受 `refs/tags/v*` release tag，唔接受 branch ref；第三方 pattern 留 vNext。 | 已確認（產品經理 2026-10-17） | 6、9 |
| 159 | Cache trust root 更新（P3）：同決定 119 一樣，跟 core release 更新，唔另設更新渠道。 | 已確認（產品經理 2026-10-17） | 6、9 |
| 160 | 冇網絡嘅 compile（P4）：用預先填好嘅本地 cache／mirror，入面每個 package 照樣做決定 155 嘅完整驗證，同埋一定要通過決定 161 嘅撤回清單檢查。 | 已確認（產品經理 2026-10-17） | 6、9 |
| 161 | Pattern 版本撤回清單（P5）：用同決定 158 一樣嘅 pinned signer 身份簽；有版本號同到期時間；比 cache 版本舊嘅清單一律拒絕（防回滾）；到期之後警告，但照用本地最新一份已驗證嘅清單，唔會因為離線就全部失敗。 | 已確認（產品經理 2026-10-17） | 6.1、9 |
| 162 | IR schema 相容（P6）：core 只接受同一 major 嘅 IR schema；唔同 major 即 compile error。 | 已確認（產品經理 2026-10-17） | 4、9 |
| 163 | Builtin 同 registry 並存（P7）：business pattern 拆出之後，builtin 版本保留一個 minor release 再移除；同時存在時一樣要用 `use:` 明確揀（決定 154）。 | 已確認（產品經理 2026-10-17） | 5、9 |
| 164 | 項目層 pin 宣告（P8）：因為 lock 唔 commit（決定 117），pin 唔可以只放 lock。會 commit 嘅 `patterns.md` frontmatter `patterns:` 記錄每個 registry pattern 嘅 `id@確實版本` 同 `sha256`；build 只解析呢度宣告嘅版本，lock 照記錄，build 檢查兩者一致，唔一致即 fail closed。 | 草稿，待產品經理確認 | 5.1、9 |
| 165 | Endpoint `egress:`（允許 host）同 `secrets:`（名）frontmatter 欄位（P9），畀決定 153 子集規則對應；唔影響 v1。 | 草稿，待產品經理確認 | 3.1、9 |
| 166 | Business pattern IR 嘅 core 強制規則：core 對所有 pattern IR 一律強制 RLS 同 `owner-key`（決定 70、114）；pattern 唔可以揀或者切換 DB role（由 executor 決定）；secret 只可以送去 `egress:` 宣告嘅 host；違反即 compile error。 | 已確認（架構師決定 2026-10-17） | 4.1 |
| 167 | `use:` 放 endpoint frontmatter（結構化，同 #92 frontmatter 優先一致），唔寫喺 prose；唔計入安全關鍵字檢查。 | 已確認（架構師決定 2026-10-17） | 5 |

## 1. 背景

- Security pattern 定義安全邊界，一定要同 SDK core 一齊 release、一齊 review（決定 113、114、119 嘅來源同部署閘門照舊）。
- Business pattern 變得快，唔定義安全邊界；同 core 綁埋一齊 release 會拖慢兩邊。v1.1 拆出去，core 照舊管安全。
- v1：全部 pattern 內置（`source: builtin`），但內部一律經 §3、§4 嘅介面寫，等 v1.1 只係改 business pattern 嘅來源。

## 2. 範圍

- 喺範圍：pattern manifest、Rust trait、揀 pattern 流程、來源驗證、lock 欄位、v1 遷移次序。
- Pattern 輸出嘅 IR 唔涉及 transport 或者 encoding（同 v2 REST＋MessagePack／CBOR 方向無關）。
- 唔喺範圍：registry 嘅營運細節、第三方 security pattern（決定 153 禁止）、runtime plugin（決定 152 禁止）。

## 3. Pattern manifest（決定 151、153）

| 欄位 | 類型 | 必填 | 說明 |
| --- | --- | --- | --- |
| `id` | string | 是 | 例如 `crud-offset-pagination`；`[a-z0-9-]`，全域唯一 |
| `version` | semver | 是 | 例如 `1.2.0` |
| `kind` | `security` \| `business` | 是 | `security` 一定 `source: builtin`（決定 153） |
| `provides` | capability 列表 | 是 | 例如 `[crud.list, pagination.offset]` |
| `requires.db` | 權限列表 | 否 | 例如 `[SELECT]`；一定要係 endpoint 已宣告嘅子集 |
| `requires.network` | host 白名單 | 否 | 預設 `none`；有值都一定要係 endpoint 宣告嘅子集 |
| `requires.secrets` | secret 名列表 | 否 | 只寫名，唔寫值 |
| `requires.pipelines` | security pipeline 列表 | 否 | 例如 `[auth, rate-limit]` |
| `ir-schema` | semver range | 是 | Pattern 輸出嘅 IR schema 版本範圍 |

- 子集規則（決定 153，MUST）：`requires` 每一項都一定要喺 endpoint lock 已經宣告（決定 112、114 嘅 frontmatter 同 policy 檢查）；超出即 compile error，唔會自動加權限。
- 唔認得嘅欄位係 error（fail closed）。

### 3.1 子集規則對應嘅 endpoint 宣告（決定 153、165）

| `requires` | 對應 endpoint 宣告 | 狀態 |
| --- | --- | --- |
| `requires.db` | v1.8 現有：source frontmatter `writes`、`public`、`public-writes`、`auth`、`provider`（決定 112、#92）；lock `## Access`（`public`、`public-writes`、`tenant`）、`## Limits` 嘅 `provider`；method lock `## Signature` 嘅 `writes`；model lock `## Policies` 嘅 `grants` 同 `owner-key`（決定 70、114）。Pattern 要 `INSERT`／`UPDATE` 而 endpoint 冇 `writes: true`，或者要嘅 table／欄位唔喺 model grants，即 compile error | v1.8 已有 |
| `requires.network` | 新加 endpoint frontmatter `egress: [host, …]`（完全相等 host，唔准 wildcard，預設冇）；同現有 `external-calls`（7.1）一致：`egress` 非空就一定要 `external-calls: allow` | 草稿（P9） |
| `requires.secrets` | 新加 endpoint frontmatter `secrets: [NAME, …]`（只寫名）；一定要等於呢個 endpoint 經 method 用到嘅 `key-ref` 同 method frontmatter `secrets`（#92）嘅聯集 | 草稿（P9） |
| `requires.pipelines` | lock `## Pipelines` | v1.8 已有 |

- `egress:` 同 `secrets:` 屬 v1.1 草稿，唔影響 v1；v1 冇呢兩個欄位，`requires.network`／`requires.secrets` 一律當空（只准冇對外網絡、冇 secret 嘅 pattern）。

## 4. Rust trait（決定 152）

```rust
pub trait Pattern {
    fn manifest(&self) -> &PatternManifest;
    /// Pure: inspects the endpoint AST only; no I/O.
    fn matches(&self, endpoint: &EndpointAst) -> bool;
    /// Pure: returns an IR fragment; no I/O, no runtime code.
    fn plan(&self, endpoint: &EndpointAst, ctx: &PlanCtx) -> Result<IrFragment, PlanError>;
}
```

- 一定要（MUST）：pattern 只輸出 IR；runtime 只執行 core 已有嘅 IR op，pattern 唔可以帶任何喺 runtime 行嘅 code、script 或者 binary。
- `matches` 同 `plan` 一定要係 deterministic、冇 I/O；`PlanCtx` 只畀讀已宣告嘅資料。
- IR schema（決定 162，MUST）：core 只接受同一 major；唔同 major 即 compile error。
- Core 驗證每個 IR fragment：只准已知 op；SQL 照舊經 sqlguard 同 policy 檢查（決定 98、114）；權限唔可以超出 `requires`。
- Registry package 只係 data（manifest 加 IR template），由 core 內置嘅 interpreter 執行 `plan`；唔載入外來 Rust code。

### 4.1 Core 強制規則（決定 166）

CRUD、upload、email 照舊屬 business pattern，但 core 對所有 pattern IR 一律：

- 一定要（MUST）強制 RLS 同 `owner-key`（決定 70、114）：IR 嘅 SQL 一定經 RLS policy 執行；有 `owner-key` 嘅 model，非 admin 只可以讀寫自己嘅列；pattern 唔可以放寬，放寬只可以喺 source 明寫（決定 70）。
- 一定要（MUST）由 executor 決定 DB role：pattern IR 唔可以指定、揀或者切換 DB role、`scope` 或者連線池（決定 97、129）；IR 帶住呢類欄位即 compile error。
- 一定要（MUST）限制 secret 去向：secret 只可以送去 endpoint `egress:` 宣告嘅 host；送去其他 host、寫入 response、log 或者 DB，即 compile error。
- 違反任何一條即 compile error，錯誤訊息講明 pattern `id@version`、IR 位置同違反嘅規則。
- 必須有嘅測試：(1) pattern IR 嘗試去掉 owner-key 條件或者繞過 RLS → compile error；(2) IR 指定 DB role 或者 `scope` → compile error；(3) secret 送去未宣告 host → compile error；(4) 合規嘅 CRUD／upload／email IR 通過，而且執行時用 executor 揀嘅 role。

## 5. 揀 pattern 流程（決定 153、154）

1. Markdown → AST。
2. AST → 需要嘅 capability。
3. 搵出 `provides` 覆蓋嗰啲 capability 嘅候選 pattern。
4. Security pattern 一定套用（例如有 `auth` 就一定用內置 auth），唔可以由 business pattern 取代或者略過。
5. 每個 capability：冇候選 → compile error；一個 → 用佢；多過一個 business 候選 → compile error（MUST），一定要喺 source 用 `use: <id>@<range>` 明確揀。冇隱含評分、冇「最新優先」。
6. 版本解析跟 5.1：build 只用 `patterns.md` pin 咗嘅確實版本，唔喺 build 時解析 range。
   - `use:`（決定 167）係 endpoint frontmatter 嘅結構化欄位，例如 `use: [crud-offset-pagination@^1.2]`；prose 入面寫 `use:` 唔生效（出 warning）；`use:` 唔計入安全關鍵字檢查，亦唔受 prose 否定規則影響。`use:` 嘅 range 一定要包含 `patterns.md` pin 嘅版本，否則 compile error。
7. Builtin 同 registry 並存（決定 163，MUST）：拆出之後 builtin 保留一個 minor release 再移除；並存時照第 5 步要 `use:`。
8. 錯誤訊息一定要列出 capability、所有候選 `id@version`，同修正方法（加 `use:`）。

### 5.1 項目層 pin（決定 164，草稿，待產品經理確認）

- 項目根目錄 `patterns.md`（會 commit）用 frontmatter 記錄：

```yaml
patterns:
  - { id: crud-offset-pagination, version: 1.2.3, sha256: "sha256:d3a8...57" }
```

- Build 只解析呢度宣告嘅 `id@version`；未宣告嘅 registry pattern 即 compile error。Builtin pattern 唔使 pin（跟 core 版本）。
- Lock 照樣記錄 `patterns[]`；build 檢查 lock、pin 同下載 artifact 嘅 sha256 三者一致，唔一致即 fail closed。
- 升級：用 `vibe patterns update <id>`（草稿）改 `patterns.md`，經 PR review merge。
- 選項：A（建議）commit `patterns.md` pin；B `use:` 一定寫確實版本，唔准 range；C 只喺決定 118 release build 解析一次。

## 6. 來源驗證（決定 155）

- 只適用於 registry package；`source: builtin` 由 core binary 本身嘅來源驗證（決定 113、119）覆蓋。
- Signer 身份（MUST）：Fulcio 證書嘅 issuer 一定等於 `https://token.actions.githubusercontent.com`，SAN 一定完全等於官方 repo 嘅 release workflow，ref 一定係 `refs/tags/v*`（決定 158；例如 `https://github.com/vibe-backend/patterns/.github/workflows/release.yml@refs/tags/v1.2.3`，repo 名只係示意）；唔接受 wildcard org、其他 repo、其他 workflow 或者 branch ref。
- Trust root（決定 159，MUST）：跟 core release 更新，規則同決定 119。
- 冇網絡（決定 160，MUST）：本地 cache／mirror 入面嘅 package 照樣做本節全部驗證，亦一定要通過 6.1 撤回清單檢查；cache 唔係信任來源。
- 離線驗證（MUST）：每個 package 帶 Sigstore bundle（證書、簽名、Rekor inclusion proof、SET）；用 cache 嘅 trust root（同決定 119 嘅 `trusted_root.json` 規則）驗證，唔使連 Rekor 或者 Fulcio。Sigstore／Rekor 用唔到都照樣可以 compile 同部署。
- 驗證失敗（簽名、身份、inclusion proof、digest 唔對、bundle 缺失）一律 fail closed，compile 失敗，冇 fallback。
- `vibe serve`（MUST）：cold start 唔下載任何嘢；只執行 artifact 入面已經 compile 好嘅 IR。Pattern 只喺 `vibe compile` 階段取得。
- Artifact sha256 一定等於 lock 記錄；唔等即 fail closed。

### 6.1 撤回清單（決定 161）

- 簽名（MUST）：撤回清單用同決定 158 一樣嘅 pinned signer 身份（issuer + SAN）簽，用 Sigstore bundle 離線驗證；驗證失敗即拒絕呢份清單。
- 內容：`version`（單調遞增整數）、`expires-at`、撤回咗嘅 `id@version` 列表同原因。
- 防回滾（MUST）：compile 記住本地 cache 已驗證嘅最高 `version`；收到 `version` 更細嘅清單一律拒絕，照用 cache 嗰份。
- 到期（MUST）：清單過咗 `expires-at`，compile 警告，但照用本地最新一份已驗證嘅清單；唔會因為離線或者攞唔到新清單就全部失敗。完全冇任何已驗證清單（例如第一次用、冇 cache）嘅處理：**待產品經理確認**。選項：A compile 失敗（fail closed）；B 用 core release 內嵌嘅一份已簽清單做初始值。建議：B 做初始值，冇內嵌清單時先用 A。
- 套用：lock 解析出嚟嘅任何 `id@version` 喺清單入面即 compile error，錯誤訊息列出原因同建議版本。本地 cache／mirror 都一樣檢查（決定 160）。
- 必須有嘅測試：(1) 回滾：較舊 `version` 嘅已簽清單被拒絕，照用 cache 版本；(2) 到期：過期清單發警告但撤回照樣生效；(3) 簽名身份唔對嘅清單被拒絕；(4) 被撤回嘅版本 compile 失敗；(5) 離線 mirror 同樣套用撤回清單。

## 7. Lock 欄位（決定 156）

- 頂層：`core: { version, hash }`、`compiler: { version }`、`ir-schema: <version>`。
- `patterns[]`：`id`、`version`、`source`（`builtin` 或者 registry URL）、`sha256`（artifact）、`sigstore`（`issuer`、`san`、`rekor-log-index`、`integrated-time`；`builtin` 冇呢欄）。
- 每個 endpoint：`patterns: { <capability>: <id>@<version> }`。
- v1：全部 `source: builtin`，`sha256` 係 core 內嵌內容嘅 hash。
- 決定 117 照舊：lock 係 build 輸出，唔 commit。

## 8. v1 遷移次序（決定 157）

- 一定要（MUST）等 #147、#95 同決定 149 實作 merge 之後先開始。
- 只可以加（additive）：現有 endpoint compile 出嚟嘅 IR 同行為一定唔變；lock 只係加欄位。
- 放喺 GA 計劃最後一項。
- 必須有嘅測試：重構前後所有現有範例 compile 出嚟嘅 IR 完全相同（golden test）；lock 新欄位齊全；多重符合同 `requires` 超出都係 compile error。

## 9. 產品經理決定（2026-10-17 10:08 HKT）

§9 原本嘅待確認問題已全部確認，寫入決定 158–163：

| # | 問題 | 決定 |
| --- | --- | --- |
| P1 | Registry 發布者 | 只限一個官方 repo＋workflow；第三方留 vNext（決定 158） |
| P2 | SAN ref | 只接受 `refs/tags/v*`（決定 158） |
| P3 | Trust root 更新 | 跟 core release，同決定 119（決定 159） |
| P4 | 冇網絡 compile | 本地 cache／mirror，照樣驗證，亦要過撤回清單檢查（決定 160） |
| P5 | 撤回版本 | 已簽、有版本號同到期時間嘅撤回清單；防回滾；到期警告但照用（決定 161，6.1） |
| P6 | IR schema 相容 | 只接受同一 major（決定 162） |
| P7 | Builtin 同 registry 並存 | Builtin 保留一個 minor release（決定 163） |

### 9.1 待產品經理確認

| # | 問題 | 選項 | 建議 |
| --- | --- | --- | --- |
| P8 | Pattern 版本 pin 放邊（決定 164） | A：commit `patterns.md`（`id@確實版本`＋`sha256`），lock 記錄並檢查一致；B：`use:` 只准確實版本；C：只喺 release build 解析一次 | A |
| P9 | Endpoint `egress:`／`secrets:` 欄位（決定 165） | A：加入 endpoint frontmatter（v1.1 草稿）；B：只用現有 method 層 `secrets` 同 `external-calls`，network 一律禁止 | A |
| — | 冇任何已驗證撤回清單（6.1） | A：fail closed；B：用 core 內嵌已簽清單做初始值 | B，冇內嵌清單時用 A |

## 10. 主要風險同緩解

| 風險 | 緩解 |
| --- | --- |
| 供應鏈（package 被換、簽錯身份、舊版本被重新用） | 決定 155、158、161：pin issuer + SAN、Rekor inclusion、sha256 寫入 lock、fail closed；security pattern 只限 builtin（決定 153） |
| Capability／權限升級 | `requires` 一定係 endpoint 宣告嘅子集（3.1）；core 強制 RLS／owner-key、executor 揀 role、secret 只去 `egress:`（決定 166）；network 預設冇；IR 只准已知 op，SQL 照舊經 sqlguard 同 policy 檢查 |
| IR 版本不一致 | Lock 記錄 `ir-schema`；manifest 宣告 range；唔相容即 compile error |
| Build 可用性（Sigstore／registry 用唔到） | 離線 bundle + cache trust root；本地 cache／mirror（P4）；`vibe serve` 唔下載 |
| 揀 pattern 唔 deterministic | 多重符合即 error、frontmatter `use:`、`patterns.md` pin（決定 164）、`matches`／`plan` 冇 I/O |

## 11. 範例

`api/products/v1/list/list.md`

```markdown
---
public: false
auth: jwt
writes: false
use: [crud-offset-pagination@^1.2]
---
# List products
GET，path /products/v1。要 JWT。
列出產品，每頁 20，最多 100，按 created_at 排序。
```

`patterns.md`（項目根目錄，會 commit，決定 164 草稿）

```markdown
---
patterns:
  - { id: crud-offset-pagination, version: 1.2.3, sha256: "sha256:d3a8...57" }
---
# Pattern pins
```

Lock 片段（`list.lock.md`）：

```markdown
## Build
- core: { version: 1.1.0, hash: sha256:7c2e...91 }
- compiler: { version: 2.1.0 }
- ir-schema: 1.0

## Patterns
| id | version | source | sha256 | sigstore |
| --- | --- | --- | --- | --- |
| auth | 1.1.0 | builtin | sha256:4f10...aa | (none) |
| rate-limit | 1.1.0 | builtin | sha256:91bd...0c | (none) |
| crud-offset-pagination | 1.2.3 | https://patterns.example.invalid/crud-offset-pagination | sha256:d3a8...57 | { issuer: https://token.actions.githubusercontent.com, san: https://github.com/vibe-backend/patterns/.github/workflows/release.yml@refs/tags/v1.2.3, rekor-log-index: 123456789, integrated-time: 2026-11-02T03:14:00Z } |

## Endpoint patterns
- auth.jwt: auth@1.1.0
- rate-limit.route: rate-limit@1.1.0
- crud.list: crud-offset-pagination@1.2.3
- pagination.offset: crud-offset-pagination@1.2.3
```

Registry URL 同 SAN 只係示意：格式跟決定 158（一個官方 repo＋release workflow、`refs/tags/v*`），repo 名未定；registry host 用保留 domain `example.invalid`。
