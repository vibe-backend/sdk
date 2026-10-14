---
lock-version: 1
kind: provider
id: main
source: providers/main.md
source-hash: sha256:b0805fe19f3e6cb6f72236e8c658821633fb8f0f6afe2a93c23f7a598d833a18
source-type: structured
dependencies: []
compiler: { name: md-logic, version: 0.1.0, model-snapshot: deterministic, llm-provider: none, endpoint-host: none, endpoint-private: false, llm-seed: 0, sent-params: {} }
compiler-hash: sha256:2d8b22eb13fc622363932d517ec318377d3df782c9b3f86527622fe416f5d7c6
status: pending
approval-mode: manual
approved-by: []
approved-at: (none)
approval-ref: (none)
approved-commit: (none)
security-items:
  - { item: provider-new, risk: high, reason: "", approved-by: [] }
signature: (none)
---

## Connection
- type: postgres
- secret: PG_MAIN

## Isolation
- mode: schema
- rls: forced

## Access
- shared: false
- allowed-endpoints: (none)

## Limits
- pool: 5
- statement-timeout: 5s

## Migrations
- path: migrations/main
