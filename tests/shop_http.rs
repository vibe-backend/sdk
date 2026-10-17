use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::{json, Value};
use tower::ServiceExt;
use vibe::compile::{compile_project, CompileOptions};
use vibe::manifest::manifest_from_project;
use vibe::runtime::{hash_api_secret, router, AppState, AuthConfig, MemoryExec, Slug, StoredKey};
use vibe::sign;

const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n";

#[tokio::test]
async fn shop_compiles_and_serves_catalog_and_cart() {
    let root = copy_shop();
    let compiled = compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    assert!(compiled.files.len() > 10);

    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: true,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));

    let key = sign::create_dev_key(&root).unwrap();
    let manifest = manifest_from_project(&root, &key).unwrap_or_else(|err| panic!("{err}"));
    assert!(manifest.routes.iter().any(|route| route.public));
    assert!(manifest
        .methods
        .iter()
        .any(|method| method.target == "product.create" && method.crud_action == "create"));

    let pepper = b"dev-pepper".to_vec();
    let read_secret = "readsecretvalue";
    let write_secret = "writesecretvalue";
    let auth = AuthConfig {
        public_pem: PUBLIC_PEM.into(),
        alg: Algorithm::ES256,
        kid: "local".into(),
        iss: "https://auth.localhost/shop".into(),
        aud: "shop".into(),
        pepper: pepper.clone(),
        api_keys: vec![
            stored_key("catalogreadx", read_secret, &pepper, &["catalog:read"]),
            stored_key("catalogwrite", write_secret, &pepper, &["catalog:write"]),
        ],
        api_keys_file: None,
        env_name: "test".into(),
        jwks: None,
        roles_claim: "roles".into(),
        signing_keys: Default::default(),
        route_jwks: Default::default(),
    };
    let loaded = vibe::runtime::load_project_slugs(&root, true).unwrap();
    assert!(loaded.slugs.contains_key("acme"));
    let mut slugs = loaded.slugs;
    slugs.insert(
        "shopslug1".into(),
        Slug {
            tenant_id: "tenant0001".into(),
            public_access: true,
            public_write: false,
        },
    );
    slugs.insert(
        "otherslug".into(),
        Slug {
            tenant_id: "tenant0002".into(),
            public_access: true,
            public_write: false,
        },
    );
    slugs.insert(
        "hidden000".into(),
        Slug {
            tenant_id: "tenant0003".into(),
            public_access: false,
            public_write: false,
        },
    );
    let state = Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth,
        dev_slugs: None,
        slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        disabled: std::collections::HashSet::new(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        trusted_proxies: Vec::new(),
        dev_hints: true,
        challenge: None,
    });
    let app = router(state);

    let health = send(&app, "GET", "/health", None, None).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json["status"], "ok");

    let staff_token = token("staff-user", &["staff"], "tenant0001");
    let customer_token = token("customer-1", &["customer"], "tenant0001");
    let other_token = token("other-user", &["staff"], "tenant0002");
    let admin_token = token("admin-user", &["admin"], "tenant0001");
    let staff = bearer(&staff_token);
    let customer = bearer(&customer_token);
    let other = bearer(&other_token);

    let denied = send(
        &app,
        "POST",
        "/catalog/v1/products",
        None,
        Some(product("sku-1", "Apple", 100)),
    )
    .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    assert!(denied.headers.contains_key("www-authenticate"));

    // Unauthenticated callers get 401 before any schema validation.
    let probe = send(
        &app,
        "POST",
        "/catalog/v1/products",
        None,
        Some(json!({"no_such_field": 1})),
    )
    .await;
    assert_eq!(probe.status, StatusCode::UNAUTHORIZED);

    let forbidden = send(
        &app,
        "POST",
        "/catalog/v1/products",
        Some(customer.clone()),
        Some(product("sku-1", "Apple", 100)),
    )
    .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    let array_body = send(
        &app,
        "POST",
        "/catalog/v1/products",
        Some(staff.clone()),
        Some(json!([{"sku": "x"}])),
    )
    .await;
    assert_eq!(array_body.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        array_body.json["error"]["message"],
        "body must be a JSON object"
    );
    for bad_tenant in [
        "zzzzzzzz, t_tenant0001",
        "TENANT0001",
        "short",
        "tenant0001\"",
        "",
    ] {
        let bad = bearer(&token("staff-user", &["staff"], bad_tenant));
        let rejected = send(
            &app,
            "POST",
            "/catalog/v1/products",
            Some(bad),
            Some(product("sku-bad", "Bad", 1)),
        )
        .await;
        assert_eq!(
            rejected.status,
            StatusCode::UNAUTHORIZED,
            "tenant claim {bad_tenant:?} must be rejected"
        );
    }

    let created = send(
        &app,
        "POST",
        "/catalog/v1/products",
        Some(staff.clone()),
        Some(product("sku-1", "Apple", 100)),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.json["id"].as_str().unwrap().to_string();
    assert!(created
        .headers
        .get("location")
        .unwrap()
        .ends_with(&format!("/{id}")));
    let etag = created.headers.get("etag").unwrap().to_string();
    assert!(etag.starts_with('"'));

    let conflict = send(
        &app,
        "POST",
        "/catalog/v1/products",
        Some(staff.clone()),
        Some(product("sku-1", "Apple again", 100)),
    )
    .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);

    let second = send(
        &app,
        "POST",
        "/catalog/v1/products",
        Some(staff.clone()),
        Some(product("sku-2", "Banana", 250)),
    )
    .await;
    assert_eq!(second.status, StatusCode::CREATED);

    let wrong_method = send(&app, "POST", "/catalog/v1/shopslug1/products", None, None).await;
    assert_eq!(wrong_method.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        wrong_method.headers.get("allow").map(String::as_str),
        Some("GET")
    );

    let missing_slug = send(&app, "GET", "/catalog/v1/nosuchslug/products", None, None).await;
    assert_eq!(missing_slug.status, StatusCode::NOT_FOUND);
    assert!(missing_slug.json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("vibe slug add"));
    let hidden = send(&app, "GET", "/catalog/v1/hidden000/products", None, None).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(hidden.json["error"]["message"], "Not found");
    let acme = send(&app, "GET", "/catalog/v1/acme/products", None, None).await;
    assert_eq!(acme.status, StatusCode::OK);
    assert_eq!(acme.json["items"].as_array().unwrap().len(), 0);

    let page = send(
        &app,
        "GET",
        "/catalog/v1/shopslug1/products?sort=name&page_size=1",
        None,
        None,
    )
    .await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(page.json["items"][0]["name"], "Apple");
    assert_eq!(page.json["has_more"], true);
    assert_eq!(page.json["page"], 1);
    assert!(page.json["items"][0].get("tenant_id").is_none());

    let page2 = send(
        &app,
        "GET",
        "/catalog/v1/shopslug1/products?sort=name&page_size=1&page=2",
        None,
        None,
    )
    .await;
    assert_eq!(page2.json["items"][0]["name"], "Banana");
    assert_eq!(page2.json["has_more"], false);

    let filtered = send(
        &app,
        "GET",
        "/catalog/v1/shopslug1/products?name=Banana",
        None,
        None,
    )
    .await;
    assert_eq!(filtered.json["items"].as_array().unwrap().len(), 1);

    let other_tenant = send(&app, "GET", "/catalog/v1/otherslug/products", None, None).await;
    assert_eq!(other_tenant.json["items"].as_array().unwrap().len(), 0);

    let got = send(
        &app,
        "GET",
        &format!("/catalog/v1/shopslug1/products/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.json["sku"], "sku-1");

    let missing_match = send(
        &app,
        "PUT",
        &format!("/catalog/v1/products/{id}"),
        Some(staff.clone()),
        Some(json!({"name": "Apricot"})),
    )
    .await;
    assert_eq!(missing_match.status, StatusCode::PRECONDITION_REQUIRED);

    let bad_match = send(
        &app,
        "PUT",
        &format!("/catalog/v1/products/{id}"),
        Some(format!(
            "If-Match: \"deadbeef\"\nAuthorization: Bearer {staff_token}"
        )),
        Some(json!({"name": "Apricot"})),
    )
    .await;
    assert_eq!(bad_match.status, StatusCode::PRECONDITION_FAILED);

    let updated = send(
        &app,
        "PUT",
        &format!("/catalog/v1/products/{id}"),
        Some(format!(
            "If-Match: {etag}\nAuthorization: Bearer {staff_token}"
        )),
        Some(json!({"name": "Apricot"})),
    )
    .await;
    assert_eq!(updated.status, StatusCode::OK);
    assert_eq!(updated.json["name"], "Apricot");
    let new_etag = updated.headers.get("etag").unwrap().to_string();
    assert_ne!(new_etag, etag, "a successful update must change the ETag");

    // The ETag from before the update is now stale: 412, and nothing changes.
    let stale = send(
        &app,
        "PUT",
        &format!("/catalog/v1/products/{id}"),
        Some(format!(
            "If-Match: {etag}\nAuthorization: Bearer {staff_token}"
        )),
        Some(json!({"name": "Overwritten"})),
    )
    .await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let after_stale = send(
        &app,
        "GET",
        &format!("/catalog/v1/shopslug1/products/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(after_stale.json["name"], "Apricot");

    let removed = send(
        &app,
        "DELETE",
        &format!("/catalog/v1/products/{id}"),
        Some(staff.clone()),
        None,
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);

    let gone = send(
        &app,
        "GET",
        &format!("/catalog/v1/shopslug1/products/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    let listed = send(
        &app,
        "GET",
        "/catalog/v1/shopslug1/products?name=Apricot",
        None,
        None,
    )
    .await;
    assert_eq!(listed.json["items"].as_array().unwrap().len(), 0);

    let read_key = api_key("catalogreadx", read_secret);
    let write_key = api_key("catalogwrite", write_secret);
    let import_denied = send(
        &app,
        "POST",
        "/catalog/v1/imports",
        Some(read_key.clone()),
        Some(product("sku-3", "Cherry", 50)),
    )
    .await;
    assert_eq!(import_denied.status, StatusCode::FORBIDDEN);

    let imported = send(
        &app,
        "POST",
        "/catalog/v1/imports",
        Some(write_key.clone()),
        Some(product("sku-3", "Cherry", 50)),
    )
    .await;
    assert_eq!(imported.status, StatusCode::CREATED);

    let exported = send(&app, "GET", "/catalog/v1/exports", Some(read_key), None).await;
    assert_eq!(exported.status, StatusCode::OK);
    assert!(exported.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["sku"] == "sku-3"));
    let exported_by_write = send(&app, "GET", "/catalog/v1/exports", Some(write_key), None).await;
    assert_eq!(exported_by_write.status, StatusCode::OK);

    let product_id = second.json["id"].as_str().unwrap();
    let spoofed = send(
        &app,
        "POST",
        "/shop/v1/cart-items",
        Some(customer.clone()),
        Some(json!({"product_id": product_id, "qty": 1, "user_id": "staff-user"})),
    )
    .await;
    assert_eq!(spoofed.status, StatusCode::UNPROCESSABLE_ENTITY);

    let cart = send(
        &app,
        "POST",
        "/shop/v1/cart-items",
        Some(customer.clone()),
        Some(json!({"product_id": product_id, "qty": 2})),
    )
    .await;
    assert_eq!(cart.status, StatusCode::CREATED);
    // #30: the second rule (`max: 99`) is enforced too.
    let too_many = send(
        &app,
        "POST",
        "/shop/v1/cart-items",
        Some(customer.clone()),
        Some(json!({"product_id": product_id, "qty": 5000})),
    )
    .await;
    assert_eq!(too_many.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(cart.json["user_id"], "customer-1");
    let cart_id = cart.json["id"].as_str().unwrap();

    let mine = send(
        &app,
        "GET",
        "/shop/v1/cart-items",
        Some(customer.clone()),
        None,
    )
    .await;
    assert_eq!(mine.json["items"].as_array().unwrap().len(), 1);
    let staff_view = send(
        &app,
        "GET",
        "/shop/v1/cart-items",
        Some(staff.clone()),
        None,
    )
    .await;
    assert_eq!(staff_view.json["items"].as_array().unwrap().len(), 0);
    let admin = bearer(&admin_token);
    let admin_view = send(&app, "GET", "/shop/v1/cart-items", Some(admin), None).await;
    assert_eq!(admin_view.json["items"].as_array().unwrap().len(), 1);
    let theirs = send(&app, "GET", "/shop/v1/cart-items", Some(other), None).await;
    assert_eq!(theirs.json["items"].as_array().unwrap().len(), 0);

    let stolen = send(
        &app,
        "DELETE",
        &format!("/shop/v1/cart-items/{cart_id}"),
        Some(staff),
        None,
    )
    .await;
    assert_eq!(stolen.status, StatusCode::NOT_FOUND);
    let dropped = send(
        &app,
        "DELETE",
        &format!("/shop/v1/cart-items/{cart_id}"),
        Some(customer),
        None,
    )
    .await;
    assert_eq!(dropped.status, StatusCode::NO_CONTENT);

    // Route rate limit: list-products allows 600 requests per minute per IP.
    // The 601st from the same IP gets 429 with Retry-After; another IP is
    // unaffected.
    let limited_ip = "X-Vibe-Test-Ip: 198.51.100.7".to_string();
    for n in 0..600 {
        let ok = send(
            &app,
            "GET",
            "/catalog/v1/shopslug1/products",
            Some(limited_ip.clone()),
            None,
        )
        .await;
        assert_eq!(ok.status, StatusCode::OK, "request {n} within the limit");
    }
    let limited = send(
        &app,
        "GET",
        "/catalog/v1/shopslug1/products",
        Some(limited_ip.clone()),
        None,
    )
    .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.json["error"]["code"], "RATE_LIMITED");
    let retry: u64 = limited.headers["retry-after"].parse().unwrap();
    assert!((1..=60).contains(&retry), "retry-after {retry}");
    let other_ip = send(
        &app,
        "GET",
        "/catalog/v1/shopslug1/products",
        Some("X-Vibe-Test-Ip: 198.51.100.8".into()),
        None,
    )
    .await;
    assert_eq!(other_ip.status, StatusCode::OK);
}

#[tokio::test]
async fn legacy_approval_fields_are_ignored_with_a_warning() {
    // v1.8: locks carry no approval metadata or signature. Older locks that
    // still have them validate (with warnings) and build a manifest.
    let root = compiled_shop().await;
    let lock = root.join("api/catalog/v1/get-product/get-product.lock.md");
    let text = fs::read_to_string(&lock).unwrap();
    let front = text.split("\n---\n").next().unwrap();
    assert!(
        !front.contains("status:") && !front.contains("signature:"),
        "{front}"
    );
    let legacy = text.replacen(
        "security-items:",
        "status: approved\napproval-mode: manual\napproved-by: [a@x, b@x]\napproved-at: 2026-10-14T00:00:00Z\napproval-ref: local\napproved-commit: local\nsecurity-items:",
        1,
    );
    let legacy = legacy.replacen("reason: \"\" }", "reason: \"\", approved-by: [a@x] }", 1);
    let legacy = legacy.replacen("\n---\n", "\nsignature: abc123\n---\n", 1);
    fs::write(&lock, &legacy).unwrap();
    let report = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    })
    .unwrap_or_else(|err| panic!("{err}"));
    let text = report.to_string();
    let file = "api/catalog/v1/get-product/get-product.lock.md";
    let lines: Vec<&str> = text.lines().filter(|l| l.contains(file)).collect();
    assert_eq!(lines.len(), 1, "one summary line per file: {text}");
    for field in [
        "status",
        "approved-by",
        "signature",
        "security-items approved-by",
    ] {
        assert!(lines[0].contains(&format!("`{field}`")), "{field}: {text}");
    }
}

fn copy_shop() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-shop-{}", uuid::Uuid::new_v4()));
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop");
    copy_dir(&src, &root);
    root
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn stored_key(key_id: &str, secret: &str, pepper: &[u8], scopes: &[&str]) -> StoredKey {
    StoredKey {
        key_id: key_id.into(),
        secret_hash: hash_api_secret(pepper, secret),
        env: "test".into(),
        tenant_id: "tenant0001".into(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        expires_unix: chrono::Utc::now().timestamp() + 86_400,
        revoked: false,
    }
}

fn product(sku: &str, name: &str, price_cents: i64) -> Value {
    json!({"sku": sku, "name": name, "price_cents": price_cents})
}

fn bearer(token: &str) -> String {
    format!("Authorization: Bearer {token}")
}

fn api_key(key_id: &str, secret: &str) -> String {
    format!("X-API-Key: mbk_test_{key_id}_{secret}")
}

fn token(sub: &str, roles: &[&str], tenant: &str) -> String {
    #[derive(Serialize)]
    struct Claims<'a> {
        sub: &'a str,
        exp: i64,
        iat: i64,
        nbf: i64,
        iss: &'a str,
        aud: &'a str,
        tenant_id: &'a str,
        roles: Vec<&'a str>,
    }
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub,
        exp: now + 600,
        iat: now,
        nbf: now,
        iss: "https://auth.localhost/shop",
        aud: "shop",
        tenant_id: tenant,
        roles: roles.to_vec(),
    };
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("local".into());
    let key = EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap();
    encode(&header, &claims, &key).unwrap()
}

