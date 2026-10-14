# export-products

## Route

- method: GET
- path: /catalog/v1/exports

## Access

- public: false

## Pipelines

### auth

- mode: api-key

## Limits

- provider: main
- timeout: 10s
- body: 1MB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| query.page | integer | min: 1 | no |
| query.page_size | integer | min: 1, max: 100 | no |
| query.sort | string | - | no |

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

### s2

- op: respond
- value: transform.out
- status: 200

## Errors

| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
