# PR review 指引（草稿，配合 spec v1.8 決定 29、37、83、89、96）

v1.8 起，`vibe` SDK 唔再要求喺 lock 入面記錄人手確認（見 spec 9C）。人手 review 改由 Git 平台負責；以下係建議設定（以 GitHub rulesets 為例）；SDK 唔檢查呢啲設定。`--release-approval` 已撤回（產品經理 2026-10-16）。標 vNext 嘅指令唔喺 1.0（決定 164）。

## 1. Ruleset（`main`）

- 用 repository ruleset（target：`main`，enforcement：Active）設定以下規則，唔好用舊式 branch protection。
- 開 **Require a pull request before merging**，唔准直接 push 去 `main`。
- **Required approvals**：最少 1 個。
- 開 **Dismiss stale pull request approvals when new commits are pushed**（取代 v1.7 嘅「確認綁定 commit hash」）。
- 開 **Require approval of the most recent reviewable push**，最後 push 嘅人唔可以自己批准。
- 開 **Require review from Code Owners**。
- **Bypass list**：`main` ruleset 保留 OrganizationAdmin bypass（日常 `--admin` merge 唔變；spec 決定 96）。`bypass_actors` 唔係空係人手／文件層面已接受例外（Jack 2026-10-16，#70）：而家 SDK 同 release workflow 都唔讀 ruleset／`bypass_actors`（決定 90／#127）；如果將來 `release.yml` 加檢查，只可以係唯讀、資訊性、永遠唔會擋 release（同決定 90、第 2C.3a 對齊）；有第二個 reviewer 之後重新檢討。剩餘風險（Jack 已接受）：admin 可以 bypass `main` ruleset 保護（OrganizationAdmin bypass）；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118；同一個帳戶（`j113203`）可以自己批准；`release` environment admin bypass 保持開啟。
- 以上 required review 同 up-to-date 設定照樣一定要開。

## 1A. Release tag ruleset（`v*`，spec 決定 96）

- 專用 **tag ruleset**（target：Tag，enforcement：Active），target `refs/tags/v*`（UI 入面 include pattern `v*`）。
- 只開 **Restrict updates**、**Restrict deletions**、**Block force pushes**；**唔開 Restrict creations**（產品經理代 Jack 決定採用 (i)）。任何有 write 權限嘅人都可以開 `v*` tag，開咗就改唔到、刪唔到。
- **Bypass list 一定要留空**，admin 都唔例外；唔設白名單。呢個 ruleset 係營運設定，SDK 唔讀、唔檢查；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（spec 決定 96、2C.3a）。
- Release 只可以由 `v*` tag 出，tag 一定要指向 `main` 上嘅 commit。
- 把關：開 tag 唔會自動發佈；發佈 job 要經 `release` environment 批准（live reviewer：`j113203`，`prevent_self_review: false`）。`--release-approval` 已撤回（產品經理 2026-10-16），冇額外保護。
- **剩餘風險（Jack 已接受）**：所有 agent 都用 `j113203` push，任何 `j113203` agent 都可以開 `v*` tag **再自己批准 release**。
- 以上係**臨時方案 (i)**。目標方案 (iii)（未生效）：專用 release GitHub App 係唯一開 `v*` tag 嘅 actor，同時開返 Prevent self-review。
- `release` environment `can_admins_bypass: true` 保持開啟（Jack 決定保留，已接受風險：admin 可以跳過 `release` environment 嘅批准，即係跳過真正人手批准閘門）。
- 唔需要讀 ruleset 嘅 GitHub App：`--release-approval` 已撤回，SDK 唔讀 tag ruleset 或者 `bypass_actors`，冇 App 或者讀唔到都唔影響任何 build；ruleset 只係營運設定，唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（spec 決定 96、2C.3a）。

## 2. CODEOWNERS（必須，spec 決定 29、37）

```text
/api/            @your-org/backend-owners
/models/         @your-org/backend-owners
/migrations/     @your-org/backend-owners @your-org/dba
/providers/      @your-org/backend-owners
/pipelines/      @your-org/backend-owners
/.compiler/      @your-org/platform-owners
/.github/        @your-org/platform-owners
/CODEOWNERS      @your-org/platform-owners
```

- CODEOWNERS 入面嘅用戶或者 team 一定要對 repo 有 **write** 權限，否則 GitHub 唔會當佢係 code owner，required review 會失效。
- `.compiler/injection-allowlist` 喺 `/.compiler/` 之下，一定受保護（決定 37）。
- vNext（決定 164）：`vibe build` 計劃功能唔喺 1.0；CODEOWNERS 覆蓋由 Git 平台把關，SDK 唔檢查（決定 93、115）。
- Lock 係 build 輸出，**唔 commit**（`*.lock.md` 加入 `.gitignore`，spec 決定 117）。Review 重點係 source 嘅 frontmatter 高風險宣告（`writes`、`public`、`auth`、`secrets`，決定 112）；產生嘅 SQL 由決定 114 policy 檢查，release summary 列出變化。

## 3. Required status checks

