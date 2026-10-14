# add-cart-item

## Route

- method: POST
- path: /shop/v1/cart-items

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
| body.product_id | uuid | - | yes |
| body.qty | integer | min: 1, max: 99 | yes |

## Response

- status: 201

## Transform

- in: {product_id: req.body.product_id, qty: req.body.qty}
- out: result

## Steps

### s1

- op: method
- target: cart-item.create
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 201

## Errors

| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
