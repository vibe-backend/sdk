> English: [../install.md](../install.md)

# 安裝與管理 `vibe` binary

`scripts/vibe-install.sh` 從 `vibe-backend/sdk` GitHub Releases 安裝 binary，支援 Linux（x86_64/aarch64）與 macOS（x86_64/arm64）。Windows 請直接下載 release zip；此 POSIX shell 管理器不支援 Windows。

預設 **vibe-sdk** 安裝做 `vibe`，只有互動 `vibe`、`vibe build`、`vibe run` 三個指令（決定 179）。
**vibe-runtime** 安裝做 `vibe-runtime`，部署時只跑 artifact，冇 LLM client、build
同互動 code。`vibe-runtime run --artifact <dir>` 或者冇 subcommand 都接受同一組 flags；
artifact 預設 `.`，支援 `--addr`、`--db`／`VIBE_DB` 同 `--allow-test-build`（只限開發）。

Archive 係 `vibe-sdk-<target>.tar.gz`／`.zip` 同 `vibe-runtime-<target>.tar.gz`／`.zip`，
SHA256SUMS 同 attestation 覆蓋兩者。`--sdk`／`--runtime` 優先過
`VIBE_INSTALL_BINARY=sdk|runtime`（預設 sdk）。兩者可以並存：SDK 用 `.vibe-managed`
同 `.vibe-install.lock`；runtime 用 `.vibe-runtime-managed` 同 `.vibe-runtime-install.lock`。
Remove 只影響揀咗嗰個 binary；archive 只准包含對應嘅單一普通檔案。
rc.2 之前只有舊名 `vibe-<target>.tar.gz`，SDK 喺 SHA256SUMS 冇新名時可以 fallback；
runtime 冇 fallback。兩者 checksum 同固定身份 attestation 驗證一樣嚴格。

## 使用方式

```sh
./scripts/vibe-install.sh install --version v1.0.0-rc.2
./scripts/vibe-install.sh install --version v1.0.0-rc.2 --runtime
VIBE_INSTALL_BINARY=runtime ./scripts/vibe-install.sh install --version v1.0.0-rc.2
./scripts/vibe-install.sh upgrade --version v1.0.0-rc.2 --non-interactive
./scripts/vibe-install.sh remove --runtime
./scripts/vibe-install.sh remove
```

`install --version` 與 `upgrade --version` 接受 `vX.Y.Z` 或 `vX.Y.Z-rc.N`。每個數字必須為零或不帶前置零的正整數；不接受其他預發佈標籤或 build metadata。

`upgrade` 冇指定 `--version` 時需要穩定版 GitHub Release；pre-release 唔算 `latest`。GA 之前請明確傳入 `--version v1.0.0-rc.2`。如果未有穩定版，腳本會停止並提示使用 `--version`。

預設安裝至 `$HOME/.local/bin/vibe`；可用 `--install-dir /absolute/path` 改寫。若目錄不在 PATH，腳本只提示，不修改 shell profile。完整選項請執行 `--help`。

## 從 source build

```sh
cargo build --release --bin vibe
cargo build --release --no-default-features --bin vibe-runtime
```

## 安全模型

1. 平台採 allowlist；不支援的平台 fail closed。
2. install 強制明確指定 `vX.Y.Z` 或 `vX.Y.Z-rc.N` tag；upgrade 未指定版本時，僅接受相同 repository 的 GitHub `releases/latest` redirect。
3. 所有下載只用 HTTPS（`--proto '=https'`）及 TLS 1.2 以上（`--tlsv1.2`）；任何下載失敗都會立即停止。
4. 資產必須先符合同一 release 的 `SHA256SUMS`，再以已登入的 GitHub CLI 通過 `gh attestation verify`（決定 118、spec 2C.3b／9C）：

   ```sh
   gh attestation verify <asset> \
     --repo vibe-backend/sdk \
     --signer-workflow vibe-backend/sdk/.github/workflows/release.yml \
     --source-ref refs/tags/v<version> \
     --source-digest <commit sha of the tag> \
     --deny-self-hosted-runners
   ```

   commit SHA 由 tag 經 `gh api repos/vibe-backend/sdk/commits/refs/tags/v<version>` 解析。穩定版與預發佈版本使用完全相同的 attestation 身份：`vibe-backend/sdk` 的 workflow `.github/workflows/release.yml`、source ref `refs/tags/v<version>`（包含任何 `-rc.N` 後綴），以及 issuer `https://token.actions.githubusercontent.com`（`gh` 預設值）。此身份寫死於腳本，`VIBE_INSTALL_REPOSITORY`／`VIBE_INSTALL_DOWNLOAD_URL` 無法改變；任一驗證失敗都不替換既有版本。
5. archive 只能包含頂層唯一、非 symlink 的所選 binary（`vibe` 或 `vibe-runtime`）。每個已驗證版本存於不可變版本目錄；對外 binary symlink 以單次 rename 原子切換。操作鎖避免 install、upgrade、remove 競爭。
6. 腳本不覆寫未管理的所選 binary。remove 僅接受指向所選 managed root 的管理 symlink，只刪除管理根目錄與該 symlink，不刪除安裝目錄或其他檔案。
7. 重複安裝相同且內容一致的版本為 no-op。

### 信任邊界與限制

checksum 可偵測傳輸損壞；GitHub build attestation 則把資產綁定至此 repository 的 release workflow、tag 與 commit。仍須保留 release environment、tag protection 與 workflow pinning 控制。腳本不使用 sudo，系統目錄需由操作者預先提供適當權限。SIGKILL/斷電可能留下未引用的版本目錄，但不會使目前 symlink 指向半成品；後續成功操作會清除舊目錄。

## 測試

```sh
python3 tests/install_manager.py
```

測試使用本機 mock release/curl/attestation，不連線 GitHub，涵蓋安裝 `vX.Y.Z` 或 `vX.Y.Z-rc.N`、stable 升級至 rc 及 rc 升級至 stable、在網路呼叫前拒絕無效與多行版本、最新版 upgrade、重複執行、remove、網路/checksum/attestation 失敗時保留既有版本（包含 rc 驗證失敗）、固定的 attestation 身份與參數、不支援平台、無效目的地、操作鎖，以及不覆寫/移除非管理檔案。