- 而家已經有：`vibe secret-scan --fail-on-find`。
- `vibe build --check`：喺 CI 由 source build 一次，驗證結果同決定 114 policy（唔同 commit 咗嘅 lock 比較，因為 lock 唔 commit；spec 決定 117）。
- 而家已經有：`vibe build --check`。
- `main` 建議用 **ruleset**（唔好用舊式 branch protection）開 pull request rule 嘅 **Require approval of the most recent reviewable push**。
- Merge queue：SDK 冇限制（原本 `--release-approval` 嘅限制已撤回）。
- 建議喺 ruleset 嘅 **Require status checks to pass** 入面開 **Require branches to be up to date before merging**（`strict_required_status_checks_policy`，最少要有一個 required status check 先生效）。

## 4. 高風險 label 要兩個 approval（可選，source PR 合併層面）

呢個係 Git 平台層面嘅可選 PR 設定，同 release 無關；release 嘅真正批准係 `release` environment（spec 決定 118）。

GitHub ruleset 冇辦法按 label 要求 approval 數量，所以要用一個自訂 required status check：

1. CI 按佢由 source build 出嚟嘅 lock 嘅 `security-items` 自動加 `security:high` 或者 `security:medium` label（vNext（決定 164）：`vibe build --labels` 輸出建議 label）。
2. 另一個 workflow（例如 `high-risk-approvals`）喺 `pull_request`、`pull_request_review` 同 `labeled` 事件觸發，讀 PR 嘅 label 同 review；有 `security:high` 但少過 2 個非作者、針對最新 commit 嘅 approval，就 fail。
3. 將 `high-risk-approvals` 設為 required status check。

高風險項目包括：`public` 變 true、剔走 `auth`、public 寫入、`sign-token`、`set-password`、`confirm-verification`、角色改動、provider 改動、對外 `call`、migration。

## 5. Fork PR 同 token 權限

- Workflow 預設 `permissions: contents: read`；fork 嚟嘅 PR 用唯讀 `GITHUB_TOKEN`，攞唔到 secret。
- 唔好用 `pull_request_target` 執行 PR 入面嘅 code。
- Fork PR 唔跑需要 `LLM_API_KEY` 嘅重編步驟（spec 2A.8.8 已經跳過）。

## 6. Release environment 同 Sigstore 簽名（spec 決定 89、92、119）

- 開一個 `release` environment，設 **Required reviewers**；**Deployment branches and tags** 揀 Selected branches and tags，只加 tag 規則 `v*`（唔加任何 branch），即係只准受 release ruleset 保護嘅 `v*` tag 部署。
- `release` environment **Required reviewers** 係 `j113203`（Jack）；**Prevent self-review 關閉**（spec 決定 96）。所有 agent 都用 `j113203` push，所以 agent 觸發嘅 release 可以由同一個帳戶自己批准；呢個風險 Jack 已經接受。`--release-approval` 已撤回；唯一人手批准閘門係 Jack 批准 `release` environment；自動閘門仍然係決定 113／114／118（spec 決定 118）。
- `release` environment 嘅 **Allow administrators to bypass configured protection rules** 保持開啟（Jack 決定）。剩餘風險：admin 可以跳過 required reviewers；Jack 已接受。
- 改 CODEOWNERS（`.github/`、root、`docs/` 任何一個）一律當高風險；release 時真正批准係 `release` environment（Jack）。
- CODEOWNERS 用 team（`@org/team`）嘅話，release workflow 要用 GitHub App installation token（organization Members: read，其他只讀），`GITHUB_TOKEN` 查唔到 team 成員；冇就失敗。
- Machine user（普通帳戶做自動化）SDK 分唔到係唔係真人，唔好畀佢哋 write 權限或者 CODEOWNERS 身份。
- `--release-approval` 已撤回（產品經理 2026-10-16，spec 2C.3a）；高風險 release 冇額外 SDK 檢查。
- Release workflow 嘅 token 權限：

  ```yaml
  permissions:
    contents: read
    pull-requests: read
    actions: read
    id-token: write   # 只畀 Sigstore keyless 簽名同 attestation 用 GitHub OIDC
  ```

- 任何 API 錯誤、404 或者 rate limit，release build 都會失敗（fail closed），唔好加 retry 去繞過。
- Release manifest 用 Sigstore keyless 簽（spec 決定 119）：release workflow 喺 `release` environment 用 GitHub OIDC 身份簽，寫入 Rekor；冇長期 key。驗證時 pin issuer `https://token.actions.githubusercontent.com`，證書身份完全等於 `https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v<部署緊嘅版本>`（只接受完全相等；如果工具一定要 regex，只可以用完全錨定、正確轉義嘅式，例如 `^https://github\.com/vibe-backend/sdk/\.github/workflows/release\.yml@refs/tags/v1\.8\.0$`），加 commit SHA；離線驗證，trust root embed 入 binary（spec 2C.3b）。Keyless 身份只證明邊個 workflow 同 tag 產生 build，唔證明邊個批准。
- Environment secret（例如簽名 secret）只可以用喺 dev／test，永遠唔簽 release。
- Sigstore 簽名失敗就失敗，唔可以 fallback 去 dev key。
- 詳細加固步驟見 issue #28。

## 7. 選用：signed commits

- 可以開 **Require signed commits**，減低帳戶被盜之後偽造 commit 嘅風險。唔係必須。

## 8. 細團隊

- 只有一個 maintainer 嘅 project：最少保留 CODEOWNERS、required status checks 同 `release` environment；高風險 PR 可以設定 merge 前等 24 小時，或者請外部 reviewer。
