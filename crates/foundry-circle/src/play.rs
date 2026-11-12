//! The play-host gate (G4.2, G4.3) and the holder's way into Foundry Setup
//! (G5).
//!
//! Caddy on the play host routes `/_circle/*` to this broker without the gate
//! and sends every other request through `forward_auth` to `GET /_circle/auth`
//! with the original `Cookie` header and `X-Forwarded-Method`/`-Uri`/`-Host`.
//! Cookies are host-only, so the play host runs its own OIDC login
//! (`/_circle/login`) and its own session kind.

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use oidc_app_auth::SessionToken;
use serde_json::json;
use sqlx::PgPool;

use crate::{
    SessionPhase, adoption,
    http::{
        AppState,
        auth::{self, LoginHost, PLAY_SESSION_COOKIE, Principal},
        session,
    },
};

/// One gate decision for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(Denial),
}

/// Why the gate refused, as the lobby's `reason` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// No valid play-host session; the gate sends the browser to log in.
    SignedOut,
    /// Signed in, but not a member of the access group.
    NoAccess,
    /// No session lease is open, so there is nothing to play.
    NoSession,
    /// Setup (and Foundry's admin login) is the lease holder's alone.
    SetupOnly,
    /// Joining goes through `/_circle/enter` in the next slice; this also
    /// blocks Foundry's `loginAs`.
    JoinBlocked,
    /// The running world has not finished adoption, so only the holder and
    /// admins may reach it.
    NotAdopted,
}

impl Denial {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SignedOut => "signed_out",
            Self::NoAccess => "no_access",
            Self::NoSession => "no_session",
            Self::SetupOnly => "setup_only",
            Self::JoinBlocked => "join_blocked",
            Self::NotAdopted => "not_adopted",
        }
    }
}

/// Everything the gate knows before it decides. `phase` is `None` when no
/// lease is open; `signed_in` is a valid `kind = 'play'` session on this host.
#[derive(Debug, Clone, Copy)]
pub struct GateFacts<'a> {
    pub signed_in: bool,
    pub in_access_group: bool,
    pub is_admin: bool,
    pub is_holder: bool,
    pub phase: Option<SessionPhase>,
    /// The running world completed adoption (G5).
    pub adopted: bool,
    pub method: &'a str,
    pub uri: &'a str,
}

/// The rule table, applied in order on every request. Method, path, role and
/// phase are evaluated per request; only the identity is worth caching later.
pub fn decide(facts: &GateFacts<'_>) -> Decision {
    if !facts.signed_in {
        return Decision::Deny(Denial::SignedOut);
    }
    if !facts.in_access_group {
        return Decision::Deny(Denial::NoAccess);
    }
    let path = normalized_path(facts.uri);
    if facts.method.eq_ignore_ascii_case("POST") && path == "/join" {
        return Decision::Deny(Denial::JoinBlocked);
    }
    let driving = facts.is_holder || facts.is_admin;
    if driving
        && matches!(
            facts.phase,
            Some(SessionPhase::Setup | SessionPhase::Running)
        )
    {
        return Decision::Allow;
    }
    if facts.phase == Some(SessionPhase::Running) {
        // Players stay out until adoption has finished (G5).
        if !facts.adopted {
            return Decision::Deny(Denial::NotAdopted);
        }
        if !is_setup_path(&path) && path != "/auth" {
            return Decision::Allow;
        }
    }
    if is_setup_path(&path) || path == "/auth" {
        Decision::Deny(Denial::SetupOnly)
    } else {
        Decision::Deny(Denial::NoSession)
    }
}

