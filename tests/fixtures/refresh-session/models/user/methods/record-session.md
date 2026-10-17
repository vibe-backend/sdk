# record-session

## Signature

- input: {device: string}
- output: (none)
- errors: []
- writes: true

## Calls

(none)

## Steps

### s1

- op: query
- table: users
- sql: update users set last_device = $1 where id::text = $2
- params: [input.device, ctx.user.sub]
- writes: true
- scope: pk
- out: r

### s2

- op: respond
- value: {}
