//! Canonical locks for the sample-shop single-tenant routes.
//!
//! `LLM_PROVIDER=fake` returns these bodies. Normalization installs the same
//! sections for a live model, so either compiler emits a lock the memory
//! runtime can execute.

use crate::lock::{parse_sections, Section};

#[derive(Clone, Copy)]
struct Endpoint {
    id: &'static str,
    method: &'static str,
    path: &'static str,
    /// `public-read`, `public-write`, `user-read`, `user-write`, `staff-read`, `staff-write`.
    access: &'static str,
    crud: Option<&'static str>,
    status: u16,
    request: &'static str,
    errors: &'static str,
}

const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        id: "users/v1/login",
        method: "POST",
        path: "/users/v1/login",
        access: "public-read",
        crud: None,
        status: 200,
        request: "\
| body.email | string | format: email, max: 254 | yes |
| body.password | string | max: 256 | yes |",
        errors: "| 401 | INVALID_CREDENTIALS | INVALID_CREDENTIALS |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "users/v1/register",
        method: "POST",
        path: "/users/v1/register",
        access: "public-write",
        crud: Some("create"),
        status: 201,
        request: "\
| body.email | string | format: email, max: 254 | yes |
| body.password | string | max: 256 | yes |
| body.display_name | string | max: 80 | yes |",
        errors: "| 409 | EMAIL_TAKEN | EMAIL_TAKEN |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "users/v1/me",
        method: "GET",
        path: "/users/v1/me",
        access: "user-read",
        crud: None,
        status: 200,
        request: "",
        errors: "| 401 | UNAUTHORIZED | UNAUTHORIZED |",
    },
    Endpoint {
        id: "catalog/v1/list-products",
        method: "GET",
        path: "/catalog/v1/products",
        access: "public-read",
        crud: None,
        status: 200,
        request: PRODUCT_QUERY,
        errors: "| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "catalog/v1/list-staff-products",
        method: "GET",
        path: "/catalog/v1/staff/products",
        access: "staff-read",
        crud: None,
        status: 200,
        request: PRODUCT_QUERY,
        errors: "| 401 | UNAUTHORIZED | UNAUTHORIZED |\n| 403 | FORBIDDEN | FORBIDDEN |",
    },
    Endpoint {
        id: "catalog/v1/get-product",
        method: "GET",
        path: "/catalog/v1/products/{id}",
        access: "public-read",
        crud: None,
        status: 200,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |",
    },
    Endpoint {
        id: "catalog/v1/get-staff-product",
        method: "GET",
        path: "/catalog/v1/staff/products/{id}",
        access: "staff-read",
        crud: None,
        status: 200,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |",
    },
    Endpoint {
        id: "catalog/v1/create-product",
        method: "POST",
        path: "/catalog/v1/products",
        access: "staff-write",
        crud: Some("create"),
        status: 201,
        request: PRODUCT_BODY,
        errors: "| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "catalog/v1/update-product",
        method: "PUT",
        path: "/catalog/v1/products/{id}",
        access: "staff-write",
        crud: Some("update"),
        status: 200,
        request: PRODUCT_UPDATE,
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |\n| 412 | PRECONDITION_FAILED | PRECONDITION_FAILED |\n| 428 | PRECONDITION_REQUIRED | PRECONDITION_REQUIRED |",
    },
    Endpoint {
        id: "catalog/v1/delete-product",
        method: "DELETE",
        path: "/catalog/v1/products/{id}",
        access: "staff-write",
        crud: Some("delete"),
        status: 204,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |",
    },
    Endpoint {
        id: "catalog/v1/list-variants",
        method: "GET",
        path: "/catalog/v1/staff/products/{pid}/variants",
        access: "staff-read",
        crud: None,
        status: 200,
        request: "| path.pid | string | max: 80 | yes |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |",
    },
    Endpoint {
        id: "catalog/v1/create-variant",
        method: "POST",
        path: "/catalog/v1/products/{pid}/variants",
        access: "staff-write",
        crud: Some("create"),
        status: 201,
        request: "\
| path.pid | string | max: 80 | yes |
| body.options | json | - | yes |
| body.price_cents | integer | min: 0 | yes |
| body.stock | integer | min: 0 | yes |
| body.image_url | string | max: 500 | no |
| body.active | boolean | - | no |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "catalog/v1/update-variant",
        method: "PUT",
        path: "/catalog/v1/products/{pid}/variants/{sid}",
        access: "staff-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.pid | string | max: 80 | yes |
| path.sid | string | max: 80 | yes |
| header.if-match | string | max: 80 | no |
| body.options | json | - | no |
| body.price_cents | integer | min: 0 | no |
| body.stock | integer | min: 0 | no |
| body.image_url | string | max: 500 | no |
| body.active | boolean | - | no |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |\n| 412 | PRECONDITION_FAILED | PRECONDITION_FAILED |\n| 428 | PRECONDITION_REQUIRED | PRECONDITION_REQUIRED |",
    },
    Endpoint {
        id: "shop/v1/list-cart-items",
        method: "GET",
        path: "/shop/v1/cart-items",
        access: "user-read",
        crud: None,
        status: 200,
        request: "",
        errors: "",
    },
    Endpoint {
        id: "shop/v1/add-cart-item",
        method: "POST",
        path: "/shop/v1/cart-items",
        access: "user-write",
        crud: Some("create"),
        status: 200,
        request: "\
| body.product_id | string | max: 80 | yes |
| body.quantity | integer | min: 1, max: 99 | yes |
| body.sku_id | string | max: 80 | no |",
        errors: "| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "shop/v1/update-cart-item",
        method: "PUT",
        path: "/shop/v1/cart-items/{id}",
        access: "user-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.id | string | max: 80 | yes |
| body.quantity | integer | min: -1, max: 99 | yes |",
        errors: "| 404 | NOT_FOUND | NOT_FOUND |",
    },
    Endpoint {
        id: "shop/v1/remove-cart-item",
        method: "DELETE",
        path: "/shop/v1/cart-items/{id}",
        access: "user-write",
        crud: Some("delete"),
        status: 200,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | NOT_FOUND | NOT_FOUND |",
    },
    Endpoint {
        id: "shop/v1/create-order",
        method: "POST",
        path: "/shop/v1/orders",
        access: "user-write",
        crud: Some("create"),
        status: 201,
        request: "\
| header.idempotency-key | string | max: 200 | no |
| body.address_id | string | max: 80 | yes |
| body.item_ids | json | - | no |
| body.buy_now | json | - | no |
| body.shipping_method | json | - | no |
| body.coupon_id | string | max: 80 | no |
| body.points | integer | min: 0 | no |
| body.note | string | max: 500 | no |",
        errors: "| 404 | ADDRESS_NOT_FOUND | ADDRESS_NOT_FOUND |\n| 409 | IDEMPOTENCY_KEY_REUSED | IDEMPOTENCY_KEY_REUSED |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "shop/v1/list-orders",
        method: "GET",
        path: "/shop/v1/orders",
        access: "user-read",
        crud: None,
        status: 200,
        request: "| query.cursor | string | max: 80 | no |",
        errors: "",
    },
    Endpoint {
        id: "shop/v1/get-order",
        method: "GET",
        path: "/shop/v1/orders/{id}",
        access: "user-read",
        crud: None,
        status: 200,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | ORDER_NOT_FOUND | ORDER_NOT_FOUND |",
    },
    Endpoint {
        id: "shop/v1/pay-order",
        method: "POST",
        path: "/shop/v1/orders/{id}/payments",
        access: "user-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.id | string | max: 80 | yes |
| header.idempotency-key | string | max: 200 | no |
| body.method | string | max: 40 | yes |
| body.simulate | string | max: 40 | yes |",
        errors: "| 402 | PAYMENT_DECLINED | PAYMENT_DECLINED |\n| 404 | ORDER_NOT_FOUND | ORDER_NOT_FOUND |\n| 409 | IDEMPOTENCY_KEY_REUSED | IDEMPOTENCY_KEY_REUSED |\n| 409 | INVALID_TRANSITION | INVALID_TRANSITION |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "shop/v1/cancel-order",
        method: "POST",
        path: "/shop/v1/orders/{id}/cancel",
        access: "user-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.id | string | max: 80 | yes |
| body.reason | string | max: 500 | no |",
        errors: "| 404 | ORDER_NOT_FOUND | ORDER_NOT_FOUND |\n| 409 | INVALID_TRANSITION | INVALID_TRANSITION |",
    },
    Endpoint {
        id: "shop/v1/confirm-delivery",
        method: "POST",
        path: "/shop/v1/orders/{id}/confirm-delivery",
        access: "user-write",
        crud: Some("update"),
        status: 200,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | ORDER_NOT_FOUND | ORDER_NOT_FOUND |\n| 409 | INVALID_TRANSITION | INVALID_TRANSITION |",
    },
    Endpoint {
        id: "shop/v1/list-staff-orders",
        method: "GET",
        path: "/shop/v1/staff/orders",
        access: "staff-read",
        crud: None,
        status: 200,
        request: "\
| query.status | string | max: 40 | no |
| query.cursor | string | max: 80 | no |",
        errors: "",
    },
    Endpoint {
        id: "shop/v1/staff-order-status",
        method: "POST",
        path: "/shop/v1/staff/orders/{id}/status",
        access: "staff-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.id | string | max: 80 | yes |
| body.status | string | max: 40 | yes |",
        errors: "| 404 | ORDER_NOT_FOUND | ORDER_NOT_FOUND |\n| 409 | INVALID_TRANSITION | INVALID_TRANSITION |",
    },
    Endpoint {
        id: "shop/v1/put-staff-order-status",
        method: "PUT",
        path: "/shop/v1/staff/orders/{id}/status",
        access: "staff-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.id | string | max: 80 | yes |
| body.status | string | max: 40 | yes |",
        errors: "| 404 | ORDER_NOT_FOUND | ORDER_NOT_FOUND |\n| 409 | INVALID_TRANSITION | INVALID_TRANSITION |",
    },
    Endpoint {
        id: "shop/v1/list-addresses",
        method: "GET",
        path: "/shop/v1/addresses",
        access: "user-read",
        crud: None,
        status: 200,
        request: "",
        errors: "",
    },
    Endpoint {
        id: "shop/v1/create-address",
        method: "POST",
        path: "/shop/v1/addresses",
        access: "user-write",
        crud: Some("create"),
        status: 201,
        request: "\
| body.recipient | string | max: 80 | yes |
| body.phone | string | max: 40 | yes |
| body.region | string | max: 40 | yes |
| body.line1 | string | max: 200 | yes |
| body.is_default | boolean | - | no |",
        errors: "| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "shop/v1/update-address",
        method: "PUT",
        path: "/shop/v1/addresses/{id}",
        access: "user-write",
        crud: Some("update"),
        status: 200,
        request: "\
| path.id | string | max: 80 | yes |
| body.recipient | string | max: 80 | yes |
| body.phone | string | max: 40 | yes |
| body.region | string | max: 40 | yes |
| body.line1 | string | max: 200 | yes |
| body.is_default | boolean | - | no |",
        errors: "| 404 | ADDRESS_NOT_FOUND | ADDRESS_NOT_FOUND |\n| 422 | VALIDATION_FAILED | VALIDATION_FAILED |",
    },
    Endpoint {
        id: "shop/v1/delete-address",
        method: "DELETE",
        path: "/shop/v1/addresses/{id}",
        access: "user-write",
        crud: Some("delete"),
        status: 204,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | ADDRESS_NOT_FOUND | ADDRESS_NOT_FOUND |",
    },
    Endpoint {
        id: "shop/v1/set-default-address",
        method: "POST",
        path: "/shop/v1/addresses/{id}/default",
        access: "user-write",
        crud: Some("update"),
        status: 200,
        request: "| path.id | string | max: 80 | yes |",
        errors: "| 404 | ADDRESS_NOT_FOUND | ADDRESS_NOT_FOUND |",
    },
];