/// The path Foundry's Express router will see: query and fragment removed,
/// percent escapes decoded once, and lowercased (Express routes
/// case-insensitively by default). `/setupx` stays `/setupx`.
fn normalized_path(uri: &str) -> String {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    percent_decode(path).to_ascii_lowercase()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (bytes.get(index + 1), bytes.get(index + 2))
            && let (Some(high), Some(low)) = (hex_digit(*high), hex_digit(*low))
        {
            out.push((high << 4 | low) as char);
            index += 3;
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_setup_path(path: &str) -> bool {
    path == "/setup" || path.starts_with("/setup/")
}

/// Caddy's forward-auth check: a 200 with an empty body admits the request, a
/// redirect refuses it and sends the browser to the lobby.
pub async fn gate(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OIDC is not configured").into_response();
    };
    // Every refusal and the signed-out redirect need the lobby's origin.
    let Some(platform) = state.console.platform_url.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the lobby is not configured",
        )
            .into_response();
    };
    let (Some(method), Some(uri)) = (
        forwarded(&headers, "x-forwarded-method"),
        forwarded(&headers, "x-forwarded-uri"),
    ) else {
        return lobby_redirect(platform, Denial::NoSession);
    };
    let Some(pool) = state.database.pool().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "database is not configured",
        )
            .into_response();
    };
    let principal = match play_principal(&pool, &headers).await {
        Ok(principal) => principal,
        Err(error) => {
            tracing::warn!(%error, "failed to resolve a play-host session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    // ponytail: every gated request reads the lease from PostgreSQL; cache the
    // open lease in memory and update it on each transition if the added
    // latency on the Foundry hot path matters.
    let open = match session::open(&pool).await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, "failed to read the open Foundry Circle session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    let facts = facts(&principal, open.as_ref(), auth, method, uri);
    match decide(&facts) {
        Decision::Allow => {
            // Every admitted request from the holder counts as presence (D3).
            if facts.is_holder
                && let Some(principal) = principal.as_ref()
            {
                session::touch(&pool, &principal.subject).await;
            }
            StatusCode::OK.into_response()
        }
        Decision::Deny(Denial::SignedOut) => play_login_redirect(uri),
        Decision::Deny(reason) => lobby_redirect(platform, reason),
    }
}

fn facts<'a>(
    principal: &Option<Principal>,
    open: Option<&session::OpenSession>,
    auth: &auth::AuthState,
    method: &'a str,
    uri: &'a str,
) -> GateFacts<'a> {
    GateFacts {
        signed_in: principal.is_some(),
        in_access_group: principal
            .as_ref()
            .is_some_and(|principal| principal.groups.contains(&auth.access_group)),
        is_admin: principal
            .as_ref()
            .is_some_and(|principal| principal.is_admin(&auth.admin_group)),
        is_holder: match (principal, open) {
            (Some(principal), Some(open)) => open.holder == principal.subject,
            _ => false,
        },
        phase: open.map(|open| open.phase),
        adopted: open.is_some_and(|open| open.adopted),
        method,
        uri,
    }
}

/// `GET /_circle/setup`: open a fresh Foundry admin session for the lease
/// holder and relay its cookie to the browser.
pub async fn setup(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OIDC is not configured").into_response();
    };
    let Some(platform) = state.console.platform_url.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the lobby is not configured",
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
    let principal = match play_principal(&pool, &headers).await {
        Ok(principal) => principal,
        Err(error) => {
            tracing::warn!(%error, "failed to resolve a play-host session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    let Some(principal) = principal else {
        // A holder arriving from the lobby has only a platform session: log
        // in on this host first, then come straight back here.
        return play_login_redirect(SETUP_ROUTE);
    };
    if !principal.groups.contains(&auth.access_group) {
        return lobby_redirect(platform, Denial::NoAccess);
    }
    let open = match session::open(&pool).await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, "failed to read the open Foundry Circle session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    let Some(open) = open else {
        return lobby_redirect(platform, Denial::NoSession);
    };
    let driving = open.holder == principal.subject || principal.is_admin(&auth.admin_group);
    if !driving || open.phase != SessionPhase::Setup {
        return lobby_redirect(platform, Denial::SetupOnly);
    }
    if !state.lease.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "session controls are not configured",
        )
            .into_response();
    }
    let session_id = match state.lease.open_setup().await {
        Ok(session_id) => session_id,
        Err(error) => {
            tracing::warn!(%error, "Foundry refused the Setup login");
            return (StatusCode::BAD_GATEWAY, "Foundry did not open Setup").into_response();
        }
    };
    // A second, broker-only admin session: adoption uses it to take over a GM
    // once the holder launches. It never reaches a browser or the driver.
    match state.lease.open_setup().await {
        Ok(adoption_session) => state.lease.remember_bootstrap(open.id, adoption_session),
        Err(error) => {
            tracing::warn!(
                %error,
                "Foundry did not open the broker's adoption session; adoption will need a relaunch"
            );
        }
    }
    if let Err(error) = session::insert_audit(
        &pool,
        Some(&principal.subject),
        "session.setup_access",
        "ok",
        json!({"sessionId": open.id}),
    )
    .await
    {
        tracing::warn!(%error, "failed to write the Foundry Circle Setup audit entry");
    }
    setup_page(&session_id)
}

