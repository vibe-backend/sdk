# counter

## Table

- provider: main
- table: counters

## Fields

| name | type | rules | system |
| --- | --- | --- | --- |
| name | string | required, unique | no |
| value | integer | required, default: 0 | no |

## CRUD

| method | options |
| --- | --- |
| create | - |
| get | - |
| update | optimistic-lock: if-match |

## Policies

- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
