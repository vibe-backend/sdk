//! In-memory sample-shop state for `VIBE_DB=memory`.
//!
//! The compiled locks stay valid stubs (`record.create` or a constant
//! response). When the route is one of the single-tenant shop paths, the HTTP
//! layer runs this store instead of those steps. Generic memory tables are
//! left empty.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Map, Value};

use super::crud::MethodFail;
use super::http::AuthConfig;

const PAGE: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Login,
    Register,
    Me,
    ListProducts,
    GetProduct,
    CreateProduct,
    UpdateProduct,
    DeleteProduct,
    ListVariants,
    CreateVariant,
    UpdateVariant,
    ListCart,
    AddCart,
    UpdateCart,
    RemoveCart,
    CreateOrder,
    ListOrders,
    GetOrder,
    PayOrder,
    CancelOrder,
    ConfirmDelivery,
    ListStaffOrders,
    StaffStatus,
    ListAddresses,
    CreateAddress,
    UpdateAddress,
    DeleteAddress,
    SetDefaultAddress,
}

/// Sample-shop locks are stubs (`record.*` or a constant response). Routes
/// that share a path but call `product.*` or `cart-item.*` keep their steps.
pub fn compiled_stub(steps: &[crate::manifest::StepSpec]) -> bool {
    let mut methods = 0usize;
    let mut respond = false;
    for step in steps {
        if step.op == "method" {
            methods += 1;
            if !matches!(
                step.target.as_str(),
                "record.create" | "record.update" | "record.delete"
            ) {
                return false;
            }
        } else if step.op == "respond" {
            respond = true;
        }
    }
    methods > 0 || respond
}

/// `path` is the route template, including an optional `/api` prefix.
pub fn classify(method: &str, path: &str) -> Option<Action> {
    let path = path.trim_matches('/');
    let path = path.strip_prefix("api/").unwrap_or(path);
    let staff = match (method, path) {
        ("GET", "catalog/v1/staff/products") => Some(Action::ListProducts),
        ("GET", "catalog/v1/staff/products/{id}") => Some(Action::GetProduct),
        ("GET", "catalog/v1/staff/products/{pid}/variants") => Some(Action::ListVariants),
        ("GET", "shop/v1/staff/orders") => Some(Action::ListStaffOrders),
        ("POST" | "PUT", "shop/v1/staff/orders/{id}/status") => Some(Action::StaffStatus),
        _ => None,
    };
    if staff.is_some() {
        return staff;
    }
    Some(match (method, path) {
        ("POST", "users/v1/login") => Action::Login,
        ("POST", "users/v1/register") => Action::Register,
        ("GET", "users/v1/me") => Action::Me,
        ("GET", "catalog/v1/products") => Action::ListProducts,
        ("GET", "catalog/v1/products/{id}") => Action::GetProduct,
        ("POST", "catalog/v1/products") => Action::CreateProduct,
        ("PUT", "catalog/v1/products/{id}") => Action::UpdateProduct,
        ("DELETE", "catalog/v1/products/{id}") => Action::DeleteProduct,
        ("POST", "catalog/v1/products/{pid}/variants") => Action::CreateVariant,
        ("PUT", "catalog/v1/products/{pid}/variants/{sid}") => Action::UpdateVariant,
        ("GET", "shop/v1/cart-items") => Action::ListCart,
        ("POST", "shop/v1/cart-items") => Action::AddCart,
        ("PUT", "shop/v1/cart-items/{id}") => Action::UpdateCart,
        ("DELETE", "shop/v1/cart-items/{id}") => Action::RemoveCart,
        ("POST", "shop/v1/orders") => Action::CreateOrder,
        ("GET", "shop/v1/orders") => Action::ListOrders,
        ("GET", "shop/v1/orders/{id}") => Action::GetOrder,
        ("POST", "shop/v1/orders/{id}/payments") => Action::PayOrder,
        ("POST", "shop/v1/orders/{id}/cancel") => Action::CancelOrder,
        ("POST", "shop/v1/orders/{id}/confirm-delivery") => Action::ConfirmDelivery,
        ("GET", "shop/v1/addresses") => Action::ListAddresses,
        ("POST", "shop/v1/addresses") => Action::CreateAddress,
        ("PUT", "shop/v1/addresses/{id}") => Action::UpdateAddress,
        ("DELETE", "shop/v1/addresses/{id}") => Action::DeleteAddress,
        ("POST", "shop/v1/addresses/{id}/default") => Action::SetDefaultAddress,
        _ => return None,
    })
}

pub struct Call<'a> {
    pub action: Action,
    pub tenant_id: &'a str,
    pub subject: &'a str,
    pub body: &'a Value,
    pub query: &'a BTreeMap<String, String>,
    pub path: &'a BTreeMap<String, String>,
    pub headers: &'a BTreeMap<String, String>,
    pub auth: &'a AuthConfig,
    /// Staff catalog and order routes include inactive rows and every buyer.
    pub staff: bool,
}

pub struct Reply {
    pub fail: Option<MethodFail>,
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
}

impl Reply {
    fn fail(fail: MethodFail) -> Self {
        Self {
            fail: Some(fail),
            status: 500,
            body: Value::Null,
            etag: None,
        }
    }

    fn json(status: u16, body: Value) -> Self {
        Self {
            fail: None,
            status,
            body,
            etag: None,
        }
    }

    fn tagged(status: u16, body: Value, version: i64) -> Self {
        Self {
            fail: None,
            status,
            body,
            etag: Some(format!("\"{version}\"")),
        }
    }

    fn empty() -> Self {
        Self {
            fail: None,
            status: 204,
            body: Value::Null,
            etag: None,
        }
    }
}

#[derive(Default)]
pub struct Store {
    inner: Mutex<Data>,
}

#[derive(Default)]
struct Data {
    tenants: HashMap<String, Tenant>,
}

#[derive(Default)]
struct Tenant {
    users: Vec<User>,
    products: Vec<Product>,
    carts: HashMap<String, Vec<CartLine>>,
    orders: Vec<Order>,
    addresses: Vec<Address>,
    idempotency: HashMap<String, Idem>,
}

struct User {
    id: String,
    email: String,
    display_name: String,
    password_hash: String,
    roles: Vec<String>,
}

struct Product {
    id: String,
    slug: String,
    name: String,
    description: String,
    price_cents: i64,
    currency: String,
    image_urls: Vec<String>,
    category: String,
    stock: i64,
    active: bool,
    brand: Option<String>,
    subcategory: Option<String>,
    attributes: BTreeMap<String, String>,
    compare_at_price_cents: Option<i64>,
    return_days: Option<i64>,
    options: Vec<Value>,
    variants: Vec<Variant>,
    version: i64,
    sold_count: Option<i64>,
    rating_avg: Option<f64>,
    review_count: Option<i64>,
    created_at: String,
}

#[derive(Clone)]
struct Variant {
    id: String,
    options: BTreeMap<String, String>,
    price_cents: i64,
    stock: i64,
    image_url: Option<String>,
    active: bool,
    version: i64,
    reserved_stock: i64,
}

struct CartLine {
    id: String,
    product_id: String,
    sku_id: Option<String>,
    quantity: i64,
}

struct Address {
    id: String,
    user_id: String,
    recipient: String,
    phone: String,
    region: String,
    line1: String,
    is_default: bool,
    created_at: String,
}

struct Order {
    id: String,
    user_id: String,
    status: String,
    payment_method: Option<String>,
    paid_at: Option<String>,
    history: Vec<(String, String)>,
    address: AddressSnap,
    items: Vec<OrderItem>,
    total_cents: i64,
    currency: String,
    created_at: String,
    note: Option<String>,
    shipping_method: Option<Value>,
    cancellation_reason: Option<String>,
}

struct AddressSnap {
    recipient: String,
    phone: String,
    region: String,
    line1: String,
}

#[derive(Clone)]
struct OrderItem {
    product_id: String,
    sku_id: Option<String>,
    sku_options: Option<BTreeMap<String, String>>,
    name: String,
    unit_price_cents: i64,
    quantity: i64,
}

struct Idem {
    hash: String,
    status: u16,
    body: Value,
}

struct CheckoutLine {
    product_id: String,
    sku_id: Option<String>,
    cart_id: Option<String>,
    quantity: i64,
}

