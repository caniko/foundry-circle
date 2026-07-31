use std::{path::PathBuf, sync::Arc, time::Duration};

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
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{sync::RwLock, time::sleep};

use crate::driver::{FakeDriver, FoundryDriver, WorldState};

#[path = "auth.rs"]
pub mod auth;

#[derive(Clone)]
pub struct AppState {
    pub driver: Arc<dyn FoundryDriver>,
    pub database: DatabaseState,
    pub auth: Option<auth::AuthState>,
}

#[derive(Clone, Default)]
pub struct DatabaseState {
    pool: Arc<RwLock<Option<PgPool>>>,
}

impl DatabaseState {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn pool(&self) -> Option<PgPool> {
        self.pool.read().await.clone()
    }

    pub async fn is_ready(&self) -> bool {
        self.pool.read().await.is_some()
    }

    pub fn supervise(&self, url: Option<String>, url_file: Option<PathBuf>) {
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                if let Some(pool) = state.pool().await {
                    if sqlx::query("SELECT 1").execute(&pool).await.is_err() {
                        *state.pool.write().await = None;
                        pool.close().await;
                        tracing::warn!("PostgreSQL connection lost; readiness is false");
                    }
                } else if let Some(database_url) = database_url(&url, url_file.as_ref()) {
                    match PgPoolOptions::new()
                        .max_connections(5)
                        .connect(&database_url)
                        .await
                    {
                        Ok(pool) => match sqlx::migrate!("./migrations").run(&pool).await {
                            Ok(()) => {
                                *state.pool.write().await = Some(pool);
                                tracing::info!("PostgreSQL is ready");
                            }
                            Err(error) => {
                                tracing::warn!(%error, "PostgreSQL migrations are not ready");
                                pool.close().await;
                            }
                        },
                        Err(error) => tracing::warn!(%error, "PostgreSQL connection is not ready"),
                    }
                }
                sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

fn database_url(url: &Option<String>, url_file: Option<&PathBuf>) -> Option<String> {
    url.as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            url_file
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
}

impl AppState {
    pub fn unconfigured() -> Self {
        Self {
            driver: Arc::new(FakeDriver::new(WorldState::Starting)),
            database: DatabaseState::new(),
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
    pub system_id: Option<String>,
    pub system_version: Option<String>,
    pub is_gm: bool,
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
    let database = state.database.is_ready().await;
    let ready = world_state == WorldState::Ready && database;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(serde_json::json!({
            "state": world_state,
            "database": database,
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
    let snapshot = state.driver.snapshot();
    let descriptor = WorldDescriptor {
        id: snapshot.id.clone().unwrap_or_else(|| "active".to_string()),
        epoch: snapshot.epoch,
        state: snapshot.state,
        foundry_version: snapshot.foundry_version.clone(),
        system_id: snapshot.system_id.clone(),
        system_version: snapshot.system_version.clone(),
        is_gm: snapshot.is_gm,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use std::{path::PathBuf, sync::Arc};
    use tower::ServiceExt;

    fn state(world_state: WorldState) -> AppState {
        AppState {
            driver: Arc::new(FakeDriver::new(world_state)),
            database: DatabaseState::new(),
            auth: None,
        }
    }

    async fn text(response: axum::response::Response) -> String {
        String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("UTF-8 response body")
    }

    async fn json(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body"),
        )
        .expect("JSON response body")
    }

    #[test]
    fn database_url_ignores_empty_environment_and_missing_credential() {
        assert_eq!(database_url(&Some("  ".into()), None), None);
        assert_eq!(
            database_url(&None, Some(&PathBuf::from("/missing/database-url"))),
            None
        );
        assert_eq!(
            database_url(&Some("postgres:///foundry".into()), None),
            Some("postgres:///foundry".into())
        );
    }

    #[tokio::test]
    async fn health_is_public_and_readiness_requires_world_and_database() {
        let response = api_router_with_state(state(WorldState::Starting))
            .oneshot(
                Request::get("/api/v1/healthz")
                    .body(Body::empty())
                    .expect("health request"),
            )
            .await
            .expect("health response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(text(response).await, "ok");

        let response = api_router_with_state(state(WorldState::Ready))
            .oneshot(
                Request::get("/api/v1/readyz")
                    .body(Body::empty())
                    .expect("readiness request"),
            )
            .await
            .expect("readiness response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json(response).await,
            serde_json::json!({
                "state": "ready",
                "database": false,
                "ready": false
            })
        );
    }

    #[tokio::test]
    async fn ready_world_response_matches_the_driver_snapshot() {
        let snapshot = crate::driver::WorldSnapshot {
            state: WorldState::Ready,
            id: Some("canary".into()),
            epoch: 7,
            foundry_version: Some("13.351".into()),
            system_id: Some("daggerheart".into()),
            system_version: Some("1.6.4".into()),
            is_gm: true,
        };
        let response = world(State(AppState {
            driver: Arc::new(FakeDriver::from_snapshot(snapshot)),
            database: DatabaseState::new(),
            auth: None,
        }))
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json(response).await,
            serde_json::json!({
                "id": "canary",
                "epoch": 7,
                "state": "ready",
                "foundryVersion": "13.351",
                "systemId": "daggerheart",
                "systemVersion": "1.6.4",
                "isGm": true
            })
        );
    }

    #[tokio::test]
    async fn protected_world_routes_fail_closed_without_oidc() {
        let requests = [
            Request::get("/api/v1")
                .body(Body::empty())
                .expect("discovery request"),
            Request::get("/api/v1/world")
                .body(Body::empty())
                .expect("world request"),
            Request::get("/api/v1/world/documents/actors/example")
                .body(Body::empty())
                .expect("document request"),
            Request::post("/api/v1/world/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"command":"scene.activate","payload":{},"idempotencyKey":"test"}"#,
                ))
                .expect("command request"),
        ];

        for request in requests {
            let response = api_router_with_state(state(WorldState::Ready))
                .oneshot(request)
                .await
                .expect("protected response");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(text(response).await, "OIDC is not configured");
        }
    }
}
