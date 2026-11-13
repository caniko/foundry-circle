use std::{sync::Arc, time::Duration};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{AppendHeaders, IntoResponse, Redirect, Response},
};
use chrono::{DateTime, Utc};
use oidc_app_auth::{
    AuthError, Callback, LoginTransaction, OidcClient, OidcConfig, SessionToken, VerifiedIdentity,
    sanitize_return_to,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    Role,
    http::{AppState, session},
};

#[derive(Clone)]
pub struct AuthState {
    pub client: Arc<OidcClient>,
    /// The play host's client: the same issuer, client id and scopes with the
    /// `/_circle/oidc/callback` redirect URI. `None` when no play host is
    /// configured.
    pub play_client: Option<Arc<OidcClient>>,
    pub access_group: String,
    pub admin_group: String,
    pub gm_group: Option<String>,
    pub session_ttl: Duration,
}

/// The two hosts that run the same OIDC login. Cookies are host-only, so each
/// host has its own session and browser-binding cookie, and the database row
/// records which host created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginHost {
    Platform,
    Play,
}

/// The play host's session cookie; the gate reads it from the original
/// request's `Cookie` header.
pub(crate) const PLAY_SESSION_COOKIE: &str = "foundry_circle_play";

impl LoginHost {
    /// The `sessions.kind` value; every session lookup filters on it, so a
    /// platform token never authenticates on the play host or the reverse.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Play => "play",
        }
    }

    const fn cookie_name(self) -> &'static str {
        match self {
            Self::Platform => "foundry_circle_session",
            Self::Play => PLAY_SESSION_COOKIE,
        }
    }

    const fn binding_cookie_name(self) -> &'static str {
        match self {
            Self::Platform => "foundry_circle_login",
            Self::Play => "foundry_circle_play_login",
        }
    }

    const fn binding_cookie_path(self) -> &'static str {
        match self {
            Self::Platform => "/api/auth",
            Self::Play => "/_circle",
        }
    }

    fn default_return_to(self) -> &'static str {
        match self {
            Self::Platform => "/api/console",
            Self::Play => "/join",
        }
    }
}

