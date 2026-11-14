use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{AppendHeaders, IntoResponse, Redirect, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use oidc_app_auth::{
    AuthError, Callback, LoginTransaction, OidcClient, OidcConfig, SessionToken, TokenSet,
    UserInfoClaims, UserInfoError, VerifiedIdentity, sanitize_return_to,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    Role, adoption,
    driver::AccountOp,
    http::{AppState, session},
};

/// How long a successful revalidation answer is trusted (D5, G6).
const FRESHNESS: Duration = Duration::from_secs(5);

/// Rauthy accepts a refresh token from `access_expires_at - 60 s` (its `nbf`
/// is issuance + access-token lifetime − 60 s), so the refresh only runs inside
/// this margin.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// One gate request's whole Rauthy budget: a refresh, when due, and userinfo
/// share it, leaving the rest of Caddy's 2 s deadline to us.
const BUDGET: Duration = Duration::from_millis(1500);

#[derive(Clone)]
pub struct AuthState {
    pub client: Arc<OidcClient>,
    /// The play host's client: the same issuer, client id and scopes with the
    /// `/_circle/oidc/callback` redirect URI. `None` when no play host is
    /// configured.
    pub play_client: Option<Arc<OidcClient>>,
    /// The play host's browser origin, without a trailing slash. The play
    /// logout requires it as the request's exact `Origin`.
    pub play_base_url: Option<String>,
    pub access_group: String,
    pub admin_group: String,
    pub gm_group: Option<String>,
    pub session_ttl: Duration,
    /// Refreshed token sets the database refused to store; see [`TokenStash`].
    stashed_tokens: TokenStash,
    /// One lock per session, so parallel gate requests for one browser do not
    /// refresh with the same rotated-away token; see [`SessionLocks`].
    session_locks: SessionLocks,
}

/// Removes a session's lock entry once nothing else uses it. Runs on drop, so
/// a timed-out or cancelled request cleans up too; the count is checked under
/// the map lock, so a request cannot take the entry between check and removal.
struct GateLease<'a> {
    locks: &'a SessionLocks,
    hash: &'a str,
    // `Option` so `drop` can release it while still holding the map lock.
    gate: Option<Arc<tokio::sync::Mutex<()>>>,
}

impl Drop for GateLease<'_> {
    fn drop(&mut self) {
        let mut gates = self.locks.gates.lock().expect("session lock map poisoned");
        if let Some(gate) = self.gate.take() {
            // The map and this lease are the only owners left.
            if Arc::strong_count(&gate) == 2 {
                gates.remove(self.hash);
            }
            // Release this owner before the map lock: two leases dropping at
            // once must not both count each other and leave the entry behind.
            drop(gate);
        }
    }
}

/// Refreshed token sets the database refused to store, keyed by the session's
/// token hash. The next revalidation of that session persists the set before
/// it does anything else, so a rotated-away refresh token is never reused.
#[derive(Clone, Default)]
struct TokenStash {
    // ponytail: one process, and a restart drops it; the affected user signs in
    // again. Persist it (e.g. a `pending_tokens` table) if that matters.
    tokens: Arc<Mutex<HashMap<String, Arc<TokenSet>>>>,
}

impl TokenStash {
    /// Clone a session's set so a request can retry the database write. The set
    /// stays stashed until that write succeeds: another request must not lose
    /// it while the first one awaits.
    fn peek(&self, hash: &str) -> Option<Arc<TokenSet>> {
        self.tokens
            .lock()
            .expect("token stash lock poisoned")
            .get(hash)
            .cloned()
    }

    /// Drop a session's set once the row has it.
    fn remove(&self, hash: &str) {
        self.tokens
            .lock()
            .expect("token stash lock poisoned")
            .remove(hash);
    }

    fn insert(&self, hash: &str, tokens: TokenSet) -> Arc<TokenSet> {
        let tokens = Arc::new(tokens);
        self.tokens
            .lock()
            .expect("token stash lock poisoned")
            .insert(hash.to_owned(), tokens.clone());
        tokens
    }
}

/// One lock per session token hash: parallel requests for one browser (asset
/// loads) must not all refresh with the same refresh token, which the provider
/// reads as reuse and answers by revoking the subject.
#[derive(Clone, Default)]
struct SessionLocks {
    // ponytail: one process, so a second broker instance could still refresh
    // the same session concurrently. A database advisory lock covers that if
    // more than one instance ever serves the gate.
    gates: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl SessionLocks {
    /// Run `work` as the one holder of `hash`'s lock, or answer `None` when
    /// `deadline` passes while waiting. The wait spends the request's budget,
    /// so an expired wait is "ask again", never a verdict on the identity.
    async fn run<T>(
        &self,
        hash: &str,
        deadline: tokio::time::Instant,
        work: impl Future<Output = T>,
    ) -> Option<T> {
        // Declared before the guard so it drops after it, on every exit:
        // completion, an expired wait, or the request being cancelled.
        let lease = GateLease {
            locks: self,
            hash,
            gate: Some(self.gate(hash)),
        };
        let _guard = tokio::time::timeout_at(
            deadline,
            lease
                .gate
                .clone()
                .expect("lease holds its gate")
                .lock_owned(),
        )
        .await
        .ok()?;
        Some(work.await)
    }

    /// The lock for `hash`, created on first use. The caller holds a clone, so
    /// an entry lives until every request using or waiting on it is done.
    fn gate(&self, hash: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .expect("session lock map poisoned")
            .entry(hash.to_owned())
            .or_default()
            .clone()
    }
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

/// `GET /api/auth/native`: a native client asks the system browser to sign in
/// and prove the result with a loopback redirect and PKCE (RFC 8252).
#[derive(Debug, Deserialize)]
pub struct NativeQuery {
    pub port: Option<String>,
    pub state: Option<String>,
    pub challenge: Option<String>,
}

/// A validated native login request. The port is the client's loopback
/// listener, the state its CSRF value, and the challenge
/// `base64url(sha256(verifier))` from RFC 7636 S256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeLogin {
    pub port: u16,
    pub state: String,
    pub challenge: String,
}

/// The loopback port must be one a client process could really own, the state
/// must comfortably fit a query string, and the challenge must be a canonical
/// 32-byte S256 digest.
pub(crate) fn native_login(query: &NativeQuery) -> Option<NativeLogin> {
    let port = query.port.as_deref()?.trim().parse::<u16>().ok()?;
    if port < 1024 {
        return None;
    }
    let state = query.state.as_deref()?;
    if !(16..=128).contains(&state.len()) || !state.bytes().all(is_base64url) {
        return None;
    }
    let challenge = query.challenge.as_deref()?;
    if challenge.len() != 43 || URL_SAFE_NO_PAD.decode(challenge).ok()?.len() != 32 {
        return None;
    }
    Some(NativeLogin {
        port,
        state: state.to_owned(),
        challenge: challenge.to_owned(),
    })
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// `base64url(sha256(verifier))`, the RFC 7636 S256 challenge.
pub(crate) fn verifier_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Whether `verifier` produces the `challenge` the login recorded. The
/// comparison is constant time, so a wrong verifier reveals nothing about how
/// much of the digest it matched.
pub(crate) fn verifier_matches(challenge: &str, verifier: &str) -> bool {
    constant_time_eq(
        challenge.as_bytes(),
        verifier_challenge(verifier).as_bytes(),
    )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |mismatch, (left, right)| mismatch | (left ^ right))
            == 0
}

/// The session row behind one request: the identity the console renders and the
/// tokens revalidation needs. There is no `Debug`: a row carries a bearer
/// access token, a refresh token and an ID token, and none of them may be
/// logged or rendered.
pub(crate) struct SessionRow {
    /// The SHA-256 of the opaque cookie, never the cookie itself: the row is
    /// updated by this key.
    pub token_hash: String,
    pub principal: Principal,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub validated_at: Option<DateTime<Utc>>,
}

/// What one gate revalidation concluded (D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Revalidation {
    /// Fresh, or just confirmed by Rauthy with access still granted.
    Valid,
    /// The identity is gone: revoke the subject and deny.
    Revoked,
    /// This row alone cannot be revalidated (it predates token storage):
    /// revoke it and treat the request as signed out. It says nothing about
    /// the subject's other sessions.
    Stale,
    /// Rauthy could not answer or the budget ran out: deny for now, do not
    /// revoke (D6).
    Unavailable,
}

