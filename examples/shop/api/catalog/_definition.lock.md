---
lock-version: 1
kind: api
scope: definition
id: catalog/_definition
source: api/catalog/_definition.md
source-hash: sha256:fd72c3ab5054e8dc8446c29fc7c3bf24edea0fc2dc96f44831ef006f24ca4dca
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
security-items: []
signature: (none)
---

## Providers
- allowlist: [main]
- default: main

## Pipelines

### auth
- mode: jwt
- roles: [staff]
- source-lines: 1-1

## Defaults
- timeout: 10s
- body: 1MB
