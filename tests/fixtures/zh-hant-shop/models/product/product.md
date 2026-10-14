# product

商品目錄，存放在 main 資料庫的 `products` 表。屬於整個商店（租戶）共享的資料。

## 欄位
- slug：字串，必填，唯一，只允許 a-z0-9-，最長 80 字。
- name：字串，必填，最長 120 字。
- description：字串，選填，最長 4000 字，預設空字串（不回傳 null）。
- category：字串，必填，最長 40 字。
- price_cents：整數，必填，最小 0。
- currency：字串，必填，固定 3 個大寫字母（例如 TWD），預設 TWD。
- image_urls：字串陣列，選填，最多 10 個網址。
- stock：整數，必填，最小 0，預設 0。
- active：布林，必填，預設 true。
- sold_count：整數，必填，最小 0，預設 0；只由 order.place 累加，建立或更新商品時不可提供。
- options：json 類型，選填；內容為規格群組陣列，每個元素含 name（字串，最長 20 字）與 values（字串陣列，1 至 20 個，每個最長 20 字），最多 3 組。例如 [{name: 顏色, values: [黑色, 米白]}]。空陣列或缺少代表此商品沒有規格。

## 規格與彙總
- 規格（SKU）存在 product-variant model。
- 商品有 active 規格時，對外輸出的 price_cents 為「active 的規格中最低的 price_cents」（庫存為 0 的 active 規格也計入），stock 為「active 的規格 stock 總和」，in_stock 為 stock > 0。
- 商品有 options 但規格全部 inactive 時，視為不可購買：variants 為空陣列、stock 為 0、in_stock 為 false，price_cents 輸出商品本身的基本價格（僅供顯示）。
- 商品沒有 options 時，沿用商品本身的 price_cents 與 stock。

## CRUD
- create、get、update（以 If-Match 樂觀鎖）、delete（軟刪除）。
- list：每頁預設 20 筆、最多 100 筆；可依 created_at、price_cents 排序；可依 category、active 篩選。

## 政策
- 啟用並強制 RLS（租戶欄位由平台自動加入，不需宣告）。
