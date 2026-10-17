# Best-practice pattern library：Spec v1.1（草稿）

<!-- spec-status: draft -->

> 狀態：草稿，未 review。基準係 vibe-backend/sdk main `9674786` 嘅 [`spec-v1.8.md`](spec-v1.8.md)。ADR 見 [`adr/0001-pattern-library.md`](adr/0001-pattern-library.md)。產品經理硬性規則（2026-10-17 09:59 HKT）寫成「一定要（MUST）」；§9 列出仲要產品經理確認嘅部分。只描述防禦設計。

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

## 1. 背景

- Security pattern 定義安全邊界，一定要同 SDK core 一齊 release、一齊 review（決定 113、114、119 嘅來源同部署閘門照舊）。
- Business pattern 變得快，唔定義安全邊界；同 core 綁埋一齊 release 會拖慢兩邊。v1.1 拆出去，core 照舊管安全。
- v1：全部 pattern 內置（`source: builtin`），但內部一律經 §3、§4 嘅介面寫，等 v1.1 只係改 business pattern 嘅來源。

## 2. 範圍

- 喺範圍：pattern manifest、Rust trait、揀 pattern 流程、來源驗證、lock 欄位、v1 遷移次序。
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
- Core 驗證每個 IR fragment：只准已知 op；SQL 照舊經 sqlguard 同 policy 檢查（決定 98、114）；權限唔可以超出 `requires`。
- Registry package 只係 data（manifest 加 IR template），由 core 內置嘅 interpreter 執行 `plan`；唔載入外來 Rust code。

## 5. 揀 pattern 流程（決定 153、154）

1. Markdown → AST。
2. AST → 需要嘅 capability。
3. 搵出 `provides` 覆蓋嗰啲 capability 嘅候選 pattern。
4. Security pattern 一定套用（例如有 `auth` 就一定用內置 auth），唔可以由 business pattern 取代或者略過。
5. 每個 capability：冇候選 → compile error；一個 → 用佢；多過一個 business 候選 → compile error（MUST），一定要喺 source 用 `use: <id>@<range>` 明確揀。冇隱含評分、冇「最新優先」。
6. `use:` 嘅 range 揀合符範圍嘅最高版本，然後寫入 lock 固定；之後 build 用 lock 嘅版本，唔重新解析。
7. 錯誤訊息一定要列出 capability、所有候選 `id@version`，同修正方法（加 `use:`）。

## 6. 來源驗證（決定 155）

- 只適用於 registry package；`source: builtin` 由 core binary 本身嘅來源驗證（決定 113、119）覆蓋。
- Signer 身份（MUST）：Fulcio 證書嘅 issuer 一定等於 `https://token.actions.githubusercontent.com`，SAN 一定完全等於一個固定 repo 同 workflow（例如 `https://github.com/vibe-backend/patterns/.github/workflows/release.yml@refs/tags/v*` 嘅確實格式待 §9 確認）；唔接受 wildcard org 或者其他 workflow。
- 離線驗證（MUST）：每個 package 帶 Sigstore bundle（證書、簽名、Rekor inclusion proof、SET）；用 cache 嘅 trust root（同決定 119 嘅 `trusted_root.json` 規則）驗證，唔使連 Rekor 或者 Fulcio。Sigstore／Rekor 用唔到都照樣可以 compile 同部署。
- 驗證失敗（簽名、身份、inclusion proof、digest 唔對、bundle 缺失）一律 fail closed，compile 失敗，冇 fallback。
- `vibe serve`（MUST）：cold start 唔下載任何嘢；只執行 artifact 入面已經 compile 好嘅 IR。Pattern 只喺 `vibe compile` 階段取得。
- Artifact sha256 一定等於 lock 記錄；唔等即 fail closed。

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

## 9. 待產品經理確認

| # | 問題 | 選項 | 建議 |
| --- | --- | --- | --- |
| P1 | Registry 喺邊同邊個可以發布 | A：只限官方 org 一個 repo＋workflow；B：容許第三方 business pattern（各自 pin 身份） | A（v1.1 只官方；第三方留 vNext） |
| P2 | 確實 SAN 格式（tag 定 branch ref） | A：只接受 `refs/tags/v*` release tag；B：容許 `refs/heads/main` | A |
| P3 | Cache 嘅 trust root 幾耐要更新、過期點算 | A：同決定 119 一樣跟 core release 更新；B：獨立 TUF 更新 | A |
| P4 | Compile 冇網絡（air-gapped CI） | A：預先填好嘅本地 cache／mirror，照樣驗證；B：唔支援 | A |
| P5 | 撤回（yank）有問題嘅 pattern 版本 | A：registry 發撤回清單，compile 拒絕；B：只靠新版本 | A，撤回清單一樣要簽 |
| P6 | IR schema 相容範圍 | A：core 只接受同一 major；B：支援前一個 major 一段時間 | A |
| P7 | 同名 builtin 同 registry 版本並存 | A：business pattern 拆出之後，builtin 版本保留一個 minor 再移除；B：即刻移除 | A |

## 10. 主要風險同緩解

| 風險 | 緩解 |
| --- | --- |
| 供應鏈（package 被換、簽錯身份） | 決定 155：pin issuer + SAN、Rekor inclusion、sha256 寫入 lock、fail closed；security pattern 只限 builtin（決定 153） |
| Capability／權限升級 | `requires` 一定係 endpoint 宣告嘅子集；network 預設冇；IR 只准已知 op，SQL 照舊經 sqlguard 同 policy 檢查 |
| IR 版本不一致 | Lock 記錄 `ir-schema`；manifest 宣告 range；唔相容即 compile error |
| Build 可用性（Sigstore／registry 用唔到） | 離線 bundle + cache trust root；本地 cache／mirror（P4）；`vibe serve` 唔下載 |
| 揀 pattern 唔 deterministic | 多重符合即 error、明確 `use:`、版本寫入 lock、`matches`／`plan` 冇 I/O |

## 11. 範例

`api/products/v1/list/list.md`

```markdown
# List products
GET，path /products/v1。要 JWT。
列出產品，每頁 20，最多 100，按 created_at 排序。
use: crud-offset-pagination@^1.2
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
| crud-offset-pagination | 1.2.3 | https://patterns.vibe-backend.dev/crud-offset-pagination | sha256:d3a8...57 | { issuer: https://token.actions.githubusercontent.com, san: https://github.com/vibe-backend/patterns/.github/workflows/release.yml@refs/tags/v1.2.3, rekor-log-index: 123456789, integrated-time: 2026-11-02T03:14:00Z } |

## Endpoint patterns
- auth.jwt: auth@1.1.0
- rate-limit.route: rate-limit@1.1.0
- crud.list: crud-offset-pagination@1.2.3
- pagination.offset: crud-offset-pagination@1.2.3
```

Registry URL 同 SAN 只係示意，確實值待 P1、P2。