/// `GET /_circle/enter`: map the signed-in player to a Foundry user and relay
/// a fresh non-admin session cookie for it.
pub async fn enter(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "OIDC is not configured").into_response();
    };
    let Some(platform) = state.console.platform_url.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the lobby is not configured",
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
    let principal = match play_principal(&pool, &headers).await {
        Ok(principal) => principal,
        Err(error) => {
            tracing::warn!(%error, "failed to resolve a play-host session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    let Some(principal) = principal else {
        return play_login_redirect(ENTER_ROUTE);
    };
    if !principal.groups.contains(&auth.access_group) {
        return lobby_redirect(platform, Denial::NoAccess);
    }
    let open = match session::open(&pool).await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, "failed to read the open Foundry Circle session");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session lookup error").into_response();
        }
    };
    let Some(open) = open else {
        return lobby_redirect(platform, Denial::NoSession);
    };
    if open.phase != SessionPhase::Running {
        return lobby_redirect(platform, Denial::NoSession);
    }
    if !open.adopted {
        return lobby_redirect(platform, Denial::NotAdopted);
    }
    let (Some(world), Some(base_url)) = (
        open.world_id.as_deref(),
        state.console.foundry_url.as_deref(),
    ) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Foundry is not configured").into_response();
    };
    let display_name = principal
        .display_name
        .clone()
        .or_else(|| principal.email.clone())
        .unwrap_or_else(|| principal.subject.clone());
    // The entry locks the lease row and decides the role from it; the `open`
    // read above is only the fast path to a redirect.
    let (session_id, is_holder) = match adoption::enter(
        &pool,
        state.driver.as_ref(),
        base_url,
        open.id,
        &principal.subject,
        &display_name,
        world,
    )
    .await
    {
        Ok(entered) => entered,
        Err(error) => {
            tracing::warn!(code = error.code(), "Foundry Circle entry was refused");
            return (error.status(), error.code()).into_response();
        }
    };
    if is_holder {
        session::touch(&pool, &principal.subject).await;
    }
    handoff_page(
        &session_id,
        "/game",
        "Opening Foundry",
        "Continue to Foundry",
    )
}

/// Foundry's cookie is `SameSite=Strict`, and a 302 at the end of a redirect
/// chain that began at Rauthy would drop it. This page sets the cookie and
/// navigates to the target as a new same-site navigation.
fn setup_page(session_id: &str) -> Response {
    handoff_page(
        session_id,
        "/setup",
        "Opening Foundry Setup",
        "Continue to Foundry Setup",
    )
}

