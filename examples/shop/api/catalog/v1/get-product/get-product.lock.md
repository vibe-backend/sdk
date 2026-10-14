---
lock-version: 1
kind: api
scope: feature
id: catalog/v1/get-product
source: api/catalog/v1/get-product/get-product.md
source-hash: sha256:54c46a430e197df26909f0ea839474f200d6ac32051fea7530690aae65a6d2ee
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
security-items:
  - { item: auth-removed, risk: high, reason: "", approved-by: [] }
  - { item: public, risk: high, reason: "", approved-by: [] }
signature: (none)
---

## Route
- method: GET
- path: /catalog/v1/{slug}/products/{id}

## Access
- public: true
- public-writes: false
- tenant: path.slug
- min-response: 300ms
- external-calls: forbidden
- challenge: { type: none, when: always }

## Pipelines

### p1 -auth
- removed: true
- source-lines: 1-1

## Limits
- provider: main
- timeout: 10s
- body: 1MB

## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string |  | yes |
| path.id | uuid |  | yes |

## Response
- status: 200

## Transform
- in: {id: req.path.id}
- out: result

## Steps

### s1
- op: method
- target: product.get
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
| 404 | NOT_FOUND | NOT_FOUND |
