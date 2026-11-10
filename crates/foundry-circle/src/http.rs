use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::{sync::RwLock, time::sleep};

use crate::{
    ConsoleData, ConsoleView, LiveGame, Role,
    driver::{FakeDriver, FoundryDriver, WorldError, WorldOp, WorldState},
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

#[derive(Debug, Deserialize)]
pub struct DocumentQuery {
    pub limit: Option<u32>,
    pub after: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCreatePayload {
    content: String,
}

#[derive(Debug, Deserialize)]
struct DiceRollPayload {
    formula: String,
}

#[derive(Debug, Deserialize)]
struct IdPayload {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CombatUpdatePayload {
    action: String,
}

#[derive(Debug, Deserialize)]
struct PlaylistControlPayload {
    id: String,
    action: String,
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
        .route("/api/v1/world/documents/{collection}", get(documents))
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
        supports_events: false,
        supports_json_patch: false,
    })
}

fn oidc_not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "oidc_not_configured"})),
    )
        .into_response()
}

/// Players share the access group with the GM browser session, so document
/// reads must not fall through to what that session can see.
fn require_reader(
    principal: &auth::Principal,
    admin_group: &str,
    gm_group: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    if principal.role(admin_group, gm_group) == Role::Player {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "permission_denied"})),
        ))
    } else {
        Ok(())
    }
}

async fn documents(
    State(state): State<AppState>,
    Extension(principal): Extension<auth::Principal>,
    Path(collection): Path<String>,
    Query(query): Query<DocumentQuery>,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return oidc_not_configured();
    };
    if let Err((status, body)) =
        require_reader(&principal, &auth.admin_group, auth.gm_group.as_deref())
    {
        return (status, body).into_response();
    }
    read_documents(
        state.driver.as_ref(),
        &collection,
        query.limit.unwrap_or(50),
        query.after,
    )
    .await
}

async fn document(
    State(state): State<AppState>,
    Extension(principal): Extension<auth::Principal>,
    Path((collection, id)): Path<(String, String)>,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return oidc_not_configured();
    };
    if let Err((status, body)) =
        require_reader(&principal, &auth.admin_group, auth.gm_group.as_deref())
    {
        return (status, body).into_response();
    }
    read_document(state.driver.as_ref(), &collection, &id).await
}

async fn read_documents(
    driver: &dyn FoundryDriver,
    collection: &str,
    limit: u32,
    after: Option<String>,
) -> Response {
    let epoch = driver.snapshot().epoch;
    let op = WorldOp::ListDocuments {
        collection: collection.to_owned(),
        limit,
        after,
    };
    match driver.call(op).await {
        Ok(mut page) => {
            if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    document_envelope(item, epoch);
                }
            }
            if let Some(object) = page.as_object_mut() {
                object.insert("worldEpoch".into(), Value::from(epoch));
            }
            (StatusCode::OK, Json(page)).into_response()
        }
        Err(error) => world_error_response(error),
    }
}

async fn read_document(driver: &dyn FoundryDriver, collection: &str, id: &str) -> Response {
    let epoch = driver.snapshot().epoch;
    let op = WorldOp::GetDocument {
        collection: collection.to_owned(),
        id: id.to_owned(),
    };
    match driver.call(op).await {
        Ok(mut document) => {
            let etag = document_envelope(&mut document, epoch);
            let mut response = (StatusCode::OK, Json(document)).into_response();
            if let Ok(value) = HeaderValue::from_str(&etag) {
                response.headers_mut().insert(header::ETAG, value);
            }
            response
        }
        Err(error) => world_error_response(error),
    }
}