struct HttpResult {
    status: StatusCode,
    json: Value,
    headers: std::collections::HashMap<String, String>,
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    extra_headers: Option<String>,
    body: Option<Value>,
) -> HttpResult {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(raw) = extra_headers {
        for line in raw.lines() {
            let (name, value) = line.split_once(':').unwrap();
            builder = builder.header(name.trim(), value.trim());
        }
    }
    let request = builder
        .body(Body::from(
            body.map(|value| value.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let mut headers = std::collections::HashMap::new();
    for (name, value) in response.headers() {
        headers.insert(
            name.as_str().to_string(),
            value.to_str().unwrap_or("").to_string(),
        );
    }
    let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    HttpResult {
        status,
        json,
        headers,
    }
}

#[tokio::test]
async fn owner_key_id_is_rejected_outside_users() {
    let root = copy_shop();
    let source = root.join("models/cart-item/cart-item.md");
    let text = fs::read_to_string(&source).unwrap();
    fs::write(
        &source,
        text.replace("- owner-key: user_id", "- owner-key: id"),
    )
    .unwrap();
    let report = compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .expect("compile must reject an invalid owner key");
    let rendered = report.to_string();
    assert!(
        rendered.contains("owner-key: id is only allowed on the users table"),
        "{rendered}"
    );
}

#[tokio::test]
async fn admin_without_ownership_role_cannot_touch_other_carts() {
    let root = copy_shop();
    let source = root.join("models/cart-item/cart-item.md");
    let text = fs::read_to_string(&source).unwrap();
    fs::write(&source, text.replace("- ownership: role(admin)\n", "")).unwrap();
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    let key = sign::create_dev_key(&root).unwrap();
    let manifest = manifest_from_project(&root, &key).unwrap_or_else(|err| panic!("{err}"));
    let cart_model = manifest
        .models
        .iter()
        .find(|m| m.table == "cart_items")
        .unwrap();
    assert_eq!(cart_model.ownership, "");
    let mut auth = AuthConfig::anonymous();
    auth.public_pem = PUBLIC_PEM.into();
    auth.alg = Algorithm::ES256;
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth,
        dev_slugs: None,
        slugs: std::collections::HashMap::new(),
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        trusted_proxies: Vec::new(),
        disabled: std::collections::HashSet::new(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        dev_hints: true,
        challenge: None,
    }));
    let customer = bearer(&token("customer-1", &["customer"], "tenant0001"));
    let admin = bearer(&token("admin-user", &["admin"], "tenant0001"));
    let cart = send(
        &app,
        "POST",
        "/shop/v1/cart-items",
        Some(customer.clone()),
        Some(json!({"product_id": uuid::Uuid::new_v4().to_string(), "qty": 1})),
    )
    .await;
    assert_eq!(cart.status, StatusCode::CREATED, "{:?}", cart.json);
    let cart_id = cart.json["id"].as_str().unwrap();
    let listed = send(
        &app,
        "GET",
        "/shop/v1/cart-items",
        Some(admin.clone()),
        None,
    )
    .await;
    assert_eq!(listed.json["items"].as_array().unwrap().len(), 0);
    let removed = send(
        &app,
        "DELETE",
        &format!("/shop/v1/cart-items/{cart_id}"),
        Some(admin),
        None,
    )
    .await;
    assert_eq!(removed.status, StatusCode::NOT_FOUND);
    let mine = send(&app, "GET", "/shop/v1/cart-items", Some(customer), None).await;
    assert_eq!(mine.json["items"].as_array().unwrap().len(), 1);
}

async fn compiled_shop() -> PathBuf {
    let root = copy_shop();
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    root
}

fn validate_text(root: &Path) -> String {
    match vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        dev: false,
        root,
        deploy: false,
    }) {
        Ok(r) | Err(r) => r.to_string(),
    }
}

