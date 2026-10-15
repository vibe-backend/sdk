# 安裝與管理 `vibe` binary

`scripts/vibe-install.sh` 從 `vibe-backend/sdk` GitHub Releases 安裝 binary，支援 Linux（x86_64/aarch64）與 macOS（x86_64/arm64）。Windows 請直接下載 release zip；此 POSIX shell 管理器不支援 Windows。

## 使用方式

```sh
./scripts/vibe-install.sh install --version v1.2.3 --non-interactive
./scripts/vibe-install.sh upgrade --version v1.3.0 --non-interactive
./scripts/vibe-install.sh upgrade --non-interactive # 解析 latest stable
./scripts/vibe-install.sh remove --non-interactive
```

預設安裝至 `$HOME/.local/bin/vibe`；可用 `--install-dir /absolute/path` 改寫。若目錄不在 PATH，腳本只提示，不修改 shell profile。完整選項請執行 `--help`。

## 安全模型

1. 平台採 allowlist；不支援的平台 fail closed。
2. install 強制明確 stable `vX.Y.Z`；upgrade 未指定版本時，僅接受相同 repository 的 GitHub `releases/latest` redirect。
3. 使用 HTTPS/TLS 1.2+ 下載；下載失敗立即停止。
4. 必須先核對同一 release 的 `SHA256SUMS`，並強制以已登入的 GitHub CLI 驗證 repository build provenance；任一驗證失敗都不替換既有版本。
5. archive 只能包含頂層唯一、非 symlink 的 `vibe`。每個已驗證版本存於不可變版本目錄；對外 `vibe` symlink 以單次 rename 原子切換。操作鎖避免 install、upgrade、remove 競爭。
6. 腳本不覆寫未管理的 `vibe`。remove 僅接受指向 `.vibe-managed/versions/.../vibe` 的管理 symlink，只刪除管理根目錄與該 symlink，不刪除安裝目錄或其他檔案。
7. 重複安裝相同且內容一致的版本為 no-op。

### 信任邊界與限制

checksum 可偵測傳輸損壞；GitHub build attestation 則把資產綁定至此 repository 的 workflow provenance。仍須保留 release environment、tag protection 與 workflow pinning 控制。腳本不使用 sudo，系統目錄需由操作者預先提供適當權限。SIGKILL/斷電可能留下未引用的版本目錄，但不會使目前 symlink 指向半成品；後續成功操作會清除舊目錄。

## 測試

```sh
python3 tests/install_manager.py
```

測試使用本機 mock release/curl/attestation，不連線 GitHub，涵蓋 fresh install、最新版 upgrade、重複執行、remove、網路/checksum/attestation 失敗時保留既有版本、不支援平台、無效目的地、操作鎖，以及不覆寫/移除非管理檔案。