impl Store {
    pub async fn handle(&self, call: Call<'_>) -> Reply {
        if self.wants_seed(call.tenant_id) {
            let hash = match demo_password_hash().await {
                Ok(hash) => hash,
                Err(fail) => return Reply::fail(fail),
            };
            if let Err(fail) = self.install_seed(call.tenant_id, &hash) {
                return Reply::fail(fail);
            }
        }
        match call.action {
            Action::Login => self.login(&call).await,
            Action::Register => self.register(&call).await,
            other => self.with_tenant(call.tenant_id, |tenant| tenant.dispatch(other, &call)),
        }
    }

    fn wants_seed(&self, tenant: &str) -> bool {
        let Ok(data) = self.inner.lock() else {
            return false;
        };
        seed_enabled()
            && data
                .tenants
                .get(tenant)
                .is_none_or(|tenant| tenant.users.is_empty())
    }

    fn install_seed(&self, tenant_id: &str, hash: &str) -> Result<(), MethodFail> {
        let mut data = self.lock()?;
        let tenant = data.tenants.entry(tenant_id.to_string()).or_default();
        if tenant.users.is_empty() && seed_enabled() {
            tenant.seed(hash);
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Data>, MethodFail> {
        self.inner
            .lock()
            .map_err(|_| MethodFail::code("INTERNAL", "shop store poisoned"))
    }

    fn with_tenant(&self, tenant_id: &str, f: impl FnOnce(&mut Tenant) -> Reply) -> Reply {
        match self.lock() {
            Ok(mut data) => {
                let tenant = data.tenants.entry(tenant_id.to_string()).or_default();
                f(tenant)
            }
            Err(fail) => Reply::fail(fail),
        }
    }

    async fn login(&self, call: &Call<'_>) -> Reply {
        let email = text(call.body, "email").to_ascii_lowercase();
        let password = text(call.body, "password");
        let stored = {
            let Ok(data) = self.lock() else {
                return Reply::fail(MethodFail::code("INTERNAL", "shop store poisoned"));
            };
            data.tenants.get(call.tenant_id).and_then(|tenant| {
                tenant
                    .users
                    .iter()
                    .find(|user| user.email == email)
                    .map(|user| {
                        (
                            user.password_hash.clone(),
                            user.id.clone(),
                            user.roles.clone(),
                        )
                    })
            })
        };
        let (hash, real) = match stored {
            Some((hash, id, roles)) => (hash, Some((id, roles))),
            None => match demo_password_hash().await {
                Ok(hash) => (hash, None),
                Err(fail) => return Reply::fail(fail),
            },
        };
        let password = password.to_string();
        let ok = match verify_password(password, hash).await {
            Ok(ok) => ok,
            Err(fail) => return Reply::fail(fail),
        };
        let Some((id, roles)) = real.filter(|_| ok) else {
            return Reply::fail(MethodFail::code(
                "INVALID_CREDENTIALS",
                "Invalid email or password",
            ));
        };
        let user = User {
            id,
            email,
            display_name: String::new(),
            password_hash: String::new(),
            roles,
        };
        match issue_token(call.auth, call.tenant_id, &user) {
            Ok(body) => Reply::json(200, body),
            Err(fail) => Reply::fail(fail),
        }
    }

    async fn register(&self, call: &Call<'_>) -> Reply {
        let email = text(call.body, "email").trim().to_ascii_lowercase();
        let display_name = text(call.body, "display_name").trim().to_string();
        let password = text(call.body, "password").to_string();
        if email.is_empty() || display_name.is_empty() {
            return Reply::fail(MethodFail::code(
                "VALIDATION_FAILED",
                "email and display_name are required",
            ));
        }
        let taken = self.lock().map(|data| {
            data.tenants
                .get(call.tenant_id)
                .is_some_and(|tenant| tenant.users.iter().any(|user| user.email == email))
        });
        match taken {
            Ok(true) => {
                return Reply::fail(MethodFail::code(
                    "EMAIL_TAKEN",
                    "Email is already registered",
                ))
            }
            Ok(false) => {}
            Err(fail) => return Reply::fail(fail),
        }
        let password_hash = match hash_new_password(password).await {
            Ok(hash) => hash,
            Err(fail) => {
                let code = if fail.code == "WEAK_PASSWORD" {
                    "VALIDATION_FAILED"
                } else {
                    fail.code.as_str()
                };
                return Reply::fail(MethodFail::code(code, fail.message));
            }
        };
        let mut data = match self.lock() {
            Ok(data) => data,
            Err(fail) => return Reply::fail(fail),
        };
        let tenant = data.tenants.entry(call.tenant_id.to_string()).or_default();
        if tenant.users.iter().any(|user| user.email == email) {
            return Reply::fail(MethodFail::code(
                "EMAIL_TAKEN",
                "Email is already registered",
            ));
        }
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            email,
            display_name,
            password_hash,
            roles: Vec::new(),
        };
        let token = match issue_token(call.auth, call.tenant_id, &user) {
            Ok(token) => token,
            Err(fail) => return Reply::fail(fail),
        };
        tenant.users.push(user);
        Reply::json(201, token)
    }
}

impl Tenant {
    fn dispatch(&mut self, action: Action, call: &Call<'_>) -> Reply {
        match action {
            Action::Me => self.me(call),
            Action::ListProducts => self.list_products(call),
            Action::GetProduct => self.get_product(call),
            Action::CreateProduct => self.create_product(call),
            Action::UpdateProduct => self.update_product(call),
            Action::DeleteProduct => self.delete_product(call),
            Action::ListVariants => self.list_variants(call),
            Action::CreateVariant => self.create_variant(call),
            Action::UpdateVariant => self.update_variant(call),
            Action::ListCart => self.cart(call.subject),
            Action::AddCart => self.add_cart(call),
            Action::UpdateCart => self.update_cart(call),
            Action::RemoveCart => self.remove_cart(call),
            Action::CreateOrder => self.create_order(call),
            Action::ListOrders => self.list_orders(call),
            Action::GetOrder => self.get_order(call),
            Action::PayOrder => self.pay_order(call),
            Action::CancelOrder => self.cancel_order(call),
            Action::ConfirmDelivery => self.confirm_delivery(call),
            Action::ListStaffOrders => self.list_staff_orders(call),
            Action::StaffStatus => self.staff_status(call),
            Action::ListAddresses => self.list_addresses(call.subject),
            Action::CreateAddress => self.create_address(call),
            Action::UpdateAddress => self.update_address(call),
            Action::DeleteAddress => self.delete_address(call),
            Action::SetDefaultAddress => self.set_default_address(call),
            Action::Login | Action::Register => {
                Reply::fail(MethodFail::code("INTERNAL", "shop auth action"))
            }
        }
    }

    fn seed(&mut self, hash: &str) {
        self.users.push(User {
            id: "11111111-1111-4111-8111-111111111111".into(),
            email: "alice@example.com".into(),
            display_name: "Alice".into(),
            password_hash: hash.into(),
            roles: Vec::new(),
        });
        self.users.push(User {
            id: "22222222-2222-4222-8222-222222222222".into(),
            email: "staff@example.com".into(),
            display_name: "Staff".into(),
            password_hash: hash.into(),
            roles: vec!["staff".into()],
        });
        self.products.push(product_seed(
            "33333333-3333-4333-8333-333333333333",
            "vibe-headphones",
            "Vibe Headphones",
            "Wireless over-ear headphones.",
            12_900,
            25,
            true,
            "electronics",
            "audio",
            Some("Vibe Audio"),
            &[("color", "black"), ("connectivity", "wireless")],
            Some(15_900),
            Some(14),
            Some(120),
            Some(4.8),
            "2024-01-01T00:00:00Z",
            &[],
            &[],
        ));
        self.products.push(product_seed(
            "44444444-4444-4444-8444-444444444444",
            "north-tee",
            "North Tee",
            "Cotton tee with two sizes.",
            8_900,
            0,
            true,
            "apparel",
            "tops",
            Some("North"),
            &[("material", "cotton")],
            None,
            Some(30),
            Some(40),
            Some(4.2),
            "2024-02-01T00:00:00Z",
            &[
                json!({"name": "size", "values": ["S", "M"]}),
                json!({"name": "color", "values": ["black", "white"]}),
            ],
            &[
                variant_seed(
                    "77777777-7777-4777-8777-777777777777",
                    &[("size", "S"), ("color", "black")],
                    8_900,
                    5,
                    Some("https://example.com/tee-black.jpg"),
                ),
                variant_seed(
                    "88888888-8888-4888-8888-888888888888",
                    &[("size", "M"), ("color", "white")],
                    9_900,
                    8,
                    Some("https://example.com/tee-white.jpg"),
                ),
            ],
        ));
        self.products.push(product_seed(
            "55555555-5555-4555-8555-555555555555",
            "north-mug",
            "North Mug",
            "Ceramic mug.",
            4_500,
            30,
            true,
            "home",
            "drinkware",
            Some("North"),
            &[("material", "ceramic")],
            None,
            Some(7),
            Some(10),
            Some(3.5),
            "2024-03-01T00:00:00Z",
            &[],
            &[],
        ));
        self.products.push(product_seed(
            "66666666-6666-4666-8666-666666666666",
            "draft-lamp",
            "Draft Lamp",
            "Unpublished lamp.",
            2_000,
            1,
            false,
            "home",
            "lighting",
            Some("Workshop"),
            &[("color", "white")],
            None,
            None,
            None,
            None,
            "2024-04-01T00:00:00Z",
            &[],
            &[],
        ));
        for product in &mut self.products {
            product.sync_stock();
        }
    }

