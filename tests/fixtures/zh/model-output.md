## Table
- provider: main
- table: products
- ownership: tenant-wide
## Fields
｜ 必填 ｜ 規則 ｜ 類型 ｜ 欄位名 ｜
｜ --- ｜ --- ｜ --- ｜ --- ｜
｜ 是 ｜ max_length: 40 ｜ string ｜ sku ｜
｜ ✓ ｜ maxLength: 120 ｜ string ｜ name ｜
## Constraints
- unique: [sku, name]
## CRUD
| method | options |
| --- | --- |
| create | - |
| update | optimistic-lock: if-match |
## Policies
(none)