const PRODUCT_QUERY: &str = "\
| query.q | string | max: 120 | no |
| query.category | string | max: 80 | no |
| query.brands | string | max: 200 | no |
| query.subcategory | string | max: 80 | no |
| query.attributes | string | max: 2000 | no |
| query.min_price_cents | integer | min: 0 | no |
| query.max_price_cents | integer | min: 0 | no |
| query.sort | string | max: 40 | no |
| query.order | string | max: 8 | no |
| query.cursor | string | max: 80 | no |
| query.limit | integer | min: 1, max: 100 | no |";

const PRODUCT_BODY: &str = "\
| body.slug | string | max: 80 | yes |
| body.name | string | max: 120 | yes |
| body.description | string | max: 4000 | no |
| body.price_cents | integer | min: 0 | yes |
| body.currency | string | max: 8 | no |
| body.image_urls | json | - | no |
| body.category | string | max: 80 | no |
| body.stock | integer | min: 0 | no |
| body.active | boolean | - | no |
| body.brand | string | max: 80 | no |
| body.subcategory | string | max: 80 | no |
| body.attributes | json | - | no |
| body.compare_at_price_cents | integer | min: 0 | no |
| body.return_days | integer | min: 0 | no |
| body.options | json | - | no |
| body.variants | json | - | no |";