/// A valid revalidation, with the principal carrying Rauthy's newest claims.
pub(crate) enum Revalidated {
    Valid(Principal),
    Revoked,
    Stale,
    Unavailable,
}

fn revalidated(outcome: Revalidation, principal: &Principal) -> Revalidated {
    match outcome {
        Revalidation::Valid => Revalidated::Valid(principal.clone()),
        Revalidation::Revoked => Revalidated::Revoked,
        Revalidation::Stale => Revalidated::Stale,
        Revalidation::Unavailable => Revalidated::Unavailable,
    }
}

impl Revalidated {
    /// The decision without the claims it may carry.
    pub(crate) fn outcome(&self) -> Revalidation {
        match self {
            Self::Valid(_) => Revalidation::Valid,
            Self::Revoked => Revalidation::Revoked,
            Self::Stale => Revalidation::Stale,
            Self::Unavailable => Revalidation::Unavailable,
        }
    }

    /// The principal Rauthy just confirmed, when the outcome is `Valid`.
    pub(crate) fn principal(self) -> Option<Principal> {
        match self {
            Self::Valid(principal) => Some(principal),
            _ => None,
        }
    }
}

/// What the optional refresh step (due in the access token's last minute) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// A new token set was issued; userinfo still decides the identity.
    Refreshed,
    /// The provider refused the grant: the identity is gone.
    Rejected,
    /// The provider could not answer: nothing is known about the identity.
    Unavailable,
}

/// What the userinfo step found. `Missing` is a session from before tokens were
/// stored: there is nothing to revalidate, so that row alone is revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserInfo {
    /// The provider answered for the expected subject.
    Claims,
    /// The provider rejected the token or the subject.
    Rejected,
    /// The row holds no access token.
    Missing,
}

/// The pure revalidation decision table (D5, G6). `userinfo` is `None` when no
/// answer arrived: the provider was unavailable, the budget ran out, or a due
/// refresh already decided the request. `cached_age` counts from when the last
/// successful check *started*, so a slow answer cannot extend the window.
pub(crate) fn decide(
    cached_age: Option<Duration>,
    refresh_due: bool,
    refresh: Option<Refresh>,
    userinfo: Option<UserInfo>,
    in_access_group: bool,
) -> Revalidation {
    if is_fresh(cached_age) {
        // A cached answer is only valid while its own claims still grant access.
        return if in_access_group {
            Revalidation::Valid
        } else {
            Revalidation::Revoked
        };
    }
    // A due refresh always runs before userinfo, unless the budget ends first.
    debug_assert!(
        !refresh_due || refresh.is_some() || userinfo.is_none(),
        "a due refresh has an outcome before userinfo runs"
    );
    debug_assert!(
        !matches!(refresh, Some(Refresh::Rejected | Refresh::Unavailable)) || userinfo.is_none(),
        "a refused or unavailable refresh never reaches userinfo"
    );
    // A refused grant proves the identity is gone; an outage proves nothing
    // about it. Only a successful refresh (or no refresh due) may go on to
    // judge the access token with userinfo.
    match refresh {
        Some(Refresh::Rejected) => return Revalidation::Revoked,
        Some(Refresh::Unavailable) => return Revalidation::Unavailable,
        Some(Refresh::Refreshed) | None => {}
    }
    match userinfo {
        Some(UserInfo::Claims) if in_access_group => Revalidation::Valid,
        Some(UserInfo::Claims | UserInfo::Rejected) => Revalidation::Revoked,
        // A row from before 0006: revoke it alone, never the subject.
        Some(UserInfo::Missing) => Revalidation::Stale,
        None => Revalidation::Unavailable,
    }
}

/// Inside the freshness window the gate answers without asking Rauthy.
fn is_fresh(cached_age: Option<Duration>) -> bool {
    cached_age.is_some_and(|age| age < FRESHNESS)
}

/// How long ago the last successful check started, if there was one.
fn cached_age(validated_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<Duration> {
    validated_at.map(|at| (now - at).to_std().unwrap_or_default())
}

/// Refresh only inside the 60 s margin before the access token expires; earlier
/// refreshes are rejected by Rauthy's refresh-token `nbf`.
fn refresh_due(
    access_expires_at: Option<DateTime<Utc>>,
    has_refresh_token: bool,
    now: DateTime<Utc>,
) -> bool {
    has_refresh_token
        && access_expires_at.is_some_and(|expires| {
            expires - now <= chrono::Duration::from_std(REFRESH_MARGIN).unwrap_or_default()
        })
}

/// The principal after a successful revalidation: Rauthy's claims replace the
/// groups, email and display name the session was created with.
fn principal_from(row: &SessionRow, claims: &UserInfoClaims) -> Principal {
    Principal {
        subject: row.principal.subject.clone(),
        issuer: row.principal.issuer.clone(),
        display_name: claims.display_name.clone(),
        email: claims.email.clone(),
        groups: claims.groups.iter().cloned().collect(),
    }
}

/// Why a principal's sessions were revoked; only ever an audit detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RevokeReason {
    /// A gate revalidation was refused (D5).
    Revoked,
    /// An admin pressed "Revoke sessions".
    Admin,
    /// The principal signed out.
    Logout,
}

impl RevokeReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Admin => "admin",
            Self::Logout => "logout",
        }
    }
}

