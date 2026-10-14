# delete-product

## Route

- method: DELETE
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

## Response

- status: 204

## Transform

- in: {id: req.path.id}
- out: result

## Steps

### s1

- op: method
- target: product.delete
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 204

## Errors

| status | code | from |
| --- | --- | --- |
| 404 | NOT_FOUND | NOT_FOUND |
