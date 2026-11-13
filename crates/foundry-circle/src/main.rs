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
    use foundry_circle::driver::{DriverConfig, LiveFoundryDriver};
    use foundry_circle::http::{AppState, ConsoleLinks, DatabaseState, assets, auth};

    tracing_subscriber::fmt::init();

    let oidc = auth::from_env().await.map_err(|error| {
        tracing::error!(%error, "Foundry Circle Rauthy configuration is invalid");
        error
    })?;

    let database = DatabaseState::new();
    database.supervise(
        std::env::var("DATABASE_URL").ok(),
        std::env::var("DATABASE_URL_FILE").ok().map(Into::into),
    );
    let driver_config = DriverConfig::from_env()?;
    let console = ConsoleLinks::from_env(&driver_config.base_url);
    let lease = foundry_circle::http::session::Lease::from_env(&driver_config.base_url);
    let driver = LiveFoundryDriver::start(driver_config.clone());

    let state = FullstackState::new(ServeConfig::new(), foundry_circle::App);
    let pages = Router::<FullstackState>::new()
        .register_server_functions()
        .serve_static_assets()
        .fallback(get(FullstackState::render_handler))
        .with_state(state);

    let app_state = AppState {
        driver: driver.clone(),
        database,
        auth: Some(oidc),
        console,
        lease: lease.clone(),
        assets: assets::AssetConfig::from_env(),
    };
    foundry_circle::http::session::spawn_reconciler(
        app_state.database.clone(),
        lease,
        driver,
        driver_config,
    );
    let app = foundry_circle::http::app(app_state, pages);

    let bind =
        std::env::var("FOUNDRY_CIRCLE_BIND").unwrap_or_else(|_| "127.0.0.1:8032".to_string());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "foundry-circle listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
