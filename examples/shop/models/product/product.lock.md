---
lock-version: 1
kind: model
id: product
source: models/product/product.md
source-hash: sha256:2db7677270ad96f3f177010327024466f93b3c8af3df39d2ed8810f7e573b32d
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
  - { item: write-sql, risk: medium, reason: "", approved-by: [] }
signature: (none)
---

## Table
- provider: main
- table: products

## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| id | uuid | primary, default: gen | yes |
| sku | string | required, unique, max: 40 | no |
| name | string | required, max: 120 | no |
| price_cents | integer | required, min: 0 | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
| --- | --- |
| create |  |
| get |  |
| list | page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [name] |
| update | optimistic-lock: if-match |
| delete | soft: true |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT], public_writer []