fn edit(root: &Path, rel: &str, from: &str, to: &str) {
    let path = root.join(rel);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{rel} lacks {from:?}");
    fs::write(&path, text.replacen(from, to, 1)).unwrap();
}

#[tokio::test]
async fn validator_rejects_steps_the_runtime_cannot_run() {
    let root = compiled_shop().await;
    let lock = "api/catalog/v1/create-product/create-product.lock.md";
    edit(
        &root,
        lock,
        "- target: product.create",
        "- method: product.create",
    );
    edit(&root, lock, "- in: {", "- in: {sku: ,");
    let text = validate_text(&root);
    assert!(
        text.contains("unknown key `method` for op method (use `target:`)"),
        "{text}"
    );
    assert!(text.contains("method step needs `target`"), "{text}");
    assert!(text.contains("(s1)"), "errors name the step: {text}");
    assert!(
        text.contains("[Transform]") && text.contains("transform in"),
        "{text}"
    );
}

#[tokio::test]
async fn validator_reports_every_broken_file_at_once() {
    let root = compiled_shop().await;
    edit(
        &root,
        "api/catalog/v1/create-product/create-product.lock.md",
        "- value: transform.out\n- status: 201",
        "- value: transform.out\n- status: 9999",
    );
    edit(
        &root,
        "api/shop/v1/add-cart-item/add-cart-item.lock.md",
        "- target: cart-item.create",
        "- target: cart-item.create\n- bogus: 1",
    );
    let text = validate_text(&root);
    assert!(
        text.contains("create-product.lock.md") && text.contains("9999"),
        "{text}"
    );
    assert!(
        text.contains("add-cart-item.lock.md") && text.contains("bogus"),
        "{text}"
    );
}