impl AuthState {
    fn client(&self, host: LoginHost) -> Option<&OidcClient> {
        match host {
            LoginHost::Platform => Some(self.client.as_ref()),
            LoginHost::Play => self.play_client.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Principal {
    pub subject: String,
    pub issuer: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

impl Principal {
    pub fn is_admin(&self, admin_group: &str) -> bool {
        self.groups.iter().any(|group| group == admin_group)
    }

    /// Admin outranks GM; any other member of the access group is a player.
    pub fn role(&self, admin_group: &str, gm_group: Option<&str>) -> Role {
        if self.is_admin(admin_group) {
            Role::Admin
        } else if gm_group.is_some_and(|gm| self.groups.iter().any(|group| group == gm)) {
            Role::Gm
        } else {
            Role::Player
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub return_to: Option<String>,
}

pub async fn from_env() -> Result<AuthState, AuthError> {
    let issuer = required_env("FOUNDRY_CIRCLE_OIDC_ISSUER")?;
    let client_id = required_env("FOUNDRY_CIRCLE_OIDC_CLIENT_ID")?;
    let public_base_url = required_env("FOUNDRY_CIRCLE_OIDC_PUBLIC_BASE_URL")?;
    let scopes: Vec<String> = std::env::var("FOUNDRY_CIRCLE_OIDC_SCOPES")
        .unwrap_or_else(|_| "openid profile email groups".into())
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let access_group = std::env::var("FOUNDRY_CIRCLE_OIDC_ACCESS_GROUP")
        .unwrap_or_else(|_| "foundry-circle-users".into());
    let admin_group = std::env::var("FOUNDRY_CIRCLE_OIDC_ADMIN_GROUP")
        .unwrap_or_else(|_| "foundry-circle-admins".into());
    let gm_group = std::env::var("FOUNDRY_CIRCLE_OIDC_GM_GROUP")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let session_ttl = std::env::var("FOUNDRY_CIRCLE_SESSION_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(43_200);
    let client = OidcClient::discover(OidcConfig {
        issuer: issuer.clone(),
        client_id: client_id.clone(),
        redirect_uri: format!("{public_base_url}/api/auth/oidc/callback"),
        scopes: scopes.clone(),
    })
    .await?;
    // A second registered redirect URI on the same Rauthy client: the play
    // host has its own origin, so it needs its own cookie-scoped login.
    let play_client = match play_base_url() {
        Some(base) => Some(Arc::new(
            OidcClient::discover(OidcConfig {
                issuer,
                client_id,
                redirect_uri: format!("{base}/_circle/oidc/callback"),
                scopes,
            })
            .await?,
        )),
        None => None,
    };
    Ok(AuthState {
        client: Arc::new(client),
        play_client,
        access_group,
        admin_group,
        gm_group,
        session_ttl: Duration::from_secs(session_ttl),
    })
}

/// The browser-facing play origin, without a trailing slash.
fn play_base_url() -> Option<String> {
    std::env::var("FOUNDRY_CIRCLE_PLAY_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
}

fn required_env(name: &str) -> Result<String, AuthError> {
    let value =
        std::env::var(name).map_err(|_| AuthError::Configuration(format!("{name} is not set")))?;
    if value.trim().is_empty() {
        Err(AuthError::Configuration(format!("{name} is empty")))
    } else {
        Ok(value)
    }
}

pub async fn login_start(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Response {
    start_login(state, LoginHost::Platform, query.return_to).await
}

/// `GET /_circle/login`: the same flow with the play host's callback and
/// cookies.
pub async fn play_login_start(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Response {
    start_login(state, LoginHost::Play, query.return_to).await
}

async fn start_login(state: AppState, host: LoginHost, return_to: Option<String>) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OIDC is not configured").into_response();
    };
    let Some(client) = auth.client(host) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "OIDC is not configured for this host",
        )
            .into_response();
    };
    let Some(pool) = state.database.pool().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "database is not configured",
        )
            .into_response();
    };
    let request = match client.authorization_request(sanitize_return_to(return_to.as_deref())) {
        Ok(request) => request,
        Err(error) => return auth_error_response(error),
    };
    if let Err(error) = save_transaction(&pool, &request.transaction).await {
        tracing::error!(%error, "failed to save OIDC login transaction");
        return (StatusCode::INTERNAL_SERVER_ERROR, "login transaction error").into_response();
    }
    (
        AppendHeaders([(
            header::SET_COOKIE,
            login_binding_cookie(host, &request.transaction.csrf_state),
        )]),
        Redirect::to(&request.url),
    )
        .into_response()
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(callback): Query<Callback>,
) -> Response {
    clear_login_binding(
        complete_login(state, LoginHost::Platform, &headers, callback).await,
        LoginHost::Platform,
    )
}

pub async fn play_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(callback): Query<Callback>,
) -> Response {
    clear_login_binding(
        complete_login(state, LoginHost::Play, &headers, callback).await,
        LoginHost::Play,
    )
}

fn clear_login_binding(mut response: Response, host: LoginHost) -> Response {
    if let Ok(cookie) = header::HeaderValue::try_from(expired_login_binding_cookie(host)) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// Binds the callback to the browser that started the login: the server-side
/// transaction alone does not prove the flow belongs to this cookie jar.
async fn complete_login(
    state: AppState,
    host: LoginHost,
    headers: &HeaderMap,
    callback: Callback,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OIDC is not configured").into_response();
    };
    let Some(client) = auth.client(host) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "OIDC is not configured for this host",
        )
            .into_response();
    };
    let Some(pool) = state.database.pool().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "database is not configured",
        )
            .into_response();
    };
    let Some(state_value) = callback.state.as_deref() else {
        return (StatusCode::BAD_REQUEST, "missing OIDC state").into_response();
    };
    if !login_binding_matches(host, headers, state_value) {
        return (
            StatusCode::BAD_REQUEST,
            "login flow is not bound to this browser",
        )
            .into_response();
    }
    let transaction = match take_transaction(&pool, state_value).await {
        Ok(Some(transaction)) => transaction,
        Ok(None) => {
            return (StatusCode::BAD_REQUEST, "invalid or replayed login flow").into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to consume OIDC login transaction");
            return (StatusCode::INTERNAL_SERVER_ERROR, "login transaction error").into_response();
        }
    };
    let identity = match client.complete(callback, transaction.clone()).await {
        Ok(identity) => identity,
        Err(error) => return auth_error_response(error),
    };
    if !identity
        .groups
        .iter()
        .any(|group| group == &auth.access_group)
    {
        return (
            StatusCode::FORBIDDEN,
            "Foundry Circle access is not provisioned",
        )
            .into_response();
    }
    let token = SessionToken::generate();
    let return_to = match host {
        LoginHost::Platform => sanitize_return_to(transaction.return_to.as_deref())
            .unwrap_or_else(|| host.default_return_to().into()),
        LoginHost::Play => play_return_to(transaction.return_to.as_deref()),
    };
    if let Err(error) = save_principal_and_session(&pool, auth, &identity, &token, host).await {
        tracing::error!(%error, "failed to create Foundry Circle session");
        return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
    }
    (
        AppendHeaders([(
            header::SET_COOKIE,
            session_cookie(host, token.as_str(), auth.session_ttl),
        )]),
        Redirect::to(&return_to),
    )
        .into_response()
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(pool) = state.database.pool().await else {
        return (StatusCode::NO_CONTENT, "").into_response();
    };
    if let Some(token) = cookie(&headers, LoginHost::Platform.cookie_name()) {
        let hash = SessionToken::hash_value(token);
        if let Err(error) = sqlx::query(
            "UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND kind = 'platform'",
        )
        .bind(hash)
        .execute(&pool)
        .await
        {
            tracing::warn!(%error, "failed to revoke Foundry Circle session");
        }
    }
    (
        AppendHeaders([(header::SET_COOKIE, expired_session_cookie())]),
        Redirect::to("/api/console"),
    )
        .into_response()
}