    fn me(&self, call: &Call<'_>) -> Reply {
        let Some(user) = self.users.iter().find(|user| user.id == call.subject) else {
            return Reply::fail(MethodFail::code("UNAUTHORIZED", "Unauthorized"));
        };
        Reply::json(
            200,
            json!({
                "id": user.id,
                "email": user.email,
                "display_name": user.display_name,
                "roles": user.roles,
            }),
        )
    }

    fn list_products(&self, call: &Call<'_>) -> Reply {
        let query = match ProductQuery::parse(call.query) {
            Ok(query) => query,
            Err(fail) => return Reply::fail(fail),
        };
        let mut rows: Vec<&Product> = self
            .products
            .iter()
            .filter(|product| call.staff || product.active)
            .filter(|product| query.matches(product))
            .collect();
        rows.sort_by(|left, right| query.cmp(left, right));
        Reply::json(200, page_products(&rows, query.cursor, query.limit))
    }

    fn get_product(&self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").map(String::as_str).unwrap_or("");
        match self.products.iter().find(|product| product.id == id) {
            Some(product) if product.active || call.staff => {
                Reply::tagged(200, product.to_json(), product.version)
            }
            _ => Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found")),
        }
    }

    fn create_product(&mut self, call: &Call<'_>) -> Reply {
        let mut product = match Product::from_body(call.body, true) {
            Ok(product) => product,
            Err(fail) => return Reply::fail(fail),
        };
        if self.products.iter().any(|row| row.slug == product.slug) {
            return Reply::fail(MethodFail::code(
                "UNIQUE_VIOLATION",
                "slug is already in use",
            ));
        }
        product.sync_stock();
        let body = product.to_json();
        let version = product.version;
        self.products.push(product);
        Reply::tagged(201, body, version)
    }

    fn update_product(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        let Some(index) = self.products.iter().position(|product| product.id == id) else {
            return Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"));
        };
        let header = call.headers.get("if-match").map(String::as_str);
        if !etag_ok(header, self.products[index].version, true) {
            return Reply::fail(precondition(header));
        }
        if let Some(slug) = call.body.get("slug").and_then(Value::as_str) {
            if self
                .products
                .iter()
                .any(|product| product.id != id && product.slug == slug)
            {
                return Reply::fail(MethodFail::code(
                    "UNIQUE_VIOLATION",
                    "slug is already in use",
                ));
            }
        }
        if let Err(fail) = self.products[index].apply(call.body) {
            return Reply::fail(fail);
        }
        self.products[index].version += 1;
        self.products[index].sync_stock();
        let version = self.products[index].version;
        Reply::tagged(200, self.products[index].to_json(), version)
    }

    fn delete_product(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").map(String::as_str).unwrap_or("");
        let before = self.products.len();
        self.products.retain(|product| product.id != id);
        if self.products.len() == before {
            Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"))
        } else {
            Reply::empty()
        }
    }

    fn list_variants(&self, call: &Call<'_>) -> Reply {
        let pid = call.path.get("pid").map(String::as_str).unwrap_or("");
        match self.products.iter().find(|product| product.id == pid) {
            Some(product) => Reply::json(
                200,
                Value::Array(product.variants.iter().map(Variant::to_json).collect()),
            ),
            None => Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found")),
        }
    }

    fn create_variant(&mut self, call: &Call<'_>) -> Reply {
        let pid = call.path.get("pid").cloned().unwrap_or_default();
        let Some(product) = self.products.iter_mut().find(|product| product.id == pid) else {
            return Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"));
        };
        let variant = match Variant::from_body(call.body, true) {
            Ok(variant) => variant,
            Err(fail) => return Reply::fail(fail),
        };
        let body = variant.to_json();
        let version = variant.version;
        product.variants.push(variant);
        product.sync_stock();
        Reply::tagged(201, body, version)
    }

    fn update_variant(&mut self, call: &Call<'_>) -> Reply {
        let pid = call.path.get("pid").cloned().unwrap_or_default();
        let sid = call.path.get("sid").cloned().unwrap_or_default();
        let Some(product) = self.products.iter_mut().find(|product| product.id == pid) else {
            return Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"));
        };
        let header = call.headers.get("if-match").map(String::as_str);
        let updated = {
            let Some(variant) = product
                .variants
                .iter_mut()
                .find(|variant| variant.id == sid)
            else {
                return Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"));
            };
            if !etag_ok(header, variant.version, true) {
                return Reply::fail(precondition(header));
            }
            if let Err(fail) = variant.apply(call.body) {
                return Reply::fail(fail);
            }
            variant.version += 1;
            (variant.to_json(), variant.version)
        };
        product.sync_stock();
        Reply::tagged(200, updated.0, updated.1)
    }

    fn cart(&self, user_id: &str) -> Reply {
        Reply::json(200, self.cart_json(user_id))
    }