#[tokio::test]
async fn manifest_output_is_relative_to_the_current_directory() {
    let root = compiled_shop().await;
    sign::create_dev_key(&root).unwrap();
    let cwd = std::env::temp_dir().join(format!("vibe-cwd-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&cwd).unwrap();
    let out = dev_command()
        .args(["manifest", "--dev", "--project"])
        .arg(&root)
        .args(["-o", "out.json"])
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(cwd.join("out.json").exists());
    assert!(!root.join("out.json").exists());
}

const PRODUCT_LOCK: &str = "models/product/product.lock.md";

fn tamper_product_lock(root: &Path) {
    let lock = root.join(PRODUCT_LOCK);
    let text = fs::read_to_string(&lock).unwrap();
    assert!(text.contains("max: 120"), "{text}");
    fs::write(&lock, text.replacen("max: 120", "max: 100000", 1)).unwrap();
}

fn deploy_validate(root: &Path) -> Result<vibe::diag::Report, vibe::diag::Report> {
    vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        dev: false,
        root,
        deploy: true,
    })
}

#[tokio::test]
async fn deploy_validate_rejects_a_hand_edited_lock() {
    let root = compiled_shop().await;
    deploy_validate(&root).unwrap_or_else(|err| panic!("{err}"));
    tamper_product_lock(&root);
    let text = deploy_validate(&root).unwrap_err().to_string();
    assert!(text.contains(PRODUCT_LOCK), "{text}");
    assert!(text.contains("hand-edited"), "{text}");
    // Dependents' hashes go stale, which plain validation also reports.
    let text = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    })
    .unwrap_err()
    .to_string();
    assert!(text.contains("dependency product hash"), "{text}");
}

#[tokio::test]
async fn manifest_refuses_a_hand_edited_lock_and_writes_nothing() {
    let root = compiled_shop().await;
    tamper_product_lock(&root);
    let key = sign::generate_key();
    let err = manifest_from_project(&root, &key).unwrap_err().to_string();
    assert!(err.contains("hand-edited"), "{err}");
    let out = root.join("out/manifest.json");
    let status = dev_command()
        .args(["manifest", "--dev", "--project"])
        .arg(&root)
        .arg("-o")
        .arg(&out)
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!out.exists(), "no partial manifest");
}

#[tokio::test]
async fn manifest_refuses_locks_that_fail_validation() {
    let root = compiled_shop().await;
    let lock = root.join("api/catalog/v1/get-product/get-product.lock.md");
    let text = fs::read_to_string(&lock).unwrap();
    fs::write(&lock, text.replace("name: md-logic", "name: hand-edited")).unwrap();
    let key = sign::generate_key();
    let err = manifest_from_project(&root, &key).unwrap_err().to_string();
    assert!(err.contains("compiler name is not md-logic"), "{err}");
}

#[tokio::test]
async fn manifest_refuses_stale_locks() {
    let root = compiled_shop().await;
    let source = root.join("api/catalog/v1/get-product/get-product.md");
    let text = fs::read_to_string(&source).unwrap();
    fs::write(&source, text.replace("timeout: 10s", "timeout: 9s")).unwrap();
    let key = sign::generate_key();
    let err = manifest_from_project(&root, &key).unwrap_err().to_string();
    assert!(err.contains("source-hash does not match"), "{err}");
}

#[tokio::test]
async fn manifest_uses_the_validated_text_not_a_later_edit() {
    // #34: read once; an edit after loading cannot reach the signed output.
    let root = compiled_shop().await;
    let loaded = vibe::validate::load_project(&root).unwrap();
    tamper_product_lock(&root);
    let key = sign::generate_key();
    let manifest = vibe::manifest::manifest_from_loaded(&root, &loaded, &key)
        .unwrap_or_else(|err| panic!("{err}"));
    let json = serde_json::to_string(&manifest).unwrap();
    assert!(
        !json.contains("100000"),
        "tampered value leaked into the manifest"
    );
    assert!(json.contains("120"));
}

#[tokio::test]
async fn manifest_with_only_warnings_builds_and_reports_them() {
    let root = compiled_shop().await;
    let report = deploy_validate(&root).unwrap_or_else(|err| panic!("{err}"));
    assert!(report.to_string().contains("no owner-key"), "{report}");
    assert!(!report.has_errors());
    let key = sign::generate_key();
    manifest_from_project(&root, &key).unwrap_or_else(|err| panic!("{err}"));
}

/// Rewrites the build record so only the targeted check can fail.
fn rerecord(root: &Path) {
    let loaded = vibe::validate::load_project(root).unwrap();
    let mut locks = std::collections::BTreeMap::new();
    for (source, text) in loaded.sources.iter().zip(&loaded.lock_texts) {
        if let Some(text) = text {
            let rel = source.rel.trim_end_matches(".md").to_string() + ".lock.md";
            locks.insert(rel, vibe::buildrecord::lock_hash(text));
        }
    }
    vibe::buildrecord::write(root, locks).unwrap();
}

