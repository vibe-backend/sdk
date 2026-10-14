# Sample shop

Structured markdown for a small catalog and cart. `vibe compile` turns these files into lock files without calling an LLM. Approve the locks, build a manifest, then serve it.

Public catalog reads use `/{slug}` and no auth. Staff product writes use a JWT with the `staff` role. Cart routes accept any valid JWT. Product import and export use API keys scoped to `catalog`.
