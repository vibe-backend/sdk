# Privacy deletion (crypto-shredding + anonymization)

## Purpose

Honor data-subject erasure requests (e.g. PDPO, GDPR) **without** ever enabling `DELETE`: sensitive personal fields are crypto-shredded, other personal fields are anonymized, and records are marked erased and soft-deleted (Decision 42, §12.10). The erasure flow is executed **by the platform, not by locks**; your job in source is to classify personal fields correctly.

## Natural-language source example

`models/customer/customer.md`

```
# customer
Customer profiles, stored in the customers table of provider main.

Fields:
- id: uuid, primary key, auto-generated.
- user_id: uuid; identifies the data subject for privacy erasure.
- email: valid email, unique; standard personal data.
- name: at most 100 characters; standard personal data.
- phone: at most 20 characters; sensitive personal data.
- address: at most 300 characters; sensitive personal data.
- date_of_birth: timestamp; sensitive personal data.
- tier: string.

Provide get, list and update CRUD. list defaults to 20 per page, at most 100, sort by created_at.
```

Erasure is triggered outside source, by a tenant admin:

```
subject erase --subject <id> --reason "..."
```

## Compiled lock md example

`models/customer/customer.lock.md` (sections only; `kind: model`)

````markdown
## Table
- provider: main
- table: customers

## Fields
| name | type | rules | system |
|---|---|---|---|
| id | uuid | primary, default: gen | yes |
| user_id | uuid | subject-key | no |
| email | string | format: email, unique, personal: standard | no |
| name | string | max: 100, personal: standard | no |
| phone | string | max: 20, sensitive, personal: sensitive | no |
| address | string | max: 300, sensitive, personal: sensitive | no |
| date_of_birth | timestamp | sensitive, personal: sensitive | no |
| tier | string | | no |
| created_at | timestamp | default: now | yes |
| updated_at | timestamp | default: now | yes |
| deleted_at | timestamp | nullable | yes |
| erased_at | timestamp | nullable | yes |
| tenant_id | string | platform | yes |

## CRUD
| method | options |
|---|---|
| get | sensitive-output: [] |
| list | pagination: offset, page-size: 20, max-page-size: 100, sort: [created_at], filter: [] |
| update | partial: true, optimistic-lock: updated_at |

## Policies
- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
- grants: app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT]
````

> ⚠️ The lock syntax for `personal: sensitive | standard`, `subject-key`, and the `erased_at` system field is **not shown in the spec**; it is illustrated here by analogy with the `## Fields` table of §12.8.

## Security considerations

- **Encryption** (`personal: sensitive`): every sensitive field of every record has its own DEK (AES-256-GCM); DEKs are wrapped by the tenant KEK in platform KMS and kept in the platform key store, separate from business data and out of DB backups. Encryption/decryption is automatic; locks never see keys (§12.10).
- **Trigger**: tenant admin via platform UI or CLI `subject erase`; reason and request reference are mandatory; one admin submits, another confirms (single-admin tenants: 24h delay) (§12.10).
- **Flow** (platform-executed): (1) find all subject records via the `subject-key` field; (2) destroy DEKs of sensitive fields (crypto-shredding); (3) overwrite standard fields with irreversible placeholders (e.g. `erased-<random>`, random unique values for unique fields) using a platform erasure role — UPDATE only, no DELETE; (4) set `erased_at` and soft-delete, keeping rows and relations; (5) revoke all of the subject's refresh-token families and API keys (§12.10).
- **Deadlines**: completed within 30 days of the request; platform target 24h after trigger.
- **Audit**: trigger, confirmer, reason, reference, affected tables/columns, record count, completion time — with pseudonymized subject id only (§12.10).
- **Backups**: shredded fields are unreadable in backups; standard fields disappear when the 35-day backup retention expires; restores replay the erasure ledger before reopening (§12.10).
- **Logs**: personal data must never be logged; logs keep pseudonymized ids only, retained 30 days (§12.10).
- **No rewrites**: locks may not write personal fields again on erased records (§12.10).
- Mark sensitive fields also `sensitive: true` to keep them out of `list` output, filter and sort (§12.9).
- `DELETE` remains forbidden everywhere (§2A.4).
- The spec's §13 asks for legal review of the PDPO/GDPR aspects — this page is not legal advice.

## Spec gaps

- Lock syntax for `personal`, `subject-key` and `erased_at` is **not specified**.
- Whether `sensitive: true` (§12.9) and `personal: sensitive` (§12.10) are the same marker or independent is **ambiguous**.
- Whether subject records in **other** models are linked via each model's own `subject-key` declaration is implied but not stated.
- §14 ("Later versions") still lists "formal privacy deletion flow and CLI", which conflicts with §0.4 Decision 42 and §12.10 describing it as finalized. Treat the `subject erase` CLI as possibly not yet available.
- How erased records interact with `unique` constraints and with `get` (it is soft-deleted, so excluded) for admins is not specified.