#[tokio::test]
async fn legacy_rejected_or_pending_locks_are_ignored_with_one_warning() {
    for status in ["rejected", "pending"] {
        let root = compiled_shop().await;
        let lock = root.join("api/catalog/v1/get-product/get-product.lock.md");
        let text = fs::read_to_string(&lock).unwrap();
        fs::write(
            &lock,
            text.replacen(
                "security-items:",
                &format!("status: {status}\nsecurity-items:"),
                1,
            ),
        )
        .unwrap();
        rerecord(&root);
        for args in [
            vec!["validate", "--project", "."],
            vec!["validate", "--deploy", "--dev", "--project", "."],
        ] {
            let (ok, out) = vibe_cli(&root, &args, &[]);
            assert!(ok, "{status}: {out}");
            let lines: Vec<&str> = out
                .lines()
                .filter(|line| line.contains("get-product.lock.md"))
                .collect();
            assert_eq!(lines.len(), 1, "one warning per file: {out}");
            assert!(
                lines[0].contains("legacy approval fields are ignored: `status`"),
                "{out}"
            );
        }
        let (ok, out) = vibe_cli(&root, &["manifest", "--dev", "--project", "."], &[]);
        assert!(ok, "{status}: {out}");
    }
}

#[tokio::test]
async fn tampered_dependency_hash_fails_deploy_and_manifest() {
    let root = compiled_shop().await;
    let lock = root.join("api/catalog/v1/get-product/get-product.lock.md");
    let text = fs::read_to_string(&lock).unwrap();
    let start = text.find("{ id: product, kind: model, hash: ").unwrap();
    let line_end = start + text[start..].find('}').unwrap();
    let edited = format!(
        "{}{{ id: product, kind: model, hash: sha256:deadbeef {}",
        &text[..start],
        &text[line_end..]
    );
    fs::write(&lock, edited).unwrap();
    rerecord(&root);
    let err = deploy_validate(&root).unwrap_err().to_string();
    assert!(
        err.contains("dependency product hash does not match"),
        "{err}"
    );
    let key = sign::generate_key();
    let err = manifest_from_project(&root, &key).unwrap_err().to_string();
    assert!(
        err.contains("dependency product hash does not match"),
        "{err}"
    );
}

#[tokio::test]
async fn removing_roles_from_a_lock_cannot_deploy() {
    let root = compiled_shop().await;
    let lock = root.join("api/catalog/v1/create-product/create-product.lock.md");
    let text = fs::read_to_string(&lock).unwrap();
    assert!(text.contains("- roles: [staff]\n"));
    fs::write(&lock, text.replacen("- roles: [staff]\n", "", 1)).unwrap();
    // Even with a forged build record the policy check refuses it.
    rerecord(&root);
    let err = deploy_validate(&root).unwrap_err().to_string();
    assert!(err.contains("create-product.lock.md"), "{err}");
    assert!(err.contains("roles"), "{err}");
    let key = sign::generate_key();
    assert!(manifest_from_project(&root, &key).is_err());
}

fn dev_command() -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"));
    // Development flows must not inherit the test runner's CI environment.
    cmd.env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env("VIBE_ENV", "development");
    cmd
}

