# cart-item

購物車項目。
欄位：cart_id（uuid，必填）、product_id（uuid，必填）、qty（整數，1 至 99）。
cart_id 同 product_id 組合唯一（同一個購物車唔可以有兩行同一件商品）。
提供 create 同 list。
