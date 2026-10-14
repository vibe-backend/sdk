# list-products

## Route

- method: GET
- path: /catalog/v1/{slug}/products

## Access

- public: true
- public-writes: false
- tenant: path.slug
- min-response: 300ms
- external-calls: forbidden
- challenge: { type: none, when: always }

## Pipelines

### -auth

- removed: true

### rate-limit

- per-minute: 600
- key: ip

## Limits

- provider: main
- timeout: 10s
- body: 1MB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
| query.page | integer | min: 1 | no |
| query.page_size | integer | min: 1, max: 100 | no |
| query.sort | string | - | no |
| query.name | string | max: 120 | no |

## Response

- status: 200

## Transform

- in: {page: req.query.page, page_size: req.query.page_size, sort: req.query.sort, filter: {name: req.query.name}}
- out: result

## Steps

### s1

- op: method
- target: product.list
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 200

## Errors

| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