pub async fn require_session(
    State(state): State<AppState>,
    mut request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OIDC is not configured").into_response();
    };
    let Some(pool) = state.database.pool().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "database is not configured",
        )
            .into_response();
    };
    let Some(token) = cookie(request.headers(), LoginHost::Platform.cookie_name()) else {
        return Redirect::to("/api/auth/login").into_response();
    };
    let hash = SessionToken::hash_value(token);
    let principal = match session_principal(&pool, &hash, LoginHost::Platform.kind()).await {
        Ok(Some(principal)) => principal,
        Ok(None) => return Redirect::to("/api/auth/login").into_response(),
        Err(error) => {
            tracing::warn!(%error, "failed to resolve Foundry Circle session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    if !principal
        .groups
        .iter()
        .any(|group| group == &auth.access_group)
    {
        return (
            StatusCode::FORBIDDEN,
            "Foundry Circle access is no longer provisioned",
        )
            .into_response();
    }
    // Every authenticated request from the holder counts as presence (D3).
    session::touch(&pool, &principal.subject).await;
    request.extensions_mut().insert(principal);
    next.run(request).await
}

/// Page-route variant of `require_session`: attaches the principal when a
/// valid session exists and lets the console render its signed-out and
/// access-denied states itself instead of redirecting or returning bare text.
pub async fn optional_session(
    State(state): State<AppState>,
    mut request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    request.extensions_mut().insert(state.clone());
    if let (Some(pool), Some(token)) = (
        state.database.pool().await,
        cookie(request.headers(), LoginHost::Platform.cookie_name()),
    ) {
        match session_principal(
            &pool,
            &SessionToken::hash_value(token),
            LoginHost::Platform.kind(),
        )
        .await
        {
            Ok(Some(principal)) => {
                // Console use by the holder counts as presence too (D3).
                if state.auth.as_ref().is_some_and(|auth| {
                    principal
                        .groups
                        .iter()
                        .any(|group| group == &auth.access_group)
                }) {
                    session::touch(&pool, &principal.subject).await;
                }
                request.extensions_mut().insert(principal);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "failed to resolve Foundry Circle session"),
        }
    }
    next.run(request).await
}

async fn save_transaction(
    pool: &PgPool,
    transaction: &LoginTransaction,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oidc_transactions (state, code_verifier, nonce, redirect_uri, return_to, expires_at) VALUES ($1, $2, $3, $4, $5, now() + interval '10 minutes')",
    )
    .bind(&transaction.csrf_state)
    .bind(transaction.pkce_verifier.as_bytes())
    .bind(&transaction.nonce)
    .bind("")
    .bind(&transaction.return_to)
    .execute(pool)
    .await
    .map(|_| ())
}

async fn take_transaction(
    pool: &PgPool,
    state: &str,
) -> Result<Option<LoginTransaction>, sqlx::Error> {
    let mut transaction: Transaction<'_, Postgres> = pool.begin().await?;
    let row = sqlx::query("SELECT code_verifier, nonce, return_to, expires_at FROM oidc_transactions WHERE state = $1 FOR UPDATE")
        .bind(state)
        .fetch_optional(&mut *transaction)
        .await?;
    let Some(row) = row else {
        transaction.commit().await?;
        return Ok(None);
    };
    sqlx::query("DELETE FROM oidc_transactions WHERE state = $1")
        .bind(state)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    let expires_at: DateTime<Utc> = row.try_get("expires_at")?;
    if expires_at <= Utc::now() {
        return Ok(None);
    }
    Ok(Some(LoginTransaction {
        pkce_verifier: String::from_utf8(row.try_get::<Vec<u8>, _>("code_verifier")?)
            .unwrap_or_default(),
        csrf_state: state.to_owned(),
        nonce: row.try_get("nonce")?,
        issued_at: unix_timestamp(expires_at - chrono::Duration::minutes(10)),
        return_to: row.try_get("return_to")?,
    }))
}

async fn save_principal_and_session(
    pool: &PgPool,
    auth: &AuthState,
    identity: &VerifiedIdentity,
    token: &SessionToken,
    host: LoginHost,
) -> Result<(), sqlx::Error> {
    let groups: Vec<String> = identity.groups.iter().cloned().collect();
    let claims =
        json!({"groups": groups, "email": identity.email, "display_name": identity.display_name});
    let configured_expiry = Utc::now()
        + chrono::Duration::from_std(auth.session_ttl)
            .unwrap_or_else(|_| chrono::Duration::hours(12));
    let expires_at = identity
        .expires_at
        .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp as i64, 0))
        .map(|token_expiry| token_expiry.min(configured_expiry))
        .unwrap_or(configured_expiry);
    let mut transaction = pool.begin().await?;
    sqlx::query("INSERT INTO principals (subject, issuer, display_name, email, claims) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (subject) DO UPDATE SET issuer = EXCLUDED.issuer, display_name = EXCLUDED.display_name, email = EXCLUDED.email, claims = EXCLUDED.claims, updated_at = now()")
        .bind(&identity.subject)
        .bind(&identity.issuer)
        .bind(&identity.display_name)
        .bind(&identity.email)
        .bind(claims)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO sessions (id, subject, token_hash, expires_at, kind) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(&identity.subject)
    .bind(token.hash())
    .bind(expires_at)
    .bind(host.kind())
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await
}