fn handoff_page(session_id: &str, path: &str, title: &str, label: &str) -> Response {
    let Ok(cookie) = HeaderValue::try_from(format!(
        "session={session_id}; Path=/; HttpOnly; Secure; SameSite=Strict"
    )) else {
        return (
            StatusCode::BAD_GATEWAY,
            "Foundry sent an invalid session cookie",
        )
            .into_response();
    };
    let mut response = Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta http-equiv=\"refresh\" content=\"0;url={path}\">\
<title>{title}</title></head>\
<body><p><a href=\"{path}\">{label}</a></p></body></html>"
    ))
    .into_response();
    let headers = response.headers_mut();
    headers.insert(header::SET_COOKIE, cookie);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn forwarded<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The play-host session behind this request, if any.
async fn play_principal(
    pool: &PgPool,
    headers: &HeaderMap,
) -> Result<Option<Principal>, sqlx::Error> {
    let Some(token) = auth::cookie(headers, PLAY_SESSION_COOKIE) else {
        return Ok(None);
    };
    let hash = SessionToken::hash_value(token);
    auth::session_principal(pool, &hash, LoginHost::Play.kind()).await
}

/// The holder's way into Foundry Setup on the play host.
const SETUP_ROUTE: &str = "/_circle/setup";

/// Every player's way into the running, adopted world.
const ENTER_ROUTE: &str = "/_circle/enter";

fn play_login_redirect(uri: &str) -> Response {
    let location = format!("/_circle/login?return_to={}", query_value(uri));
    Redirect::to(&location).into_response()
}

fn lobby_redirect(platform: &str, reason: Denial) -> Response {
    let location = format!("{platform}/api/console?reason={}", reason.code());
    Redirect::to(&location).into_response()
}

