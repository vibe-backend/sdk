# cart-item

## Table

- provider: main
- table: cart_items
- owner-key: user_id

## Fields

| name | type | rules | system |
| --- | --- | --- | --- |
| user_id | string | required | yes |
| product_id | uuid | required | no |
| qty | integer | required, min: 1, max: 99 | no |

## CRUD

| method | options |
| --- | --- |
| create | - |
| list | page-size: 20, max-page-size: 100, sort: [created_at] |
| delete | soft: true |

## Policies

- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
