# Sigstore test fixtures

- `gha-dsse.sigstore.json` — public-good Sigstore bundle (v0.3) from a GitHub
  Actions run (`wolfv/sigstore-test`), used to exercise identity checks after
  Fulcio/Rekor verification. Subject digest
  `59ed81ee7a2485c47588ebdbad14764bf722c93438b43fe953a651747bc62ad7`.
- `bundle-v0.1-set-only.sigstore.json` — bundle media type v0.1 with a SET and
  no inclusion proof; must be refused.