    fn add_cart(&mut self, call: &Call<'_>) -> Reply {
        let product_id = text(call.body, "product_id").to_string();
        let quantity = call
            .body
            .get("quantity")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let sku_id = optional_text(call.body, "sku_id");
        if self.product_ref(&product_id, sku_id.as_deref()).is_err() {
            return Reply::fail(MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"));
        }
        let lines = self.carts.entry(call.subject.to_string()).or_default();
        if let Some(line) = lines.iter_mut().find(|line| {
            line.product_id == product_id && line.sku_id.as_deref() == sku_id.as_deref()
        }) {
            line.quantity = (line.quantity + quantity).min(99);
        } else {
            lines.push(CartLine {
                id: uuid::Uuid::new_v4().to_string(),
                product_id,
                sku_id,
                quantity,
            });
        }
        Reply::json(200, self.cart_json(call.subject))
    }

    fn update_cart(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        let quantity = call
            .body
            .get("quantity")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let Some(lines) = self.carts.get_mut(call.subject) else {
            return Reply::fail(MethodFail::code("NOT_FOUND", "Cart item not found"));
        };
        if quantity <= 0 {
            let before = lines.len();
            lines.retain(|line| line.id != id);
            if lines.len() == before {
                return Reply::fail(MethodFail::code("NOT_FOUND", "Cart item not found"));
            }
        } else if let Some(line) = lines.iter_mut().find(|line| line.id == id) {
            line.quantity = quantity;
        } else {
            return Reply::fail(MethodFail::code("NOT_FOUND", "Cart item not found"));
        }
        Reply::json(200, self.cart_json(call.subject))
    }

    fn remove_cart(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").map(String::as_str).unwrap_or("");
        let Some(lines) = self.carts.get_mut(call.subject) else {
            return Reply::fail(MethodFail::code("NOT_FOUND", "Cart item not found"));
        };
        let before = lines.len();
        lines.retain(|line| line.id != id);
        if lines.len() == before {
            Reply::fail(MethodFail::code("NOT_FOUND", "Cart item not found"))
        } else {
            Reply::json(200, self.cart_json(call.subject))
        }
    }

    fn create_order(&mut self, call: &Call<'_>) -> Reply {
        let key = match idem_key(call) {
            Ok(key) => key,
            Err(fail) => return Reply::fail(fail),
        };
        let hash = canonical(call.body);
        if let Some(reply) = self.replay(call.subject, "order", &key, &hash) {
            return reply;
        }
        let address_id = text(call.body, "address_id");
        let Some(address) = self
            .addresses
            .iter()
            .find(|row| row.id == address_id && row.user_id == call.subject)
        else {
            return Reply::fail(MethodFail::code("ADDRESS_NOT_FOUND", "Address not found"));
        };
        let snap = AddressSnap {
            recipient: address.recipient.clone(),
            phone: address.phone.clone(),
            region: address.region.clone(),
            line1: address.line1.clone(),
        };
        let lines = match self.checkout_lines(call) {
            Ok(lines) => lines,
            Err(fail) => return Reply::fail(fail),
        };
        let built = match self.price_lines(&lines) {
            Ok(built) => built,
            Err(fail) => return Reply::fail(fail),
        };
        if let Err(fail) = self.deduct(&built) {
            return Reply::fail(fail);
        }
        let cart_ids: Vec<String> = lines
            .iter()
            .filter_map(|line| line.cart_id.clone())
            .collect();
        if let Some(cart) = self.carts.get_mut(call.subject) {
            cart.retain(|line| !cart_ids.iter().any(|id| id == &line.id));
        }
        let now = now_stamp();
        let mut order = Order {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: call.subject.to_string(),
            status: "pending".into(),
            payment_method: None,
            paid_at: None,
            history: vec![("pending".into(), now.clone())],
            address: snap,
            items: built,
            total_cents: 0,
            currency: String::new(),
            created_at: now,
            note: optional_text(call.body, "note"),
            shipping_method: call.body.get("shipping_method").cloned(),
            cancellation_reason: None,
        };
        order.total_cents = order.items.iter().map(|item| item.line_total()).sum();
        order.currency = self
            .products
            .iter()
            .find(|product| product.id == order.items[0].product_id)
            .map(|product| product.currency.clone())
            .unwrap_or_else(|| "HKD".into());
        let body = order.to_json();
        self.orders.push(order);
        self.remember(call.subject, "order", &key, &hash, 201, &body);
        Reply::json(201, body)
    }

    fn list_orders(&self, call: &Call<'_>) -> Reply {
        self.order_page(call, Some(call.subject), false)
    }

    fn get_order(&self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").map(String::as_str).unwrap_or("");
        match self
            .orders
            .iter()
            .find(|order| order.id == id && order.user_id == call.subject)
        {
            Some(order) => Reply::json(200, order.to_json()),
            None => Reply::fail(MethodFail::code("ORDER_NOT_FOUND", "Order not found")),
        }
    }

    fn pay_order(&mut self, call: &Call<'_>) -> Reply {
        let key = match idem_key(call) {
            Ok(key) => key,
            Err(fail) => return Reply::fail(fail),
        };
        let id = call.path.get("id").cloned().unwrap_or_default();
        let hash = format!("{id}\n{}", canonical(call.body));
        if let Some(reply) = self.replay(call.subject, "pay", &key, &hash) {
            return reply;
        }
        let method = text(call.body, "method");
        let simulate = text(call.body, "simulate");
        if !matches!(method, "mock_card" | "mock_alipay")
            || !matches!(simulate, "success" | "failure")
        {
            return Reply::fail(MethodFail::code(
                "VALIDATION_FAILED",
                "method and simulate are invalid",
            ));
        }
        let body = {
            let Some(order) = self
                .orders
                .iter_mut()
                .find(|order| order.id == id && order.user_id == call.subject)
            else {
                return Reply::fail(MethodFail::code("ORDER_NOT_FOUND", "Order not found"));
            };
            if order.status != "pending" {
                return Reply::fail(MethodFail::code(
                    "INVALID_TRANSITION",
                    "Status transition is not allowed",
                ));
            }
            if simulate == "failure" {
                return Reply::fail(MethodFail::code("PAYMENT_DECLINED", "Payment was declined"));
            }
            let now = now_stamp();
            order.status = "paid".into();
            order.payment_method = Some(method.to_string());
            order.paid_at = Some(now.clone());
            order.history.push(("paid".into(), now));
            order.to_json()
        };
        self.remember(call.subject, "pay", &key, &hash, 200, &body);
        Reply::json(200, body)
    }

    fn cancel_order(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        let reason = optional_text(call.body, "reason").unwrap_or_else(|| "cancelled".into());
        self.transition(&id, Some(call.subject), "cancelled", Some(reason), false)
    }

    fn confirm_delivery(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        self.transition(&id, Some(call.subject), "delivered", None, false)
    }

    fn list_staff_orders(&self, call: &Call<'_>) -> Reply {
        self.order_page(call, None, true)
    }

    fn staff_status(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        let status = text(call.body, "status");
        if !matches!(status, "shipped" | "cancelled") {
            return Reply::fail(MethodFail::code(
                "VALIDATION_FAILED",
                "status must be shipped or cancelled",
            ));
        }
        let reason = (status == "cancelled").then(|| "cancelled".to_string());
        self.transition(&id, None, status, reason, true)
    }

    fn list_addresses(&self, user_id: &str) -> Reply {
        let mut rows: Vec<&Address> = self
            .addresses
            .iter()
            .filter(|row| row.user_id == user_id)
            .collect();
        rows.sort_by(|left, right| {
            right
                .is_default
                .cmp(&left.is_default)
                .then(right.created_at.cmp(&left.created_at))
                .then(right.id.cmp(&left.id))
        });
        Reply::json(
            200,
            Value::Array(rows.into_iter().map(Address::to_json).collect()),
        )
    }

    fn create_address(&mut self, call: &Call<'_>) -> Reply {
        let make_default = call
            .body
            .get("is_default")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || !self.addresses.iter().any(|row| row.user_id == call.subject);
        if make_default {
            for row in self
                .addresses
                .iter_mut()
                .filter(|row| row.user_id == call.subject)
            {
                row.is_default = false;
            }
        }
        let address = Address {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: call.subject.to_string(),
            recipient: text(call.body, "recipient").to_string(),
            phone: text(call.body, "phone").to_string(),
            region: text(call.body, "region").to_string(),
            line1: text(call.body, "line1").to_string(),
            is_default: make_default,
            created_at: now_stamp(),
        };
        let body = address.to_json();
        self.addresses.push(address);
        Reply::json(201, body)
    }

    fn update_address(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").map(String::as_str).unwrap_or("");
        let Some(address) = self
            .addresses
            .iter_mut()
            .find(|row| row.id == id && row.user_id == call.subject)
        else {
            return Reply::fail(MethodFail::code("ADDRESS_NOT_FOUND", "Address not found"));
        };
        address.recipient = text(call.body, "recipient").to_string();
        address.phone = text(call.body, "phone").to_string();
        address.region = text(call.body, "region").to_string();
        address.line1 = text(call.body, "line1").to_string();
        Reply::json(200, address.to_json())
    }

    fn delete_address(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        let Some(index) = self
            .addresses
            .iter()
            .position(|row| row.id == id && row.user_id == call.subject)
        else {
            return Reply::fail(MethodFail::code("ADDRESS_NOT_FOUND", "Address not found"));
        };
        let was_default = self.addresses[index].is_default;
        let user_id = self.addresses[index].user_id.clone();
        self.addresses.remove(index);
        if was_default {
            if let Some(next) = self
                .addresses
                .iter_mut()
                .filter(|row| row.user_id == user_id)
                .max_by(|left, right| left.created_at.cmp(&right.created_at))
            {
                next.is_default = true;
            }
        }
        Reply::empty()
    }

    fn set_default_address(&mut self, call: &Call<'_>) -> Reply {
        let id = call.path.get("id").cloned().unwrap_or_default();
        if !self
            .addresses
            .iter()
            .any(|row| row.id == id && row.user_id == call.subject)
        {
            return Reply::fail(MethodFail::code("ADDRESS_NOT_FOUND", "Address not found"));
        }
        for row in self
            .addresses
            .iter_mut()
            .filter(|row| row.user_id == call.subject)
        {
            row.is_default = row.id == id;
        }
        let body = self
            .addresses
            .iter()
            .find(|row| row.id == id)
            .map(Address::to_json)
            .unwrap_or(Value::Null);
        Reply::json(200, body)
    }

    fn cart_json(&self, user_id: &str) -> Value {
        let mut items = Vec::new();
        let mut subtotal = 0_i64;
        for line in self.carts.get(user_id).into_iter().flatten() {
            let view = self.line_view(line);
            if !view.unavailable {
                subtotal += view.line_total;
            }
            items.push(view.json);
        }
        json!({"items": items, "subtotal_cents": subtotal})
    }

    fn line_view(&self, line: &CartLine) -> LineView {
        let Some(product) = self
            .products
            .iter()
            .find(|product| product.id == line.product_id)
        else {
            return LineView {
                unavailable: true,
                line_total: 0,
                json: json!({
                    "id": line.id,
                    "product": {
                        "id": line.product_id,
                        "slug": "",
                        "name": "",
                        "price_cents": 0,
                        "currency": "HKD",
                        "image_url": null,
                        "in_stock": false
                    },
                    "sku": null,
                    "quantity": line.quantity,
                    "line_total_cents": 0,
                    "stock": 0,
                    "unavailable": true
                }),
            };
        };
        let variant = line
            .sku_id
            .as_ref()
            .and_then(|sku| product.variants.iter().find(|variant| variant.id == *sku));
        let price = variant
            .map(|variant| variant.price_cents)
            .unwrap_or_else(|| product.effective_price());
        let stock = variant
            .map(|variant| variant.stock)
            .unwrap_or_else(|| product.effective_stock());
        let image = variant
            .and_then(|variant| variant.image_url.clone())
            .or_else(|| product.image_urls.first().cloned());
        let missing_sku = product.variants.is_empty() && line.sku_id.is_some()
            || !product.variants.is_empty() && variant.is_none();
        let inactive_sku = variant.is_some_and(|variant| !variant.active);
        let unavailable = !product.active || missing_sku || inactive_sku || stock < line.quantity;
        let line_total = price.saturating_mul(line.quantity);
        let sku = variant.map(|variant| {
            json!({
                "id": variant.id,
                "options": variant.options,
            })
        });
        LineView {
            unavailable,
            line_total,
            json: json!({
                "id": line.id,
                "product": {
                    "id": product.id,
                    "slug": product.slug,
                    "name": product.name,
                    "price_cents": price,
                    "currency": product.currency,
                    "image_url": image,
                    "in_stock": product.active && stock > 0
                },
                "sku": sku,
                "quantity": line.quantity,
                "line_total_cents": line_total,
                "stock": stock,
                "unavailable": unavailable
            }),
        }
    }

    fn checkout_lines(&self, call: &Call<'_>) -> Result<Vec<CheckoutLine>, MethodFail> {
        if let Some(buy) = call.body.get("buy_now").filter(|value| !value.is_null()) {
            let product_id = text(buy, "product_id").to_string();
            if product_id.is_empty() {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    "buy_now.product_id is required",
                ));
            }
            let quantity = buy.get("quantity").and_then(Value::as_i64).unwrap_or(0);
            if !(1..=99).contains(&quantity) {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    "buy_now.quantity is invalid",
                ));
            }
            return Ok(vec![CheckoutLine {
                product_id,
                sku_id: optional_text(buy, "sku_id"),
                cart_id: None,
                quantity,
            }]);
        }
        let cart = self.carts.get(call.subject);
        let selected: Vec<&CartLine> = if let Some(ids) = call.body.get("item_ids") {
            let ids = ids.as_array().ok_or_else(|| {
                MethodFail::code("VALIDATION_FAILED", "item_ids must be an array")
            })?;
            let mut lines = Vec::new();
            for id in ids {
                let id = id.as_str().ok_or_else(|| {
                    MethodFail::code("VALIDATION_FAILED", "item_ids must be strings")
                })?;
                let line = cart
                    .into_iter()
                    .flatten()
                    .find(|line| line.id == id)
                    .ok_or_else(|| MethodFail::code("NOT_FOUND", "Cart item not found"))?;
                lines.push(line);
            }
            lines
        } else {
            cart.into_iter().flatten().collect()
        };
        if selected.is_empty() {
            return Err(MethodFail::code("VALIDATION_FAILED", "cart is empty"));
        }
        Ok(selected
            .into_iter()
            .map(|line| CheckoutLine {
                product_id: line.product_id.clone(),
                sku_id: line.sku_id.clone(),
                cart_id: Some(line.id.clone()),
                quantity: line.quantity,
            })
            .collect())
    }

    fn price_lines(&self, lines: &[CheckoutLine]) -> Result<Vec<OrderItem>, MethodFail> {
        let mut items = Vec::new();
        for line in lines {
            let product = self
                .products
                .iter()
                .find(|product| product.id == line.product_id)
                .ok_or_else(|| MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"))?;
            if !product.active {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    "product is not available",
                ));
            }
            let variant = match &line.sku_id {
                Some(sku) => Some(
                    product
                        .variants
                        .iter()
                        .find(|variant| variant.id == *sku && variant.active)
                        .ok_or_else(|| {
                            MethodFail::code("PRODUCT_NOT_FOUND", "Product not found")
                        })?,
                ),
                None if !product.variants.is_empty() => {
                    return Err(MethodFail::code("VALIDATION_FAILED", "sku_id is required"))
                }
                None => None,
            };
            let stock = variant
                .map(|variant| variant.stock)
                .unwrap_or(product.stock);
            if stock < line.quantity {
                return Err(MethodFail::code("VALIDATION_FAILED", "insufficient stock"));
            }
            let price = variant
                .map(|variant| variant.price_cents)
                .unwrap_or(product.price_cents);
            items.push(OrderItem {
                product_id: product.id.clone(),
                sku_id: variant.map(|variant| variant.id.clone()),
                sku_options: variant.map(|variant| variant.options.clone()),
                name: product.name.clone(),
                unit_price_cents: price,
                quantity: line.quantity,
            });
        }
        Ok(items)
    }

    fn deduct(&mut self, items: &[OrderItem]) -> Result<(), MethodFail> {
        for item in items {
            let product = self
                .products
                .iter_mut()
                .find(|product| product.id == item.product_id)
                .ok_or_else(|| MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"))?;
            if let Some(sku) = &item.sku_id {
                let variant = product
                    .variants
                    .iter_mut()
                    .find(|variant| variant.id == *sku)
                    .ok_or_else(|| MethodFail::code("PRODUCT_NOT_FOUND", "Product not found"))?;
                if variant.stock < item.quantity {
                    return Err(MethodFail::code("VALIDATION_FAILED", "insufficient stock"));
                }
                variant.stock -= item.quantity;
            } else {
                if product.stock < item.quantity {
                    return Err(MethodFail::code("VALIDATION_FAILED", "insufficient stock"));
                }
                product.stock -= item.quantity;
            }
            product.sync_stock();
            if let Some(sold) = product.sold_count.as_mut() {
                *sold += item.quantity;
            }
        }
        Ok(())
    }

    fn restore(&mut self, items: &[OrderItem]) {
        for item in items {
            let Some(product) = self
                .products
                .iter_mut()
                .find(|product| product.id == item.product_id)
            else {
                continue;
            };
            if let Some(sku) = &item.sku_id {
                if let Some(variant) = product
                    .variants
                    .iter_mut()
                    .find(|variant| variant.id == *sku)
                {
                    variant.stock += item.quantity;
                }
            } else {
                product.stock += item.quantity;
            }
            product.sync_stock();
            if let Some(sold) = product.sold_count.as_mut() {
                *sold = sold.saturating_sub(item.quantity);
            }
        }
    }

    fn transition(
        &mut self,
        id: &str,
        user_id: Option<&str>,
        next: &str,
        reason: Option<String>,
        staff: bool,
    ) -> Reply {
        let index = self
            .orders
            .iter()
            .position(|order| order.id == id && user_id.is_none_or(|user| order.user_id == user));
        let Some(index) = index else {
            return Reply::fail(MethodFail::code("ORDER_NOT_FOUND", "Order not found"));
        };
        let allowed = match next {
            "cancelled" => matches!(self.orders[index].status.as_str(), "pending" | "paid"),
            "delivered" => self.orders[index].status == "shipped",
            "shipped" => self.orders[index].status == "paid",
            _ => false,
        };
        if !allowed {
            return Reply::fail(MethodFail::code(
                "INVALID_TRANSITION",
                "Status transition is not allowed",
            ));
        }
        let items = self.orders[index].items.clone();
        self.orders[index].status = next.to_string();
        self.orders[index]
            .history
            .push((next.to_string(), now_stamp()));
        if next == "cancelled" {
            self.orders[index].cancellation_reason = reason;
        }
        if next == "cancelled" {
            self.restore(&items);
        }
        let buyer_id = self.orders[index].user_id.clone();
        let body = if staff {
            let buyer = self
                .users
                .iter()
                .find(|user| user.id == buyer_id)
                .map(User::buyer_json)
                .unwrap_or(Value::Null);
            self.orders[index].to_staff_json(buyer)
        } else {
            self.orders[index].to_json()
        };
        Reply::json(200, body)
    }

    fn order_page(&self, call: &Call<'_>, user_id: Option<&str>, staff: bool) -> Reply {
        let cursor = match offset(call.query.get("cursor").map(String::as_str)) {
            Ok(cursor) => cursor,
            Err(fail) => return Reply::fail(fail),
        };
        let status = call
            .query
            .get("status")
            .map(String::as_str)
            .filter(|s| !s.is_empty());
        if let Some(status) = status {
            if !matches!(
                status,
                "pending" | "paid" | "shipped" | "delivered" | "cancelled"
            ) {
                return Reply::fail(MethodFail::code("VALIDATION_FAILED", "unknown status"));
            }
        }
        let mut rows: Vec<&Order> = self
            .orders
            .iter()
            .filter(|order| user_id.is_none_or(|user| order.user_id == user))
            .filter(|order| status.is_none_or(|status| order.status == status))
            .collect();
        rows.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then(right.id.cmp(&left.id))
        });
        let end = cursor.saturating_add(PAGE).min(rows.len());
        let slice = if cursor >= rows.len() {
            &[][..]
        } else {
            &rows[cursor..end]
        };
        let items: Vec<Value> = slice
            .iter()
            .map(|order| {
                if staff {
                    let buyer = self
                        .users
                        .iter()
                        .find(|user| user.id == order.user_id)
                        .map(User::buyer_json)
                        .unwrap_or(Value::Null);
                    order.to_staff_json(buyer)
                } else {
                    order.to_json()
                }
            })
            .collect();
        let next = (end < rows.len() && cursor < rows.len()).then(|| end.to_string());
        Reply::json(200, json!({"items": items, "next_cursor": next}))
    }

    fn product_ref(&self, product_id: &str, sku: Option<&str>) -> Result<(), ()> {
        let product = self
            .products
            .iter()
            .find(|product| product.id == product_id)
            .ok_or(())?;
        if let Some(sku) = sku {
            if !product.variants.iter().any(|variant| variant.id == sku) {
                return Err(());
            }
        }
        Ok(())
    }

    fn replay(&self, user: &str, scope: &str, key: &str, hash: &str) -> Option<Reply> {
        let row = self.idempotency.get(&idem_slot(user, scope, key))?;
        if row.hash != hash {
            return Some(Reply::fail(MethodFail::code(
                "IDEMPOTENCY_KEY_REUSED",
                "Idempotency key was reused with a different request",
            )));
        }
        Some(Reply::json(row.status, row.body.clone()))
    }

    fn remember(
        &mut self,
        user: &str,
        scope: &str,
        key: &str,
        hash: &str,
        status: u16,
        body: &Value,
    ) {
        self.idempotency.insert(
            idem_slot(user, scope, key),
            Idem {
                hash: hash.to_string(),
                status,
                body: body.clone(),
            },
        );
    }
}

