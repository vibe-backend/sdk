# Refresh token 輪換

## 用途

讓 access token 保持短效（≤ 1 小時），同時讓用戶端維持登入狀態，方法是使用帶重用偵測的**輪換 refresh token**（Decision 6、§4A.1）。Refresh 由內置 `token-refresh` pipeline 處理（Decision 45）；登出由內置 `token-revoke` pipeline 處理（Decision 65）。第一個 refresh token 在登入時簽發：`sign-jwt` 配 `issue-refresh: new-family`（§4A.1、F10；見 [user-login.md](user-login.md)）。

## 自然語言 source 範例

`api/auth/v1/refresh/refresh.md`（路由 `POST /auth/v1/{slug}/refresh`）

```text
# Refresh token
POST, path /auth/v1/{slug}/refresh.
This is the refresh token endpoint. Public (the caller only holds a refresh token), no auth, tenant is determined by slug.
Sign new access tokens with user.sign-token. The refresh token is sent in the body.
Request body: refresh_token (string, required).
Return 200 with the new token and refresh_token.
```

`api/auth/v1/logout/logout.md`（路由 `POST /auth/v1/logout`）

```text
# Logout
POST. Requires JWT. Revokes the refresh token family of the given refresh token.
Request body: refresh_token (string, required).
Return 204.
```

## 編譯後的 lock md 範例

`api/auth/v1/refresh/refresh.lock.md`（僅列出 section；frontmatter 依照 §2A.1，`kind: api`、`scope: feature`；安全項目 `public` 和 `auth-removed`，高風險）

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/refresh

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: false
- external-calls: forbidden
- min-response: 300ms

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 30
- key: ip

### p3 rate-limit
- per-minute: 10
- key: family

### p4 token-refresh
- sign-method: user.sign-token
- refresh-ttl: 30d
- idle-ttl: 7d
- reuse-grace: 10s
- transport: body

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | | yes |
| body.refresh_token | string | | yes |

## Response
| status | schema | headers |
| --- | --- | --- |
| 200 | { token, refresh_token } | |

## Transform
(none)

## Steps
(none)

## Errors
| status | code | from |
| --- | --- | --- |
| 401 | INVALID_CREDENTIALS | token-refresh |
| 422 | VALIDATION_FAILED | platform |
````

`api/auth/v1/logout/logout.lock.md`（只列 `## Pipelines`）

````markdown
## Pipelines
### p1 auth
- mode: jwt

### p2 token-revoke
- scope: family
- token-source: body
````

> ⚠️ `token-refresh` 和 `token-revoke` 的參數，以及 refresh 的分類（public、每 IP 每分鐘 30 次、每家族每分鐘 10 次、300ms），均來自規格（§4A.1、Decisions 45、65、72）。回應欄位名稱、401 錯誤碼、空的 `## Transform`／`## Steps`，以及省略 `role`，均屬**假設**：這裡的 `INVALID_CREDENTIALS` 是本範例**自行定義的錯誤碼**；規格沒有為 `token-refresh` 定義錯誤碼。

## 安全考量

平台強制規則（§4A.1、Decisions 6、45、60、61、72）：

- **Access token**：JWT，ttl ≤ 1 小時。
- **Refresh token**：不透明隨機值（≥ 256 bit）；只以 `HMAC-SHA-256(pepper, token)` 存在平台表 `_platform_refresh_tokens`（每個租戶 schema 一張，強制 RLS）；用戶 lock 不可查詢；只有平台 `refresh_store` role 可讀寫。
- **壽命**：`refresh-ttl` 30d，由家族首次簽發起計的絕對壽命（上限 30d）；`idle-ttl` 7d（上限 7d）。
- **每次使用都輪換**：發新的 refresh 和 access token；舊 refresh token 即時失效。
- **家族**：重用已失效的 token 會撤銷整個家族、寫審計記錄並通知用戶。撤銷以 UPDATE `revoked_at`／`revoke_reason` 進行，絕不 DELETE。
- **寬限期**（Decision 61，`reuse-grace` ≤ 10s）：寬限期內平台**不會**回傳先前的明文（它只有 hash）。它撤銷上一次發出的子 token（`revoke_reason: grace-reissue`），在同一家族發新的子 token，並設定 `grace_reissued_at`；以條件更新 `update ... where grace_reissued_at is null` 確保兩個並發請求只有一個成功。每個舊 token 最多重發一次；第二次使用、超過寬限期使用、或子 token 已再被輪換後使用，都當作重用並撤銷家族。`used_at` 不變，所以寬限期不會延長。
- **Cookie 傳送**（`transport: cookie`）：一律 `HttpOnly; Secure; SameSite=Strict; Path=/{definition}/{version}/{slug}/refresh`（Decision 60）。
- **分類**（Decision 72）：`public: true`（沒有 access token），但**不屬**公開寫入：用戶 lock 沒有寫入 step，只有平台寫自己的表，因此不計每日公開寫入配額，亦不需要 captcha；固定限流為每 IP 每分鐘 30 次、每家族每分鐘 10 次；最低回應耗時 300ms。
- **登出**（`token-revoke`，Decision 65）：必須經 `auth`；`scope: family` 撤銷當前家族（登出），`scope: all` 撤銷該用戶所有家族（全部裝置登出）；只可撤銷呼叫者自己（`sub`）的家族；即使 token 不存在或已撤銷也回 204；cookie 傳送時會清除 cookie（`Max-Age=0`）。
- **其他撤銷**：電郵驗證成功會撤銷所有家族（`email-verified`，Decision 67）；私隱刪除會撤銷所有家族（`erasure`，§12.10）。
- **用戶端儲存**（一般建議，規格以外）：瀏覽器優先用 `transport: cookie`；原生 app 應把 token 存在作業系統的 keychain／keystore。絕不要把 refresh token 存在 `localStorage`／`sessionStorage`。
- Token 永不寫入 log（§9）。簽發只經 `sign-token` capability（§12.6），見 [sign-token.md](sign-token.md)。

## 規格缺口

- `token-refresh` 的錯誤碼和回應欄位名稱**未有規定**。
- 由 pipeline 產生回應時，feature lock 的 `## Transform`／`## Steps` 應包含甚麼**未有規定**。
- Decision 72 說固定的 refresh 限流會「用 §4.3 的 schema 寫入 lock」，但沒有展示是以明確的 `rate-limit` step 出現（如本頁所示），還是由 `token-refresh` 隱含。
- 寬限期內可選的用戶端指紋比對（§4A.1 第 5 點）沒有對應參數。