/// Only an admin may revoke another principal's sessions. The console's
/// "Revoke sessions" button and the JSON route both ask this one question.
pub(crate) fn may_revoke(principal: &Principal, admin_group: &str) -> bool {
    principal.is_admin(admin_group)
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
    let play_base = play_base_url();
    let play_client = match play_base.as_deref() {
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
        play_base_url: play_base,
        access_group,
        admin_group,
        gm_group,
        session_ttl: Duration::from_secs(session_ttl),
        stashed_tokens: TokenStash::default(),
        session_locks: SessionLocks::default(),
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
    start_login(state, LoginHost::Platform, query.return_to, None).await
}

/// `GET /api/auth/native`: the platform login with the browser handing the
/// result to a native client's loopback listener instead of setting a cookie.
/// No session is required, exactly like `login_start`.
pub async fn native_login_start(
    State(state): State<AppState>,
    Query(query): Query<NativeQuery>,
) -> Response {
    let Some(native) = native_login(&query) else {
        return (StatusCode::BAD_REQUEST, "invalid native login request").into_response();
    };
    start_login(state, LoginHost::Platform, None, Some(&native)).await
}

/// `GET /_circle/login`: the same flow with the play host's callback and
/// cookies.
pub async fn play_login_start(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Response {
    start_login(state, LoginHost::Play, query.return_to, None).await
}

async fn start_login(
    state: AppState,
    host: LoginHost,
    return_to: Option<String>,
    native: Option<&NativeLogin>,
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
    let request = match client.authorization_request(sanitize_return_to(return_to.as_deref())) {
        Ok(request) => request,
        Err(error) => return auth_error_response(error),
    };
    if let Err(error) = save_transaction(&pool, &request.transaction, native).await {
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
    let (transaction, native) = match take_transaction(&pool, state_value).await {
        Ok(Some(transaction)) => transaction,
        Ok(None) => {
            return (StatusCode::BAD_REQUEST, "invalid or replayed login flow").into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to consume OIDC login transaction");
            return (StatusCode::INTERNAL_SERVER_ERROR, "login transaction error").into_response();
        }
    };
    let (identity, tokens) = match client
        .complete_with_tokens(callback, transaction.clone())
        .await
    {
        Ok(completed) => completed,
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
    if let Err(error) =
        save_principal_and_session(&pool, auth, &identity, &tokens, &token, host).await
    {
        tracing::error!(%error, "failed to create Foundry Circle session");
        return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
    }
    if let Some(native) = native {
        // The browser keeps no session: the native client that started this
        // flow receives its own one-time code instead, and the binding cookie
        // is cleared as on every callback.
        return match complete_native(&pool, &native, &token).await {
            Ok(redirect) => Redirect::to(&redirect).into_response(),
            Err(error) => {
                tracing::error!(%error, "failed to record a Foundry Circle native handoff");
                (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response()
            }
        };
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

/// Record the one-time code a native client redeems for the session just
/// created (RFC 8252 loopback). Only the code's hash is stored, and only for
/// two minutes; the session token itself is in the row because the client has
/// not received it yet.
async fn complete_native(
    pool: &PgPool,
    native: &NativeLogin,
    token: &SessionToken,
) -> Result<String, sqlx::Error> {
    let code = SessionToken::generate();
    sqlx::query(
        "INSERT INTO native_handoffs (code_hash, challenge, session_token, expires_at) VALUES ($1, $2, $3, now() + interval '2 minutes')",
    )
    .bind(code.hash())
    .bind(&native.challenge)
    .bind(token.as_str())
    .execute(pool)
    .await?;
    Ok(format!(
        "http://127.0.0.1:{}/callback?code={}&state={}",
        native.port,
        code.as_str(),
        native.state
    ))
}

/// `POST /api/auth/native/token`: redeem the one-time code and prove the PKCE
/// verifier. The `Json` body is the CSRF guard, like every other POST here; the
/// code is single use even when the exchange fails.
#[derive(Debug, Deserialize)]
pub struct NativeTokenRequest {
    pub code: String,
    pub verifier: String,
}

#[derive(Debug, Serialize)]
struct NativeGrant {
    token: String,
    cookie: String,
    expires_at: String,
}

pub async fn native_token(
    State(state): State<AppState>,
    Json(request): Json<NativeTokenRequest>,
) -> Response {
    let Some(pool) = state.database.pool().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "database_unavailable"})),
        )
            .into_response();
    };
    match exchange_native_code(&pool, &request).await {
        Ok(grant) => (StatusCode::OK, Json(grant)).into_response(),
        Err(error) => {
            if let Some(error) = error {
                tracing::warn!(%error, "Foundry Circle native sign-in exchange failed");
            }
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_grant"})),
            )
                .into_response()
        }
    }
}

/// Consume one handoff and return the session it carries. `Err(None)` is a
/// refused grant (unknown, expired, replayed, wrong verifier or a session that
/// is already gone); `Err(Some(_))` is a database failure.
async fn exchange_native_code(
    pool: &PgPool,
    request: &NativeTokenRequest,
) -> Result<NativeGrant, Option<sqlx::Error>> {
    // Opportunistic cleanup: an expired code is useless to everyone.
    sqlx::query("DELETE FROM native_handoffs WHERE expires_at <= now()")
        .execute(pool)
        .await
        .map_err(Some)?;
    // DELETE ... RETURNING makes the code single use: the row is gone whatever
    // the verifier turns out to be.
    let row = sqlx::query(
        "DELETE FROM native_handoffs WHERE code_hash = $1 RETURNING challenge, session_token, expires_at",
    )
    .bind(SessionToken::hash_value(&request.code))
    .fetch_optional(pool)
    .await
    .map_err(Some)?;
    let Some(row) = row else {
        return Err(None);
    };
    let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(Some)?;
    if expires_at <= Utc::now() {
        return Err(None);
    }
    let challenge: String = row.try_get("challenge").map_err(Some)?;
    if !verifier_matches(&challenge, &request.verifier) {
        return Err(None);
    }
    let token: String = row.try_get("session_token").map_err(Some)?;
    // A revoked or expired session has nothing left to hand out; report the
    // session's own expiry so the client knows when to sign in again.
    let session_expires: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT expires_at FROM sessions WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(SessionToken::hash_value(&token))
    .fetch_optional(pool)
    .await
    .map_err(Some)?;
    let Some(expires_at) = session_expires else {
        return Err(None);
    };
    Ok(NativeGrant {
        token,
        cookie: LoginHost::Platform.cookie_name().to_owned(),
        expires_at: expires_at.to_rfc3339(),
    })
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    end_session(state, LoginHost::Platform, headers).await
}

/// `POST /_circle/logout`: the play host's own cookie, ended the same way, and
/// the browser lands on the platform console.
pub async fn play_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    end_session(state, LoginHost::Play, headers).await
}

/// Revoke every session the caller holds on both hosts, expire this host's
/// cookie, then hand the browser to Rauthy's RP-initiated logout so the SSO
/// session ends too (G6).
///
/// The request's `Origin` must be the configured origin of *this* host
/// exactly: `SameSite=Lax` still lets a same-site sibling origin POST here,
/// and a logout must not be triggerable there. A refused origin is answered
/// before anything is revoked.
async fn end_session(state: AppState, host: LoginHost, headers: HeaderMap) -> Response {
    let play = state
        .auth
        .as_ref()
        .and_then(|auth| auth.play_base_url.as_deref());
    if !logout_origin_allowed(host, &headers, state.console.platform_url.as_deref(), play) {
        return (StatusCode::FORBIDDEN, "cross-origin logout refused").into_response();
    }
    let mut id_token = None;
    let token = cookie(&headers, host.cookie_name());
    let pool = state.database.pool().await;
    match logout_plan(token, pool.is_some()) {
        // This host's cookie names sessions that must be revoked; without the
        // database they stay valid, so refuse instead of pretending otherwise.
        LogoutPlan::Refuse => {
            tracing::warn!("logout cannot revoke its session: the database is not ready");
            return (StatusCode::SERVICE_UNAVAILABLE, "logout failed").into_response();
        }
        // Nothing this host could revoke: clearing the cookie is the logout.
        LogoutPlan::Clear => {}
        LogoutPlan::Revoke => {
            let (Some(pool), Some(token)) = (pool.as_ref(), token) else {
                return (StatusCode::SERVICE_UNAVAILABLE, "logout failed").into_response();
            };
            let hash = SessionToken::hash_value(token);
            let row = match session_row(pool, &hash, host.kind()).await {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(%error, "failed to resolve a Foundry Circle session on logout");
                    return (StatusCode::SERVICE_UNAVAILABLE, "logout failed").into_response();
                }
            };
            if let Some(row) = row {
                id_token = row.id_token.clone();
                if let Err(error) =
                    revoke_subject(&state, &row.principal.subject, RevokeReason::Logout).await
                {
                    tracing::warn!(error = %error, "failed to revoke Foundry Circle sessions on logout");
                    return (StatusCode::SERVICE_UNAVAILABLE, "logout failed").into_response();
                }
            }
        }
    }
    // The registered post-logout URI lives on the platform origin; without one
    // Rauthy's end_session call has nowhere to return to.
    let target = state.console.platform_url.as_deref().and_then(|platform| {
        state
            .auth
            .as_ref()?
            .client
            .end_session_url(id_token.as_deref(), &format!("{platform}/api/console"))
    });
    (
        AppendHeaders([(header::SET_COOKIE, expired_session_cookie(host))]),
        Redirect::to(&logout_destination(target)),
    )
        .into_response()
}

/// What a logout must do before it may tell the browser it signed out. A
/// request carrying this host's cookie must revoke its sessions first; a
/// cookie-less request has nothing to revoke, so it only clears and redirects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogoutPlan {
    Revoke,
    Clear,
    Refuse,
}

fn logout_plan(session_cookie: Option<&str>, pool_ready: bool) -> LogoutPlan {
    match (
        session_cookie.is_some_and(|token| !token.is_empty()),
        pool_ready,
    ) {
        (true, false) => LogoutPlan::Refuse,
        (true, true) => LogoutPlan::Revoke,
        (false, _) => LogoutPlan::Clear,
    }
}

/// A logout's `Origin` must equal the configured origin of the host whose
/// cookie it ends, with no normalisation: missing or different means refused.
fn logout_origin_allowed(
    host: LoginHost,
    headers: &HeaderMap,
    platform: Option<&str>,
    play: Option<&str>,
) -> bool {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let configured = match host {
        LoginHost::Platform => platform,
        LoginHost::Play => play,
    };
    crate::http::same_origin(origin, configured)
}