struct LineView {
    unavailable: bool,
    line_total: i64,
    json: Value,
}

struct ProductQuery {
    q: String,
    category: String,
    brands: Vec<String>,
    subcategory: String,
    attributes: BTreeMap<String, Vec<String>>,
    min_price: Option<i64>,
    max_price: Option<i64>,
    sort: String,
    desc: bool,
    cursor: usize,
    limit: usize,
}

impl ProductQuery {
    fn parse(query: &BTreeMap<String, String>) -> Result<Self, MethodFail> {
        let sort = query
            .get("sort")
            .cloned()
            .unwrap_or_else(|| "created_at".into());
        if !matches!(
            sort.as_str(),
            "price" | "created_at" | "sales" | "rating" | "relevance"
        ) {
            return Err(MethodFail::code("VALIDATION_FAILED", "unknown sort"));
        }
        let order = query.get("order").map(String::as_str).unwrap_or("desc");
        if !matches!(order, "asc" | "desc") {
            return Err(MethodFail::code("VALIDATION_FAILED", "unknown order"));
        }
        let attributes = match query.get("attributes") {
            Some(raw) => parse_attributes(raw)?,
            None => BTreeMap::new(),
        };
        let brands = query
            .get("brands")
            .map(|raw| {
                raw.split(',')
                    .map(|part| part.trim().to_string())
                    .filter(|part| !part.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            q: query
                .get("q")
                .map(|value| value.to_ascii_lowercase())
                .unwrap_or_default(),
            category: query.get("category").cloned().unwrap_or_default(),
            brands,
            subcategory: query.get("subcategory").cloned().unwrap_or_default(),
            attributes,
            min_price: int_query(query, "min_price_cents")?,
            max_price: int_query(query, "max_price_cents")?,
            sort,
            desc: order == "desc",
            cursor: offset(query.get("cursor").map(String::as_str))?,
            limit: int_query(query, "limit")?
                .map(|limit| usize::try_from(limit).unwrap_or(PAGE))
                .unwrap_or(PAGE),
        })
    }

    fn matches(&self, product: &Product) -> bool {
        if !self.category.is_empty() && product.category != self.category {
            return false;
        }
        if !self.subcategory.is_empty()
            && product.subcategory.as_deref() != Some(self.subcategory.as_str())
        {
            return false;
        }
        if !self.brands.is_empty()
            && !self.brands.iter().any(|brand| {
                product
                    .brand
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(brand))
            })
        {
            return false;
        }
        let price = product.effective_price();
        if self.min_price.is_some_and(|min| price < min)
            || self.max_price.is_some_and(|max| price > max)
        {
            return false;
        }
        for (key, values) in &self.attributes {
            let Some(have) = product.attributes.get(key) else {
                return false;
            };
            if !values.iter().any(|value| value == have) {
                return false;
            }
        }
        if self.q.is_empty() {
            return true;
        }
        [
            product.name.as_str(),
            product.slug.as_str(),
            product.description.as_str(),
            product.brand.as_deref().unwrap_or(""),
            product.category.as_str(),
            product.subcategory.as_deref().unwrap_or(""),
        ]
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(&self.q))
    }

    fn cmp(&self, left: &Product, right: &Product) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let primary = match self.sort.as_str() {
            "price" => left.effective_price().cmp(&right.effective_price()),
            "sales" => left
                .sold_count
                .unwrap_or(0)
                .cmp(&right.sold_count.unwrap_or(0)),
            "rating" => left
                .rating_avg
                .unwrap_or(0.0)
                .partial_cmp(&right.rating_avg.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            "relevance" => self.relevance(left).cmp(&self.relevance(right)),
            _ => left.created_at.cmp(&right.created_at),
        };
        let primary = if self.desc {
            primary.reverse()
        } else {
            primary
        };
        primary.then(left.id.cmp(&right.id))
    }

    fn relevance(&self, product: &Product) -> i64 {
        if self.q.is_empty() {
            return 0;
        }
        let name = product.name.to_ascii_lowercase().contains(&self.q) as i64;
        let extra = product.description.to_ascii_lowercase().contains(&self.q) as i64;
        name * 2 + extra
    }
}

