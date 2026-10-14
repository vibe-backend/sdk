# PR review 指引（草稿，配合 spec v1.8 決定 29、37、83、89、96）

v1.8 起，`vibe` SDK 唔再要求喺 lock 入面記錄人手確認（見 spec 9C）。人手 review 改由 Git 平台負責；以下係建議設定（以 GitHub rulesets 為例；`vibe build --release-approval` 只認 ruleset，唔認舊式 branch protection）。標「計劃中」嘅指令係 v1.8 設計，未實作。

## 1. Ruleset（`main`）

- 用 repository ruleset（target：`main`，enforcement：Active）設定以下規則，唔好用舊式 branch protection。
- 開 **Require a pull request before merging**，唔准直接 push 去 `main`。
- **Required approvals**：最少 1 個。
- 開 **Dismiss stale pull request approvals when new commits are pushed**（取代 v1.7 嘅「確認綁定 commit hash」）。
- 開 **Require approval of the most recent reviewable push**，最後 push 嘅人唔可以自己批准。
- 開 **Require review from Code Owners**。
- **Bypass list**：`main` ruleset 可以保留 OrganizationAdmin bypass（日常 `--admin` merge 唔變；spec 決定 96 修訂，待產品經理確認）。SDK 唔會檢查 `main` 嘅 bypass；`--release-approval` 實作之後，高風險 release 先會獨立驗證 PR review 同 tree 相等，令 admin bypass merge 出唔到高風險 release（`--release-approval` 未實作，實作之前唔成立，今日冇保護）。
- 以上 required review 同 up-to-date 設定照樣一定要開。

## 1A. Release tag ruleset（`v*`，spec 決定 96）

- 專用 **tag ruleset**（target：Tag，enforcement：Active），target `refs/tags/v*`（UI 入面 include pattern `v*`）。
- 只開 **Restrict updates**、**Restrict deletions**、**Block force pushes**；**唔開 Restrict creations**（產品經理代 Jack 決定採用 (i)）。任何有 write 權限嘅人都可以開 `v*` tag，開咗就改唔到、刪唔到。
- **Bypass list 一定要留空**，admin 都唔例外；唔設白名單。高風險 release build 會讀呢個 ruleset，讀唔到、唔係空、唔係 Active、缺少上面三條規則、或者冇 target 到今次嘅 tag 就失敗。
- Release 只可以由 `v*` tag 出，tag 一定要指向 `main` 上嘅 commit。
- 把關：開 tag 唔會自動發佈；發佈 job 要經 `release` environment 批准（live reviewer：`j113203`，`prevent_self_review: false`）。`--release-approval` 係計劃中、**未實作**，今日唔提供任何保護。
- **剩餘風險（Jack 已接受）**：所有 agent 都用 `j113203` push，任何 `j113203` agent 都可以開 `v*` tag **再自己批准 release**。
- 以上係**臨時方案 (i)**。目標方案 (iii)（未生效）：專用 release GitHub App 係唯一開 `v*` tag 嘅 actor，同時開返 Prevent self-review。
- Live `release` environment `can_admins_bypass` 而家係 `true`，要關閉（Jack 負責）。
- 讀 ruleset 嘅專用 GitHub App：只裝喺呢個 repo，喺 `release` environment job 入面 mint installation token：
  - 權限只畀 repository **Administration: read** 同 **Metadata: read**；**唔畀 Administration: write**。
  - GitHub 只會將 `bypass_actors` 回畀對 ruleset 有寫權限嘅 caller；如果 read 睇唔到，高風險 release build 會失敗，要另外搵產品經理批准，唔好自己加權限。
  - App private key 放 `release` environment secret；mint token 時收窄到呢個 repo；token 唔好印出嚟。
  - 冇呢個 App 嘅話，高風險 release build 會失敗；普通 build 只記錄。

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
- 計劃中：`vibe build` release 模式會檢查 CODEOWNERS 覆蓋晒以上路徑，冇就失敗（決定 93）。
- Lock 係 build 輸出，**唔 commit**（`*.lock.md` 加入 `.gitignore`，spec 決定 117）。Review 重點係 source 嘅 frontmatter 高風險宣告（`writes`、`public`、`auth`、`secrets`，決定 112）；產生嘅 SQL 由決定 114 policy 檢查，release summary 列出變化。

## 3. Required status checks

