# user

## Table

- provider: main
- table: users

## Fields

| name | type | rules | system |
| --- | --- | --- | --- |
| email | string | required, unique, max: 254 | no |
| roles | string[] | - | no |
| last_device | string | max: 50 | no |

## CRUD

| method | options |
| --- | --- |
| get | - |
| update | optimistic-lock: if-match |

## Policies

- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
