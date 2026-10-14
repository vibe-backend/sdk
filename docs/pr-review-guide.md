# PR review 指引（草稿，配合 spec v1.8 決定 83）

v1.8 起，`vibe` SDK 唔再要求人手確認 lock：通過驗證器同 CI 重編檢查嘅 LLM 編譯結果可以直接部署（見 spec 9C）。如果你想保留人手 review，請喺 Git 平台自己設定。以下係建議設定（以 GitHub 為例）。

## 1. Branch protection（`main`）

- 開 **Require a pull request before merging**，唔准直接 push 去 `main`。
- **Required approvals**：最少 1 個。
- 開 **Dismiss stale pull request approvals when new commits are pushed**（取代 v1.7 嘅「確認綁定 commit hash」）。
- 開 **Require review from Code Owners**。
- 開 **Do not allow bypassing the above settings**，admin 都唔例外。

## 2. CODEOWNERS

將以下路徑交俾負責安全嘅 owner：

```text
/api/            @your-org/backend-owners
/models/         @your-org/backend-owners
/migrations/     @your-org/backend-owners @your-org/dba
**/*.lock.md     @your-org/backend-owners
/providers/      @your-org/backend-owners
/.github/        @your-org/platform-owners
/.compiler/      @your-org/platform-owners
/CODEOWNERS      @your-org/platform-owners
```

- 一定要將 lock 同 source 一齊 commit（spec 決定 80），reviewer 先睇到 LLM 實際編譯咗乜。

## 3. Required status checks

- `vibe build --check-determinism`：編譯、驗證、重編比對、manifest；唔通過唔准 merge。
- Secret scan（`vibe secret-scan --fail-on-find`）。
- 整合測試一定要對真 PostgreSQL 跑（`vibe dev --db postgres` 或者 CI 嘅 Postgres service）；`vibe dev` 預設嘅 in-memory 資料庫唔可以代替（spec 2C.4 列咗語意差異）。
- 開 **Require branches to be up to date before merging**。

## 4. 高風險 label 要兩個 reviewer

- CI 跑 `vibe build --labels`，按 lock 嘅 `security-items` 自動加 `security:high` 或者 `security:medium` label。
- 用 GitHub ruleset 或者一個 required check：PR 有 `security:high` 就要最少 2 個 approval（而且唔係作者）。
- 高風險項目包括：`public` 變 true、剔走 `auth`、public 寫入、`sign-token`、`set-password`、`confirm-verification`、角色改動、provider 改動、對外 `call`、migration。

## 5. 選用：signed commits

- 可以開 **Require signed commits**，減低帳戶被盜之後偽造 commit 嘅風險。唔係必須。

## 6. 細團隊

- 只有一個 maintainer 嘅 project：最少保留 required status checks 同 CODEOWNERS；高風險 PR 可以設定 merge 前等 24 小時，或者請外部 reviewer。