- 而家已經有：`vibe secret-scan --fail-on-find`。
- `vibe compile --check`：喺 CI 由 source build 一次，驗證結果同決定 114 policy（唔同 commit 咗嘅 lock 比較，因為 lock 唔 commit；spec 決定 117）。
- 而家已經有：`vibe validate`。
- `main` 一定要用 **ruleset**（唔好用舊式 branch protection）開 pull request rule 嘅 **Require approval of the most recent reviewable push**：`vibe build --release-approval` 會用 `GET /repos/{owner}/{repo}/rules/branches/main` 確認，冇就失敗。舊式 branch protection 要 admin 權限先讀到，所以唔接受（spec 2C.3a）。
- 唔好用 merge queue：高風險 release build 唔支援（spec 2C.3a）。
- 一定要喺 ruleset 嘅 **Require status checks to pass** 入面開 **Require branches to be up to date before merging**（`strict_required_status_checks_policy`，最少要有一個 required status check 先生效）：`vibe build --release-approval` 會比較 build commit 同 PR head 嘅 tree，冇開呢個設定就可能唔一樣，release build 會失敗（spec 2C.3a）。

## 4. 高風險 label 要兩個 approval

GitHub ruleset 冇辦法按 label 要求 approval 數量，所以要用一個自訂 required status check：

1. CI 按佢由 source build 出嚟嘅 lock 嘅 `security-items` 自動加 `security:high` 或者 `security:medium` label（計劃中：`vibe build --labels` 輸出建議 label）。
2. 另一個 workflow（例如 `high-risk-approvals`）喺 `pull_request`、`pull_request_review` 同 `labeled` 事件觸發，讀 PR 嘅 label 同 review；有 `security:high` 但少過 2 個非作者、針對最新 commit 嘅 approval，就 fail。
3. 將 `high-risk-approvals` 設為 required status check。

高風險項目包括：`public` 變 true、剔走 `auth`、public 寫入、`sign-token`、`set-password`、`confirm-verification`、角色改動、provider 改動、對外 `call`、migration。

## 5. Fork PR 同 token 權限

- Workflow 預設 `permissions: contents: read`；fork 嚟嘅 PR 用唯讀 `GITHUB_TOKEN`，攞唔到 secret。
- 唔好用 `pull_request_target` 執行 PR 入面嘅 code。
- Fork PR 唔跑需要 `LLM_API_KEY` 嘅重編步驟（spec 2A.8.8 已經跳過）。

## 6. Release environment 同 KMS（spec 決定 89、92）

- 開一個 `release` environment，設 **Required reviewers**；**Deployment branches and tags** 揀 Selected branches and tags，只加 tag 規則 `v*`（唔加任何 branch），即係只准受 release ruleset 保護嘅 `v*` tag 部署。
- `release` environment **Required reviewers** 係 `j113203`（Jack）；**Prevent self-review 關閉**（spec 決定 96）。所有 agent 都用 `j113203` push，所以 agent 觸發嘅 release 可以由同一個帳戶自己批准；呢個風險 Jack 已經接受。`--release-approval` 未實作；實作後 `j113203` 觸發 run 嘅 env 批准唔計入高風險 2 人。
- `release` environment **唔好開 Allow administrators to bypass configured protection rules**，否則 admin 可以跳過 required reviewers。
- 改 CODEOWNERS（`.github/`、root、`docs/` 任何一個）一律當高風險，要 2 個批准。
- CODEOWNERS 用 team（`@org/team`）嘅話，release workflow 要用 GitHub App installation token（organization Members: read，其他只讀），`GITHUB_TOKEN` 查唔到 team 成員；冇就失敗。
- Machine user（普通帳戶做自動化）SDK 分唔到係唔係真人，唔好畀佢哋 write 權限或者 CODEOWNERS 身份。
- 高風險 release 要用 `vibe build --release-approval pr:<number>`、`env:<run_id>` 或者 `pr:<number>,env:<run_id>`（計劃中）。Build 會經 GitHub REST API 驗證：build commit 屬於唯一一個 merged PR、批准喺 PR 最後 head SHA 上而且 tree 一樣、批准人有 write 權限、唔係 PR 任何 commit 嘅 author／committer／co-author、唔係 PR 作者或者 env run 觸發者、唔係 bot、高風險路徑要係 CODEOWNER（由 PR base 讀），同埋有 2 個唔同批准人（PR review 同 environment approval 合併計）。
- Release workflow 嘅 token 權限：

  ```yaml
  permissions:
    contents: read
    pull-requests: read
    actions: read
    id-token: write   # 只畀 OIDC 換 KMS 用
  ```

- 任何 API 錯誤、404 或者 rate limit，release build 都會失敗（fail closed），唔好加 retry 去繞過。
- Release manifest 簽名 key 放喺 KMS，用 GitHub OIDC 綁定 `release` environment 同 ref 先可以用；唔好將 key 放做 repository secret。
- CI release build 攞唔到 KMS key 就失敗，唔可以 fallback 去 dev key。
- 詳細加固步驟見 issue #28。

## 7. 選用：signed commits

- 可以開 **Require signed commits**，減低帳戶被盜之後偽造 commit 嘅風險。唔係必須。

## 8. 細團隊

- 只有一個 maintainer 嘅 project：最少保留 CODEOWNERS、required status checks 同 `release` environment；高風險 PR 可以設定 merge 前等 24 小時，或者請外部 reviewer。
