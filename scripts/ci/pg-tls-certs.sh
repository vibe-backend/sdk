#!/usr/bin/env bash
# Generates throwaway TLS material for the Postgres TLS integration tests.
# Run once per CI job; nothing here is a secret or is stored anywhere.
#
#   scripts/ci/pg-tls-certs.sh <out-dir>
#
# Writes:
#   ca.crt          test CA (trusted by the tests via sslrootcert)
#   server.crt/.key server certificate for DNS:localhost only (no IP SAN),
#                   ECDSA P-256 / SHA-256 so SCRAM channel binding works
#   other-ca.crt    an unrelated CA, used to check that an untrusted CA fails
#   pg_hba.conf     TLS-only, SCRAM-SHA-256 authentication
set -euo pipefail

out=${1:?usage: pg-tls-certs.sh <out-dir>}
mkdir -p "$out"
cd "$out"

ec() { openssl ecparam -name prime256v1 -genkey -noout -out "$1"; }

ec ca.key
openssl req -x509 -new -key ca.key -sha256 -days 2 -subj "/CN=vibe-ci-test-ca" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out ca.crt

ec other-ca.key
openssl req -x509 -new -key other-ca.key -sha256 -days 2 -subj "/CN=vibe-ci-other-ca" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out other-ca.crt

ec server.key
openssl req -new -key server.key -subj "/CN=localhost" -out server.csr
printf '%s\n' \
  "basicConstraints=critical,CA:FALSE" \
  "keyUsage=critical,digitalSignature" \
  "extendedKeyUsage=serverAuth" \
  "subjectAltName=DNS:localhost" > server.ext
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -sha256 -days 2 \
  -extfile server.ext -out server.crt
chmod 600 server.key
rm -f ca.key other-ca.key server.csr server.ext ca.srl

cat > pg_hba.conf <<'HBA'
local     all all     trust
hostssl   all all all scram-sha-256
hostnossl all all all reject
HBA
