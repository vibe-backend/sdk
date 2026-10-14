# get-product

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

### -auth

- removed: true

## Limits

- provider: main
- timeout: 10s
- body: 1MB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
| path.id | uuid | - | yes |

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

### s2

- op: respond
- value: transform.out
- status: 200

## Errors

| status | code | from |
| --- | --- | --- |
| 404 | NOT_FOUND | NOT_FOUND |
