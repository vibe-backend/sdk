//! PostgreSQL over TLS. These run when `VIBE_TEST_PG_TLS_URL` points at a
//! TLS-only server (CI starts one with per-run certificates from
//! `scripts/ci/pg-tls-certs.sh`) and `VIBE_TEST_PG_TLS_CERTS` names the
//! directory holding `ca.crt` and `other-ca.crt`. The URL host must be
//! `localhost`, the only name in the server certificate. When
//! `VIBE_TEST_PG_TLS_REQUIRED` is set, a missing setup fails instead of
//! skipping.

use vibe::runtime::PgExec;

struct Tls {
    url: url::Url,
    certs: std::path::PathBuf,
}

fn tls() -> Option<Tls> {
    let url = std::env::var("VIBE_TEST_PG_TLS_URL").ok();
    let certs = std::env::var("VIBE_TEST_PG_TLS_CERTS").ok();
    match (url, certs) {
        (Some(url), Some(certs)) => Some(Tls {
            url: url::Url::parse(&url).expect("VIBE_TEST_PG_TLS_URL"),
            certs: certs.into(),
        }),
        _ => {
            if std::env::var_os("VIBE_TEST_PG_TLS_REQUIRED").is_some() {
                panic!("VIBE_TEST_PG_TLS_URL / VIBE_TEST_PG_TLS_CERTS are not set; the TLS tests cannot be skipped");
            }
            eprintln!("SKIPPED: VIBE_TEST_PG_TLS_URL not set; skipping Postgres TLS test");
            None
        }
    }
}

impl Tls {
    fn url(&self, host: &str, query: &[(&str, &str)]) -> String {
        let mut url = self.url.clone();
        url.set_host(Some(host)).unwrap();
        url.set_query(None);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        url.to_string()
    }

    fn ca(&self, name: &str) -> String {
        self.certs.join(name).to_string_lossy().into_owned()
    }
}

async fn connect_err(url: &str) -> String {
    match PgExec::connect(url).await {
        Ok(_) => panic!("connection to {url} must be refused"),
        Err(err) => err,
    }
}

/// Refused during the TLS handshake because of the server certificate.
async fn cert_rejected(url: &str) {
    let err = connect_err(url).await;
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("tls handshake") || lower.contains("certificate"),
        "expected a TLS handshake/certificate error, got: {err}"
    );
}

#[tokio::test]
async fn require_connects_with_trusted_ca_and_channel_binding() {
    let Some(t) = tls() else { return };
    let ca = t.ca("ca.crt");
    let url = t.url(
        "localhost",
        &[
            ("sslmode", "require"),
            ("sslrootcert", &ca),
            ("channel_binding", "require"),
        ],
    );
    let exec = PgExec::connect(&url).await.unwrap();
    let rows = exec
        .query_texts(
            "SELECT ssl::text || ':' || $1 FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            "tls",
        )
        .await
        .unwrap();
    assert_eq!(rows, vec!["true:tls".to_string()]);
}

#[tokio::test]
async fn verify_full_with_trusted_ca_connects() {
    let Some(t) = tls() else { return };
    let ca = t.ca("ca.crt");
    let url = t.url(
        "localhost",
        &[("sslmode", "verify-full"), ("sslrootcert", &ca)],
    );
    PgExec::connect(&url).await.unwrap();
}

#[tokio::test]
async fn untrusted_ca_is_refused() {
    let Some(t) = tls() else { return };
    // Only the webpki roots: the per-run test CA is not among them.
    cert_rejected(&t.url("localhost", &[("sslmode", "require")])).await;
    // An unrelated private CA.
    let other = t.ca("other-ca.crt");
    cert_rejected(&t.url(
        "localhost",
        &[("sslmode", "require"), ("sslrootcert", &other)],
    ))
    .await;
}

#[tokio::test]
async fn hostname_mismatch_is_refused() {
    let Some(t) = tls() else { return };
    let ca = t.ca("ca.crt");
    // Same server, trusted CA, but the certificate only names `localhost`.
    cert_rejected(&t.url("127.0.0.1", &[("sslmode", "require"), ("sslrootcert", &ca)])).await;
}

#[tokio::test]
async fn plaintext_is_refused_by_the_tls_only_server() {
    let Some(t) = tls() else { return };
    connect_err(&t.url("localhost", &[("sslmode", "disable")])).await;
}

#[tokio::test]
async fn unreadable_root_cert_is_an_error() {
    let Some(t) = tls() else { return };
    let missing = t.ca("does-not-exist.crt");
    let err = connect_err(&t.url(
        "localhost",
        &[("sslmode", "require"), ("sslrootcert", &missing)],
    ))
    .await;
    assert!(err.contains("sslrootcert"), "{err}");
}