fn vibe_cli(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = dev_command();
    cmd.args(args).current_dir(root);
    for key in [
        "VIBE_ENV",
        "VIBE_RELEASE_VERSION",
        "VIBE_MANIFEST_BUNDLE",
        "GITHUB_SHA",
        "GITHUB_REF_NAME",
        "LLM_API_KEY",
        "VIBE_LLM_API_KEY",
        "OPENAI_API_KEY",
    ] {
        cmd.env_remove(key);
    }
    for (key, _) in std::env::vars() {
        if key.starts_with("LLM_") || key.starts_with("OPENAI_") {
            cmd.env_remove(key);
        }
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[tokio::test]
async fn manifest_dev_creates_a_key_and_keygen_refuses_overwrite() {
    let root = compiled_shop().await;
    let (ok, out) = vibe_cli(&root, &["manifest", "--dev", "--project", "."], &[]);
    assert!(ok, "{out}");
    assert!(root.join(".vibe/signing.key").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.join(".vibe/signing.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::set_permissions(
            root.join(".vibe/signing.key"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let (ok, out) = vibe_cli(&root, &["manifest", "--dev", "--project", "."], &[]);
        assert!(!ok);
        assert!(out.contains("must be mode 0600"), "{out}");
    }
    let (ok, out) = vibe_cli(&root, &["keygen", "--project", "."], &[]);
    assert!(!ok && out.contains("not overwriting"), "{out}");
}

#[tokio::test]
async fn manifest_defaults_to_release() {
    let root = compiled_shop().await;
    let (ok, out) = vibe_cli(&root, &["manifest", "--project", "."], &[]);
    let (release_ok, release_out) =
        vibe_cli(&root, &["manifest", "--release", "--project", "."], &[]);
    assert!(!ok, "{out}");
    assert_eq!(ok, release_ok);
    assert_eq!(out, release_out);
    assert!(!root.join(".vibe/signing.key").exists());
    let (ok, out) = vibe_cli(&root, &["manifest", "--dev", "--release"], &[]);
    assert!(!ok && out.contains("cannot be used with"), "{out}");
}

#[tokio::test]
async fn approve_is_gone() {
    let root = copy_shop();
    let (ok, out) = vibe_cli(&root, &["approve", "--reviewer", "a@x"], &[]);
    assert!(!ok);
    assert!(
        out.contains("has been removed in spec v1.8 (decision 77)"),
        "{out}"
    );
}

#[tokio::test]
async fn release_build_needs_a_version_and_a_full_commit() {
    let root = copy_shop();
    let (ok, out) = vibe_cli(
        &root,
        &[
            "manifest",
            "--project",
            ".",
            "--release",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
        ],
        &[],
    );
    assert!(!ok);
    assert!(out.contains("--release-version"), "{out}");
    let (ok, out) = vibe_cli(
        &root,
        &[
            "manifest",
            "--project",
            ".",
            "--release",
            "--release-version",
            "1.8.0",
            "--commit",
            "abc123",
        ],
        &[],
    );
    assert!(!ok);
    assert!(out.contains("40-character"), "{out}");
}

#[tokio::test]
async fn release_build_ignores_lock_files_in_the_repo() {
    // (b) An old or hand-edited lock committed to the repo is never a
    // release input: the release build compiles from source in memory.
    let root = compiled_shop().await;
    tamper_product_lock(&root);
    rerecord(&root);
    let (loaded, _) = vibe::compile::compile_in_memory(&root, None, None)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let product = loaded
        .lock_texts
        .iter()
        .flatten()
        .find(|text| text.contains("id: product\n"))
        .expect("product lock");
    assert!(
        !product.contains("100000"),
        "repo lock leaked into the release"
    );
    let on_disk = fs::read_to_string(root.join(PRODUCT_LOCK)).unwrap();
    assert_ne!(*product, on_disk);
    // The CLI release build compiles from source, lists high-risk items
    // without blocking (decision 90 withdrawn), and writes an unsigned
    // manifest for the workflow's Sigstore keyless signature.
    let (ok, out) = vibe_cli(
        &root,
        &[
            "manifest",
            "--project",
            ".",
            "--release",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
            "-o",
            "release.json",
        ],
        &[("GITHUB_REF_NAME", "v1.8.0")],
    );
    assert!(ok, "{out}");
    assert!(out.contains("high-risk items"), "{out}");
    let release: vibe::manifest::Manifest =
        serde_json::from_slice(&fs::read(root.join("release.json")).unwrap()).unwrap();
    assert_eq!(release.build_mode, Some(vibe::manifest::BuildMode::Release));
    assert_eq!(release.provenance.version, "1.8.0");
    assert_eq!(
        release.provenance.commit,
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert!(release.signature.is_empty());
    assert!(!serde_json::to_string(&release.models)
        .unwrap()
        .contains("100000"));
    // The same old lock cannot reach a dev manifest either.
    sign::create_dev_key(&root).unwrap();
    let (ok, out) = vibe_cli(&root, &["manifest", "--dev", "--project", "."], &[]);
    assert!(!ok, "{out}");
}

#[tokio::test]
async fn release_build_never_reads_the_project_env_file() {
    let root = compiled_shop().await;
    fs::write(
        root.join(".env"),
        "LLM_BASE_URL=https://attacker.example/v1\nLLM_API_KEY=sk-from-env-file\nLLM_MODEL=gpt-5.1-2026-01-01\n",
    )
    .unwrap();
    let (ok, out) = vibe_cli(
        &root,
        &[
            "manifest",
            "--project",
            ".",
            "--release",
            "--release-version",
            "1.8.0",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
            "-o",
            "release.json",
        ],
        &[],
    );
    assert!(ok, "{out}");
    assert!(!out.contains("attacker.example"), "{out}");
}

#[tokio::test]
async fn release_build_refuses_a_non_allowlisted_llm_host() {
    let root = compiled_shop().await;
    let (ok, out) = vibe_cli(
        &root,
        &[
            "manifest",
            "--project",
            ".",
            "--release",
            "--release-version",
            "1.8.0",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
            "-o",
            "release.json",
        ],
        &[
            ("VIBE_LLM_API_KEY", "sk-release-compiler-key"),
            ("LLM_MODEL", "gpt-5.1-2026-01-01"),
            ("LLM_BASE_URL", "https://llm.attacker.example/v1"),
        ],
    );
    assert!(!ok);
    assert!(out.contains("allowlisted host"), "{out}");
    assert!(out.contains("llm.attacker.example"), "{out}");
    assert!(!root.join("release.json").exists());
    // The default endpoint (LLM_BASE_URL unset) is allowlisted.
    let config = vibe::llm::load_llm_config(
        &[
            ("LLM_API_KEY".to_string(), "sk-test-value".to_string()),
            ("LLM_MODEL".to_string(), "gpt-5.1-2026-01-01".to_string()),
        ]
        .into_iter()
        .collect(),
        true,
    )
    .unwrap();
    assert_eq!(config.endpoint_host, "api.openai.com");
    vibe::llm::check_release_endpoint(&config).unwrap();
}

#[tokio::test]
async fn release_build_is_immune_to_lock_edits_during_the_build() {
    // (a) Locks are compiled, validated, hashed and signed in memory in one
    // process; writing a lock file mid-build changes nothing.
    let root = copy_shop();
    let (loaded, _) = vibe::compile::compile_in_memory(&root, None, None)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    // Mid-build: drop a hand-edited lock (and a forged record) on disk.
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap();
    tamper_product_lock(&root);
    rerecord(&root);
    let key = sign::generate_key();
    let manifest = vibe::manifest::build_signed(
        &root,
        &loaded,
        Some(&key),
        &vibe::manifest::BuildInfo {
            release: false,
            version: String::new(),
            commit: "abc".into(),
        },
    )
    .unwrap_or_else(|err| panic!("{err}"));
    let json = serde_json::to_string(&manifest.models).unwrap();
    assert!(!json.contains("100000"), "mid-build edit leaked");
    let product = manifest
        .provenance
        .locks
        .iter()
        .find(|l| l.id == "product")
        .unwrap();
    let on_disk = fs::read_to_string(root.join(PRODUCT_LOCK)).unwrap();
    assert_ne!(product.lock_sha256, vibe::buildrecord::lock_hash(&on_disk));
}

#[tokio::test]
async fn release_build_lists_high_risk_items_and_takes_no_key() {
    let root = copy_shop();
    let (loaded, _) = vibe::compile::compile_in_memory(&root, None, None)
        .await
        .unwrap();
    let info = vibe::manifest::BuildInfo {
        release: true,
        version: "1.8.0".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
    };
    let manifest = vibe::manifest::build_signed(&root, &loaded, None, &info)
        .unwrap_or_else(|err| panic!("{err}"));
    assert!(!manifest.provenance.high_risk().is_empty());
    assert!(manifest.signature.is_empty());
    // A release manifest is never signed with a key.
    let key = sign::generate_key();
    let err = vibe::manifest::build_signed(&root, &loaded, Some(&key), &info)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Sigstore keyless only"), "{err}");
}

#[tokio::test]
async fn production_runs_only_release_manifests() {
    let root = compiled_shop().await;
    let key = sign::generate_key();
    let dev = manifest_from_project(&root, &key).unwrap();
    assert_eq!(dev.build_mode, Some(vibe::manifest::BuildMode::Dev));
    assert!(!dev.provenance.locks.is_empty());
    vibe::manifest::check_deployable(&dev, true).unwrap();
    let err = vibe::manifest::check_deployable(&dev, false).unwrap_err();
    assert!(err.contains("only release manifests"), "{err}");
    // A dev manifest relabelled as release still has a key signature and no
    // Sigstore bundle, so it is refused before anything runs.
    let mut forged = dev.clone();
    forged.build_mode = Some(vibe::manifest::BuildMode::Release);
    forged.provenance.version = "1.8.0".into();
    let path = root.join("forged.json");
    vibe::manifest::write_manifest(&path, &forged).unwrap();
    let err =
        vibe::manifest::read_release_manifest(&path, &root.join("none.sigstore.json"), "1.8.0")
            .unwrap_err()
            .to_string();
    assert!(err.contains("no key signature"), "{err}");
    forged.signature.clear();
    vibe::manifest::write_manifest(&path, &forged).unwrap();
    let err =
        vibe::manifest::read_release_manifest(&path, &root.join("none.sigstore.json"), "1.8.1")
            .unwrap_err()
            .to_string();
    assert!(err.contains("not the deployed version"), "{err}");
    let err =
        vibe::manifest::read_release_manifest(&path, &root.join("none.sigstore.json"), "1.8.0")
            .unwrap_err()
            .to_string();
    assert!(err.contains("Sigstore bundle"), "{err}");
}

#[tokio::test]
async fn production_serve_ignores_the_project_public_key() {
    let root = compiled_shop().await;
    sign::create_dev_key(&root).unwrap();
    let (ok, out) = vibe_cli(&root, &["manifest", "--dev", "--project", "."], &[]);
    assert!(ok, "{out}");
    let (ok, out) = vibe_cli(
        &root,
        &["serve", "--project", ".", "--addr", "127.0.0.1:0"],
        &[
            ("VIBE_ENV", "production"),
            (
                "VIBE_API_KEY_PEPPER",
                "0123456789abcdef0123456789abcdef0123",
            ),
            ("VIBE_DATABASE_URL", "postgres://invalid.invalid/x"),
        ],
    );
    assert!(!ok);
    assert!(
        out.contains("this manifest was built with --dev and is refused"),
        "{out}"
    );
    // With a release version set, the dev manifest is still refused.
    let (ok, out) = vibe_cli(
        &root,
        &["serve", "--project", ".", "--addr", "127.0.0.1:0"],
        &[
            ("VIBE_ENV", "production"),
            (
                "VIBE_API_KEY_PEPPER",
                "0123456789abcdef0123456789abcdef0123",
            ),
            ("VIBE_DATABASE_URL", "postgres://invalid.invalid/x"),
            ("VIBE_RELEASE_VERSION", "1.8.0"),
        ],
    );
    assert!(!ok);
    assert!(
        out.contains("this manifest was built with --dev and is refused"),
        "{out}"
    );
}

#[tokio::test]
async fn auth_mode_change_is_high_risk_and_keeps_the_inherited_roles() {
    let root = compiled_shop().await;
    let lock =
        fs::read_to_string(root.join("api/catalog/v1/import-product/import-product.lock.md"))
            .unwrap();
    assert!(lock.contains("auth-mode-change"), "{lock}");

    // A mode change cannot widen the inherited roles.
    let root = copy_shop();
    edit(
        &root,
        "api/catalog/v1/import-product/import-product.md",
        "- mode: api-key",
        "- mode: api-key\n- roles: [staff, customer]",
    );
    let err = compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .map(|r| r.to_string())
    .unwrap_or_default();
    let text = if err.is_empty() {
        validate_text(&root)
    } else {
        err
    };
    assert!(
        text.contains("auth role customer widens the inherited roles"),
        "{text}"
    );
}

#[tokio::test]
async fn auth_mode_change_without_the_high_risk_item_fails() {
    let root = compiled_shop().await;
    let rel = "api/catalog/v1/import-product/import-product.lock.md";
    let text = fs::read_to_string(root.join(rel)).unwrap();
    let stripped: String = text
        .lines()
        .filter(|line| !line.contains("auth-mode-change"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_ne!(text, stripped);
    fs::write(root.join(rel), stripped).unwrap();
    let report = validate_text(&root);
    assert!(
        report.contains("not recorded as a high-risk item")
            || report.contains("security-items do not match"),
        "{report}"
    );
}

#[tokio::test]
async fn auth_roles_merge_every_parent_auth_block() {
    // The version adds its own auth block; the definition's roles still apply.
    let root = copy_shop();
    edit(
        &root,
        "api/catalog/v1/_version.md",
        "## Pipelines\n\n(none)",
        "## Pipelines\n\n### auth\n\n- mode: jwt\n- roles: [admin]",
    );
    edit(
        &root,
        "api/catalog/v1/import-product/import-product.md",
        "- mode: api-key",
        "- mode: jwt\n- roles: [staff, admin, customer]",
    );
    let err = compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .map(|r| r.to_string())
    .unwrap_or_default();
    let text = if err.is_empty() {
        validate_text(&root)
    } else {
        err
    };
    assert!(text.contains("auth role customer widens"), "{text}");
    assert!(!text.contains("auth role staff widens"), "{text}");
}

/// Lock `### auth` iss/aud/jwks-uri flow through validate, the manifest and
/// the HTTP router: only the route issuer's tokens are accepted there.
#[tokio::test]
async fn route_jwt_overrides_end_to_end() {
    struct Keys;
    #[async_trait::async_trait]
    impl vibe::runtime::jwks::JwksSource for Keys {
        async fn fetch(&self) -> Result<Value, String> {
            Ok(json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "r1",
                "x": "0mqNqll14aIJEhYqCiRLWJX63Ty6Q0UxBzbmJ2wI9FM",
                "y": "FSS8OFxIbiCRaiHQPz-m-qM0jRsjlGSuzoyHxiraTWM"}]}))
        }
    }
    const IDP: &str = "https://idp.example.com/t1";
    const URI: &str = "https://idp.example.com/t1/keys";
    let build = |jwks: &'static str| async move {
        let root = copy_shop();
        // v1.8: locks must match a recompile, so the override lives in the source.
        let source = root.join("api/catalog/v1/create-product/create-product.md");
        let text = fs::read_to_string(&source).unwrap();
        fs::write(
            &source,
            text.replacen(
                "## Pipelines\n\n(none)",
                &format!(
                    "## Pipelines\n\n### auth\n\n- mode: jwt\n- iss: {IDP}\n- aud: catalog-api\n- jwks-uri: {jwks}\n- roles: [staff]"
                ),
                1,
            ),
        )
        .unwrap();
        let compiled = compile_project(&CompileOptions {
            root: &root,
            llm: None,
            llm_config: None,
            check: false,
        })
        .await;
        if let Err(err) = compiled {
            return (root, Err(err.to_string()));
        }
        let report = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
            dev: false,
            root: &root,
            deploy: false,
        });
        (
            root,
            report.map(|r| r.to_string()).map_err(|r| r.to_string()),
        )
    };
    // A private JWKS host is a validate error.
    let (_, bad) = build("https://169.254.169.254/keys").await;
    let err = bad.unwrap_err();
    assert!(err.contains("host must be public"), "{err}");

    let (root, ok) = build(URI).await;
    ok.unwrap_or_else(|err| panic!("{err}"));
    // v1.8: no approve step.
    let key = sign::generate_key();
    let manifest = manifest_from_project(&root, &key).unwrap_or_else(|err| panic!("{err}"));
    let route = manifest
        .routes
        .iter()
        .find(|r| r.path == "/catalog/v1/products" && r.method == "POST")
        .unwrap();
    assert_eq!(
        (
            route.jwt_iss.as_str(),
            route.jwt_aud.as_str(),
            route.jwks_uri.as_str()
        ),
        (IDP, "catalog-api", URI)
    );
    let mut auth = AuthConfig::anonymous();
    auth.public_pem = PUBLIC_PEM.into();
    auth.alg = Algorithm::ES256;
    auth.route_jwks.insert(
        URI.into(),
        Arc::new(vibe::runtime::jwks::JwksCache::new(Arc::new(Keys))),
    );
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth,
        dev_slugs: None,
        slugs: std::collections::HashMap::new(),
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        trusted_proxies: Vec::new(),
        disabled: std::collections::HashSet::new(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        dev_hints: true,
        challenge: None,
    }));
    let now = chrono::Utc::now().timestamp();
    let mint = |patch: Value, alg: Algorithm| {
        let mut claims = json!({
            "sub": "staff-1", "exp": now + 600, "iat": now, "nbf": now,
            "iss": IDP, "aud": "catalog-api", "tenant_id": "tenant0001", "roles": ["staff"]
        });
        for (k, v) in patch.as_object().unwrap() {
            claims[k] = v.clone();
        }
        let mut header = Header::new(alg);
        header.kid = Some("r1".into());
        let key = if alg == Algorithm::HS256 {
            EncodingKey::from_secret(b"guess")
        } else {
            EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap()
        };
        bearer(&encode(&header, &claims, &key).unwrap())
    };
    let post = |auth: String, sku: &'static str| {
        let app = app.clone();
        async move {
            send(
                &app,
                "POST",
                "/catalog/v1/products",
                Some(auth),
                Some(product(sku, "Widget", 100)),
            )
            .await
        }
    };
    let created = post(mint(json!({}), Algorithm::ES256), "SKU-1").await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.json);
    // The default issuer's token is refused on this route.
    let env = post(bearer(&token("staff-1", &["staff"], "tenant0001")), "SKU-2").await;
    assert_eq!(env.status, StatusCode::UNAUTHORIZED);
    for (label, patch, alg) in [
        ("wrong aud", json!({"aud": "shop"}), Algorithm::ES256),
        (
            "wrong iss",
            json!({"iss": "https://auth.localhost/shop"}),
            Algorithm::ES256,
        ),
        (
            "expired",
            json!({"exp": now - 120, "iat": now - 600, "nbf": now - 600}),
            Algorithm::ES256,
        ),
        (
            "not yet valid",
            json!({"nbf": now + 600, "exp": now + 900}),
            Algorithm::ES256,
        ),
        (
            "bad tenant",
            json!({"tenant_id": "x, t_other"}),
            Algorithm::ES256,
        ),
        ("alg mismatch", json!({}), Algorithm::HS256),
    ] {
        let r = post(mint(patch, alg), "SKU-3").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{label}");
    }
    // Roles still apply on override routes.
    let r = post(
        mint(json!({"roles": ["customer"]}), Algorithm::ES256),
        "SKU-4",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn private_endpoint_deployment_requires_signed_dev_mode() {
    use vibe::lock::{compiler_hash, parse_lock, render_lock};
    use vibe::manifest::{
        manifest_from_project_with_mode, read_manifest, write_manifest, BuildMode,
    };
    use vibe::validate::{validate_project, ValidateOptions};

    let root = copy_shop();
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let lock_path = root.join("api/catalog/v1/get-product/get-product.lock.md");
    let mut lock = parse_lock(&fs::read_to_string(&lock_path).unwrap()).unwrap();
    lock.meta.compiler.endpoint_private = true;
    lock.meta.compiler.endpoint_host = "localhost".into();
    lock.meta.compiler_hash = compiler_hash(&lock.meta.compiler);
    let docs = std::collections::HashMap::from([(lock.meta.id.clone(), lock.clone())]);
    assert!(vibe::manifest::build_manifest(&docs)
        .unwrap_err()
        .to_string()
        .contains("private endpoint cannot be deployed"));
    fs::write(&lock_path, render_lock(&lock)).unwrap();
    rerecord(&root);
    let key = sign::load_or_create(&root).unwrap();
    let verify = key.verifying_key();
    for dev in [false, true] {
        let result = validate_project(&ValidateOptions {
            root: &root,
            deploy: true,
            dev,
        });
        if dev {
            result.unwrap_or_else(|e| panic!("{e}"));
        } else {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("private endpoint cannot be deployed"));
        }
    }
    for dev in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"));
        command
            .env_clear()
            .env("VIBE_ENV", "development")
            .args(["validate", "--deploy", "--project"])
            .arg(&root);
        if dev {
            command.arg("--dev");
        }
        let output = command.output().unwrap();
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), dev, "{err}");
        if !dev {
            assert!(err.contains("private endpoint cannot be deployed"), "{err}");
        }
    }
    assert!(manifest_from_project(&root, &key).is_err());
    let manifest =
        manifest_from_project_with_mode(&root, &key, true).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(manifest.build_mode, Some(BuildMode::Dev));
    let path = root.join("dev.json");
    write_manifest(&path, &manifest).unwrap();
    assert_eq!(read_manifest(&path, &verify).unwrap(), manifest);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .env("VIBE_ENV", "production")
        .env(
            "VIBE_API_KEY_PEPPER",
            "test-production-pepper-at-least-32-bytes",
        )
        .args(["serve", "--project"])
        .arg(&root)
        .arg("--manifest")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("this manifest was built with --dev and is refused when VIBE_ENV is"),
        "{err}"
    );

    let mut json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("build_mode");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(read_manifest(&path, &verify)
        .unwrap_err()
        .to_string()
        .contains("signature is invalid"));

    // A release manifest omits the field, preserving the older signed format.
    let mut release = vibe::manifest::build_manifest(&Default::default()).unwrap();
    release.signature = sign::sign_bytes(&serde_json::to_vec(&release).unwrap(), &key);
    write_manifest(&path, &release).unwrap();
    assert!(read_manifest(&path, &verify).unwrap().build_mode.is_none());
}