/// Adds the read envelope; returns the computed etag.
fn document_envelope(document: &mut Value, epoch: u64) -> String {
    // ponytail: serde_json sorted keys, not full RFC 8785; switch to a JCS crate if clients need cross-language etags
    let source = document.get("source").cloned().unwrap_or(Value::Null);
    let bytes = serde_json::to_vec(&source).unwrap_or_default();
    let etag = format!("sha256-{}", hex::encode(Sha256::digest(bytes)));
    if let Some(object) = document.as_object_mut() {
        object.insert("etag".into(), Value::String(etag.clone()));
        object.insert("worldEpoch".into(), Value::from(epoch));
    }
    etag
}

fn error_code(error: &WorldError) -> &'static str {
    match error {
        WorldError::NotReady => "world_not_ready",
        WorldError::Unsupported(_) => "capability_unsupported",
        WorldError::NotFound => "document_not_found",
        WorldError::Invalid(_) => "validation_failed",
        WorldError::Timeout => "browser_timeout",
        WorldError::Browser => "browser_error",
    }
}

fn world_error_parts(error: WorldError) -> (StatusCode, Json<Value>) {
    let status = match &error {
        WorldError::NotReady => StatusCode::SERVICE_UNAVAILABLE,
        WorldError::Unsupported(_) | WorldError::NotFound => StatusCode::NOT_FOUND,
        WorldError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        WorldError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        WorldError::Browser => StatusCode::BAD_GATEWAY,
    };
    let mut body = json!({"error": error_code(&error)});
    match error {
        WorldError::Unsupported(message) | WorldError::Invalid(message) => {
            body["message"] = message.into();
        }
        _ => {}
    }
    (status, Json(body))
}

fn world_error_response(error: WorldError) -> Response {
    world_error_parts(error).into_response()
}

fn parse_command(command: &str, payload: &Value) -> Result<WorldOp, (StatusCode, Json<Value>)> {
    let op = match command {
        "chat.create" => {
            from_payload::<ChatCreatePayload>(payload).map(|payload| WorldOp::ChatCreate {
                content: payload.content,
            })
        }
        "dice.roll" => from_payload::<DiceRollPayload>(payload).map(|payload| WorldOp::DiceRoll {
            formula: payload.formula,
        }),
        "scene.activate" => from_payload::<IdPayload>(payload)
            .map(|payload| WorldOp::SceneActivate { id: payload.id }),
        "combat.update" => {
            from_payload::<CombatUpdatePayload>(payload).map(|payload| WorldOp::CombatUpdate {
                action: payload.action,
            })
        }
        "playlist.control" => from_payload::<PlaylistControlPayload>(payload).map(|payload| {
            WorldOp::PlaylistControl {
                id: payload.id,
                action: payload.action,
            }
        }),
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": "capability_unsupported"})),
            ));
        }
    };
    let op = op.map_err(|message| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "validation_failed", "message": message})),
        )
    })?;
    crate::driver::validate_op(&op).map_err(world_error_parts)?;
    Ok(op)
}

fn from_payload<T: serde::de::DeserializeOwned>(payload: &Value) -> Result<T, String> {
    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())
}

enum Claim {
    Execute,
    Conflict,
    Replay(Value),
}

fn request_hash(command: &str, payload: &Value) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(command.as_bytes());
    hasher.update(serde_json::to_vec(payload).unwrap_or_default());
    hasher.finalize().to_vec()
}

async fn claim_idempotency(
    pool: &PgPool,
    subject: &str,
    key: &str,
    command: &str,
    payload: &Value,
) -> Result<Claim, sqlx::Error> {
    let hash = request_hash(command, payload);
    let inserted = sqlx::query(
        "INSERT INTO idempotency_keys (subject, key, request_hash) VALUES ($1, $2, $3) ON CONFLICT (subject, key) DO NOTHING",
    )
    .bind(subject)
    .bind(key)
    .bind(hash.as_slice())
    .execute(pool)
    .await?
    .rows_affected()
        > 0;
    if inserted {
        return Ok(Claim::Execute);
    }
    let row = sqlx::query(
        "SELECT request_hash, response FROM idempotency_keys WHERE subject = $1 AND key = $2",
    )
    .bind(subject)
    .bind(key)
    .fetch_one(pool)
    .await?;
    let stored: Vec<u8> = row.try_get("request_hash")?;
    if stored != hash {
        return Ok(Claim::Conflict);
    }
    let response: Option<Value> = row.try_get("response")?;
    Ok(response.map_or(Claim::Conflict, Claim::Replay))
}