/// Rauthy's RP-initiated logout when discovery advertised one, else the console
/// on this host.
fn logout_destination(end_session: Option<String>) -> String {
    end_session.unwrap_or_else(|| "/api/console".into())
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
    let row = match session_row(&pool, &hash, LoginHost::Platform.kind()).await {
        Ok(Some(row)) => row,
        Ok(None) => return Redirect::to("/api/auth/login").into_response(),
        Err(error) => {
            tracing::warn!(%error, "failed to resolve Foundry Circle session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    let principal = match revalidate(auth, &pool, &row).await {
        Revalidated::Valid(principal) => principal,
        Revalidated::Revoked => {
            if let Err(error) =
                revoke_subject(&state, &row.principal.subject, RevokeReason::Revoked).await
            {
                tracing::warn!(error = %error, "failed to revoke a rejected Foundry Circle session");
                return (StatusCode::SERVICE_UNAVAILABLE, "session revocation failed")
                    .into_response();
            }
            return Redirect::to("/api/auth/login").into_response();
        }
        Revalidated::Stale => {
            invalidate_session(&pool, &row.token_hash).await;
            return Redirect::to("/api/auth/login").into_response();
        }
        Revalidated::Unavailable => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Rauthy is unavailable").into_response();
        }
    };
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
    if let (Some(auth), Some(pool)) = (state.auth.as_ref(), state.database.pool().await)
        && let Some(token) = cookie(request.headers(), LoginHost::Platform.cookie_name())
        && let Ok(Some(row)) = session_row(
            &pool,
            &SessionToken::hash_value(token),
            LoginHost::Platform.kind(),
        )
        .await
    {
        match revalidate(auth, &pool, &row).await {
            Revalidated::Valid(principal) => {
                // Console use by the holder counts as presence too (D3).
                session::touch(&pool, &principal.subject).await;
                request.extensions_mut().insert(principal);
            }
            Revalidated::Revoked => {
                if let Err(error) =
                    revoke_subject(&state, &row.principal.subject, RevokeReason::Revoked).await
                {
                    tracing::warn!(
                        error = %error,
                        "failed to revoke a rejected Foundry Circle session"
                    );
                }
            }
            Revalidated::Stale => invalidate_session(&pool, &row.token_hash).await,
            // Render signed out, but leave the session for the next check.
            Revalidated::Unavailable => {}
        }
    }
    next.run(request).await
}

/// The newest token set a check may reason from: a refreshed set whose storage
/// failed on an earlier request is newer than the row, and its refresh token is
/// the only one Rauthy still accepts.
struct TokenBasis {
    access_token: String,
    access_expires_at: Option<DateTime<Utc>>,
    refresh_token: Option<String>,
}

fn token_basis(carried: Option<&TokenSet>, row: &SessionRow, original: &str) -> TokenBasis {
    let Some(tokens) = carried else {
        return TokenBasis {
            access_token: original.to_owned(),
            access_expires_at: row.access_expires_at,
            refresh_token: row.refresh_token.clone(),
        };
    };
    TokenBasis {
        access_token: tokens.access_token.clone(),
        access_expires_at: timestamp(tokens.access_expires_at),
        refresh_token: tokens
            .refresh_token
            .clone()
            .or_else(|| row.refresh_token.clone()),
    }
}

/// Revalidate one session (D5, G6): answer from the 5 s cache when it is fresh,
/// else spend at most [`BUDGET`] on a refresh (when due) and one userinfo call.
/// A refreshed set is persisted *before* userinfo: the provider has already
/// rotated its refresh token away, so losing the new set would strand it. The
/// returned principal carries the claims Rauthy just confirmed.
///
/// Only the fresh answer runs unlocked. Everything after it is serialized per
/// session, so parallel gate requests for one browser (asset loads) refresh at
/// most once with a given refresh token.
pub(crate) async fn revalidate(auth: &AuthState, pool: &PgPool, row: &SessionRow) -> Revalidated {
    let started = Utc::now();
    let age = cached_age(row.validated_at, started);
    if is_fresh(age) {
        let in_access_group = row.principal.groups.contains(&auth.access_group);
        let outcome = decide(age, false, None, None, in_access_group);
        return revalidated(outcome, &row.principal);
    }
    // The wait for the session's lock spends the same budget as Rauthy: one
    // that runs out is a retry, not a verdict on the identity.
    let deadline = tokio::time::Instant::now() + BUDGET;
    auth.session_locks
        .run(
            &row.token_hash,
            deadline,
            revalidate_serial(auth, pool, &row.token_hash, started, deadline),
        )
        .await
        .unwrap_or(Revalidated::Unavailable)
}

/// The serialized part of [`revalidate`], holding the session's lock. The
/// request before it may have just refreshed: re-read the row first and answer
/// from its write when it recorded a freshness this request can trust.
async fn revalidate_serial(
    auth: &AuthState,
    pool: &PgPool,
    hash: &str,
    started: DateTime<Utc>,
    deadline: tokio::time::Instant,
) -> Revalidated {
    let row = match session_row_by_hash(pool, hash).await {
        Ok(Some(row)) => row,
        // Revoked or expired while this request waited: deny this one only.
        Ok(None) => return Revalidated::Stale,
        Err(error) => {
            tracing::warn!(%error, "failed to re-read a Foundry Circle session");
            return Revalidated::Unavailable;
        }
    };
    let age = cached_age(row.validated_at, started);
    let in_access_group = |principal: &Principal| principal.groups.contains(&auth.access_group);
    if is_fresh(age) {
        let outcome = decide(age, false, None, None, in_access_group(&row.principal));
        return revalidated(outcome, &row.principal);
    }
    let Some(original) = row.access_token.as_deref() else {
        // A row from before 0006: there is nothing to revalidate, so it alone
        // is invalidated. A fresh legacy row already answered above.
        let outcome = decide(
            age,
            false,
            None,
            Some(UserInfo::Missing),
            in_access_group(&row.principal),
        );
        return revalidated(outcome, &row.principal);
    };
    // A set the database refused on an earlier request is newer than the row:
    // retry the write before using it, and keep it if the retry fails too. The
    // session's lock is held, so no other request reads or removes it.
    let carried = match auth.stashed_tokens.peek(hash) {
        Some(tokens) => {
            if persist_tokens(pool, hash, &tokens, &row, None).await {
                auth.stashed_tokens.remove(hash);
                Some(tokens)
            } else {
                // Nothing new is known and no freshness is recorded: the next
                // request retries once more.
                return Revalidated::Unavailable;
            }
        }
        None => None,
    };
    let basis = token_basis(carried.as_deref(), &row, original);
    let due = refresh_due(
        basis.access_expires_at,
        basis.refresh_token.is_some(),
        started,
    );
    let mut refresh = None;
    let mut refreshed = None;
    if due && let Some(refresh_token) = basis.refresh_token.as_deref() {
        match tokio::time::timeout_at(
            deadline,
            auth.client.refresh(refresh_token, &row.principal.subject),
        )
        .await
        {
            Ok(Ok(tokens)) => {
                // Once Rauthy has rotated the refresh token the old row can
                // never renew again, so the new set is stashed BEFORE the write
                // is awaited: a request cancelled mid-write (Caddy gives up at
                // 2 s) still leaves it for the next one, and it is dropped only
                // after the row has it. Persisting precedes userinfo.
                let tokens = auth.stashed_tokens.insert(hash, tokens);
                if persist_tokens(pool, hash, &tokens, &row, carried.as_deref()).await {
                    auth.stashed_tokens.remove(hash);
                    refreshed = Some(tokens);
                    refresh = Some(Refresh::Refreshed);
                } else {
                    return Revalidated::Unavailable;
                }
            }
            Ok(Err(UserInfoError::Rejected)) => {
                // The grant itself is gone: no userinfo call can change that.
                tracing::debug!("Foundry Circle token refresh was rejected");
                refresh = Some(Refresh::Rejected);
            }
            // Retryable: the expiring access token says nothing now, so do not
            // judge it with userinfo. New requests fail closed, nothing is
            // revoked (D6).
            Ok(Err(UserInfoError::Unavailable)) | Err(_) => {
                refresh = Some(Refresh::Unavailable);
            }
        }
    }
    let access = refreshed
        .as_ref()
        .map_or(basis.access_token.as_str(), |tokens| {
            tokens.access_token.as_str()
        });
    // Only a successful refresh (or no refresh due) may judge the token with
    // userinfo: a refusal or an outage has already decided the request.
    let mut claims = None;
    let userinfo = if refresh.is_none_or(|refresh| refresh == Refresh::Refreshed) {
        match tokio::time::timeout_at(
            deadline,
            auth.client.userinfo(access, &row.principal.subject),
        )
        .await
        {
            Ok(Ok(found)) => {
                let verdict = if found.groups.contains(&auth.access_group) {
                    UserInfo::Claims
                } else {
                    UserInfo::Rejected
                };
                claims = Some(found);
                Some(verdict)
            }
            Ok(Err(UserInfoError::Rejected)) => Some(UserInfo::Rejected),
            // Retryable: new requests fail closed, the session stays.
            Ok(Err(UserInfoError::Unavailable)) | Err(_) => None,
        }
    } else {
        None
    };
    // The verdict's own claims decide membership: a group added in Rauthy
    // since login counts as access now.
    let confirmed_group = claims
        .as_ref()
        .is_some_and(|claims| claims.groups.contains(&auth.access_group));
    match decide(age, due, refresh, userinfo, confirmed_group) {
        Revalidation::Valid => {
            let Some(claims) = claims else {
                return Revalidated::Valid(row.principal.clone());
            };
            let principal = principal_from(&row, &claims);
            apply_claims(pool, hash, &principal, started).await;
            Revalidated::Valid(principal)
        }
        Revalidation::Revoked => Revalidated::Revoked,
        Revalidation::Stale => Revalidated::Stale,
        Revalidation::Unavailable => Revalidated::Unavailable,
    }
}

async fn save_transaction(
    pool: &PgPool,
    transaction: &LoginTransaction,
    native: Option<&NativeLogin>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oidc_transactions (state, code_verifier, nonce, redirect_uri, return_to, expires_at, native_port, native_state, native_challenge) VALUES ($1, $2, $3, $4, $5, now() + interval '10 minutes', $6, $7, $8)",
    )
    .bind(&transaction.csrf_state)
    .bind(transaction.pkce_verifier.as_bytes())
    .bind(&transaction.nonce)
    .bind("")
    .bind(&transaction.return_to)
    .bind(native.map(|native| i32::from(native.port)))
    .bind(native.map(|native| native.state.as_str()))
    .bind(native.map(|native| native.challenge.as_str()))
    .execute(pool)
    .await
    .map(|_| ())
}