#[tokio::test]
async fn memory_dev_default_and_live_slug_reload() {
    let root = copy_shop();
    fs::remove_file(root.join("slugs.example.json")).unwrap();
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap();
    let key = sign::create_dev_key(&root).unwrap();
    let manifest = manifest_from_project(&root, &key).unwrap();
    let mut slugs = vibe::runtime::load_project_slugs(&root, true)
        .unwrap()
        .slugs;
    assert!(vibe::runtime::apply_dev_default(&mut slugs, true, true));
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth: AuthConfig::anonymous(),
        dev_slugs: Some(vibe::runtime::DevSlugs::new(&root, slugs.clone())),
        slugs,
        rates: Default::default(),
        quotas: Default::default(),
        disabled: Default::default(),
        min_override_ms: Some(0),
        honor_test_ip: false,
        trusted_proxies: vec![],
        dev_hints: true,
        challenge: None,
    }));
    let get = |slug: &str| {
        Request::builder()
            .uri(format!("/catalog/v1/{slug}/products"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(get("dev")).await.unwrap().status(),
        StatusCode::OK
    );
    vibe::runtime::add_slug(&root, "newslug", "tenant0001", false).unwrap();
    assert_eq!(
        app.clone().oneshot(get("newslug")).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone().oneshot(get("dev")).await.unwrap().status(),
        StatusCode::OK
    );
    fs::write(root.join(".vibe/slugs.json"), "broken").unwrap();
    // A miss forces the changed-file check; errors retain the last valid map.
    app.clone().oneshot(get("missing")).await.unwrap();
    assert_eq!(
        app.oneshot(get("newslug")).await.unwrap().status(),
        StatusCode::OK
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn missing_definition_suggests_scaffold() {
    let root = copy_shop();
    fs::remove_file(root.join("api/shop/_definition.md")).unwrap();
    let error = compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("api/shop/_definition.md"), "{error}");
    assert!(error.contains("Run `vibe` (interactive)"), "{error}");
    fs::remove_dir_all(root).unwrap();
}
