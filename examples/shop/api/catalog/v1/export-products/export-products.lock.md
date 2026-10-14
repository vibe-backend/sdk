---
lock-version: 1
kind: api
scope: feature
id: catalog/v1/export-products
source: api/catalog/v1/export-products/export-products.md
source-hash: sha256:82b96972402ccedd20d50eac7c0dc8cc30fcd421d1d1a2934f31c79a717d91e4
source-type: structured
dependencies:
  - { id: catalog/_definition, kind: api, hash: sha256:c16a6e3eda96f583d72eb38a9982a05c5ab2034684bc1ad71ab9428cdc7c76bd }
  - { id: catalog/v1/_version, kind: api, hash: sha256:f4b09c4bcb9221ed116a46898eb53aa73ad833e5a01da4326f1e6a19a5ae06df }
  - { id: main, kind: provider, hash: sha256:cc41434f7e49a87619d2dc00cf191a721cba44c02e3a34cebd870c24185cb2ab }
  - { id: product, kind: model, hash: sha256:bd0ad8d5359681b634c5fd6eb88fc99c5848c5957a6e35adf0c9056c81be42ef }
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

## Route
- method: GET
- path: /catalog/v1/exports

## Access
- public: false
- external-calls: forbidden
- challenge: { type: none, when: always }

## Pipelines

### p1 auth
- mode: api-key
- source-lines: 1-1

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| query.page | integer | min: 1 | no |
| query.page_size | integer | min: 1, max: 100 | no |
| query.sort | string |  | no |

## Response
- status: 200

## Transform
- in: {page: req.query.page, page_size: req.query.page_size, sort: req.query.sort}
- out: result

## Steps

### s1
- op: method
- target: product.list
- args: transform.in
- out: result
- source-lines: 1-1

### s2
- op: respond
- value: transform.out
- status: 200
- source-lines: 1-1

## Errors
| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
