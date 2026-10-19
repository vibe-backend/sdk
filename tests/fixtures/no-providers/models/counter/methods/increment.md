# increment

## Signature

- input: {name: string}
- output: {value: integer}
- errors: []
- writes: true

## Calls

(none)

## Steps

### s1

- op: query
- table: counters
- sql: INSERT INTO counters (name, value) VALUES ($1, 1) ON CONFLICT (name) DO UPDATE SET value = counters.value + EXCLUDED.value RETURNING value
- params: [input.name]
- writes: true
- out: result

### s2

- op: respond
- value: result
