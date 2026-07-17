use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;

use crate::driver::{FakeDriver, FoundryDriver, WorldState};

#[path = "auth.rs"]
pub mod auth;

#[derive(Clone)]
pub struct AppState {
    pub driver: Arc<dyn FoundryDriver>,
    pub database: Option<PgPool>,
    pub auth: Option<auth::AuthState>,
}

impl AppState {
    pub fn unconfigured() -> Self {
        Self {
            driver: Arc::new(FakeDriver::new(WorldState::Starting)),
            database: None,
            auth: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldDescriptor {
    pub id: String,
    pub epoch: u64,
    pub state: WorldState,
    pub foundry_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub collections: Vec<&'static str>,
    pub commands: Vec<&'static str>,
    pub supports_events: bool,
    pub supports_json_patch: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRequest {
    pub command: String,
    pub payload: serde_json::Value,
    pub idempotency_key: Option<String>,
}

/// Build the non-Dioxus HTTP surface.  The routes are deliberately separate
/// from the browser renderer so API readiness can be tested without a browser.
pub fn api_router() -> Router {
    api_router_with_state(AppState::unconfigured())
}

pub fn api_router_with_state(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v1", get(discovery))
        .route("/api/v1/me", get(me))
        .route("/api/v1/world", get(world))
        .route("/api/v1/world/capabilities", get(capabilities))
        .route("/api/v1/world/documents/{collection}/{id}", get(document))
        .route("/api/v1/world/commands", post(command))
        .layer(from_fn_with_state(state.clone(), auth::require_session));

    Router::new()
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/readyz", get(readyz))
        .route("/api/auth/login", get(auth::login_start))
        .route("/api/auth/oidc/callback", get(auth::oidc_callback))
        .route("/api/auth/logout", post(auth::logout))
        .merge(protected)
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let world_state = state.driver.world_state();
    let ready = world_state == WorldState::Ready && state.database.is_some();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(serde_json::json!({
            "state": world_state,
            "database": state.database.is_some(),
            "ready": ready,
        })),
    )
}

async fn discovery() -> impl IntoResponse {
    Json(serde_json::json!({
        "apiVersion": "v1",
        "service": "foundry-circle",
        "contract": "typed-world-v1",
        "links": {
            "healthz": "/api/v1/healthz",
            "readyz": "/api/v1/readyz",
            "world": "/api/v1/world",
            "capabilities": "/api/v1/world/capabilities",
            "console": "/api/console",
        }
    }))
}

async fn me(Extension(principal): Extension<auth::Principal>) -> impl IntoResponse {
    Json(json!({
        "subject": principal.subject,
        "issuer": principal.issuer,
        "displayName": principal.display_name,
        "email": principal.email,
        "groups": principal.groups,
    }))
}

async fn world(State(state): State<AppState>) -> impl IntoResponse {
    let descriptor = WorldDescriptor {
        id: "active".to_string(),
        epoch: 0,
        state: state.driver.world_state(),
        foundry_version: None,
    };
    if descriptor.state == WorldState::Ready {
        (StatusCode::OK, Json(serde_json::json!(descriptor)))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "world_not_ready",
                "world": descriptor,
            })),
        )
    }
}

async fn capabilities() -> impl IntoResponse {
    Json(Capabilities {
        collections: vec!["actors", "scenes", "messages", "playlists", "tables"],
        commands: vec![
            "chat.create",
            "dice.roll",
            "scene.activate",
            "combat.update",
            "playlist.control",
        ],
        supports_events: true,
        supports_json_patch: true,
    })
}

async fn document(
    State(state): State<AppState>,
    Path((collection, id)): Path<(String, String)>,
) -> impl IntoResponse {
    if state.driver.world_state() != WorldState::Ready {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "world_not_ready",
                "collection": collection,
                "id": id,
            })),
        );
    }
    // Foundry remains authoritative; this handler will call the typed browser
    // driver once a live fixture certifies the collection contract.
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "driver_contract_pending",
            "collection": collection,
            "id": id,
        })),
    )
}

async fn command(
    State(state): State<AppState>,
    Extension(principal): Extension<auth::Principal>,
    Json(request): Json<CommandRequest>,
) -> impl IntoResponse {
    let Some(auth) = state.auth.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "oidc_not_configured"})),
        );
    };
    if !principal.is_admin(&auth.admin_group) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "admin_required"})),
        );
    }
    if state.driver.world_state() != WorldState::Ready {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "world_not_ready"})),
        );
    }
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "driver_contract_pending",
            "command": request.command,
            "accepted": false,
        })),
    )
}
