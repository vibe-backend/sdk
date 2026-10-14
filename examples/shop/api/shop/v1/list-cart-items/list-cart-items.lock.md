---
lock-version: 1
kind: api
scope: feature
id: shop/v1/list-cart-items
source: api/shop/v1/list-cart-items/list-cart-items.md
source-hash: sha256:eab11abbc339fa17f7390e85e19e59c5bb432f39a26b5b09ee6738fc7c29b5df
source-type: structured
dependencies:
  - { id: cart-item, kind: model, hash: sha256:0023830861c29224f7648aab89450fdfcdc412a8117df27189665721919a57e0 }
  - { id: main, kind: provider, hash: sha256:cc41434f7e49a87619d2dc00cf191a721cba44c02e3a34cebd870c24185cb2ab }
  - { id: shop/_definition, kind: api, hash: sha256:a00debaa272ac9d4661b8d65df6c7f46fa69cbee5018ba1b1c233a59834f075c }
  - { id: shop/v1/_version, kind: api, hash: sha256:540e17a303a36215645277fa333b163120f49a018b8d7560f4aa16aad94821a0 }
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
- path: /shop/v1/cart-items

## Access
- public: false
- external-calls: forbidden
- challenge: { type: none, when: always }

## Pipelines

### p1 auth
- mode: jwt
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

## Response
- status: 200

## Transform
- in: {page: req.query.page, page_size: req.query.page_size}
- out: result

## Steps

### s1
- op: method
- target: cart-item.list
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
