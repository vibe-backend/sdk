# session (writes users.last_device through user.record-session)

## Route

- method: POST
- path: /auth/v1/session

## Access

- public: false

## Pipelines

(none)

## Limits

- provider: main
- timeout: 10s
- body: 4KB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| body.device | string | max: 50 | yes |

## Response

- status: 204

## Transform

- in: {device: req.body.device}
- out: result

## Steps

### s1

- op: method
- target: user.record-session
- args: transform.in
- out: result

### s2

- op: respond
- value: transform.out
- status: 204

## Errors

| status | code | from |
| --- | --- | --- |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
