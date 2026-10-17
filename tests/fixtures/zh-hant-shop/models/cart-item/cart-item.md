# cart-item

使用者購物車中的品項，存放在 main 資料庫的 `cart_items` 表。每位使用者只有一個購物車，因此不另建 cart 表。

## 欄位

- user_id：uuid，必填；由平台以 ctx.user.id 填入，呼叫者不可提供。
- product_id：uuid，必填，指向 product。
- sku_id：uuid，選填，指向 product-variant；商品沒有規格時為 null。
- quantity：整數，必填，最小 1，最大 99。

## 擁有權

- owner-key 為 `user_id`：使用者只能讀寫自己的品項。

## CRUD

- get、update（只可改 quantity）、delete（軟刪除）。
- list：每頁預設 50 筆、最多 100 筆，依 created_at 排序。

## 政策

- 啟用並強制 RLS（租戶欄位由平台自動加入，不需宣告）。