fn idempotency_conflict() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error": "idempotency_conflict"})),
    )
        .into_response()
}

async fn audit_command(
    state: &AppState,
    subject: &str,
    command: &str,
    outcome: &Result<Value, WorldError>,
) {
    let Some(pool) = state.database.pool().await else {
        return;
    };
    let outcome = match outcome {
        Ok(_) => "ok",
        Err(error) => error_code(error),
    };
    if let Err(error) = sqlx::query(
        "INSERT INTO audit_log (subject, action, resource, outcome, details) VALUES ($1, $2, 'world', $3, '{}'::jsonb)",
    )
    .bind(subject)
    .bind(command)
    .bind(outcome)
    .execute(&pool)
    .await
    {
        tracing::warn!(%error, "failed to write Foundry Circle audit entry");
    }
}

async fn command(
    State(state): State<AppState>,
    Extension(principal): Extension<auth::Principal>,
    Json(request): Json<CommandRequest>,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return oidc_not_configured();
    };
    if !principal.is_admin(&auth.admin_group) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "admin_required"})),
        )
            .into_response();
    }
    let op = match parse_command(&request.command, &request.payload) {
        Ok(op) => op,
        Err((status, body)) => return (status, body).into_response(),
    };
    let pool = state.database.pool().await;
    let mut idempotency = None;
    if let (Some(key), Some(pool)) = (request.idempotency_key.as_deref(), pool.as_ref()) {
        match claim_idempotency(
            pool,
            &principal.subject,
            key,
            &request.command,
            &request.payload,
        )
        .await
        {
            Ok(Claim::Replay(response)) => return (StatusCode::OK, Json(response)).into_response(),
            Ok(Claim::Conflict) => return idempotency_conflict(),
            Ok(Claim::Execute) => idempotency = Some((pool.clone(), key.to_owned())),
            Err(error) => {
                tracing::warn!(%error, "failed to claim Foundry Circle idempotency key");
            }
        }
    }
    let outcome = state.driver.call(op).await;
    audit_command(&state, &principal.subject, &request.command, &outcome).await;
    match outcome {
        Ok(value) => {
            let body = json!({"command": request.command, "result": value});
            if let Some((pool, key)) = idempotency {
                if let Err(error) = sqlx::query(
                    "UPDATE idempotency_keys SET response = $3 WHERE subject = $1 AND key = $2",
                )
                .bind(&principal.subject)
                .bind(&key)
                .bind(body.clone())
                .execute(&pool)
                .await
                {
                    tracing::warn!(%error, "failed to store Foundry Circle idempotent response");
                }
            }
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(error) => {
            // Release the claim so the caller can retry a failed command. A
            // timeout may still have run in Foundry, so that key stays claimed.
            if let (Some((pool, key)), false) = (idempotency, error == WorldError::Timeout) {
                if let Err(error) =
                    sqlx::query("DELETE FROM idempotency_keys WHERE subject = $1 AND key = $2")
                        .bind(&principal.subject)
                        .bind(&key)
                        .execute(&pool)
                        .await
                {
                    tracing::warn!(%error, "failed to release Foundry Circle idempotency key");
                }
            }
            world_error_response(error)
        }
    }
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

    #[tokio::test]
    async fn ready_world_response_matches_the_driver_snapshot() {
        struct SnapshotDriver(crate::driver::WorldSnapshot);
        impl FoundryDriver for SnapshotDriver {
            fn snapshot(&self) -> crate::driver::WorldSnapshot {
                self.0.clone()
            }
        }

        let state = AppState {
            driver: Arc::new(SnapshotDriver(crate::driver::WorldSnapshot {
                state: WorldState::Ready,
                id: Some("canary".into()),
                epoch: 7,
                foundry_version: Some("13.351".into()),
                system_id: Some("daggerheart".into()),
                system_version: Some("1.6.4".into()),
                is_gm: true,
            })),
            ..AppState::unconfigured()
        };
        let response = world(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
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
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let requests = [
            Request::get("/api/v1").body(Body::empty()),
            Request::get("/api/v1/me").body(Body::empty()),
            Request::get("/api/v1/world").body(Body::empty()),
            Request::get("/api/v1/world/capabilities").body(Body::empty()),
            Request::get("/api/v1/world/documents/actors").body(Body::empty()),
            Request::get("/api/v1/world/documents/actors/example").body(Body::empty()),
            Request::post("/api/v1/world/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"command":"scene.activate","payload":{},"idempotencyKey":"test"}"#,
                )),
        ];
        for request in requests {
            let request = request.unwrap();
            let path = request.uri().path().to_owned();
            let state = AppState {
                driver: Arc::new(FakeDriver::new(WorldState::Ready)),
                ..AppState::unconfigured()
            };
            let response = api_router_with_state(state).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(&body[..], b"OIDC is not configured", "{path}");
        }
    }

    fn principal(groups: &[&str]) -> auth::Principal {
        auth::Principal {
            subject: "subject".into(),
            issuer: "https://id.example".into(),
            display_name: None,
            email: None,
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        }
    }

    async fn body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn players_cannot_read_world_documents() {
        let player = principal(&["foundry-circle-users"]);
        let (status, Json(body)) =
            require_reader(&player, "foundry-circle-admins", Some("foundry-circle-gms"))
                .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "permission_denied");
    }

    #[tokio::test]
    async fn gms_read_fixture_documents_with_a_stable_etag() {
        let gm = principal(&["foundry-circle-users", "foundry-circle-gms"]);
        assert!(require_reader(&gm, "foundry-circle-admins", Some("foundry-circle-gms")).is_ok());

        let driver = FakeDriver::new(WorldState::Ready);
        let first = read_document(&driver, "actors", "a1").await;
        assert_eq!(first.status(), StatusCode::OK);
        let header_etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        let first = body(first).await;
        assert_eq!(first["id"], "a1");
        assert_eq!(first["source"]["_id"], "a1");
        assert_eq!(first["etag"], header_etag);
        assert!(header_etag.starts_with("sha256-"));

        let second = body(read_document(&driver, "actors", "a1").await).await;
        assert_eq!(second["etag"], first["etag"]);
    }

    #[tokio::test]
    async fn unknown_collections_are_reported_as_unsupported() {
        let response = read_documents(&FakeDriver::new(WorldState::Ready), "items", 50, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body(response).await["error"], "capability_unsupported");
    }

    #[tokio::test]
    async fn document_reads_require_a_ready_world() {
        let response = read_document(&FakeDriver::new(WorldState::Starting), "actors", "a1").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body(response).await["error"], "world_not_ready");
    }

    #[tokio::test]
    async fn oversized_chat_commands_fail_validation() {
        let payload = json!({"content": "x".repeat(4001)});
        let (status, Json(body)) = parse_command("chat.create", &payload).unwrap_err();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "validation_failed");
        assert!(body["message"].as_str().unwrap().contains("4000"));
    }

    #[tokio::test]
    async fn capabilities_only_advertise_implemented_surfaces() {
        let body = body(capabilities().await.into_response()).await;
        assert_eq!(body["supportsEvents"], false);
        assert_eq!(body["supportsJsonPatch"], false);
        assert_eq!(
            body["collections"],
            json!(["actors", "scenes", "messages", "playlists", "tables"])
        );
        assert_eq!(
            body["commands"],
            json!([
                "chat.create",
                "dice.roll",
                "scene.activate",
                "combat.update",
                "playlist.control"
            ])
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
