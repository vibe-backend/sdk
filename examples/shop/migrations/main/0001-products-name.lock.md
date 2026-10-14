---
lock-version: 1
kind: migration
id: migrations/main/0001-products-name
source: migrations/main/0001-products-name.md
source-hash: sha256:c9a856ba1f2d3dbd262ae2e2be480e9d935a61adb7188d58bc1830f0865ef2cc
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
  - { item: migration-ddl, risk: high, reason: "", approved-by: [] }
signature: (none)
---

## Target
- provider: main
- seq: 0001

## Up

### s1
- sql: CREATE INDEX CONCURRENTLY IF NOT EXISTS products_name_active ON products (name)
- source-lines: 1-1

## Verify
(none)

## Data
(none)
