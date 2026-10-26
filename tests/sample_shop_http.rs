#![cfg(feature = "sdk")]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use vibe::runtime::{ensure_dev_jwt, router, AppState, AuthConfig, MemoryExec, Slug};
use vibe::shop_compile::sample_project_files;

#[tokio::test]
async fn sample_shop_memory_and_fake_llm_cover_the_contract() {
    let root = std::env::temp_dir().join(format!("vibe-sample-shop-{}", uuid::Uuid::new_v4()));
    for (rel, text) in sample_project_files() {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::write(
        root.join(".gitignore"),
        ".env\n*.lock.md\nmanifest.json\n.vibe/*\n!.vibe/slugs.json\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".vibe")).unwrap();
    std::fs::write(
        root.join(".vibe/slugs.json"),
        r#"{"dev":{"tenant_id":"devtenant","public_access":true,"public_write":false}}"#,
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"))
        .current_dir(&root)
        .env_clear()
        .env("LLM_PROVIDER", "fake")
        .arg("build")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let key = vibe::sign::load_dev_key(&root).unwrap();
    let manifest = vibe::manifest::manifest_from_project(&root, &key).unwrap();
    assert!(manifest.routes.len() >= 30, "{}", manifest.routes.len());
    let mut auth = AuthConfig::anonymous();
    ensure_dev_jwt(&mut auth).unwrap();
    let mut slugs = std::collections::HashMap::new();
    slugs.insert(
        "dev".into(),
        Slug {
            tenant_id: "devtenant".into(),
            public_access: true,
            public_write: false,
        },
    );
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth,
        slugs,
        dev_slugs: None,
        rates: Default::default(),
        quotas: Default::default(),
        disabled: Default::default(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        trusted_proxies: Vec::new(),
        dev_hints: false,
        challenge: None,
    }));

    let anon = send(&app, "GET", "/api/users/v1/me", &[], None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);

    let bad = send(
        &app,
        "POST",
        "/api/users/v1/login",
        &[],
        Some(json!({"email": "alice@example.com", "password": "not-the-password"})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert_eq!(bad.json["error"]["code"], "INVALID_CREDENTIALS");

    let weak = send(
        &app,
        "POST",
        "/api/users/v1/register",
        &[],
        Some(json!({"email": "short@example.com", "password": "password123", "display_name": "Short"})),
    )
    .await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);

    let buyer = send(
        &app,
        "POST",
        "/api/users/v1/login",
        &[],
        Some(json!({"email": "alice@example.com", "password": "password123"})),
    )
    .await;
    assert_eq!(buyer.status, StatusCode::OK, "{:?}", buyer.json);
    let alice = buyer.json["access_token"].as_str().unwrap().to_string();
    assert!(buyer.json["expires_at"].as_str().unwrap().ends_with('Z'));

    let me = send(
        &app,
        "GET",
        "/api/users/v1/me",
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(me.json["email"], "alice@example.com");
    assert_eq!(me.json["roles"], json!([]));

    let staff_login = send(
        &app,
        "POST",
        "/api/users/v1/login",
        &[],
        Some(json!({"email": "staff@example.com", "password": "password123"})),
    )
    .await;
    let staff = staff_login.json["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    let staff_me = send(
        &app,
        "GET",
        "/api/users/v1/me",
        &[auth_header(&staff)],
        None,
    )
    .await;
    assert_eq!(staff_me.json["roles"], json!(["staff"]));

    let created = send(
        &app,
        "POST",
        "/api/users/v1/register",
        &[],
        Some(json!({
            "email": "new@example.com",
            "password": "horse-battery-staple",
            "display_name": "New"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.json);
    let newbie = created.json["access_token"].as_str().unwrap().to_string();
    let newbie_me = send(
        &app,
        "GET",
        "/api/users/v1/me",
        &[auth_header(&newbie)],
        None,
    )
    .await;
    assert_eq!(newbie_me.json["display_name"], "New");
    assert_eq!(newbie_me.json["roles"], json!([]));
    let taken = send(
        &app,
        "POST",
        "/api/users/v1/register",
        &[],
        Some(json!({
            "email": "new@example.com",
            "password": "horse-battery-staple",
            "display_name": "New"
        })),
    )
    .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.json["error"]["code"], "EMAIL_TAKEN");

    let listed = send(
        &app,
        "GET",
        "/api/catalog/v1/products?sort=price&order=asc",
        &[],
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{:?}", listed.json);
    let names: Vec<_> = listed.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["North Mug", "North Tee", "Vibe Headphones"]);
    assert!(
        listed.json["items"][1]["variants"]
            .as_array()
            .unwrap()
            .len()
            == 2
    );

    let filtered = send(
        &app,
        "GET",
        "/api/catalog/v1/products?q=tee&category=apparel&brands=North&min_price_cents=8000&max_price_cents=10000&attributes=%7B%22material%22%3A%5B%22cotton%22%5D%7D",
        &[],
        None,
    )
    .await;
    assert_eq!(filtered.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered.json["items"][0]["slug"], "north-tee");

    let hidden = send(
        &app,
        "GET",
        "/api/catalog/v1/products/66666666-6666-4666-8666-666666666666",
        &[],
        None,
    )
    .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(hidden.json["error"]["code"], "PRODUCT_NOT_FOUND");

    let phones = send(
        &app,
        "GET",
        "/api/catalog/v1/products/33333333-3333-4333-8333-333333333333",
        &[],
        None,
    )
    .await;
    assert_eq!(phones.status, StatusCode::OK);
    assert_eq!(
        phones
            .headers
            .get("etag")
            .and_then(|value| value.to_str().ok()),
        Some("\"1\"")
    );

    let staff_hidden = send(
        &app,
        "GET",
        "/api/catalog/v1/staff/products/66666666-6666-4666-8666-666666666666",
        &[auth_header(&staff)],
        None,
    )
    .await;
    assert_eq!(staff_hidden.status, StatusCode::OK);
    assert_eq!(staff_hidden.json["active"], false);

    let missing_match = send(
        &app,
        "PUT",
        "/api/catalog/v1/products/33333333-3333-4333-8333-333333333333",
        &[auth_header(&staff)],
        Some(json!({"name": "Renamed"})),
    )
    .await;
    assert_eq!(missing_match.status, StatusCode::PRECONDITION_REQUIRED);
    let wrong_match = send(
        &app,
        "PUT",
        "/api/catalog/v1/products/33333333-3333-4333-8333-333333333333",
        &[auth_header(&staff), "If-Match: \"9\"".to_string()],
        Some(json!({"name": "Renamed"})),
    )
    .await;
    assert_eq!(wrong_match.status, StatusCode::PRECONDITION_FAILED);
    let renamed = send(
        &app,
        "PUT",
        "/api/catalog/v1/products/33333333-3333-4333-8333-333333333333",
        &[auth_header(&staff), "If-Match: \"1\"".to_string()],
        Some(json!({"name": "Renamed Headphones"})),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::OK, "{:?}", renamed.json);
    assert_eq!(
        renamed
            .headers
            .get("etag")
            .and_then(|value| value.to_str().ok()),
        Some("\"2\"")
    );
    assert_eq!(renamed.json["name"], "Renamed Headphones");

    let made = send(
        &app,
        "POST",
        "/api/catalog/v1/products",
        &[auth_header(&staff)],
        Some(json!({
            "slug": "desk-pen",
            "name": "Desk Pen",
            "price_cents": 1200,
            "currency": "HKD",
            "category": "home",
            "stock": 4,
            "active": true
        })),
    )
    .await;
    assert_eq!(made.status, StatusCode::CREATED, "{:?}", made.json);
    let pen = made.json["id"].as_str().unwrap();
    assert_eq!(
        made.headers
            .get("etag")
            .and_then(|value| value.to_str().ok()),
        Some("\"1\"")
    );

    let variants = send(
        &app,
        "GET",
        "/api/catalog/v1/staff/products/44444444-4444-4444-8444-444444444444/variants",
        &[auth_header(&staff)],
        None,
    )
    .await;
    assert_eq!(variants.status, StatusCode::OK);
    assert!(variants.json.as_array().unwrap().len() >= 2);
    let sku = variants.json[0]["id"].as_str().unwrap().to_string();
    let version = variants.json[0]["version"].as_i64().unwrap();
    let added_sku = send(
        &app,
        "POST",
        "/api/catalog/v1/products/44444444-4444-4444-8444-444444444444/variants",
        &[auth_header(&staff)],
        Some(json!({"options": {"size": "L", "color": "navy"}, "price_cents": 10900, "stock": 2})),
    )
    .await;
    assert_eq!(
        added_sku.status,
        StatusCode::CREATED,
        "{:?}",
        added_sku.json
    );
    let edited_sku = send(
        &app,
        "PUT",
        &format!("/api/catalog/v1/products/44444444-4444-4444-8444-444444444444/variants/{sku}"),
        &[auth_header(&staff), format!("If-Match: \"{version}\"")],
        Some(json!({"stock": 6})),
    )
    .await;
    assert_eq!(edited_sku.status, StatusCode::OK, "{:?}", edited_sku.json);
    assert_eq!(edited_sku.json["stock"], 6);

    let cart = send(
        &app,
        "POST",
        "/api/shop/v1/cart-items",
        &[auth_header(&alice)],
        Some(json!({"product_id": "33333333-3333-4333-8333-333333333333", "quantity": 2})),
    )
    .await;
    assert_eq!(cart.status, StatusCode::OK, "{:?}", cart.json);
    assert_eq!(cart.json["items"][0]["quantity"], 2);
    assert_eq!(cart.json["subtotal_cents"], 12900 * 2);
    let again = send(
        &app,
        "POST",
        "/api/shop/v1/cart-items",
        &[auth_header(&alice)],
        Some(json!({"product_id": "33333333-3333-4333-8333-333333333333", "quantity": 1})),
    )
    .await;
    assert_eq!(again.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(again.json["items"][0]["quantity"], 3);
    let with_sku = send(
        &app,
        "POST",
        "/api/shop/v1/cart-items",
        &[auth_header(&alice)],
        Some(json!({"product_id": "44444444-4444-4444-8444-444444444444", "sku_id": sku, "quantity": 1})),
    )
    .await;
    assert_eq!(with_sku.json["items"].as_array().unwrap().len(), 2);
    let line = with_sku.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sku"]["id"] == sku)
        .unwrap();
    assert_eq!(line["sku"]["options"]["size"], "S");
    let tee_line = line["id"].as_str().unwrap().to_string();
    let phone_line = again.json["items"][0]["id"].as_str().unwrap().to_string();
    let cleared = send(
        &app,
        "PUT",
        &format!("/api/shop/v1/cart-items/{phone_line}"),
        &[auth_header(&alice)],
        Some(json!({"quantity": 0})),
    )
    .await;
    assert_eq!(cleared.json["items"].as_array().unwrap().len(), 1);
    let removed = send(
        &app,
        "DELETE",
        &format!("/api/shop/v1/cart-items/{tee_line}"),
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(removed.json["items"], json!([]));
    let got = send(
        &app,
        "GET",
        "/api/shop/v1/cart-items",
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(got.json["subtotal_cents"], 0);

    let home = send(
        &app,
        "POST",
        "/api/shop/v1/addresses",
        &[auth_header(&alice)],
        Some(json!({"recipient": "Alice", "phone": "91234567", "region": "香港島", "line1": "1 Home"})),
    )
    .await;
    assert_eq!(home.status, StatusCode::CREATED, "{:?}", home.json);
    assert_eq!(home.json["is_default"], true);
    let home_id = home.json["id"].as_str().unwrap().to_string();
    let office = send(
        &app,
        "POST",
        "/api/shop/v1/addresses",
        &[auth_header(&alice)],
        Some(json!({"recipient": "Alice", "phone": "91234567", "region": "九龍", "line1": "2 Office", "is_default": false})),
    )
    .await;
    let office_id = office.json["id"].as_str().unwrap().to_string();
    assert_eq!(office.json["is_default"], false);
    let listed_addresses = send(
        &app,
        "GET",
        "/api/shop/v1/addresses",
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(listed_addresses.json[0]["id"], home_id);
    let office_default = send(
        &app,
        "POST",
        &format!("/api/shop/v1/addresses/{office_id}/default"),
        &[auth_header(&alice)],
        Some(json!({})),
    )
    .await;
    assert_eq!(office_default.json["is_default"], true);
    let edited = send(
        &app,
        "PUT",
        &format!("/api/shop/v1/addresses/{office_id}"),
        &[auth_header(&alice)],
        Some(json!({"recipient": "Alice Work", "phone": "91234567", "region": "九龍", "line1": "2B Office", "is_default": false})),
    )
    .await;
    assert_eq!(edited.json["line1"], "2B Office");
    assert_eq!(edited.json["is_default"], true);
    let deleted = send(
        &app,
        "DELETE",
        &format!("/api/shop/v1/addresses/{office_id}"),
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let after = send(
        &app,
        "GET",
        "/api/shop/v1/addresses",
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(after.json.as_array().unwrap().len(), 1);
    assert_eq!(after.json[0]["id"], home_id);
    assert_eq!(after.json[0]["is_default"], true);

    let stocked = send(
        &app,
        "POST",
        "/api/shop/v1/cart-items",
        &[auth_header(&alice)],
        Some(json!({"product_id": "33333333-3333-4333-8333-333333333333", "quantity": 1})),
    )
    .await;
    let item_id = stocked.json["items"][0]["id"].as_str().unwrap().to_string();
    let missing_key = send(
        &app,
        "POST",
        "/api/shop/v1/orders",
        &[auth_header(&alice)],
        Some(json!({"address_id": home_id, "note": "hi"})),
    )
    .await;
    assert_eq!(missing_key.status, StatusCode::UNPROCESSABLE_ENTITY);
    let order_body = json!({"address_id": home_id, "item_ids": [item_id], "note": "leave at door"});
    let ordered = send(
        &app,
        "POST",
        "/api/shop/v1/orders",
        &[auth_header(&alice), "Idempotency-Key: order-1".to_string()],
        Some(order_body.clone()),
    )
    .await;
    assert_eq!(ordered.status, StatusCode::CREATED, "{:?}", ordered.json);
    assert_eq!(ordered.json["status"], "pending");
    assert!(ordered.json["payment_method"].is_null());
    assert!(ordered.json["paid_at"].is_null());
    assert_eq!(ordered.json["status_history"][0]["status"], "pending");
    assert_eq!(ordered.json["shipping_address"]["line1"], "1 Home");
    assert_eq!(ordered.json["total_cents"], 12900);
    let order_id = ordered.json["id"].as_str().unwrap().to_string();
    let replay = send(
        &app,
        "POST",
        "/api/shop/v1/orders",
        &[auth_header(&alice), "Idempotency-Key: order-1".to_string()],
        Some(order_body),
    )
    .await;
    assert_eq!(replay.json["id"], order_id);
    let reused = send(
        &app,
        "POST",
        "/api/shop/v1/orders",
        &[auth_header(&alice), "Idempotency-Key: order-1".to_string()],
        Some(json!({"address_id": home_id, "note": "different"})),
    )
    .await;
    assert_eq!(reused.status, StatusCode::CONFLICT);
    assert_eq!(reused.json["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    let empty_cart = send(
        &app,
        "GET",
        "/api/shop/v1/cart-items",
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(empty_cart.json["items"], json!([]));

    let declined = send(
        &app,
        "POST",
        &format!("/api/shop/v1/orders/{order_id}/payments"),
        &[auth_header(&alice), "Idempotency-Key: pay-fail".to_string()],
        Some(json!({"method": "mock_card", "simulate": "failure"})),
    )
    .await;
    assert_eq!(declined.status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(declined.json["error"]["code"], "PAYMENT_DECLINED");
    let still = send(
        &app,
        "GET",
        &format!("/api/shop/v1/orders/{order_id}"),
        &[auth_header(&alice)],
        None,
    )
    .await;
    assert_eq!(still.json["status"], "pending");
    let paid = send(
        &app,
        "POST",
        &format!("/api/shop/v1/orders/{order_id}/payments"),
        &[auth_header(&alice), "Idempotency-Key: pay-ok".to_string()],
        Some(json!({"method": "mock_card", "simulate": "success"})),
    )
    .await;
    assert_eq!(paid.status, StatusCode::OK, "{:?}", paid.json);
    assert_eq!(paid.json["status"], "paid");
    assert_eq!(paid.json["payment_method"], "mock_card");
    assert!(paid.json["paid_at"].is_string());
    let cancelled = send(
        &app,
        "POST",
        &format!("/api/shop/v1/orders/{order_id}/cancel"),
        &[auth_header(&alice)],
        Some(json!({"reason": "changed mind"})),
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{:?}", cancelled.json);
    assert_eq!(cancelled.json["status"], "cancelled");
    assert_eq!(cancelled.json["cancellation_reason"], "changed mind");

    let restocked = send(
        &app,
        "GET",
        "/api/catalog/v1/products/33333333-3333-4333-8333-333333333333",
        &[],
        None,
    )
    .await;
    assert_eq!(restocked.json["stock"], 25);

    let second = send(
        &app,
        "POST",
        "/api/shop/v1/orders",
        &[auth_header(&alice), "Idempotency-Key: order-2".to_string()],
        Some(json!({
            "address_id": home_id,
            "buy_now": {"product_id": "55555555-5555-4555-8555-555555555555", "quantity": 2}
        })),
    )
    .await;
    assert_eq!(second.status, StatusCode::CREATED, "{:?}", second.json);
    let second_id = second.json["id"].as_str().unwrap();
    let paid_second = send(
        &app,
        "POST",
        &format!("/api/shop/v1/orders/{second_id}/payments"),
        &[auth_header(&alice), "Idempotency-Key: pay-2".to_string()],
        Some(json!({"method": "mock_alipay", "simulate": "success"})),
    )
    .await;
    assert_eq!(paid_second.json["status"], "paid");
    let staff_orders = send(
        &app,
        "GET",
        "/api/shop/v1/staff/orders?status=paid",
        &[auth_header(&staff)],
        None,
    )
    .await;
    assert_eq!(
        staff_orders.status,
        StatusCode::OK,
        "{:?}",
        staff_orders.json
    );
    assert_eq!(
        staff_orders.json["items"][0]["buyer"]["email"],
        "alice@example.com"
    );
    let shipped = send(
        &app,
        "POST",
        &format!("/api/shop/v1/staff/orders/{second_id}/status"),
        &[auth_header(&staff)],
        Some(json!({"status": "shipped"})),
    )
    .await;
    assert_eq!(shipped.status, StatusCode::OK, "{:?}", shipped.json);
    assert_eq!(shipped.json["status"], "shipped");
    let delivered = send(
        &app,
        "POST",
        &format!("/api/shop/v1/orders/{second_id}/confirm-delivery"),
        &[auth_header(&alice)],
        Some(json!({})),
    )
    .await;
    assert_eq!(delivered.json["status"], "delivered");

    let third = send(
        &app,
        "POST",
        "/api/shop/v1/orders",
        &[auth_header(&alice), "Idempotency-Key: order-3".to_string()],
        Some(json!({
            "address_id": home_id,
            "buy_now": {"product_id": "55555555-5555-4555-8555-555555555555", "quantity": 1}
        })),
    )
    .await;
    let third_id = third.json["id"].as_str().unwrap();
    let _paid_third = send(
        &app,
        "POST",
        &format!("/api/shop/v1/orders/{third_id}/payments"),
        &[auth_header(&alice), "Idempotency-Key: pay-3".to_string()],
        Some(json!({"method": "mock_card", "simulate": "success"})),
    )
    .await;
    let put_shipped = send(
        &app,
        "PUT",
        &format!("/api/shop/v1/staff/orders/{third_id}/status"),
        &[auth_header(&staff)],
        Some(json!({"status": "shipped"})),
    )
    .await;
    assert_eq!(put_shipped.status, StatusCode::OK, "{:?}", put_shipped.json);
    assert_eq!(put_shipped.json["buyer"]["id"], me.json["id"]);

    let gone = send(
        &app,
        "DELETE",
        &format!("/api/catalog/v1/products/{pen}"),
        &[auth_header(&staff)],
        None,
    )
    .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    let missing_pen = send(
        &app,
        "GET",
        &format!("/api/catalog/v1/staff/products/{pen}"),
        &[auth_header(&staff)],
        None,
    )
    .await;
    assert_eq!(missing_pen.status, StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(root);
}

struct Sent {
    status: StatusCode,
    json: Value,
    headers: axum::http::HeaderMap,
}

fn auth_header(token: &str) -> String {
    format!("Authorization: Bearer {token}")
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    headers: &[String],
    body: Option<Value>,
) -> Sent {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for line in headers {
        let (name, value) = line.split_once(':').unwrap();
        builder = builder.header(name.trim(), value.trim());
    }
    let request = builder
        .body(Body::from(
            body.map(|value| value.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Sent {
        status,
        json,
        headers,
    }
}