const PRODUCT_UPDATE: &str = "\
| path.id | string | max: 80 | yes |
| header.if-match | string | max: 80 | no |
| body.slug | string | max: 80 | no |
| body.name | string | max: 120 | no |
| body.description | string | max: 4000 | no |
| body.price_cents | integer | min: 0 | no |
| body.currency | string | max: 8 | no |
| body.image_urls | json | - | no |
| body.category | string | max: 80 | no |
| body.stock | integer | min: 0 | no |
| body.active | boolean | - | no |
| body.brand | string | max: 80 | no |
| body.subcategory | string | max: 80 | no |
| body.attributes | json | - | no |
| body.compare_at_price_cents | integer | min: 0 | no |
| body.return_days | integer | min: 0 | no |
| body.options | json | - | no |
| body.variants | json | - | no |";

pub fn is_shop_source(id: &str) -> bool {
    ENDPOINTS.iter().any(|endpoint| endpoint.id == id)
}

/// True when this natural source is the sample-shop contract for `id`.
/// Other projects reuse some of these ids (multi-tenant catalog, zh-Hant cart)
/// and must keep their own locks.
pub fn adopts_sample_contract(id: &str, text: &str) -> bool {
    let Some((method, path)) = route_for(id) else {
        return false;
    };
    text.contains(&format!("Path: {method} `{path}`"))
}