impl Product {
    fn effective_price(&self) -> i64 {
        self.variants
            .iter()
            .map(|variant| variant.price_cents)
            .min()
            .unwrap_or(self.price_cents)
    }

    fn effective_stock(&self) -> i64 {
        if self.variants.is_empty() {
            self.stock
        } else {
            self.variants.iter().map(|variant| variant.stock).sum()
        }
    }

    fn sync_stock(&mut self) {
        if self.variants.is_empty() {
            return;
        }
        self.stock = self.effective_stock();
        if let Some(price) = self
            .variants
            .iter()
            .map(|variant| variant.price_cents)
            .min()
        {
            self.price_cents = price;
        }
    }

    fn from_body(body: &Value, creating: bool) -> Result<Self, MethodFail> {
        let mut product = Self {
            id: uuid::Uuid::new_v4().to_string(),
            slug: String::new(),
            name: String::new(),
            description: String::new(),
            price_cents: 0,
            currency: "HKD".into(),
            image_urls: Vec::new(),
            category: String::new(),
            stock: 0,
            active: true,
            brand: None,
            subcategory: None,
            attributes: BTreeMap::new(),
            compare_at_price_cents: None,
            return_days: None,
            options: Vec::new(),
            variants: Vec::new(),
            version: 1,
            sold_count: None,
            rating_avg: None,
            review_count: None,
            created_at: now_stamp(),
        };
        product.apply(body)?;
        if creating && (product.slug.is_empty() || product.name.is_empty()) {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                "slug and name are required",
            ));
        }
        Ok(product)
    }

    fn apply(&mut self, body: &Value) -> Result<(), MethodFail> {
        if body.get("slug").is_some() {
            self.slug = text(body, "slug").to_string();
        }
        if body.get("name").is_some() {
            self.name = text(body, "name").to_string();
        }
        if body.get("description").is_some() {
            self.description = text(body, "description").to_string();
        }
        if let Some(price) = body.get("price_cents").and_then(Value::as_i64) {
            self.price_cents = price;
        }
        if body.get("currency").is_some() {
            let currency = text(body, "currency");
            if !currency.is_empty() {
                self.currency = currency.to_string();
            }
        }
        if let Some(urls) = body.get("image_urls") {
            self.image_urls = string_list(urls)?;
        }
        if body.get("category").is_some() {
            self.category = text(body, "category").to_string();
        }
        if let Some(stock) = body.get("stock").and_then(Value::as_i64) {
            self.stock = stock;
        }
        if let Some(active) = body.get("active").and_then(Value::as_bool) {
            self.active = active;
        }
        if body.get("brand").is_some() {
            self.brand = optional_text(body, "brand");
        }
        if body.get("subcategory").is_some() {
            self.subcategory = optional_text(body, "subcategory");
        }
        if let Some(attributes) = body.get("attributes") {
            self.attributes = string_map(attributes)?;
        }
        if let Some(price) = body.get("compare_at_price_cents") {
            self.compare_at_price_cents = price.as_i64();
        }
        if let Some(days) = body.get("return_days") {
            self.return_days = days.as_i64();
        }
        if let Some(options) = body.get("options") {
            self.options = options.as_array().cloned().unwrap_or_default();
        }
        if let Some(variants) = body.get("variants") {
            let rows = variants.as_array().ok_or_else(|| {
                MethodFail::code("VALIDATION_FAILED", "variants must be an array")
            })?;
            self.variants = rows
                .iter()
                .map(|row| Variant::from_body(row, false))
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        let mut body = Map::new();
        body.insert("id".into(), self.id.clone().into());
        body.insert("slug".into(), self.slug.clone().into());
        body.insert("name".into(), self.name.clone().into());
        body.insert("description".into(), self.description.clone().into());
        body.insert("price_cents".into(), self.effective_price().into());
        body.insert("currency".into(), self.currency.clone().into());
        body.insert(
            "image_urls".into(),
            Value::Array(self.image_urls.iter().cloned().map(Value::String).collect()),
        );
        body.insert("category".into(), self.category.clone().into());
        let stock = self.effective_stock();
        body.insert("stock".into(), stock.into());
        body.insert("in_stock".into(), Value::Bool(self.active && stock > 0));
        body.insert("active".into(), Value::Bool(self.active));
        body.insert("created_at".into(), self.created_at.clone().into());
        if let Some(sold) = self.sold_count {
            body.insert("sold_count".into(), sold.into());
        }
        if let Some(rating) = self.rating_avg {
            body.insert(
                "rating_avg".into(),
                serde_json::Number::from_f64(rating)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            );
        }
        if let Some(reviews) = self.review_count {
            body.insert("review_count".into(), reviews.into());
        }
        if let Some(brand) = &self.brand {
            body.insert("brand".into(), brand.clone().into());
        }
        if let Some(sub) = &self.subcategory {
            body.insert("subcategory".into(), sub.clone().into());
        }
        if !self.attributes.is_empty() {
            body.insert("attributes".into(), json!(self.attributes));
        }
        if let Some(price) = self.compare_at_price_cents {
            body.insert("compare_at_price_cents".into(), price.into());
        }
        if let Some(days) = self.return_days {
            body.insert("return_days".into(), days.into());
        }
        if !self.options.is_empty() {
            body.insert("options".into(), Value::Array(self.options.clone()));
        }
        if !self.variants.is_empty() {
            body.insert(
                "variants".into(),
                Value::Array(self.variants.iter().map(Variant::to_json).collect()),
            );
        }
        Value::Object(body)
    }
}

