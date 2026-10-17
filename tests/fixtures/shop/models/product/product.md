# product

## Table

- provider: main
- table: products

## Fields

| name | type | rules | system |
| --- | --- | --- | --- |
| sku | string | required, unique, max: 40 | no |
| name | string | required, max: 120 | no |
| price_cents | integer | required, min: 0 | no |

## CRUD

| method | options |
| --- | --- |
| create | - |
| get | - |
| list | page-size: 20, max-page-size: 100, sort: [created_at, name], filter: [name] |
| update | optimistic-lock: if-match |
| delete | soft: true |

## Policies

- rls: enable, force
- policy: tenant_isolation TO tenant_<id>_rw, tenant_<id>_public_reader, tenant_<id>_public_writer USING (tenant_id = '<id>') WITH CHECK (tenant_id = '<id>')