/// Resolve an opaque session token for one host kind. A token created on the
/// other host never matches.
pub(crate) async fn session_principal(
    pool: &PgPool,
    hash: &str,
    kind: &str,
) -> Result<Option<Principal>, sqlx::Error> {
    let row = sqlx::query("SELECT p.subject, p.issuer, p.display_name, p.email, p.claims FROM sessions s JOIN principals p ON p.subject = s.subject WHERE s.token_hash = $1 AND s.kind = $2 AND s.revoked_at IS NULL AND s.expires_at > now()")
        .bind(hash)
        .bind(kind)
        .fetch_optional(pool)
        .await?;
    row.map(|row| {
        let claims: serde_json::Value = row.try_get("claims")?;
        let groups = claims
            .get("groups")
            .and_then(|groups| groups.as_array())
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|group| group.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Principal {
            subject: row.try_get("subject")?,
            issuer: row.try_get("issuer")?,
            display_name: row.try_get("display_name")?,
            email: row.try_get("email")?,
            groups,
        })
    })
    .transpose()
}

fn auth_error_response(error: AuthError) -> Response {
    tracing::warn!(%error, "OIDC authentication failed");
    (StatusCode::BAD_GATEWAY, "OIDC authentication failed").into_response()
}

/// Read one cookie by exact name from a `Cookie` request header.
pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