impl Variant {
    fn from_body(body: &Value, fresh_id: bool) -> Result<Self, MethodFail> {
        let mut variant = Self {
            id: if fresh_id {
                uuid::Uuid::new_v4().to_string()
            } else {
                text(body, "id").to_string()
            },
            options: BTreeMap::new(),
            price_cents: body.get("price_cents").and_then(Value::as_i64).unwrap_or(0),
            stock: body.get("stock").and_then(Value::as_i64).unwrap_or(0),
            image_url: None,
            active: true,
            version: body.get("version").and_then(Value::as_i64).unwrap_or(1),
            reserved_stock: 0,
        };
        if variant.id.is_empty() {
            variant.id = uuid::Uuid::new_v4().to_string();
        }
        variant.apply(body)?;
        Ok(variant)
    }

    fn apply(&mut self, body: &Value) -> Result<(), MethodFail> {
        if let Some(options) = body.get("options") {
            self.options = string_map(options)?;
        }
        if let Some(price) = body.get("price_cents").and_then(Value::as_i64) {
            self.price_cents = price;
        }
        if let Some(stock) = body.get("stock").and_then(Value::as_i64) {
            self.stock = stock;
        }
        if body.get("image_url").is_some() {
            self.image_url = optional_text(body, "image_url");
        }
        if let Some(active) = body.get("active").and_then(Value::as_bool) {
            self.active = active;
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "options": self.options,
            "price_cents": self.price_cents,
            "stock": self.stock,
            "in_stock": self.active && self.stock > 0,
            "image_url": self.image_url,
            "active": self.active,
            "version": self.version,
            "reserved_stock": self.reserved_stock,
        })
    }
}

impl Address {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "recipient": self.recipient,
            "phone": self.phone,
            "region": self.region,
            "line1": self.line1,
            "is_default": self.is_default,
            "created_at": self.created_at,
        })
    }
}

impl OrderItem {
    fn line_total(&self) -> i64 {
        self.unit_price_cents.saturating_mul(self.quantity)
    }

    fn to_json(&self) -> Value {
        json!({
            "product_id": self.product_id,
            "sku_id": self.sku_id,
            "sku_options": self.sku_options,
            "name": self.name,
            "unit_price_cents": self.unit_price_cents,
            "quantity": self.quantity,
            "line_total_cents": self.line_total(),
        })
    }
}

impl Order {
    fn to_json(&self) -> Value {
        let mut body = Map::new();
        body.insert("id".into(), self.id.clone().into());
        body.insert("status".into(), self.status.clone().into());
        body.insert(
            "payment_method".into(),
            self.payment_method.clone().map(Value::String).into(),
        );
        body.insert(
            "paid_at".into(),
            self.paid_at.clone().map(Value::String).into(),
        );
        body.insert(
            "status_history".into(),
            Value::Array(
                self.history
                    .iter()
                    .map(|(status, at)| json!({"status": status, "at": at}))
                    .collect(),
            ),
        );
        body.insert(
            "shipping_address".into(),
            json!({
                "recipient": self.address.recipient,
                "phone": self.address.phone,
                "region": self.address.region,
                "line1": self.address.line1,
            }),
        );
        body.insert(
            "items".into(),
            Value::Array(self.items.iter().map(OrderItem::to_json).collect()),
        );
        body.insert("total_cents".into(), self.total_cents.into());
        body.insert("currency".into(), self.currency.clone().into());
        body.insert("created_at".into(), self.created_at.clone().into());
        if let Some(note) = &self.note {
            body.insert("note".into(), note.clone().into());
        }
        if let Some(method) = &self.shipping_method {
            body.insert("shipping_method".into(), method.clone());
        }
        if let Some(reason) = &self.cancellation_reason {
            body.insert("cancellation_reason".into(), reason.clone().into());
        }
        Value::Object(body)
    }

    fn to_staff_json(&self, buyer: Value) -> Value {
        let mut body = match self.to_json() {
            Value::Object(body) => body,
            other => return other,
        };
        body.insert("buyer".into(), buyer);
        Value::Object(body)
    }
}

