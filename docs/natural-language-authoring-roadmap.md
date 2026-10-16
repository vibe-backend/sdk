# 自然語言 Backend Authoring：Release Roadmap

<!-- spec-status: proposal -->

本 roadmap 把「以極簡自然語言建立安全、高效能 backend」拆成可獨立審查、實作及發布的增量。詳細首增量見 [`natural-language-authoring-v0.1.md`](natural-language-authoring-v0.1.md)。

| Release | 可獨立交付的範圍 | 明確延後 |
| --- | --- | --- |
| 0.1 Plan / Preview / Apply | 一段自然語言 → strict declarative plan → deterministic validation/preview → 明確確認 → 原子寫入既有 markdown source；private/authenticated/owner-scoped/bounded defaults | deploy、public write、任意 SQL/code、secret 管理、複雜角色 |
| 0.2 Clarify / Edit | 最多三題安全澄清、多輪 plan diff、假設修改、跨語言 evaluation、離線安全 template | production migration 與 rollback |
| 0.3 Permissions / Secrets | role matrix、effective grants diff、secret reference 建立/輪替/撤銷、provider retention controls | secret 值進入 prompt/source |
| 0.4 Validate / Recover | migration dry-run、backup gate、rollback/roll-forward、crash journal UX、錯誤分類 | 高階 runtime performance tuning |
| 0.5 Abuse Hardening | injection/red-team corpus、project-content injection、provider compromise、fuzz budgets、privacy-aware audit | 以 LLM 信心分數作安全批准 |
| 0.6 Performance / SLO / Observability | latency/cost/query budgets、load tests、index evidence、metrics/traces/logs、SLO/error budget | 未量測的「高效能」宣稱 |
| 0.7 Compatibility / Release | plan/source/compiler matrix、deprecation、safe migration、provenance/SBOM、release channel、整合 `vibe build` | 不相容變更的靜默 fallback |
| 1.0 Stable Capability Gates | 經風險審查逐項開放 public write、outbound、jobs、進階 policy；穩定 CLI/SDK contract | 全能、無限制 code generation |

## Release 原則

每一個增量必須：

1. 有單一清楚的使用者成果及非目標；
2. 保持 runtime 不解讀自然語言；
3. 在 mutation／manifest 前 deterministic、fail-closed 驗證；
4. 附 API/CLI contract、negative tests、驗收證據及相容說明；
5. 不以互動確認取代 security review、CI 或 release approval；
6. 如改動重大產品邊界，先取得產品決策並記錄在該 release 規格。

## 與既有規格的關係

- `spec-v1.7.md` 維持現行定稿。
- `spec-v1.8.md` 仍是草稿；其 `vibe dev/build`、權限、租戶、效能與 release hardening 決定由相應後續 release 引用，不在 roadmap 文件複製。
- roadmap 狀態不代表任何功能已實作或可供 production 使用。