fn session_cookie(host: LoginHost, token: &str, ttl: Duration) -> String {
    format!(
        "{}={token}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Lax",
        host.cookie_name(),
        ttl.as_secs()
    )
}

fn expired_session_cookie() -> String {
    "foundry_circle_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax".into()
}

/// After the play-host login only a same-host path is followed; anything else
/// (an absolute URL, `//host`, a backslash) lands on `/join`.
pub(crate) fn play_return_to(value: Option<&str>) -> String {
    sanitize_return_to(value).unwrap_or_else(|| LoginHost::Play.default_return_to().into())
}

/// SameSite=Lax is enough for the binding cookie because the IdP returns the
/// callback as a top-level GET redirect; it is not a bearer credential.
fn login_binding_cookie(host: LoginHost, state: &str) -> String {
    format!(
        "{0}={state}; Path={1}; Max-Age=600; HttpOnly; Secure; SameSite=Lax",
        host.binding_cookie_name(),
        host.binding_cookie_path()
    )
}

fn expired_login_binding_cookie(host: LoginHost) -> String {
    format!(
        "{0}=; Path={1}; Max-Age=0; HttpOnly; Secure; SameSite=Lax",
        host.binding_cookie_name(),
        host.binding_cookie_path()
    )
}

fn login_binding_matches(host: LoginHost, headers: &HeaderMap, state: &str) -> bool {
    cookie(headers, host.binding_cookie_name()) == Some(state)
}

