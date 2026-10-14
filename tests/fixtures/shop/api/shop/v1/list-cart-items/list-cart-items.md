# list-cart-items

## Route

- method: GET
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

### s2

- op: respond
- value: transform.out
- status: 200

## Errors

| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
