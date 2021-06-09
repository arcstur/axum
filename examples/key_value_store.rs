//! Simple in-memory key/value store showing features of tower-web.
//!
//! Run with:
//!
//! ```not_rust
//! RUST_LOG=tower_http=debug,key_value_store=trace cargo run --example key_value_store
//! ```

use bytes::Bytes;
use http::{Request, StatusCode};
use hyper::Server;
use std::{
    borrow::Cow,
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};
use tower::{make::Shared, BoxError, ServiceBuilder};
use tower_http::{
    add_extension::AddExtensionLayer, auth::RequireAuthorizationLayer,
    compression::CompressionLayer, trace::TraceLayer,
};
use tower_web::{
    body::{Body, BoxBody},
    extract::{BytesMaxLength, Extension, UrlParams},
    prelude::*,
    response::IntoResponse,
    routing::BoxRoute,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Build our application by composing routes
    let app = route(
        "/:key",
        // Add compression to `kv_get`
        get(kv_get.layer(CompressionLayer::new()))
            // But don't compress `kv_set`
            .post(kv_set),
    )
    .route("/keys", get(list_keys))
    // Nest our admin routes under `/admin`
    .nest("/admin", admin_routes())
    // Add middleware to all routes
    .layer(
        ServiceBuilder::new()
            .load_shed()
            .concurrency_limit(1024)
            .timeout(Duration::from_secs(10))
            .layer(TraceLayer::new_for_http())
            .layer(AddExtensionLayer::new(SharedState::default()))
            .into_inner(),
    )
    // Handle errors from middleware
    .handle_error(handle_error);

    // Run our app with hyper
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    tracing::debug!("listening on {}", addr);
    let server = Server::bind(&addr).serve(Shared::new(app));
    server.await.unwrap();
}

type SharedState = Arc<RwLock<State>>;

#[derive(Default)]
struct State {
    db: HashMap<String, Bytes>,
}

async fn kv_get(
    _req: Request<Body>,
    UrlParams((key,)): UrlParams<(String,)>,
    Extension(state): Extension<SharedState>,
) -> Result<Bytes, StatusCode> {
    let db = &state.read().unwrap().db;

    if let Some(value) = db.get(&key) {
        Ok(value.clone())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn kv_set(
    _req: Request<Body>,
    UrlParams((key,)): UrlParams<(String,)>,
    BytesMaxLength(value): BytesMaxLength<{ 1024 * 5_000 }>, // ~5mb
    Extension(state): Extension<SharedState>,
) {
    state.write().unwrap().db.insert(key, value);
}

async fn list_keys(_req: Request<Body>, Extension(state): Extension<SharedState>) -> String {
    let db = &state.read().unwrap().db;

    db.keys()
        .map(|key| key.to_string())
        .collect::<Vec<String>>()
        .join("\n")
}

fn admin_routes() -> BoxRoute<BoxBody> {
    async fn delete_all_keys(_req: Request<Body>, Extension(state): Extension<SharedState>) {
        state.write().unwrap().db.clear();
    }

    async fn remove_key(
        _req: Request<Body>,
        UrlParams((key,)): UrlParams<(String,)>,
        Extension(state): Extension<SharedState>,
    ) {
        state.write().unwrap().db.remove(&key);
    }

    route("/keys", delete(delete_all_keys))
        .route("/key/:key", delete(remove_key))
        // Require beare auth for all admin routes
        .layer(RequireAuthorizationLayer::bearer("secret-token"))
        .boxed()
}

fn handle_error(error: BoxError) -> impl IntoResponse {
    if error.is::<tower::timeout::error::Elapsed>() {
        return (StatusCode::REQUEST_TIMEOUT, Cow::from("request timed out"));
    }

    if error.is::<tower::load_shed::error::Overloaded>() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Cow::from("service is overloaded, try again later"),
        );
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Cow::from(format!("Unhandled internal error: {}", error)),
    )
}