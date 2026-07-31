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

use crate::http::AppState;

#[derive(Clone)]
pub struct AuthState {
    pub client: Arc<OidcClient>,
    pub access_group: String,
    pub admin_group: String,
    pub session_ttl: Duration,
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
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub return_to: Option<String>,
}

pub async fn from_env() -> Result<AuthState, AuthError> {
    let issuer = required_env("FOUNDRY_CIRCLE_OIDC_ISSUER")?;
    let client_id = required_env("FOUNDRY_CIRCLE_OIDC_CLIENT_ID")?;
    let public_base_url = required_env("FOUNDRY_CIRCLE_OIDC_PUBLIC_BASE_URL")?;
    let scopes = std::env::var("FOUNDRY_CIRCLE_OIDC_SCOPES")
        .unwrap_or_else(|_| "openid profile email groups".into())
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let access_group = std::env::var("FOUNDRY_CIRCLE_OIDC_ACCESS_GROUP")
        .unwrap_or_else(|_| "foundry-circle-users".into());
    let admin_group = std::env::var("FOUNDRY_CIRCLE_OIDC_ADMIN_GROUP")
        .unwrap_or_else(|_| "foundry-circle-admins".into());
    let session_ttl = std::env::var("FOUNDRY_CIRCLE_SESSION_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(43_200);
    let client = OidcClient::discover(OidcConfig {
        issuer,
        client_id,
        redirect_uri: format!("{public_base_url}/api/auth/oidc/callback"),
        scopes,
    })
    .await?;
    Ok(AuthState {
        client: Arc::new(client),
        access_group,
        admin_group,
        session_ttl: Duration::from_secs(session_ttl),
    })
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
    let request = match auth
        .client
        .authorization_request(sanitize_return_to(query.return_to.as_deref()))
    {
        Ok(request) => request,
        Err(error) => return auth_error_response(error),
    };
    if let Err(error) = save_transaction(&pool, &request.transaction).await {
        tracing::error!(%error, "failed to save OIDC login transaction");
        return (StatusCode::INTERNAL_SERVER_ERROR, "login transaction error").into_response();
    }
    Redirect::to(&request.url).into_response()
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    Query(callback): Query<Callback>,
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
    let Some(state_value) = callback.state.as_deref() else {
        return (StatusCode::BAD_REQUEST, "missing OIDC state").into_response();
    };
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
    let identity = match auth.client.complete(callback, transaction.clone()).await {
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
    let return_to = sanitize_return_to(transaction.return_to.as_deref())
        .unwrap_or_else(|| "/api/console".into());
    if let Err(error) = save_principal_and_session(&pool, auth, &identity, &token).await {
        tracing::error!(%error, "failed to create Foundry Circle session");
        return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
    }
    (
        AppendHeaders([(
            header::SET_COOKIE,
            session_cookie(token.as_str(), auth.session_ttl),
        )]),
        Redirect::to(&return_to),
    )
        .into_response()
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(pool) = state.database.pool().await else {
        return (StatusCode::NO_CONTENT, "").into_response();
    };
    if let Some(token) = cookie(&headers, "foundry_circle_session") {
        let hash = SessionToken::hash_value(token);
        if let Err(error) =
            sqlx::query("UPDATE sessions SET revoked_at = now() WHERE token_hash = $1")
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
    let Some(token) = cookie(request.headers(), "foundry_circle_session") else {
        return Redirect::to("/api/auth/login").into_response();
    };
    let hash = SessionToken::hash_value(token);
    let principal = match session_principal(&pool, &hash).await {
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
    request.extensions_mut().insert(principal);
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
        "INSERT INTO sessions (id, subject, token_hash, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(&identity.subject)
    .bind(token.hash())
    .bind(expires_at)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await
}

async fn session_principal(pool: &PgPool, hash: &str) -> Result<Option<Principal>, sqlx::Error> {
    let row = sqlx::query("SELECT p.subject, p.issuer, p.display_name, p.email, p.claims FROM sessions s JOIN principals p ON p.subject = s.subject WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()")
        .bind(hash)
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

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
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

fn session_cookie(token: &str, ttl: Duration) -> String {
    format!(
        "foundry_circle_session={token}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Lax",
        ttl.as_secs()
    )
}

fn expired_session_cookie() -> String {
    "foundry_circle_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax".into()
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
        let cookie = session_cookie("opaque", Duration::from_secs(60));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=60"));
    }

    #[test]
    fn logout_cookie_expires_the_session() {
        let cookie = expired_session_cookie();
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
    }
}
