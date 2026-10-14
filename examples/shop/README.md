# Sample shop

Structured markdown for a small catalog and cart. `vibe compile` turns these files into lock files without calling an LLM. Validate them, build a manifest, then serve it.

Public catalog reads use `/{slug}` and no auth. Staff product writes use a JWT with the `staff` role. Cart routes accept any valid JWT. Product import and export use API keys scoped to `catalog`.

Review lock changes in a pull request; `vibe` does not record approvals in locks.

Public routes resolve `{slug}` from `.vibe/slugs.json`. This example ships [slugs.example.json](slugs.example.json) with slug `acme` and tenant `acme0001`. `vibe serve` uses that file when `.vibe/slugs.json` is absent, and prints a warning. Register another slug with:

```bash
vibe slug add shop --tenant tenant0001 --project examples/shop
```

Then `curl http://127.0.0.1:8080/catalog/v1/acme/products` lists the public catalog for that tenant.

Cart items declare `owner-key: user_id`. Create stores the JWT `sub` in `user_id`. List, get, update, and delete only return that caller's rows, unless the JWT role is `admin`, which the model allows explicitly with `ownership: role(admin)`. Without that line no role bypasses the owner filter. A client-supplied `user_id` is rejected.