fn unix_timestamp(value: DateTime<Utc>) -> u64 {
    value.timestamp().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_group_is_checked_separately_from_access() {
        let principal = Principal {
            subject: "sub".into(),
            issuer: "https://id.example/".into(),
            display_name: None,
            email: None,
            groups: vec![
                "foundry-circle-users".into(),
                "foundry-circle-admins".into(),
            ],
        };
        assert!(principal.is_admin("foundry-circle-admins"));
        assert_eq!(
            principal.role("foundry-circle-admins", Some("foundry-circle-gms")),
            Role::Admin
        );
        let gm = Principal {
            groups: vec!["foundry-circle-users".into(), "foundry-circle-gms".into()],
            ..principal.clone()
        };
        assert_eq!(
            gm.role("foundry-circle-admins", Some("foundry-circle-gms")),
            Role::Gm
        );
        assert_eq!(gm.role("foundry-circle-admins", None), Role::Player);
        assert!(
            principal
                .groups
                .iter()
                .any(|group| group == "foundry-circle-users")
        );
    }

    #[test]
    fn non_admin_access_cannot_enter_admin_routes() {
        let principal = Principal {
            subject: "sub".into(),
            issuer: "https://id.example/".into(),
            display_name: None,
            email: None,
            groups: vec!["foundry-circle-users".into()],
        };

        assert!(!principal.is_admin("foundry-circle-admins"));
        assert!(!principal.is_admin("foundry-circle-user"));
    }

    #[test]
    fn cookie_parser_requires_the_exact_session_name() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=value; foundry_circle_session=opaque"
                .parse()
                .unwrap(),
        );
        assert_eq!(cookie(&headers, "foundry_circle_session"), Some("opaque"));

        headers.insert(
            header::COOKIE,
            "foundry_circle_session_extra=attacker".parse().unwrap(),
        );
        assert_eq!(cookie(&headers, "foundry_circle_session"), None);
    }

    #[test]
    fn session_cookie_has_browser_security_flags() {
        let cookie = session_cookie(LoginHost::Platform, "opaque", Duration::from_secs(60));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=60"));
        assert!(cookie.starts_with("foundry_circle_session=opaque;"));
    }

    #[test]
    fn the_play_session_cookie_is_host_only_and_path_rooted() {
        let cookie = session_cookie(LoginHost::Play, "opaque", Duration::from_secs(60));
        assert!(cookie.starts_with("foundry_circle_play=opaque;"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
    }

    #[test]
    fn logout_cookie_expires_the_session() {
        let cookie = expired_session_cookie();
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
    }

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, value.parse().unwrap());
        headers
    }

    #[test]
    fn login_binding_requires_the_matching_cookie_value() {
        let headers = headers_with_cookie("other=value; foundry_circle_login=state-1");
        assert!(login_binding_matches(
            LoginHost::Platform,
            &headers,
            "state-1"
        ));
        assert!(!login_binding_matches(
            LoginHost::Platform,
            &headers,
            "state-2"
        ));
    }

    #[test]
    fn login_binding_rejects_a_missing_cookie() {
        assert!(!login_binding_matches(
            LoginHost::Platform,
            &HeaderMap::new(),
            "state-1"
        ));
    }

    #[test]
    fn login_binding_rejects_a_similar_cookie_name() {
        let headers = headers_with_cookie("foundry_circle_login_x=state-1");
        assert!(!login_binding_matches(
            LoginHost::Platform,
            &headers,
            "state-1"
        ));
    }

    #[test]
    fn the_play_binding_cookie_is_the_only_accepted_one() {
        let headers = headers_with_cookie("foundry_circle_play_login=state-1");
        assert!(login_binding_matches(LoginHost::Play, &headers, "state-1"));
        // The platform's binding cookie never satisfies the play flow.
        assert!(!login_binding_matches(LoginHost::Play, &headers, "state-2"));
        let headers = headers_with_cookie("foundry_circle_login=state-1");
        assert!(!login_binding_matches(LoginHost::Play, &headers, "state-1"));
    }

    #[test]
    fn login_binding_cookie_is_scoped_to_the_auth_routes() {
        let cookie = login_binding_cookie(LoginHost::Platform, "state-1");
        assert!(cookie.starts_with("foundry_circle_login=state-1;"));
        assert!(cookie.contains("Path=/api/auth"));
        assert!(cookie.contains("Max-Age=600"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
    }

    #[test]
    fn the_play_binding_cookie_lives_below_circle() {
        let cookie = login_binding_cookie(LoginHost::Play, "state-1");
        assert!(cookie.starts_with("foundry_circle_play_login=state-1;"));
        assert!(cookie.contains("Path=/_circle"));
        assert!(!cookie.contains("Path=/api/auth"));
    }

    #[test]
    fn expired_login_binding_cookie_clears_the_flow() {
        for (host, path) in [
            (LoginHost::Platform, "Path=/api/auth"),
            (LoginHost::Play, "Path=/_circle"),
        ] {
            let cookie = expired_login_binding_cookie(host);
            assert!(cookie.contains(path), "{cookie}");
            assert!(cookie.contains("Max-Age=0"));
            assert!(cookie.contains("HttpOnly"));
            assert!(cookie.contains("Secure"));
            assert!(cookie.contains("SameSite=Lax"));
        }
    }

    #[test]
    fn play_returns_are_same_host_paths_only() {
        assert_eq!(play_return_to(Some("/game?token=1")), "/game?token=1");
        assert_eq!(play_return_to(None), "/join");
        // Absolute URLs, protocol-relative hosts and backslashes land on /join.
        assert_eq!(play_return_to(Some("https://evil.invalid/game")), "/join");
        assert_eq!(play_return_to(Some("//evil.invalid/game")), "/join");
        assert_eq!(play_return_to(Some("/game\\@evil.invalid")), "/join");
        assert_eq!(play_return_to(Some("game")), "/join");
    }
}
