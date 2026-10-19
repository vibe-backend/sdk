# update-product

## Route

- method: PUT
- path: /catalog/v1/products/{id}

## Access

- public: false

## Pipelines

(none)

## Limits

- provider: main
- timeout: 10s
- body: 1MB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| path.id | uuid | - | yes |
| header.if-match | string | - | no |
| body.sku | string | max: 40 | no |
| body.name | string | max: 120 | no |
| body.price_cents | integer | min: 0 | no |

## Response

- status: 200

## Transform

- in: {id: req.path.id, if_match: req.header.if-match, sku: req.body.sku, name: req.body.name, price_cents: req.body.price_cents}
- out: result

## Steps

### s1

- op: method
- target: product.update
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 200

## Errors

| status | code | from |
| --- | --- | --- |
| 412 | PRECONDITION_FAILED | PRECONDITION_FAILED |
| 428 | PRECONDITION_REQUIRED | PRECONDITION_REQUIRED |
