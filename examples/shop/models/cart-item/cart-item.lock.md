---
lock-version: 1
kind: model
id: cart-item
source: models/cart-item/cart-item.md
source-hash: sha256:e81d6115e3559e62fbb62c909ecfe4cf42d43503f3cdae6773f452607750ccaa
source-type: structured
dependencies:
  - { id: main, kind: provider, hash: sha256:cc41434f7e49a87619d2dc00cf191a721cba44c02e3a34cebd870c24185cb2ab }
compiler: { name: md-logic, version: 0.1.0, model-snapshot: deterministic, llm-provider: none, endpoint-host: none, endpoint-private: false, llm-seed: 0, sent-params: {} }
compiler-hash: sha256:2d8b22eb13fc622363932d517ec318377d3df782c9b3f86527622fe416f5d7c6
status: pending
approval-mode: manual
approved-by: []
approved-at: (none)
approval-ref: (none)
approved-commit: (none)
security-items:
  - { item: ownership, risk: high, reason: "", approved-by: [] }
  - { item: write-sql, risk: medium, reason: "", approved-by: [] }
signature: (none)
---

## Table
- provider: main
- table: cart_items
- owner-key: user_id
- ownership: role(admin)

## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| id | uuid | primary, default: gen | yes |
| user_id | string | required | yes |
| product_id | uuid | required | no |
| qty | integer | required, min: 1, max: 99 | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
| --- | --- |
| create |  |
| list | page-size: 20, max-page-size: 100, sort: [created_at] |
| delete | soft: true |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer []
