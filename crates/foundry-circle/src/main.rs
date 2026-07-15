#[cfg(all(feature = "web", target_arch = "wasm32"))]
#[wasm_bindgen::prelude::wasm_bindgen]
unsafe extern "C" {}

#[cfg(not(feature = "server"))]
fn main() {
    dioxus::launch(foundry_circle::App);
}

#[cfg(feature = "server")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use axum::{Router, routing::get};
    use dioxus::server::{DioxusRouterExt, FullstackState, ServeConfig};
    use foundry_circle::http::api_router;
    use tower_http::compression::CompressionLayer;
    use tower_http::limit::RequestBodyLimitLayer;
    use tower_http::trace::TraceLayer;

    tracing_subscriber::fmt::init();

    let state = FullstackState::new(ServeConfig::new(), foundry_circle::App);
    let pages = Router::<FullstackState>::new()
        .register_server_functions()
        .serve_static_assets()
        .fallback(get(FullstackState::render_handler))
        .with_state(state);

    let app: Router = api_router()
        .merge(pages)
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    let bind =
        std::env::var("FOUNDRY_CIRCLE_BIND").unwrap_or_else(|_| "127.0.0.1:8031".to_string());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "foundry-circle listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
