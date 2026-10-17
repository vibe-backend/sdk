# refresh

## Route

- method: POST
- path: /auth/v1/{slug}/refresh

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

### token-refresh

- sign-method: user.sign-token
- refresh-ttl: 30d
- idle-ttl: 7d
- reuse-grace: 10s
- transport: body

## Limits

- provider: main
- timeout: 10s
- body: 4KB

## Request

| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
| body.refresh_token | string | - | yes |

## Response

- status: 200

## Transform

- in: {}
- out: result

## Steps

(none)

## Errors

(none)
