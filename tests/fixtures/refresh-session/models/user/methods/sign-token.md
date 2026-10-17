# sign-token

## Signature

- input: {sub: uuid}
- output: {access_token: string, refresh_token: string}
- errors: []
- writes: false
- capabilities: [sign-token]

## Calls

(none)

## Steps

### s1

- op: query
- table: users
- sql: select id::text as id from users where id::text = $1
- params: [input.sub]
- writes: false
- out: u

### s2

- op: sign-jwt
- alg: ES256
- key-ref: JWT_SIGNING_KEY
- claims: {sub: u.id}
- ttl: 1h
- issue-refresh: platform-decided
- out: tok

### s3

- op: respond
- value: tok