impl User {
    fn buyer_json(&self) -> Value {
        json!({
            "id": self.id,
            "display_name": self.display_name,
            "email": self.email,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn product_seed(
    id: &str,
    slug: &str,
    name: &str,
    description: &str,
    price: i64,
    stock: i64,
    active: bool,
    category: &str,
    subcategory: &str,
    brand: Option<&str>,
    attributes: &[(&str, &str)],
    compare: Option<i64>,
    return_days: Option<i64>,
    sold: Option<i64>,
    rating: Option<f64>,
    created_at: &str,
    options: &[Value],
    variants: &[Variant],
) -> Product {
    Product {
        id: id.into(),
        slug: slug.into(),
        name: name.into(),
        description: description.into(),
        price_cents: price,
        currency: "HKD".into(),
        image_urls: vec![format!("https://example.com/{slug}.jpg")],
        category: category.into(),
        stock,
        active,
        brand: brand.map(str::to_string),
        subcategory: Some(subcategory.into()),
        attributes: attributes
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        compare_at_price_cents: compare,
        return_days,
        options: options.to_vec(),
        variants: variants.to_vec(),
        version: 1,
        sold_count: sold,
        rating_avg: rating,
        review_count: sold.map(|sold| sold / 10),
        created_at: created_at.into(),
    }
}

fn variant_seed(
    id: &str,
    options: &[(&str, &str)],
    price: i64,
    stock: i64,
    image: Option<&str>,
) -> Variant {
    Variant {
        id: id.into(),
        options: options
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        price_cents: price,
        stock,
        image_url: image.map(str::to_string),
        active: true,
        version: 1,
        reserved_stock: 0,
    }
}

fn page_products(rows: &[&Product], cursor: usize, limit: usize) -> Value {
    let end = cursor.saturating_add(limit).min(rows.len());
    let slice = if cursor >= rows.len() {
        &[][..]
    } else {
        &rows[cursor..end]
    };
    let next = (end < rows.len() && cursor < rows.len()).then(|| end.to_string());
    json!({
        "items": slice.iter().map(|product| product.to_json()).collect::<Vec<_>>(),
        "next_cursor": next,
    })
}

fn issue_token(auth: &AuthConfig, tenant: &str, user: &User) -> Result<Value, MethodFail> {
    let key = auth
        .signing_keys
        .get("JWT_SIGNING_KEY")
        .ok_or_else(|| MethodFail::code("INTERNAL", "signing key JWT_SIGNING_KEY is not loaded"))?;
    if key.alg != jsonwebtoken::Algorithm::ES256 {
        return Err(MethodFail::code("INTERNAL", "shop tokens require ES256"));
    }
    let now = chrono::Utc::now().timestamp();
    let ttl = 3600_i64;
    let mut claims = Map::new();
    claims.insert("sub".into(), user.id.clone().into());
    claims.insert("iss".into(), auth.iss.clone().into());
    claims.insert("aud".into(), auth.aud.clone().into());
    claims.insert("iat".into(), now.into());
    claims.insert("nbf".into(), now.into());
    claims.insert("exp".into(), (now + ttl).into());
    claims.insert("tenant_id".into(), tenant.into());
    claims.insert(
        auth.roles_claim.clone(),
        Value::Array(user.roles.iter().cloned().map(Value::String).collect()),
    );
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some(key.kid.clone());
    let token = jsonwebtoken::encode(&header, &Value::Object(claims), &key.key)
        .map_err(|_| MethodFail::code("INTERNAL", "could not sign access token"))?;
    let expires_at = chrono::DateTime::from_timestamp(now + ttl, 0)
        .map(|stamp| stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
    Ok(json!({"access_token": token, "expires_at": expires_at}))
}

fn text<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or("")
}

fn optional_text(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_list(value: &Value) -> Result<Vec<String>, MethodFail> {
    value
        .as_array()
        .ok_or_else(|| MethodFail::code("VALIDATION_FAILED", "expected a string array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| MethodFail::code("VALIDATION_FAILED", "expected a string array"))
        })
        .collect()
}

fn string_map(value: &Value) -> Result<BTreeMap<String, String>, MethodFail> {
    let object = value
        .as_object()
        .ok_or_else(|| MethodFail::code("VALIDATION_FAILED", "expected an object of strings"))?;
    object
        .iter()
        .map(|(key, item)| {
            item.as_str()
                .map(|text| (key.clone(), text.to_string()))
                .ok_or_else(|| {
                    MethodFail::code("VALIDATION_FAILED", "expected an object of strings")
                })
        })
        .collect()
}

fn parse_attributes(raw: &str) -> Result<BTreeMap<String, Vec<String>>, MethodFail> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|_| MethodFail::code("VALIDATION_FAILED", "attributes must be JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MethodFail::code("VALIDATION_FAILED", "attributes must be an object"))?;
    object
        .iter()
        .map(|(key, item)| Ok((key.clone(), string_list(item)?)))
        .collect()
}

fn int_query(query: &BTreeMap<String, String>, key: &str) -> Result<Option<i64>, MethodFail> {
    match query.get(key) {
        None => Ok(None),
        Some(raw) => raw.parse().map(Some).map_err(|_| {
            MethodFail::code("VALIDATION_FAILED", format!("{key} must be an integer"))
        }),
    }
}

fn offset(raw: Option<&str>) -> Result<usize, MethodFail> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(0),
        Some(raw) => raw
            .parse()
            .map_err(|_| MethodFail::code("VALIDATION_FAILED", "cursor is invalid")),
    }
}

fn etag_ok(header: Option<&str>, version: i64, required: bool) -> bool {
    let Some(header) = header.map(str::trim).filter(|value| !value.is_empty()) else {
        return !required;
    };
    let tag = header.strip_prefix("W/").unwrap_or(header).trim();
    tag.trim_matches('"') == version.to_string()
}

fn precondition(header: Option<&str>) -> MethodFail {
    if header.map(str::trim).is_none_or(str::is_empty) {
        MethodFail::code("PRECONDITION_REQUIRED", "If-Match is required")
    } else {
        MethodFail::code("PRECONDITION_FAILED", "If-Match does not match")
    }
}

fn idem_key(call: &Call<'_>) -> Result<String, MethodFail> {
    call.headers
        .get("idempotency-key")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MethodFail::code("VALIDATION_FAILED", "Idempotency-Key is required"))
}

fn idem_slot(user: &str, scope: &str, key: &str) -> String {
    format!("{user}\u{0}{scope}\u{0}{key}")
}

fn canonical(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn seed_enabled() -> bool {
    match std::env::var("VIBE_SEED_DEMO") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

async fn demo_password_hash() -> Result<String, MethodFail> {
    static HASH: OnceLock<String> = OnceLock::new();
    if let Some(hash) = HASH.get() {
        return Ok(hash.clone());
    }
    let hash = tokio::task::spawn_blocking(|| platform_hash("password123"))
        .await
        .map_err(|_| MethodFail::code("INTERNAL", "argon2 task failed"))?
        .map_err(|err| MethodFail::code("INTERNAL", err))?;
    Ok(HASH.get_or_init(|| hash).clone())
}

async fn hash_new_password(plain: String) -> Result<String, MethodFail> {
    tokio::task::spawn_blocking(move || super::crud::hash_password(&plain))
        .await
        .map_err(|_| MethodFail::code("INTERNAL", "argon2 task failed"))?
}

async fn verify_password(plain: String, hash: String) -> Result<bool, MethodFail> {
    tokio::task::spawn_blocking(move || {
        use argon2::password_hash::{phc::PasswordHash, PasswordVerifier};
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return false;
        };
        argon2::Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .map_err(|_| MethodFail::code("INTERNAL", "argon2 task failed"))
}

fn platform_hash(plain: &str) -> Result<String, String> {
    let params = argon2::Params::new(
        super::crud::ARGON2_M_COST,
        super::crud::ARGON2_T_COST,
        super::crud::ARGON2_P_COST,
        Some(32),
    )
    .map_err(|err| err.to_string())?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    argon2::password_hash::PasswordHasher::hash_password(&argon, plain.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_sample_shop_templates() {
        assert_eq!(classify("POST", "/api/users/v1/login"), Some(Action::Login));
        assert_eq!(
            classify("GET", "/api/catalog/v1/products"),
            Some(Action::ListProducts)
        );
        assert_eq!(
            classify("PUT", "/api/shop/v1/staff/orders/{id}/status"),
            Some(Action::StaffStatus)
        );
        assert_eq!(classify("GET", "/catalog/v1/{slug}/products"), None);
        assert!(etag_ok(Some("\"3\""), 3, true));
        assert!(!etag_ok(None, 3, true));
    }
}