/// Consume one login transaction, with the native handoff it carries when the
/// flow was started by a native client.
async fn take_transaction(
    pool: &PgPool,
    state: &str,
) -> Result<Option<(LoginTransaction, Option<NativeLogin>)>, sqlx::Error> {
    let mut transaction: Transaction<'_, Postgres> = pool.begin().await?;
    let row = sqlx::query("SELECT code_verifier, nonce, return_to, expires_at, native_port, native_state, native_challenge FROM oidc_transactions WHERE state = $1 FOR UPDATE")
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
    // A transaction missing any of the three native columns is a browser flow,
    // and an out-of-range port is treated the same way: no loopback handoff.
    let native = match (
        row.try_get::<Option<i32>, _>("native_port")?,
        row.try_get::<Option<String>, _>("native_state")?,
        row.try_get::<Option<String>, _>("native_challenge")?,
    ) {
        (Some(port), Some(state), Some(challenge)) => u16::try_from(port)
            .ok()
            .filter(|port| *port >= 1024)
            .map(|port| NativeLogin {
                port,
                state,
                challenge,
            }),
        _ => None,
    };
    Ok(Some((
        LoginTransaction {
            pkce_verifier: String::from_utf8(row.try_get::<Vec<u8>, _>("code_verifier")?)
                .unwrap_or_default(),
            csrf_state: state.to_owned(),
            nonce: row.try_get("nonce")?,
            issued_at: unix_timestamp(expires_at - chrono::Duration::minutes(10)),
            return_to: row.try_get("return_to")?,
        },
        native,
    )))
}

async fn save_principal_and_session(
    pool: &PgPool,
    auth: &AuthState,
    identity: &VerifiedIdentity,
    tokens: &TokenSet,
    token: &SessionToken,
    host: LoginHost,
) -> Result<(), sqlx::Error> {
    let groups: Vec<String> = identity.groups.iter().cloned().collect();
    let claims =
        json!({"groups": groups, "email": identity.email, "display_name": identity.display_name});
    // G7: the session lives for the broker's own TTL. Revalidation (D5) bounds
    // membership, so it is no longer capped at the ID token's expiry.
    let expires_at = Utc::now()
        + chrono::Duration::from_std(auth.session_ttl)
            .unwrap_or_else(|_| chrono::Duration::hours(12));
    let mut transaction = pool.begin().await?;
    sqlx::query("INSERT INTO principals (subject, issuer, display_name, email, claims) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (subject) DO UPDATE SET issuer = EXCLUDED.issuer, display_name = EXCLUDED.display_name, email = EXCLUDED.email, claims = EXCLUDED.claims, updated_at = now()")
        .bind(&identity.subject)
        .bind(&identity.issuer)
        .bind(&identity.display_name)
        .bind(&identity.email)
        .bind(claims)
        .execute(&mut *transaction)
        .await?;
    // `validated_at` stays NULL: the login's userinfo answer can arrive long
    // after it started, so treating it as a completed revalidation would open
    // a fresh 5 s window. The first authenticated request revalidates.
    sqlx::query(
        "INSERT INTO sessions (id, subject, token_hash, expires_at, kind, access_token, refresh_token, id_token, access_expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::new_v4())
    .bind(&identity.subject)
    .bind(token.hash())
    .bind(expires_at)
    .bind(host.kind())
    .bind(&tokens.access_token)
    .bind(tokens.refresh_token.as_deref())
    .bind(tokens.id_token.as_deref())
    .bind(timestamp(tokens.access_expires_at))
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await
}

/// The Unix seconds from a provider token response, as a database timestamp.
fn timestamp(seconds: Option<u64>) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(seconds? as i64, 0)
}

/// Resolve an opaque session token for one host kind, with the tokens the gate
/// revalidates. A token created on the other host never matches.
pub(crate) async fn session_row(
    pool: &PgPool,
    hash: &str,
    kind: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    fetch_session_row(pool, hash, Some(kind)).await
}

/// Re-read a session whatever its host kind, for a request that already
/// resolved it through [`session_row`] and holds its lock: the row it must use
/// is the one the previous holder wrote, not its own snapshot.
async fn session_row_by_hash(pool: &PgPool, hash: &str) -> Result<Option<SessionRow>, sqlx::Error> {
    fetch_session_row(pool, hash, None).await
}

async fn fetch_session_row(
    pool: &PgPool,
    hash: &str,
    kind: Option<&str>,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT p.subject, p.issuer, p.display_name, p.email, p.claims,
                s.access_token, s.refresh_token, s.id_token, s.access_expires_at, s.validated_at
           FROM sessions s JOIN principals p ON p.subject = s.subject
          WHERE s.token_hash = $1 AND ($2::text IS NULL OR s.kind = $2)
            AND s.revoked_at IS NULL AND s.expires_at > now()",
    )
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
        Ok(SessionRow {
            token_hash: hash.to_owned(),
            principal: Principal {
                subject: row.try_get("subject")?,
                issuer: row.try_get("issuer")?,
                display_name: row.try_get("display_name")?,
                email: row.try_get("email")?,
                groups,
            },
            access_token: row.try_get("access_token")?,
            refresh_token: row.try_get("refresh_token")?,
            id_token: row.try_get("id_token")?,
            access_expires_at: row.try_get("access_expires_at")?,
            validated_at: row.try_get("validated_at")?,
        })
    })
    .transpose()
}

/// Record a confirmed revalidation: Rauthy's claims on the principal, and the
/// freshness anchor as when the check *started*.
async fn apply_claims(pool: &PgPool, hash: &str, principal: &Principal, started: DateTime<Utc>) {
    let claims = json!({
        "groups": principal.groups,
        "email": principal.email,
        "display_name": principal.display_name,
    });
    let result = async {
        sqlx::query("UPDATE principals SET display_name = $2, email = $3, claims = $4, updated_at = now() WHERE subject = $1")
            .bind(&principal.subject)
            .bind(&principal.display_name)
            .bind(&principal.email)
            .bind(claims)
            .execute(pool)
            .await?;
        sqlx::query("UPDATE sessions SET validated_at = $2 WHERE token_hash = $1")
            .bind(hash)
            .bind(started)
            .execute(pool)
            .await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "failed to record a Foundry Circle revalidation");
    }
}