pub fn route_for(id: &str) -> Option<(&'static str, &'static str)> {
    ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.id == id)
        .map(|endpoint| (endpoint.method, endpoint.path))
}

pub fn lock_body(id: &str) -> Option<String> {
    ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.id == id)
        .map(render_lock)
}

pub fn sections_for(id: &str) -> Option<Vec<Section>> {
    let text = lock_body(id)?;
    Some(parse_sections(&text).expect("sample-shop lock"))
}

/// Fake LLM body when the prompt is a sample-shop source for this contract.
pub fn fake_lock(input: &str) -> Option<String> {
    let id = input.lines().find_map(|line| line.strip_prefix("ID: "))?;
    let kind = input.lines().find_map(|line| line.strip_prefix("KIND: "))?;
    if kind != "api" || !adopts_sample_contract(id.trim(), input) {
        return None;
    }
    lock_body(id.trim())
}

pub fn endpoint_count() -> usize {
    ENDPOINTS.len()
}

/// Natural-language sources plus the `record` model the write routes call.
pub fn sample_project_files() -> Vec<(String, String)> {
    let mut files = vec![(
        "models/record/record.md".to_string(),
        "# record\n\n## Table\n\n- provider: main\n- table: records\n\n## Fields\n\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| name | string | - | no |\n\n## CRUD\n\n| method | options |\n| --- | --- |\n| create | - |\n| update | - |\n| delete | - |\n\n## Policies\n\n(none)\n".to_string(),
    )];
    for endpoint in ENDPOINTS {
        files.push((format!("api/{}.md", endpoint.id), render_source(endpoint)));
    }
    files
}

