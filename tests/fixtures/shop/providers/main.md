# main

## Connection

- type: postgres
- secret: PG_MAIN

## Isolation

- mode: schema
- rls: forced

## Access

- shared: false
- allowed-endpoints: (none)

## Limits

- pool: 5
- statement-timeout: 5s

## Migrations

- path: migrations/main
