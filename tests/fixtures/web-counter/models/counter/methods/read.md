# read

## Signature

- input: {name: string}
- output: {value: integer}
- errors: []
- writes: false

## Calls

(none)

## Steps

### s1

- op: query
- table: counters
- sql: SELECT COALESCE(MAX(value), 0) AS value FROM counters WHERE name = $1
- params: [input.name]
- writes: false
- out: result

### s2

- op: respond
- value: result