/// Persist a refreshed token set, returning whether the row was written. The
/// provider may omit the refresh token: the previous one stays, because it is
/// still the only way to renew; the same holds for the ID token the logout hint
/// needs.
async fn persist_tokens(
    pool: &PgPool,
    hash: &str,
    tokens: &TokenSet,
    row: &SessionRow,
    previous: Option<&TokenSet>,
) -> bool {
    let refresh = tokens
        .refresh_token
        .as_deref()
        .or_else(|| previous.and_then(|previous| previous.refresh_token.as_deref()))
        .or(row.refresh_token.as_deref());
    let id_token = tokens
        .id_token
        .as_deref()
        .or_else(|| previous.and_then(|previous| previous.id_token.as_deref()))
        .or(row.id_token.as_deref());
    match sqlx::query(
        "UPDATE sessions SET access_token = $2, refresh_token = $3, id_token = $4, access_expires_at = $5 WHERE token_hash = $1",
    )
    .bind(hash)
    .bind(&tokens.access_token)
    .bind(refresh)
    .bind(id_token)
    .bind(timestamp(tokens.access_expires_at))
    .execute(pool)
    .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, "failed to store refreshed Foundry Circle tokens");
            false
        }
    }
}

/// Why a subject-wide revocation could not complete. Every entry point turns
/// this into a 503: the browser must not be told the sessions are gone while
/// the database still has them.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RevokeError {
    /// The database pool is not ready.
    #[error("database is not configured")]
    DatabaseUnavailable,
    /// The revocation UPDATE failed; nothing was revoked.
    #[error("session revocation failed: {0}")]
    Store(#[from] sqlx::Error),
}

/// Revoke one session row. A row from before 0006 holds no tokens, so nothing
/// can revalidate it; the request is signed out without touching the subject's
/// newer sessions.
pub(crate) async fn invalidate_session(pool: &PgPool, hash: &str) {
    if let Err(error) = sqlx::query(
        "UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(hash)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, "failed to invalidate a Foundry Circle session");
    }
}

/// Revoke every session a subject holds and begin ending the lease they hold,
/// in one transaction, then rotate their Foundry users' passwords (D5, G6).
/// The database rows and their audit entries are never best effort: a failure
/// rolls back both and is returned to the caller. The reconciler finishes an
/// `ending` lease (stop the world, restart Foundry) without depending on this
/// request. Only the Foundry password rotation is detached and best effort,
/// because it would outlast Caddy's 2 s gate deadline and the revoked play
/// session already makes every gated Foundry request fail.
pub(crate) async fn revoke_subject(
    state: &AppState,
    subject: &str,
    reason: RevokeReason,
) -> Result<(), RevokeError> {
    let pool = state
        .database
        .pool()
        .await
        .ok_or(RevokeError::DatabaseUnavailable)?;
    let mut transaction = pool.begin().await?;
    // Lock order matches `/_circle/enter`, which locks the lease row first: the
    // `game_session` row must come before the subject's `sessions` rows, or the
    // two writers can deadlock. A plain row-level transition is enough; the
    // reconciler owns Foundry from here.
    let ending = sqlx::query(
        "UPDATE game_session SET phase = 'ending', end_reason = 'revoked'
         WHERE ended_at IS NULL AND holder = $1 AND phase IN ('setup', 'running')
         RETURNING id",
    )
    .bind(subject)
    .fetch_optional(&mut *transaction)
    .await?;
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE subject = $1 AND revoked_at IS NULL")
        .bind(subject)
        .execute(&mut *transaction)
        .await?;
    // The audit records what actually happened: "ok" only after both writes.
    session::insert_audit(
        &mut *transaction,
        Some(subject),
        "session.revoke",
        "ok",
        json!({"subject": subject, "reason": reason.as_str()}),
    )
    .await?;
    if let Some(ending) = ending {
        let id: Uuid = ending.try_get("id")?;
        session::insert_audit(
            &mut *transaction,
            Some(subject),
            "session.stop",
            "ok",
            json!({"sessionId": id, "reason": "revoked"}),
        )
        .await?;
    }
    // The open world, if any, holds the subject's mapped Foundry users. The
    // rotation reads it after commit, so the row is only observed here.
    let world: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT world_id FROM game_session WHERE ended_at IS NULL",
    )
    .fetch_optional(&mut *transaction)
    .await?
    .flatten();
    transaction.commit().await?;
    // Defense in depth only: the revoked play session already closes the gate
    // and `stream_reauth` drops open sockets. A cancelled gate request must not
    // lose it, so it stays detached.
    if let Some(world) = world {
        let state = state.clone();
        let subject = subject.to_owned();
        tokio::spawn(async move {
            rotate_mapped_users(&state, &pool, &subject, &world).await;
        });
    }
    Ok(())
}

