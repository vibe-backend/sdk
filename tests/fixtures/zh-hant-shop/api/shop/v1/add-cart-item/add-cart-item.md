---
public: false
auth: jwt
writes: true
---

# 加入購物車

POST `/shop/v1/cart-items`。需要 JWT。

- body：product_id（uuid，必填）、sku_id（uuid，選填；商品有規格時必填）、quantity（整數 1 至 99，必填）。
- 呼叫 cart-item.add；product_id 與 sku_id 都相同時才累加數量。成功後回傳整個購物車。
- 錯誤：PRODUCT_NOT_FOUND 回 404；PRODUCT_UNAVAILABLE 回 409（商品規格全部停用）；SKU_NOT_FOUND 回 404；SKU_REQUIRED 回 422；OUT_OF_STOCK 回 409；QUANTITY_LIMIT 回 422；驗證不通過回 422 VALIDATION_FAILED。
- cart-item.add 成功後，再呼叫 cart-item.summary，回應內容改為整個購物車（items 與 subtotal_cents），狀態碼 200。
