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

use crate::{
    ConsoleData, ConsoleView, LiveGame, Role,
    driver::{FakeDriver, FoundryDriver, WorldState},
};

#[path = "auth.rs"]
pub mod auth;

#[derive(Clone)]
pub struct AppState {
    pub driver: Arc<dyn FoundryDriver>,
    pub database: DatabaseState,
    pub auth: Option<auth::AuthState>,
    pub console: ConsoleLinks,
}

/// Origins the lobby links to and polls; all optional so a partially
/// configured service still renders.
#[derive(Clone, Default)]
pub struct ConsoleLinks {
    /// Internal Foundry origin used for the public `/api/status` poll.
    pub foundry_url: Option<String>,
    /// Gameplay origin; the lobby links to `<play_url>/join`.
    pub play_url: Option<String>,
    /// Platform origin; GMs get `<platform_url>/setup`.
    pub platform_url: Option<String>,
}

impl ConsoleLinks {
    pub fn from_env(foundry_url: &str) -> Self {
        let origin = |name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_owned())
                .filter(|value| !value.is_empty())
        };
        Self {
            foundry_url: Some(foundry_url.trim_end_matches('/').to_owned()),
            play_url: origin("FOUNDRY_CIRCLE_PLAY_BASE_URL"),
            platform_url: origin("FOUNDRY_CIRCLE_OIDC_PUBLIC_BASE_URL"),
        }
    }
}

#[derive(Clone, Default)]
pub struct DatabaseState {
    pool: Arc<RwLock<Option<PgPool>>>,
    last_error: Arc<RwLock<Option<String>>>,
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

    /// The last connect or migration failure, as a sanitized category.
    pub async fn last_error(&self) -> Option<String> {
        self.last_error.read().await.clone()
    }

    pub fn supervise(&self, url: Option<String>, url_file: Option<PathBuf>) {
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                if let Some(pool) = state.pool().await {
                    if let Err(error) = sqlx::query("SELECT 1").execute(&pool).await {
                        *state.pool.write().await = None;
                        *state.last_error.write().await = Some(sqlx_error_kind(&error));
                        pool.close().await;
                        tracing::warn!(%error, "PostgreSQL connection lost; readiness is false");
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
                                *state.last_error.write().await = None;
                                tracing::info!("PostgreSQL is ready");
                            }
                            Err(error) => {
                                tracing::warn!(%error, "PostgreSQL migrations are not ready");
                                *state.last_error.write().await = Some(migrate_error_kind(&error));
                                pool.close().await;
                            }
                        },
                        Err(error) => {
                            tracing::warn!(%error, "PostgreSQL connection is not ready");
                            *state.last_error.write().await = Some(sqlx_error_kind(&error));
                        }
                    }
                }
                sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

/// Readiness is unauthenticated, so report only a fixed category (plus
/// numeric migration versions and SQLSTATE codes), never the error text, which
/// can embed the DATABASE_URL, user names or hostnames.
fn sqlx_error_kind(error: &sqlx::Error) -> String {
    match error {
        sqlx::Error::Configuration(_) => "invalid database configuration".into(),
        sqlx::Error::Io(io) => format!("connection failed ({:?})", io.kind()),
        sqlx::Error::Tls(_) => "TLS error".into(),
        sqlx::Error::Database(db) => match db.code() {
            Some(code) => format!("database error (SQLSTATE {code})"),
            None => "database error".into(),
        },
        sqlx::Error::PoolTimedOut => "connection timed out".into(),
        _ => "database error".into(),
    }
}

fn migrate_error_kind(error: &sqlx::migrate::MigrateError) -> String {
    use sqlx::migrate::MigrateError as E;
    match error {
        E::Execute(e) => format!("migration failed: {}", sqlx_error_kind(e)),
        E::ExecuteMigration(e, v) => format!("migration {v} failed: {}", sqlx_error_kind(e)),
        E::VersionMismatch(v) => format!("migration {v} checksum mismatch"),
        E::VersionMissing(v) => format!("migration {v} missing from source"),
        E::VersionNotPresent(v) => format!("migration {v} not present in source"),
        E::VersionTooOld(v, _) => format!("migration {v} older than latest applied"),
        E::VersionTooNew(v, _) => format!("migration {v} newer than latest applied"),
        E::Dirty(v) => format!("migration {v} partially applied"),
        _ => "migration failed".into(),
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
            console: ConsoleLinks::default(),
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
    // Ready means the process can serve the console: the database is up. A
    // stopped or not-yet-launched world is reported in `state`, not as an outage.
    let ready = database;
    let database_error = state.database.last_error().await;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let mut body = serde_json::json!({
        "state": world_state,
        "database": database,
        "ready": ready,
    });
    if let Some(error) = database_error {
        body["databaseError"] = error.into();
    }
    (status, Json(body))
}

/// Poll Foundry's public `/api/status`; any failure is a state, never an error.
async fn live_game(foundry_url: Option<&str>) -> LiveGame {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let Some(url) = foundry_url else {
        return LiveGame::Unreachable;
    };
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default()
    });
    let response = client.get(format!("{url}/api/status")).send().await;
    match response {
        Ok(response) if response.status().is_success() => match response.json().await {
            Ok(status) => LiveGame::from_status(status),
            Err(_) => LiveGame::Unreachable,
        },
        _ => LiveGame::Unreachable,
    }
}

