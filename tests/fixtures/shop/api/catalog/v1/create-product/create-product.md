# create-product

## Route

- method: POST
- path: /catalog/v1/products

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
| body.sku | string | max: 40 | yes |
| body.name | string | max: 120 | yes |
| body.price_cents | integer | min: 0 | yes |

## Response

- status: 201

## Transform

- in: {sku: req.body.sku, name: req.body.name, price_cents: req.body.price_cents}
- out: result

## Steps

### s1

- op: method
- target: product.create
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 201

## Errors

| status | code | from |
| --- | --- | --- |
| 409 | UNIQUE_VIOLATION | UNIQUE_VIOLATION |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