/// Rotate each of the subject's mapped Foundry users' passwords in `world`.
/// Foundry deletes a user's HTTP sessions on a password change, and the play
/// host's `stream_reauth` then closes their socket because the gate refuses it.
async fn rotate_mapped_users(state: &AppState, pool: &PgPool, subject: &str, world: &str) {
    let users: Vec<String> = match sqlx::query_scalar(
        "SELECT user_id FROM foundry_users WHERE subject = $1 AND world_id = $2",
    )
    .bind(subject)
    .bind(world)
    .fetch_all(pool)
    .await
    {
        Ok(users) => users,
        Err(error) => {
            tracing::warn!(%error, "failed to read a revoked principal's Foundry users");
            return;
        }
    };
    for user_id in users {
        let op = AccountOp::SetPassword {
            user_id,
            password: adoption::random_password(),
        };
        if let Err(error) = state.driver.account(op).await {
            tracing::warn!(%error, world, "Foundry refused a revocation password rotation");
        }
    }
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

fn expired_session_cookie(host: LoginHost) -> String {
    format!(
        "{}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax",
        host.cookie_name()
    )
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
        for (host, name) in [
            (LoginHost::Platform, "foundry_circle_session="),
            (LoginHost::Play, "foundry_circle_play="),
        ] {
            let cookie = expired_session_cookie(host);
            assert!(cookie.starts_with(name), "{cookie}");
            assert!(cookie.contains("Max-Age=0"));
            assert!(cookie.contains("HttpOnly"));
            assert!(cookie.contains("Secure"));
            assert!(cookie.contains("SameSite=Lax"));
        }
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

    /// RFC 7636 appendix B: the verifier and the S256 challenge it must meet.
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn native(
        port: Option<&str>,
        state: Option<&str>,
        challenge: Option<&str>,
    ) -> Option<NativeLogin> {
        native_login(&NativeQuery {
            port: port.map(str::to_owned),
            state: state.map(str::to_owned),
            challenge: challenge.map(str::to_owned),
        })
    }

    #[test]
    fn a_native_login_needs_a_loopback_port_a_state_and_an_s256_challenge() {
        let expected = NativeLogin {
            port: 8787,
            state: "0123456789abcdef".into(),
            challenge: CHALLENGE.into(),
        };
        assert_eq!(
            native(Some("8787"), Some("0123456789abcdef"), Some(CHALLENGE)),
            Some(expected)
        );
        // Ports below 1024 are privileged, and a port that is not one is not a
        // loopback listener.
        for port in [Some("80"), Some("0"), Some("70000"), Some("http"), None] {
            assert_eq!(
                native(port, Some("0123456789abcdef"), Some(CHALLENGE)),
                None,
                "{port:?}"
            );
        }
        // The state must fit a query string and stay URL-safe.
        for state in [Some("too-short"), Some("0123456789abcde+"), None] {
            assert_eq!(
                native(Some("8787"), state, Some(CHALLENGE)),
                None,
                "{state:?}"
            );
        }
        // The challenge is a canonical 32-byte digest: no padding, no short cut.
        for challenge in [
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-c"),
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM="),
            None,
        ] {
            assert_eq!(
                native(Some("8787"), Some("0123456789abcdef"), challenge),
                None,
                "{challenge:?}"
            );
        }
        // Every RFC 7636 verifier is 43..=128 characters; a longer state is fine.
        let long_state = "a".repeat(128);
        assert!(native(Some("8787"), Some(&long_state), Some(CHALLENGE)).is_some());
        assert_eq!(
            native(Some("8787"), Some(&"a".repeat(129)), Some(CHALLENGE)),
            None
        );
    }

    #[test]
    fn the_pkce_verifier_must_produce_the_recorded_challenge() {
        assert_eq!(verifier_challenge(VERIFIER), CHALLENGE);
        assert!(verifier_matches(CHALLENGE, VERIFIER));
        // A verifier that differs in one byte is refused.
        assert!(!verifier_matches(
            CHALLENGE,
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXj"
        ));
        assert!(!verifier_matches(CHALLENGE, ""));
        // A challenge that is not base64url is refused rather than panicking.
        assert!(!verifier_matches("not a challenge", VERIFIER));
        assert_eq!(verifier_challenge(VERIFIER).len(), 43);
    }

    fn stored_row() -> SessionRow {
        SessionRow {
            token_hash: "hash".into(),
            principal: Principal {
                subject: "sub".into(),
                issuer: "https://id.example/".into(),
                display_name: Some("Robin".into()),
                email: Some("robin@example.test".into()),
                groups: vec!["foundry-circle-users".into()],
            },
            access_token: Some("access".into()),
            refresh_token: Some("refresh".into()),
            id_token: Some("id".into()),
            access_expires_at: None,
            validated_at: None,
        }
    }

    #[test]
    fn a_fresh_answer_is_used_without_asking_rauthy() {
        // Inside 5 s the cached answer wins even with no Rauthy results at all.
        assert_eq!(
            decide(Some(Duration::from_millis(4_999)), false, None, None, true),
            Revalidation::Valid
        );
        // Exactly at the window the check must run again; with no answer yet it
        // cannot be trusted.
        assert_eq!(
            decide(Some(Duration::from_secs(5)), false, None, None, true),
            Revalidation::Unavailable
        );
        // A legacy row (never validated) is never fresh, and it is stale alone:
        // it must not revoke the subject's newer sessions.
        assert_eq!(
            decide(None, false, None, Some(UserInfo::Missing), true),
            Revalidation::Stale
        );
    }

    #[test]
    fn a_tokenless_row_is_stale_and_never_revokes_the_subject() {
        // A row from before 0006 cannot be revalidated, so it is invalidated
        // alone: an old tab must not end the subject's newer sessions or lease.
        let outcome = decide(None, false, None, Some(UserInfo::Missing), true);
        assert_eq!(outcome, Revalidation::Stale);
        assert_ne!(outcome, Revalidation::Revoked);
        assert_eq!(
            decide(None, false, None, Some(UserInfo::Missing), false),
            Revalidation::Stale
        );
    }

    #[test]
    fn a_login_row_has_no_freshness_anchor() {
        // `save_principal_and_session` inserts `validated_at = NULL`, so a
        // login-time userinfo answer that arrived late cannot open a window:
        // the first authenticated request asks again.
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        assert_eq!(cached_age(None, now), None);
        assert_eq!(
            decide(None, false, None, Some(UserInfo::Claims), true),
            Revalidation::Valid
        );
    }

    #[test]
    fn only_a_confirmed_member_is_valid() {
        let stale = Some(Duration::from_secs(30));
        assert_eq!(
            decide(stale, false, None, Some(UserInfo::Claims), true),
            Revalidation::Valid
        );
        assert_eq!(
            decide(stale, false, None, Some(UserInfo::Claims), false),
            Revalidation::Revoked
        );
        // Even a cached answer stops being valid when its claims lost access.
        assert_eq!(
            decide(Some(Duration::from_secs(1)), false, None, None, false),
            Revalidation::Revoked
        );
    }

    #[test]
    fn a_rejected_identity_is_revoked_and_an_outage_is_not() {
        let stale = Some(Duration::from_secs(30));
        assert_eq!(
            decide(stale, false, None, Some(UserInfo::Rejected), true),
            Revalidation::Revoked
        );
        assert_eq!(
            decide(
                stale,
                true,
                Some(Refresh::Refreshed),
                Some(UserInfo::Rejected),
                true
            ),
            Revalidation::Revoked
        );
        assert_eq!(
            decide(stale, false, None, None, true),
            Revalidation::Unavailable
        );
    }

    #[test]
    fn a_refused_refresh_revokes_without_judging_the_access_token() {
        let stale = Some(Duration::from_secs(30));
        for in_access_group in [true, false] {
            assert_eq!(
                decide(stale, true, Some(Refresh::Rejected), None, in_access_group),
                Revalidation::Revoked
            );
        }
    }

    #[test]
    fn an_unavailable_refresh_is_unavailable_without_judging_the_access_token() {
        let stale = Some(Duration::from_secs(30));
        assert_eq!(
            decide(stale, true, Some(Refresh::Unavailable), None, true),
            Revalidation::Unavailable
        );
    }

    #[test]
    fn a_successful_refresh_still_lets_userinfo_decide() {
        let stale = Some(Duration::from_secs(30));
        assert_eq!(
            decide(
                stale,
                true,
                Some(Refresh::Refreshed),
                Some(UserInfo::Claims),
                true
            ),
            Revalidation::Valid
        );
        assert_eq!(
            decide(stale, true, Some(Refresh::Refreshed), None, true),
            Revalidation::Unavailable
        );
    }

    #[test]
    fn the_budget_running_out_is_unavailable() {
        // A refresh that used the whole budget leaves no userinfo answer: new
        // requests fail closed and nothing is revoked (D6).
        assert_eq!(
            decide(
                Some(Duration::from_secs(30)),
                true,
                Some(Refresh::Refreshed),
                None,
                true
            ),
            Revalidation::Unavailable
        );
    }

    #[test]
    fn freshness_is_anchored_at_the_start_of_the_check() {
        let started = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        // A check that took 1.4 s still anchors at `started`, so 5.1 s after it
        // began the answer is stale, even though it arrived 3.7 s earlier.
        let age = cached_age(
            Some(started),
            started + chrono::Duration::milliseconds(5_100),
        );
        assert_eq!(age, Some(Duration::from_millis(5_100)));
        assert_eq!(
            decide(age, false, None, None, true),
            Revalidation::Unavailable
        );
        // Anchored at completion it would still be fresh: the difference the
        // start anchor makes.
        let at_completion = cached_age(
            Some(started),
            started + chrono::Duration::milliseconds(1_400),
        );
        assert_eq!(
            decide(at_completion, false, None, None, true),
            Revalidation::Valid
        );
    }

    #[test]
    fn a_carried_refresh_wins_over_the_stored_row() {
        let mut row = stored_row();
        row.access_expires_at = DateTime::<Utc>::from_timestamp(1_600_000_000, 0);
        row.refresh_token = Some("old-refresh".into());
        let carried = TokenSet {
            access_token: "new-access".into(),
            refresh_token: Some("new-refresh".into()),
            id_token: None,
            access_expires_at: Some(1_700_000_000),
        };
        let basis = token_basis(Some(&carried), &row, "old-access");
        // The carried set is newer than the row, and its refresh token is the
        // only one Rauthy still accepts.
        assert_eq!(basis.access_token, "new-access");
        assert_eq!(
            basis.access_expires_at,
            DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
        );
        assert_eq!(basis.refresh_token.as_deref(), Some("new-refresh"));
        // Nothing carried: the row is the basis, as before.
        let basis = token_basis(None, &row, "old-access");
        assert_eq!(basis.access_token, "old-access");
        assert_eq!(basis.access_expires_at, row.access_expires_at);
        assert_eq!(basis.refresh_token.as_deref(), Some("old-refresh"));
        // A carried set without a refresh token keeps the row's.
        let carried = TokenSet {
            refresh_token: None,
            ..carried
        };
        assert_eq!(
            token_basis(Some(&carried), &row, "old-access")
                .refresh_token
                .as_deref(),
            Some("old-refresh")
        );
    }

    #[test]
    fn a_stashed_refresh_is_peeked_and_kept_until_the_row_has_it() {
        let stash = TokenStash::default();
        let tokens = TokenSet {
            access_token: "access-1".into(),
            refresh_token: Some("refresh-1".into()),
            id_token: None,
            access_expires_at: None,
        };
        stash.insert("hash", tokens);
        // The next check clones the set to retry the database write, and the
        // set stays stashed: a failing write must not lose it.
        let carried = stash.peek("hash").expect("a stashed set");
        assert_eq!(carried.access_token, "access-1");
        assert!(stash.peek("hash").is_some());
        // The retry failed, so the check after finds it again.
        assert_eq!(
            stash.peek("hash").map(|tokens| tokens.access_token.clone()),
            Some("access-1".into())
        );
        // Only a write that succeeded removes it.
        stash.remove("hash");
        assert!(stash.peek("hash").is_none());
        // Another session's stash is separate.
        assert!(stash.peek("other").is_none());
    }

    #[tokio::test]
    async fn parallel_revalidations_of_one_session_refresh_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // One fake request: its "row" is fresh only once the other task wrote
        // it. `yield_now` lets both tasks pile up on the lock, as parallel
        // asset loads do.
        async fn request(
            locks: &SessionLocks,
            refreshes: Arc<AtomicUsize>,
            stored: Arc<Mutex<Option<&'static str>>>,
        ) -> Option<&'static str> {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            locks
                .run("hash", deadline, async move {
                    if let Some(seen) = *stored.lock().unwrap() {
                        return seen;
                    }
                    tokio::task::yield_now().await;
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    *stored.lock().unwrap() = Some("refreshed");
                    "refreshed"
                })
                .await
        }

        let locks = SessionLocks::default();
        let refreshes = Arc::new(AtomicUsize::new(0));
        // The stand-in for the shared row: whichever request refreshes records
        // its result here, which is what the other one must observe instead of
        // refreshing again with the same refresh token.
        let stored: Arc<Mutex<Option<&'static str>>> = Arc::new(Mutex::new(None));
        let (first, second) = tokio::join!(
            request(&locks, refreshes.clone(), stored.clone()),
            request(&locks, refreshes.clone(), stored.clone()),
        );
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        // The second caller answered from the first caller's write.
        assert_eq!(first, Some("refreshed"));
        assert_eq!(second, Some("refreshed"));
        // Nothing outlives the requests that used the session.
        assert!(locks.gates.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_waiter_past_the_budget_gets_no_verdict() {
        let locks = SessionLocks::default();
        let holder_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let waiter_deadline = tokio::time::Instant::now() + Duration::from_millis(10);
        let (held, waited) = tokio::join!(
            locks.run("hash", holder_deadline, async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                7
            }),
            locks.run("hash", waiter_deadline, async { 8 }),
        );
        assert_eq!(held, Some(7));
        // Out of budget: the request is retried, not judged.
        assert_eq!(waited, None);
        // Neither the expired waiter nor the holder leaves an entry behind.
        assert!(locks.gates.lock().unwrap().is_empty());
    }

    #[test]
    fn leases_dropped_together_leave_no_lock_entry() {
        // Two leases on one entry released from two threads at once: the
        // last one out must still see itself as the only user and remove it.
        for _ in 0..200 {
            let locks = SessionLocks::default();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                for _ in 0..2 {
                    scope.spawn(|| {
                        let lease = GateLease {
                            locks: &locks,
                            hash: "hash",
                            gate: Some(locks.gate("hash")),
                        };
                        barrier.wait();
                        drop(lease);
                    });
                }
            });
            assert!(locks.gates.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_cancelled_holder_leaves_no_lock_entry() {
        let locks = SessionLocks::default();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        // Caddy dropping the request at 2 s cancels the future mid-work.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(10),
            locks.run("hash", deadline, std::future::pending::<()>()),
        )
        .await;
        assert!(cancelled.is_err());
        assert!(locks.gates.lock().unwrap().is_empty());
        // And the lock is free for the next request.
        assert_eq!(locks.run("hash", deadline, async { 1 }).await, Some(1));
    }

    #[test]
    fn a_refresh_is_only_due_inside_the_last_minute() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let at = |seconds: i64| Some(now + chrono::Duration::seconds(seconds));
        assert!(refresh_due(at(60), true, now));
        assert!(refresh_due(at(1), true, now));
        assert!(refresh_due(at(-30), true, now));
        assert!(!refresh_due(at(61), true, now));
        // No refresh token, no refresh; no expiry, no refresh.
        assert!(!refresh_due(at(1), false, now));
        assert!(!refresh_due(None, true, now));
    }

    #[test]
    fn revoke_reasons_are_the_audit_strings() {
        assert_eq!(RevokeReason::Revoked.as_str(), "revoked");
        assert_eq!(RevokeReason::Admin.as_str(), "admin");
        assert_eq!(RevokeReason::Logout.as_str(), "logout");
        // The lease records the same word the gate used.
        assert_eq!(RevokeReason::Revoked.as_str(), "revoked");
    }

    #[test]
    fn only_an_admin_may_revoke_sessions() {
        let groups = |groups: &[&str]| Principal {
            subject: "sub".into(),
            issuer: "https://id.example/".into(),
            display_name: None,
            email: None,
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        };
        let admin = groups(&["foundry-circle-users", "foundry-circle-admins"]);
        let gm = groups(&["foundry-circle-users", "foundry-circle-gms"]);
        let player = groups(&["foundry-circle-users"]);
        assert!(may_revoke(&admin, "foundry-circle-admins"));
        assert!(!may_revoke(&gm, "foundry-circle-admins"));
        assert!(!may_revoke(&player, "foundry-circle-admins"));
        // A similar group name is not the admin group.
        assert!(!may_revoke(
            &groups(&["foundry-circle-admin"]),
            "foundry-circle-admins"
        ));
    }

    #[test]
    fn logout_uses_rauthy_when_it_advertises_an_end_session_url() {
        let url = "https://id.example/auth/v1/logout?client_id=fc".to_owned();
        assert_eq!(logout_destination(Some(url.clone())), url);
        assert_eq!(logout_destination(None), "/api/console");
    }

    #[test]
    fn logout_requires_the_exact_origin_of_its_host() {
        let platform = Some("https://vtt.example");
        let play = Some("https://play.vtt.example");
        let origin = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, value.parse().unwrap());
            headers
        };
        assert!(logout_origin_allowed(
            LoginHost::Platform,
            &origin("https://vtt.example"),
            platform,
            play
        ));
        // The play host's origin never authorises the platform logout.
        assert!(!logout_origin_allowed(
            LoginHost::Platform,
            &origin("https://play.vtt.example"),
            platform,
            play
        ));
        assert!(logout_origin_allowed(
            LoginHost::Play,
            &origin("https://play.vtt.example"),
            platform,
            play
        ));
        // The platform origin never authorises the play logout.
        assert!(!logout_origin_allowed(
            LoginHost::Play,
            &origin("https://vtt.example"),
            platform,
            play
        ));
        // Missing `Origin` is a refusal, never an accepted logout.
        assert!(!logout_origin_allowed(
            LoginHost::Platform,
            &HeaderMap::new(),
            platform,
            play
        ));
        assert!(!logout_origin_allowed(
            LoginHost::Play,
            &HeaderMap::new(),
            platform,
            play
        ));
        // A lookalike origin and an unconfigured host are refusals too.
        assert!(!logout_origin_allowed(
            LoginHost::Platform,
            &origin("https://vtt.example.evil"),
            platform,
            play
        ));
        assert!(!logout_origin_allowed(
            LoginHost::Play,
            &origin("https://play.vtt.example"),
            platform,
            None
        ));
    }

    #[test]
    fn a_logout_with_a_session_cookie_needs_the_database() {
        // No cookie: nothing this host could revoke, so the cookie is cleared
        // and the browser leaves even while the database is down.
        assert_eq!(logout_plan(None, false), LogoutPlan::Clear);
        assert_eq!(logout_plan(None, true), LogoutPlan::Clear);
        // A cookie names sessions that stay valid without the database: the
        // logout refuses instead of clearing the cookie and redirecting as if
        // they were gone.
        assert_eq!(logout_plan(Some("opaque"), false), LogoutPlan::Refuse);
        assert_eq!(logout_plan(Some("opaque"), true), LogoutPlan::Revoke);
        // An empty cookie carries no session.
        assert_eq!(logout_plan(Some(""), false), LogoutPlan::Clear);
    }

    #[test]
    fn revalidated_claims_replace_the_login_snapshot() {
        let row = stored_row();
        let claims = UserInfoClaims {
            subject: "sub".into(),
            email: Some("new@example.test".into()),
            display_name: Some("Robin (2)".into()),
            groups: ["foundry-circle-users", "foundry-circle-gms"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        let principal = principal_from(&row, &claims);
        assert_eq!(principal.subject, "sub");
        assert_eq!(principal.issuer, "https://id.example/");
        assert_eq!(principal.email.as_deref(), Some("new@example.test"));
        assert_eq!(principal.display_name.as_deref(), Some("Robin (2)"));
        assert_eq!(
            principal.groups,
            vec!["foundry-circle-gms", "foundry-circle-users"]
        );
    }
}