/// Resolve what the signed-in (or not) caller of the console should see.
pub async fn console_view() -> ConsoleView {
    use dioxus::fullstack::FullstackContext;
    let Some(context) = FullstackContext::current() else {
        return ConsoleView::SignedOut;
    };
    let (Some(state), Some(principal)) = (
        context.extension::<AppState>(),
        context.extension::<auth::Principal>(),
    ) else {
        return ConsoleView::SignedOut;
    };
    let name = principal
        .display_name
        .clone()
        .or_else(|| principal.email.clone())
        .unwrap_or_else(|| principal.subject.clone());
    let Some(auth) = state.auth.as_ref() else {
        return ConsoleView::SignedOut;
    };
    if !principal.groups.contains(&auth.access_group) {
        return ConsoleView::Denied { name };
    }
    let role = principal.role(&auth.admin_group, auth.gm_group.as_deref());
    let links = &state.console;
    let join = |origin: &Option<String>, path: &str| origin.as_ref().map(|o| format!("{o}{path}"));
    ConsoleView::Console(Box::new(ConsoleData {
        name,
        email: principal.email.clone(),
        role,
        game: live_game(links.foundry_url.as_deref()).await,
        join_url: join(&links.play_url, "/join"),
        setup_url: (role != Role::Player)
            .then(|| join(&links.platform_url, "/setup"))
            .flatten(),
        world_state: state.driver.world_state(),
        database: state.database.is_ready().await,
    }))
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

    #[test]
    fn database_errors_never_carry_the_url_or_credentials() {
        let url = "postgres://svc:hunter2@db.internal:5432/foundry";
        let config = sqlx::Error::Configuration(format!("bad {url}").into());
        let io = sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("refused {url}"),
        ));
        for error in [config, io] {
            let kind = sqlx_error_kind(&error);
            assert!(
                !kind.contains("hunter2") && !kind.contains("db.internal"),
                "{kind}"
            );
        }
        assert_eq!(
            sqlx_error_kind(&sqlx::Error::Io(
                std::io::ErrorKind::ConnectionRefused.into()
            )),
            "connection failed (ConnectionRefused)"
        );
        let migrate = sqlx::migrate::MigrateError::ExecuteMigration(
            sqlx::Error::Configuration(url.into()),
            2,
        );
        assert_eq!(
            migrate_error_kind(&migrate),
            "migration 2 failed: invalid database configuration"
        );
        assert_eq!(
            migrate_error_kind(&sqlx::migrate::MigrateError::VersionMismatch(2)),
            "migration 2 checksum mismatch"
        );
    }

    #[tokio::test]
    async fn readyz_reports_database_error_only_when_present() {
        let state = AppState::unconfigured();
        *state.database.last_error.write().await = Some("migration 2 checksum mismatch".into());
        let response = readyz(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "state": "starting",
                "database": false,
                "ready": false,
                "databaseError": "migration 2 checksum mismatch",
            })
        );
    }

    #[tokio::test]
    async fn unconfigured_database_is_not_ready() {
        assert_eq!(healthz().await.into_response().status(), StatusCode::OK);
        let response = readyz(State(AppState::unconfigured()))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"state": "starting", "database": false, "ready": false})
        );
    }

    #[tokio::test]
    async fn ready_world_requires_a_control_plane_database() {
        let state = AppState {
            driver: Arc::new(FakeDriver::new(WorldState::Ready)),
            database: DatabaseState::new(),
            auth: None,
            console: ConsoleLinks::default(),
        };
        let response = readyz(State(state.clone())).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["state"], "ready");
        assert_eq!(body["ready"], false);
        assert_eq!(
            world(State(state)).await.into_response().status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn healthy_database_is_ready_without_a_launched_world() {
        let state = AppState {
            driver: Arc::new(FakeDriver::new(WorldState::WorldStopped)),
            ..AppState::unconfigured()
        };
        *state.database.pool.write().await = Some(
            PgPoolOptions::new()
                .connect_lazy("postgresql:///foundry_circle?host=/run/postgresql&user=fc")
                .unwrap(),
        );
        let response = readyz(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"state": "world-stopped", "database": true, "ready": true})
        );
    }

    #[test]
    fn database_url_with_empty_host_authority_is_rejected() {
        use sqlx::postgres::PgConnectOptions;
        use std::str::FromStr;
        let options = PgConnectOptions::from_str(
            "postgresql:///foundry_circle?host=/run/postgresql&user=foundry-circle",
        )
        .unwrap();
        assert_eq!(options.get_username(), "foundry-circle");
        assert_eq!(options.get_database(), Some("foundry_circle"));
        assert!(
            PgConnectOptions::from_str(
                "postgresql://foundry-circle@/foundry_circle?host=/run/postgresql"
            )
            .is_err()
        );
    }

    #[test]
    fn foundry_status_maps_to_a_live_game_or_none() {
        let live = r#"{"active":true,"version":"13.351","world":"canary","system":"daggerheart","systemVersion":"1.6.4","users":4,"usersActive":2}"#;
        let LiveGame::Live(game) = LiveGame::from_status(serde_json::from_str(live).unwrap())
        else {
            panic!("active world is live");
        };
        assert_eq!(game.world.as_deref(), Some("canary"));
        assert_eq!(game.users_active, Some(2));
        let idle = r#"{"active":false,"version":"13.351"}"#;
        assert!(matches!(
            LiveGame::from_status(serde_json::from_str(idle).unwrap()),
            LiveGame::None
        ));
    }
}
