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
    use axum::{Router, middleware::from_fn_with_state, routing::get};
    use dioxus::server::{DioxusRouterExt, FullstackState, ServeConfig};
    use foundry_circle::http::{AppState, api_router_with_state, auth};
    use tower_http::compression::CompressionLayer;
    use tower_http::limit::RequestBodyLimitLayer;
    use tower_http::trace::TraceLayer;

    tracing_subscriber::fmt::init();

    let oidc = auth::from_env().await.map_err(|error| {
        tracing::error!(%error, "Foundry Circle Rauthy configuration is invalid");
        error
    })?;

    let database_url = std::env::var("DATABASE_URL").ok().or_else(|| {
        std::env::var("DATABASE_URL_FILE")
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|value| value.trim().to_owned())
    });
    let database = match database_url {
        Some(url) => match sqlx::PgPool::connect(&url).await {
            Ok(pool) => match sqlx::migrate!("./migrations").run(&pool).await {
                Ok(()) => Some(pool),
                Err(error) => {
                    tracing::error!(%error, "PostgreSQL migrations failed");
                    None
                }
            },
            Err(error) => {
                tracing::error!(%error, "PostgreSQL connection failed");
                None
            }
        },
        None => {
            tracing::warn!("DATABASE_URL is unset; readiness will remain false");
            None
        }
    };

    let state = FullstackState::new(ServeConfig::new(), foundry_circle::App);
    let pages = Router::<FullstackState>::new()
        .register_server_functions()
        .serve_static_assets()
        .fallback(get(FullstackState::render_handler))
        .with_state(state);

    let app_state = AppState {
        driver: std::sync::Arc::new(foundry_circle::driver::FakeDriver::new(
            foundry_circle::driver::WorldState::Starting,
        )),
        database,
        auth: Some(oidc),
    };
    let app: Router = api_router_with_state(app_state.clone())
        .merge(pages.layer(from_fn_with_state(app_state.clone(), auth::require_session)))
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    let bind =
        std::env::var("FOUNDRY_CIRCLE_BIND").unwrap_or_else(|_| "127.0.0.1:8032".to_string());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "foundry-circle listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