fn render_source(endpoint: &Endpoint) -> String {
    let public = endpoint.access.starts_with("public");
    let write = endpoint.crud.is_some();
    let mut front = String::from("---\n");
    if public {
        front.push_str("public: true\n");
    }
    front.push_str(if write {
        "writes: true\n"
    } else {
        "writes: false\n"
    });
    if public && write {
        // Register writes, and the validator requires public-writes on a public
        // method call. Memory mode still serves it when the dev slug is read-only.
        front.push_str("public-writes: true\n");
    }
    front.push_str("---\n\n");
    let intro = if public {
        "Anyone can call this with no authentication.\n"
    } else if endpoint.access.starts_with("staff") {
        "A staff role is required.\n"
    } else {
        "The signed-in caller uses this route.\n"
    };
    let verb = match endpoint.crud {
        Some("create") => "This creates a row.\n",
        Some("update") => "This updates a row.\n",
        Some("delete") => "This deletes a row.\n",
        _ => "",
    };
    // The path sits in backticks so a segment such as `login` is a route,
    // not a security-prose requirement (clearly_public reads the path token).
    format!(
        "{front}{intro}Path: {} `{}`\n{verb}",
        endpoint.method, endpoint.path
    )
}

fn render_lock(endpoint: &Endpoint) -> String {
    let public = endpoint.access.starts_with("public");
    let staff = endpoint.access.starts_with("staff");
    let public_writes = endpoint.access == "public-write";
    let min = if endpoint.id.ends_with("/login") {
        "500ms"
    } else if public {
        "300ms"
    } else {
        "0ms"
    };
    let pipelines = if public {
        "### -auth\n- removed: true"
    } else if staff {
        "### auth\n- roles: [staff]"
    } else {
        "(none)"
    };
    let request = if endpoint.request.is_empty() {
        "(none)".to_string()
    } else {
        format!(
            "| field | type | rules | required |\n| --- | --- | --- | --- |\n{}",
            endpoint.request.trim_end()
        )
    };
    let errors = if endpoint.errors.is_empty() {
        "(none)".to_string()
    } else {
        format!(
            "| status | code | from |\n| --- | --- | --- |\n{}",
            endpoint.errors.trim_end()
        )
    };
    let (transform, steps) = if let Some(crud) = endpoint.crud {
        (
            "- in: {name: \"shop\"}\n- out: result",
            format!(
                "### s1\n- op: method\n- target: record.{crud}\n- args: transform.in\n- out: result\n- source-lines: 1-1\n### s2\n- op: respond\n- value: transform.out\n- status: {status}\n- source-lines: 1-1",
                status = endpoint.status
            ),
        )
    } else {
        (
            "- in: {}\n- out: {\"ok\": true}",
            format!(
                "### s1\n- op: respond\n- value: transform.out\n- status: {status}\n- source-lines: 1-1",
                status = endpoint.status
            ),
        )
    };
    format!(
        "## Route\n- method: {method}\n- path: {path}\n## Access\n- public: {public}\n- public-writes: {public_writes}\n- tenant: default-slug\n- min-response: {min}\n- external-calls: forbidden\n- challenge: {{ type: none, when: always }}\n## Pipelines\n{pipelines}\n## Limits\n- provider: main\n- timeout: 10s\n- body: 1MB\n## Request\n{request}\n## Response\n- status: {status}\n## Transform\n{transform}\n## Steps\n{steps}\n## Errors\n{errors}\n",
        method = endpoint.method,
        path = endpoint.path,
        public = public,
        status = endpoint.status,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_shop_routes_have_locks() {
        assert!(endpoint_count() >= 30, "{}", endpoint_count());
        assert!(lock_body("users/v1/login").unwrap().contains("POST"));
        assert!(lock_body("shop/v1/put-staff-order-status")
            .unwrap()
            .contains("PUT"));
        assert!(fake_lock(
            "KIND: api\nID: catalog/v1/list-products\nPath: GET `/catalog/v1/products`\n"
        )
        .is_some());
        assert!(fake_lock("KIND: api\nID: catalog/v1/list-products\n").is_none());
        assert!(fake_lock("KIND: model\nID: catalog/v1/list-products\n").is_none());
    }
}
