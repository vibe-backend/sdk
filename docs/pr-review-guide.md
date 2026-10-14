# PR review 指引（草稿，配合 spec v1.8 決定 29、37、83、89）

v1.8 起，`vibe` SDK 唔再要求喺 lock 入面記錄人手確認（見 spec 9C）。人手 review 改由 Git 平台負責；以下係建議設定（以 GitHub 為例）。標「計劃中」嘅指令係 v1.8 設計，未實作。

## 1. Branch protection 或者 ruleset（`main`）

- 開 **Require a pull request before merging**，唔准直接 push 去 `main`。
- **Required approvals**：最少 1 個。
- 開 **Dismiss stale pull request approvals when new commits are pushed**（取代 v1.7 嘅「確認綁定 commit hash」）。
- 開 **Require approval of the most recent reviewable push**，最後 push 嘅人唔可以自己批准。
- 開 **Require review from Code Owners**。
- 開 **Do not allow bypassing the above settings**，admin 都唔例外。

## 2. CODEOWNERS（必須，spec 決定 29、37）

```text
/api/            @your-org/backend-owners
/models/         @your-org/backend-owners
/migrations/     @your-org/backend-owners @your-org/dba
/providers/      @your-org/backend-owners
/pipelines/      @your-org/backend-owners
**/*.lock.md     @your-org/backend-owners
/.compiler/      @your-org/platform-owners
/.github/        @your-org/platform-owners
/CODEOWNERS      @your-org/platform-owners
```

- CODEOWNERS 入面嘅用戶或者 team 一定要對 repo 有 **write** 權限，否則 GitHub 唔會當佢係 code owner，required review 會失效。
- `.compiler/injection-allowlist` 喺 `/.compiler/` 之下，一定受保護（決定 37）。
- 計劃中：`vibe build` release 模式會檢查 CODEOWNERS 覆蓋晒以上路徑，冇就失敗（決定 93）。
- 一定要將 lock 同 source 一齊 commit（決定 80），reviewer 先睇到 LLM 實際編譯咗乜。

## 3. Required status checks

- 而家已經有：`vibe secret-scan --fail-on-find`。
- 而家已經有：`vibe compile --check`（重編比對）同 `vibe validate`。
- 計劃中（v1.8 實作）：`vibe build --check-determinism`，取代上面兩個。
- 一定要開 **Require branches to be up to date before merging**：`vibe build --release-approval` 會比較 build commit 同 PR head 嘅 tree，冇開呢個設定就可能唔一樣，release build 會失敗（spec 2C.3a）。

## 4. 高風險 label 要兩個 approval

GitHub ruleset 冇辦法按 label 要求 approval 數量，所以要用一個自訂 required status check：

1. CI 按 lock 嘅 `security-items` 自動加 `security:high` 或者 `security:medium` label（計劃中：`vibe build --labels` 輸出建議 label）。
2. 另一個 workflow（例如 `high-risk-approvals`）喺 `pull_request`、`pull_request_review` 同 `labeled` 事件觸發，讀 PR 嘅 label 同 review；有 `security:high` 但少過 2 個非作者、針對最新 commit 嘅 approval，就 fail。
3. 將 `high-risk-approvals` 設為 required status check。

高風險項目包括：`public` 變 true、剔走 `auth`、public 寫入、`sign-token`、`set-password`、`confirm-verification`、角色改動、provider 改動、對外 `call`、migration。

## 5. Fork PR 同 token 權限

- Workflow 預設 `permissions: contents: read`；fork 嚟嘅 PR 用唯讀 `GITHUB_TOKEN`，攞唔到 secret。
- 唔好用 `pull_request_target` 執行 PR 入面嘅 code。
- Fork PR 唔跑需要 `LLM_API_KEY` 嘅重編步驟（spec 2A.8.8 已經跳過）。

## 6. Release environment 同 KMS（spec 決定 89、92）

- 開一個 `release` environment，設 **Required reviewers**，只准受保護 ref（`main`、受 ruleset 保護嘅 `v*` tag）部署。
- `release` environment 一定要開 **Prevent self-review**，觸發部署嘅人唔可以自己批准（spec 決定 90）。
- 高風險 release 要用 `vibe build --release-approval pr:<number>`、`env:<run_id>` 或者 `pr:<number>,env:<run_id>`（計劃中）。Build 會經 GitHub REST API 驗證：build commit 屬於唯一一個 merged PR、批准喺 PR 最後 head SHA 上而且 tree 一樣、批准人有 write 權限、唔係作者、唔係最後 push 嘅人、唔係 bot、高風險路徑要係 CODEOWNER，同埋有 2 個唔同批准人（PR review 同 environment approval 合併計）。
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