/// Percent-encode a value for a query string, leaving the path separators
/// readable.
fn query_value(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_out_setup_visit_logs_in_on_the_play_host_and_returns() {
        // A holder coming from the lobby has only a platform session.
        let response = play_login_redirect(SETUP_ROUTE);
        let location = response.headers()[header::LOCATION].to_str().unwrap();
        assert_eq!(location, "/_circle/login?return_to=/_circle/setup");
        assert_eq!(auth::play_return_to(Some(SETUP_ROUTE)), SETUP_ROUTE);
    }

    fn facts<'a>(
        signed_in: bool,
        is_holder: bool,
        phase: Option<SessionPhase>,
        method: &'a str,
        uri: &'a str,
    ) -> GateFacts<'a> {
        GateFacts {
            signed_in,
            in_access_group: true,
            is_admin: false,
            is_holder,
            phase,
            adopted: true,
            method,
            uri,
        }
    }

    fn denial(facts: &GateFacts<'_>) -> Denial {
        match decide(facts) {
            Decision::Allow => panic!("expected a denial for {}", facts.uri),
            Decision::Deny(reason) => reason,
        }
    }

    fn allowed(facts: &GateFacts<'_>) -> bool {
        decide(facts) == Decision::Allow
    }

    #[test]
    fn a_signed_out_request_goes_to_the_play_login() {
        for phase in [None, Some(SessionPhase::Setup), Some(SessionPhase::Running)] {
            for is_holder in [true, false] {
                assert_eq!(
                    denial(&facts(false, is_holder, phase, "GET", "/game")),
                    Denial::SignedOut
                );
            }
        }
    }

    #[test]
    fn a_principal_outside_the_access_group_is_denied_everywhere() {
        let mut outside = facts(true, false, Some(SessionPhase::Running), "GET", "/game");
        outside.in_access_group = false;
        assert_eq!(denial(&outside), Denial::NoAccess);
        outside.is_admin = true;
        outside.is_holder = true;
        assert_eq!(denial(&outside), Denial::NoAccess);
    }

    #[test]
    fn posting_to_join_is_denied_for_everyone() {
        for phase in [Some(SessionPhase::Setup), Some(SessionPhase::Running)] {
            for is_holder in [true, false] {
                let mut facts = facts(true, is_holder, phase, "POST", "/join");
                facts.is_admin = true;
                assert_eq!(denial(&facts), Denial::JoinBlocked);
            }
        }
        // The method check is case-insensitive and ignores the query string.
        assert_eq!(
            denial(&facts(
                true,
                false,
                Some(SessionPhase::Running),
                "post",
                "/join?x=1"
            )),
            Denial::JoinBlocked
        );
        assert_eq!(
            denial(&facts(
                true,
                false,
                Some(SessionPhase::Running),
                "POST",
                "/JOIN"
            )),
            Denial::JoinBlocked
        );
        // A GET to /join is not a join and stays subject to the path rules.
        assert!(allowed(&facts(
            true,
            false,
            Some(SessionPhase::Running),
            "GET",
            "/join"
        )));
    }

    #[test]
    fn the_holder_and_admins_reach_every_path_while_the_lease_is_open() {
        for phase in [SessionPhase::Setup, SessionPhase::Running] {
            for path in ["/", "/game", "/setup", "/setup/worlds", "/auth", "/socket"] {
                assert!(
                    allowed(&facts(true, true, Some(phase), "GET", path)),
                    "holder {path} in {phase:?}"
                );
                assert!(
                    allowed(&facts(true, true, Some(phase), "POST", path)),
                    "holder POST {path} in {phase:?}"
                );
            }
        }
        let mut admin = facts(true, false, Some(SessionPhase::Setup), "GET", "/setup");
        admin.is_admin = true;
        assert!(allowed(&admin));
    }

    #[test]
    fn a_player_reaches_the_game_only_while_the_world_runs() {
        let running = Some(SessionPhase::Running);
        assert!(allowed(&facts(true, false, running, "GET", "/game")));
        assert!(allowed(&facts(true, false, running, "GET", "/join")));
        assert!(allowed(&facts(
            true,
            false,
            running,
            "GET",
            "/assets/ui/logo.webp"
        )));
        assert!(allowed(&facts(
            true,
            false,
            running,
            "GET",
            "/socket.io/?EIO=4"
        )));
        // Setup, its assets and Foundry's admin login are the holder's alone.
        assert_eq!(
            denial(&facts(true, false, running, "GET", "/setup")),
            Denial::SetupOnly
        );
        assert_eq!(
            denial(&facts(true, false, running, "GET", "/setup/worlds")),
            Denial::SetupOnly
        );
        assert_eq!(
            denial(&facts(true, false, running, "GET", "/auth")),
            Denial::SetupOnly
        );
        // A path that merely starts with the same letters is not Setup.
        assert!(allowed(&facts(true, false, running, "GET", "/setupx")));
        assert!(allowed(&facts(
            true,
            false,
            running,
            "GET",
            "/game/setup-guide"
        )));
    }

    #[test]
    fn setup_prefix_matching_is_exact_or_slash_bound() {
        let running = Some(SessionPhase::Running);
        assert_eq!(
            denial(&facts(true, false, running, "GET", "/setup/")),
            Denial::SetupOnly
        );
        assert_eq!(
            denial(&facts(true, false, running, "GET", "/setup?tab=1")),
            Denial::SetupOnly
        );
        assert_eq!(
            denial(&facts(true, false, running, "GET", "/setup/worlds?x/y")),
            Denial::SetupOnly
        );
        // Express routes case-insensitively and decodes percent escapes, so
        // the gate must match what Foundry will actually route.
        for path in ["/Setup", "/SETUP", "/%73etup", "/setup%2Fworlds"] {
            assert_eq!(
                denial(&facts(true, false, running, "GET", path)),
                Denial::SetupOnly,
                "{path}"
            );
        }
        assert!(allowed(&facts(true, false, running, "GET", "/%73etupx")));
    }

    #[test]
    fn the_ending_phase_and_a_closed_lease_admit_nobody() {
        for is_holder in [true, false] {
            let mut facts = facts(true, is_holder, Some(SessionPhase::Ending), "GET", "/game");
            facts.is_admin = true;
            assert_eq!(denial(&facts), Denial::NoSession);
        }
        assert_eq!(
            denial(&facts(true, false, None, "GET", "/game")),
            Denial::NoSession
        );
        assert_eq!(
            denial(&facts(true, true, None, "GET", "/setup")),
            Denial::SetupOnly
        );
        // A player inside another GM's setup phase has nothing to reach.
        assert_eq!(
            denial(&facts(
                true,
                false,
                Some(SessionPhase::Setup),
                "GET",
                "/game"
            )),
            Denial::NoSession
        );
        assert_eq!(
            denial(&facts(
                true,
                false,
                Some(SessionPhase::Setup),
                "GET",
                "/setup"
            )),
            Denial::SetupOnly
        );
    }

    #[test]
    fn query_strings_never_change_the_path_rules() {
        assert!(allowed(&facts(
            true,
            false,
            Some(SessionPhase::Running),
            "GET",
            "/game?foo=/setup"
        )));
        assert_eq!(
            denial(&facts(
                true,
                false,
                Some(SessionPhase::Running),
                "GET",
                "/setup?/game"
            )),
            Denial::SetupOnly
        );
        assert!(allowed(&facts(
            true,
            false,
            Some(SessionPhase::Running),
            "GET",
            "/join?action=join"
        )));
        assert_eq!(
            denial(&facts(
                true,
                false,
                Some(SessionPhase::Running),
                "POST",
                "/join?action=join"
            )),
            Denial::JoinBlocked
        );
    }

    #[test]
    fn path_normalization_matches_express_routing() {
        assert_eq!(normalized_path("/setup?x=1"), "/setup");
        assert_eq!(normalized_path("/Setup#frag"), "/setup");
        assert_eq!(normalized_path("/%73etup"), "/setup");
        assert_eq!(normalized_path("/%2573etup"), "/%73etup");
        assert_eq!(normalized_path(""), "");
    }

    #[test]
    fn an_unadopted_world_admits_only_the_holder_and_admins() {
        let running = Some(SessionPhase::Running);
        let mut player = facts(true, false, running, "GET", "/game");
        player.adopted = false;
        assert_eq!(denial(&player), Denial::NotAdopted);
        let mut holder = facts(true, true, running, "GET", "/setup");
        holder.adopted = false;
        assert!(allowed(&holder));
        let mut admin = facts(true, false, running, "GET", "/game");
        admin.adopted = false;
        admin.is_admin = true;
        assert!(allowed(&admin));
    }

    #[test]
    fn denial_codes_are_the_lobby_reasons() {
        assert_eq!(Denial::SignedOut.code(), "signed_out");
        assert_eq!(Denial::NoAccess.code(), "no_access");
        assert_eq!(Denial::NoSession.code(), "no_session");
        assert_eq!(Denial::SetupOnly.code(), "setup_only");
        assert_eq!(Denial::JoinBlocked.code(), "join_blocked");
        assert_eq!(Denial::NotAdopted.code(), "not_adopted");
    }

    #[test]
    fn the_login_return_path_is_query_encoded() {
        assert_eq!(query_value("/game?x=1&y=2"), "/game%3Fx%3D1%26y%3D2");
        assert_eq!(query_value("/game"), "/game");
    }

    #[test]
    fn the_setup_page_relays_strict_cookie_attributes() {
        let response = setup_page("abc123");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::SET_COOKIE],
            "session=abc123; Path=/; HttpOnly; Secure; SameSite=Strict"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
    }

    #[tokio::test]
    async fn the_setup_page_navigates_itself_to_setup() {
        let response = setup_page("abc123");
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("http-equiv=\"refresh\" content=\"0;url=/setup\""));
        assert!(body.contains("href=\"/setup\""));
        assert!(!body.contains("http://"));
        assert!(!body.contains("https://"));
    }

    #[test]
    fn the_setup_page_refuses_a_header_splitting_session_id() {
        let response = setup_page("abc\r\nSet-Cookie: evil=1");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
