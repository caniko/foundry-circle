use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};

use crate::driver::WorldState;

/// Build the non-Dioxus HTTP surface.  The routes are deliberately separate
/// from the browser renderer so API readiness can be tested without a browser.
pub fn api_router() -> Router {
    Router::new()
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/readyz", get(readyz))
        .route("/api/v1", get(discovery))
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz() -> impl IntoResponse {
    // The live driver and PostgreSQL checks are intentionally not fabricated;
    // this foundation reports a non-ready state until those dependencies are
    // wired by the service module and certified against a real world.
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "state": WorldState::Starting,
            "reason": "foundry driver and PostgreSQL readiness are not configured",
        })),
    )
}

async fn discovery() -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "apiVersion": "v1",
        "service": "foundry-circle",
        "links": {
            "healthz": "/api/v1/healthz",
            "readyz": "/api/v1/readyz",
            "console": "/api/console",
        }
    }))
}
