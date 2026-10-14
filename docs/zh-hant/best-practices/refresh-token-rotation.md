# Refresh token 輪換

## 用途

讓 access token 保持短效（≤ 1 小時），同時讓用戶端維持登入狀態，方法是使用附重用偵測的**輪換式 refresh token**（Decision 6、§4A.1）。refresh 流程由平台的**內建 `token-refresh` pipeline** 實作，它透過 `method` op 呼叫帶 `capabilities: [sign-token]` 的 model method（例如 `user.sign-token`）簽發新的 access token。

## 自然語言 source 範例

`api/auth/v1/refresh/refresh.md`（路由 `POST /auth/v1/{slug}/refresh`）

```text
# Refresh token
POST, path /auth/v1/{slug}/refresh.
Public, no auth (the caller only holds a refresh token), tenant is determined by slug.
Use the built-in token-refresh pipeline, signing new access tokens with user.sign-token.
Each IP at most 30 requests per minute.
Request body: refresh_token (string, required).
Return 200 with the new access token and new refresh token.
An invalid, expired, revoked or reused refresh token returns 401 INVALID_CREDENTIALS.
```

## 編譯後的 lock md 範例

`api/auth/v1/refresh/refresh.lock.md`（僅列 section；frontmatter 依 §2A.1，`kind: api`、`scope: feature`；`dependencies` 包含 `token-refresh`（pipeline）及 `user/methods/sign-token`；安全項目為 `public` 和 `auth-removed`，屬高風險）

````markdown
## Route
- method: POST
- path: /auth/v1/{slug}/refresh

## Access
- public: true
- tenant: { from: path, param: slug }
- public-writes: false
- role: public_reader
- external-calls: forbidden

## Pipelines
### p1 -auth
- removed: true

### p2 rate-limit
- per-minute: 30
- key: ip

### p3 token-refresh
- sign-method: user.sign-token

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
|---|---|---|---|
| path.slug | string | | yes |
| body.refresh_token | string | | yes |

## Response
| status | schema | headers |
|---|---|---|
| 200 | { access_token, refresh_token } | |

## Transform
(none)

## Steps
(none)

## Errors
| status | code | from |
|---|---|---|
| 401 | INVALID_CREDENTIALS | token-refresh |
| 422 | VALIDATION_FAILED | platform |
````

> ⚠️ `token-refresh` pipeline 由平台提供，其 `## Parameters` schema **規格中沒有公開**。`sign-method`、回應欄位名稱、`public-writes: false` 以及空白的 `## Steps` 都是用作示意的**假設**。`## Errors` 中的 `INVALID_CREDENTIALS` 是本範例**自行定義的錯誤碼**（沿用登入的錯誤碼）；規格沒有為 `token-refresh` 定義任何錯誤碼。見規格缺口。

## 安全考量

由平台強制執行的規則（§4A.1、Decision 6）：

- **Access token**：JWT，ttl ≤ **1 小時**。
- **Refresh token**：至少 **256 位元**的不透明隨機值；資料庫只儲存其**雜湊值**。
- **有效期**：由該家族首次簽發起計，絕對上限為 **30 日**；閒置 **7 日**即失效。
- **每次使用都輪換**：會簽發新的 refresh token 和新的 access token，舊的 refresh token 即時失效。
- **家族**：同一次登入產生的所有 refresh token 共用一個 `family_id`。重用已失效的 token 會**撤銷整個家族**，寫入審計紀錄並通知用戶。
- **重試寬限**：輪換後 **10 秒**內重用同一個舊 token，會回傳同一組新 token，不視為重用。
- **登出**會撤銷目前的家族；「登出所有裝置」會撤銷該用戶的所有家族。
- 私隱刪除會撤銷當事人所有的 refresh token 家族（§12.10）。
- pipeline 不能直接執行 `query` 或 `call`；只能透過 `method` 存取 model（§4）。
- token 永遠不會寫入日誌（§9）。
- **用戶端儲存**（一般建議，規格以外）：瀏覽器應把 refresh token 存放在 `HttpOnly`、`Secure`、`SameSite` 的 cookie 中；原生 app 則存放在作業系統的 keychain／keystore（iOS Keychain、Android Keystore）。絕不可存放在 `localStorage`／`sessionStorage`。規格只展示了 body 欄位 `refresh_token`；以 cookie 傳送未有規定。簽發只能經 `sign-token` capability 進行，並受 claim 白名單及 ttl 上限約束（§12.6），見 [sign-token.md](sign-token.md)。

## 規格缺口

- `token-refresh` pipeline 的**參數、回應格式及錯誤碼**均未有規定。
- **登入時如何簽發第一個 refresh token**（以及由哪個元件建立家族）未有規定；§12.8 的 `user.login` 範例只回傳 access token。
- refresh token **存放在哪裡**（平台儲存還是 model 資料表）未有規定；pipeline 不能 `query`，也沒有用來產生不透明隨機 token 的 op。
- **登出／登出全部裝置的端點**：規格定義了應有的行為，但沒有規定用於撤銷的 pipeline、op 或 method。
- refresh 端點是否算作公開寫入（它會改變 token 狀態），以及是否需要 `public-writes: true`、配額或 challenge，**未有規定**。
- refresh 端點屬於「登入類」（500ms）還是一般公開端點（300ms）未有規定；按 §7.2 的定義（含有 `verify-password` 步驟才算登入類），應為 300ms。
- **10 秒寬限與只存雜湊值的矛盾**：§4A.1 要求舊 token 在 10 秒內被重用時回傳*同一組*新 token，但同時規定 refresh token 只儲存雜湊值。要再次回傳同一個明文 refresh token，就必須在 10 秒內保留它（或可還原的形式），這與只存雜湊值互相矛盾（應由規格負責人解決）。
- 以 cookie 傳送 refresh token 未有規定。
- 當回應由 pipeline 產生時，feature lock 的 `## Steps` 應包含甚麼，未有規定。
