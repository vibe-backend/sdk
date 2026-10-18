---
auth: none
---
# increment-counter

## Route

- method: POST
- path: /counter/v1/{slug}/counter/increment

## Access

- public: true
- public-writes: true
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

## Response

- status: 200

## Transform

- in: {name: "counter"}
- out: result

## Steps

### s1

- op: method
- target: counter.increment
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 200

## Errors

(none)
