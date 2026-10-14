---
writes: true
---
# add

把商品（或其規格）加入目前使用者的購物車；同一 product_id 且同一 sku_id 的品項已存在時累加數量，否則新增一列。

## 輸入
- product_id（uuid，必填）、sku_id（uuid，選填）、quantity（1 至 99，必填）。

## 做法
1. 查詢 product：必須存在、active 為 true、未刪除；否則回 `PRODUCT_NOT_FOUND`。
2. 查詢此商品的規格。
   - 商品有 options 但沒有任何 active 規格：回 `PRODUCT_UNAVAILABLE`。
   - 有規格而未提供 sku_id：回 `SKU_REQUIRED`。
   - 提供 sku_id：以 sku_id 查詢 product_variants，必須存在、active、未刪除，且 product_id 相同；否則回 `SKU_NOT_FOUND`。
   - 沒有規格而提供 sku_id：回 `SKU_NOT_FOUND`。
3. 查詢目前使用者（ctx.user.id）在相同 product_id 與 sku_id（皆為 null 時視為相同）的既有 quantity（沒有則為 0）。
4. 新數量 = 原數量 + 輸入 quantity；超過 99 回 `QUANTITY_LIMIT`；超過可用庫存（有 sku 時為該規格 stock，否則為商品 stock）回 `OUT_OF_STOCK`。
5. 已有品項時以主鍵更新 quantity；沒有時新增一列，user_id 用 ctx.user.id。
6. 回傳該品項 id、product_id、sku_id、quantity。

## 錯誤
- PRODUCT_NOT_FOUND、PRODUCT_UNAVAILABLE、SKU_REQUIRED、SKU_NOT_FOUND、QUANTITY_LIMIT、OUT_OF_STOCK。

此 method 會寫入資料（writes: true）。
