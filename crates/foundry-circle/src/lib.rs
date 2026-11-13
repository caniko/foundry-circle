//! Foundry Circle's initial application boundary.
//!
//! The public API is intentionally small while the live Foundry contract is
//! being certified.  Foundry remains the authority for world documents; this
//! crate owns transport, readiness, authorization, and the typed driver seam.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use tartan_ui_core::{Feedback, FeedbackKind, Metric};
use tartan_ui_dioxus::{
    CardGrid, DashboardHeader, EmptyState, FeedbackBanner, MetricStrip, TagList,
};
use theme_broker::{Mode, role, stylesheet};

#[cfg(feature = "server")]
pub mod http;

pub mod driver;

/// What the signed-in account may do in the console.  Resolved from the OIDC
/// groups claim: admin outranks GM, and any other access-group member plays.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Admin,
    Gm,
    Player,
}

/// Foundry's public `GET /api/status` payload; every field is optional
/// because the shape varies with the Foundry version and world state.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct GameStatus {
    pub active: bool,
    pub version: Option<String>,
    pub world: Option<String>,
    pub system: Option<String>,
    pub system_version: Option<String>,
    pub users: Option<u64>,
    pub users_active: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum LiveGame {
    Live(GameStatus),
    /// Foundry answered but no world is launched.
    None,
    /// Foundry did not answer within the poll timeout.
    Unreachable,
}

impl LiveGame {
    pub fn from_status(status: GameStatus) -> Self {
        if status.active {
            Self::Live(status)
        } else {
            Self::None
        }
    }
}

/// Where the one open session lease stands.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionPhase {
    Setup,
    Running,
    Ending,
}

impl SessionPhase {
    /// The database spelling; the CHECK constraint uses the same three values.
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "setup" => Some(Self::Setup),
            "running" => Some(Self::Running),
            "ending" => Some(Self::Ending),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionHolder {
    /// Display name, never the OIDC subject.
    pub name: String,
}

/// One open lease, as the JSON API and the lobby report it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub phase: SessionPhase,
    pub holder: SessionHolder,
    pub world_id: Option<String>,
    pub started_at: String,
    pub is_holder: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConsoleData {
    pub name: String,
    pub email: Option<String>,
    pub role: Role,
    pub game: LiveGame,
    /// `<playBaseUrl>/join`, when configured.
    pub join_url: Option<String>,
    /// `<platformBaseUrl>/setup`; only resolved for GMs and admins.
    pub setup_url: Option<String>,
    /// `<playBaseUrl>/_circle/setup` while the viewer holds the lease in
    /// `setup`; the gate turns it into a Foundry admin session.
    pub play_setup_url: Option<String>,
    pub world_state: driver::WorldState,
    pub database: bool,
    /// The open session lease, if one is held.
    pub session: Option<SessionView>,
    /// A GM or admin may open the one session now.
    pub can_start_session: bool,
    /// A world runs with no lease; starting is refused until it stops.
    pub unmanaged_world: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum ConsoleView {
    SignedOut,
    /// Authenticated but not in the access group.
    Denied {
        name: String,
    },
    Console(Box<ConsoleData>),
}

#[server]
async fn console_view() -> Result<ConsoleView, ServerFnError> {
    Ok(http::console_view().await)
}

/// The lobby buttons run the same lease operations as the JSON routes; the
/// optional code is what the card shows when the action is refused.
#[server]
async fn start_session() -> Result<Option<String>, ServerFnError> {
    Ok(http::console_start().await)
}

#[server]
async fn return_to_setup(id: String) -> Result<Option<String>, ServerFnError> {
    Ok(http::console_setup(&id).await)
}

#[server]
async fn end_session(id: String) -> Result<Option<String>, ServerFnError> {
    Ok(http::console_stop(&id).await)
}

/// Dioxus console root.  The server resolves the caller's role and the live
/// game; this only renders what it was told, so nothing here is authorization.
#[component]
pub fn App() -> Element {
    let view = use_server_future(console_view)?;
    let refresh = EventHandler::new(move |()| {
        // A copy of the resource: restarting reloads the server view.
        let mut view = view;
        view.restart();
    });
    let view = match &*view.read() {
        Some(Ok(view)) => view.clone(),
        _ => ConsoleView::SignedOut,
    };
    rsx! {
        Lobby { view, refresh }
    }
}

/// Canonical Blackbox dark Orange accent, read from the theme broker's generated
/// snapshot so this repository never copies palette values.
fn orange() -> &'static str {
    role(Mode::Dark, "syntax", "constant")
        .expect("theme-broker Blackbox dark must expose syntax.constant")
}

/// The readable broker state for the admin strip.
fn broker_label(state: driver::WorldState) -> &'static str {
    match state {
        driver::WorldState::Starting => "Starting",
        driver::WorldState::WorldStopped => "World stopped",
        driver::WorldState::Ready => "Ready",
        driver::WorldState::Error => "Error",
    }
}

/// Lobby-only bridge and layout. Palette values stay in the producer
/// stylesheet; this file contains no copied palette values.
pub const STYLES: &str = r##"
:where(html, body) {
  margin: 0;
  padding: 0;
}

.lobby {
  /* The pinned tartan stylesheet still uses literal --tartan-* values, so the
     broker aliases are rebound once at this scope instead of per component. */
  --tb-ui-accent: var(--lobby-orange);
  --tb-ui-focus: var(--lobby-orange);
  --tb-ui-link: var(--lobby-orange);
  --tartan-paper: var(--tb-ui-background);
  --tartan-ink: var(--tb-ui-foreground);
  --tartan-muted: var(--tb-ui-foreground-muted);
  --tartan-line: var(--tb-ui-border);
  --tartan-accent: var(--lobby-orange);
  --tartan-accent-soft: var(--tb-ui-selection-background);
  --tartan-on-accent: var(--tb-ui-background);
  --tartan-surface: var(--tb-ui-surface);
  --tartan-danger: var(--tb-status-error);
  --tartan-warning: var(--tb-status-warning);
  --tartan-success: var(--tb-status-success);
  --tartan-info: var(--tb-status-info);
  --tartan-focus: var(--lobby-orange);
  min-height: 100vh;
  background: var(--tb-ui-background);
  color: var(--tb-ui-foreground);
  font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  font-size: 14px;
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
}

/* :where keeps the reset below every single-class rule it could otherwise
   override. */
:where(.lobby) :where(*, *::before, *::after) {
  box-sizing: border-box;
}

:where(.lobby) button {
  border: 0;
  font: inherit;
  cursor: pointer;
}

/* The pinned tartan rulesets hardcode white surfaces and white button text,
   which the dark palette has to override. */
.lobby .tartan-card {
  background: var(--tb-ui-background-alt);
}

.lobby .tartan-button {
  color: var(--tb-ui-background);
}

.lobby .tartan-feedback {
  background: var(--tb-ui-surface);
  color: var(--tb-ui-foreground);
}

.lobby .tartan-feedback--error {
  border-color: var(--tb-status-error);
}

.lobby .tartan-card-grid > * {
  min-width: 0;
}

.lobby .tartan-tag,
.lobby .tartan-metric strong,
.lobby .tartan-dashboard-header h1,
.lobby .tartan-resource-card h2 {
  overflow-wrap: anywhere;
}

.lobby-skip {
  position: absolute;
  top: 0;
  left: 0;
  transform: translateY(-150%);
  margin: .5rem;
  border: 1px solid var(--lobby-orange);
  border-radius: .25rem;
  background: var(--tb-ui-background);
  color: var(--tb-ui-foreground);
  padding: .5rem .75rem;
  text-decoration: none;
}

/* Focused, the link joins the flow above the header instead of covering the
   brand. */
.lobby-skip:focus {
  position: static;
  transform: none;
}

.lobby-header {
  border-bottom: 1px solid var(--tb-ui-border);
}

.lobby-header__inner {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: .5rem 1rem;
  width: min(100% - 1.5rem, 76rem);
  min-height: 3.5rem;
  margin: 0 auto;
}

.lobby-brand {
  color: var(--tb-ui-foreground);
  font-weight: 700;
  text-decoration: none;
}

.lobby-account {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: .5rem .75rem;
  min-width: 0;
}

.lobby-account__name {
  color: var(--tb-ui-foreground-muted);
  overflow-wrap: anywhere;
}

.lobby-main {
  width: min(100% - 1.5rem, 76rem);
  margin: 0 auto;
  padding: 2rem 0 3rem;
}

.lobby :where(a, button):focus-visible {
  outline: 2px solid var(--lobby-orange);
  outline-offset: 2px;
}

@media (max-width: 30rem) {
  .lobby-header__inner {
    align-items: flex-start;
    flex-direction: column;
    padding-block: .75rem;
  }
}

@media (prefers-reduced-motion: reduce) {
  .lobby :where(*, *::before, *::after) {
    animation-duration: .01ms;
    animation-iteration-count: 1;
    scroll-behavior: auto;
    transition-duration: .01ms;
  }
}
"##;

/// Pure lobby rendering. `App` resolves the server view and delegates here so
/// every view is renderable without a server.
#[component]
fn Lobby(view: ConsoleView, refresh: EventHandler<()>) -> Element {
    let accent = orange();
    let account = match &view {
        ConsoleView::SignedOut => None,
        ConsoleView::Denied { name } => Some(name.clone()),
        ConsoleView::Console(data) => Some(data.name.clone()),
    };
    let content = match view {
        ConsoleView::SignedOut => rsx! {
            DashboardHeader {
                eyebrow: "Foundry Circle".to_string(),
                heading: "Sign in to play".to_string(),
                description: "Use your Rauthy account to see the live game.".to_string(),
            }
        },
        ConsoleView::Denied { name } => rsx! {
            FeedbackBanner {
                feedback: Feedback {
                    kind: FeedbackKind::Error,
                    message: format!("Signed in as {name}, but this account has no Foundry Circle access."),
                },
            }
            EmptyState {
                heading: "Access not provisioned".to_string(),
                message: "Ask your GM to add you to the foundry-circle-users group, then sign in again.".to_string(),
            }
        },
        ConsoleView::Console(data) => {
            let (eyebrow, heading, description) = match data.role {
                Role::Admin => (
                    "Admin console",
                    "World operator console",
                    "A typed broker for the active Foundry world.",
                ),
                Role::Gm => (
                    "GM lobby",
                    "Run the table",
                    "Create worlds in Foundry setup, then invite your players.",
                ),
                Role::Player => (
                    "Player lobby",
                    "Tonight at the table",
                    "Join the live game when you are ready.",
                ),
            };
            rsx! {
                // One-line soft refresh while the world starts.
                document::Meta { http_equiv: "refresh", content: "60" }
                DashboardHeader {
                    eyebrow: eyebrow.to_string(),
                    heading: heading.to_string(),
                    description: description.to_string(),
                }
                if data.role == Role::Admin {
                    MetricStrip {
                        metrics: vec![
                            Metric { label: "Broker state".into(), value: broker_label(data.world_state).into(), description: None },
                            Metric { label: "Database".into(), value: if data.database { "Ready" } else { "Down" }.into(), description: None },
                            Metric { label: "Your role".into(), value: "Admin".into(), description: None },
                        ],
                    }
                }
                CardGrid {
                    GameCard { data: data.as_ref().clone() }
                    SessionCard { data: data.as_ref().clone(), refresh }
                    if data.role != Role::Player {
                        InviteCard { join_url: data.join_url.clone() }
                    }
                }
            }
        }
    };
    rsx! {
        document::Title { "Foundry Circle" }
        document::Stylesheet { href: tartan_ui_dioxus::SHARED_STYLES }
        document::Style { {stylesheet()} }
        document::Style { {STYLES} }
        div { class: "lobby", "data-theme": "dark", style: "--lobby-orange: {accent}",
            a { class: "lobby-skip", href: "#main", "Skip to content" }
            header { class: "lobby-header",
                div { class: "lobby-header__inner",
                    a { class: "lobby-brand", href: "/api/console", "Foundry Circle" }
                    div { class: "lobby-account",
                        if let Some(name) = account {
                            span { class: "lobby-account__name", {name} }
                            form { class: "lobby-signout", method: "post", action: "/api/auth/logout",
                                button { r#type: "submit", class: "tartan-button", "Sign out" }
                            }
                        } else {
                            a { class: "tartan-button", href: "/api/auth/login", "Sign in" }
                        }
                    }
                }
            }
            main { id: "main", class: "lobby-main", tabindex: "-1", {content} }
        }
    }
}

#[component]
fn GameCard(data: ConsoleData) -> Element {
    match data.game {
        LiveGame::Live(game) => {
            let title = game
                .world
                .clone()
                .unwrap_or_else(|| "Untitled world".into());
            let subtitle = [game.system.clone(), game.system_version.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            let mut tags = Vec::new();
            tags.extend(game.system.clone());
            tags.extend(game.version.as_ref().map(|v| format!("v{v}")));
            let mut metrics = Vec::new();
            if let Some(active) = game.users_active {
                metrics.push(Metric {
                    label: "Online now".into(),
                    value: active.to_string(),
                    description: None,
                });
            }
            if let Some(users) = game.users {
                metrics.push(Metric {
                    label: "Players".into(),
                    value: users.to_string(),
                    description: None,
                });
            }
            rsx! {
                article { class: "tartan-card tartan-resource-card",
                    div { class: "tartan-resource-card__body",
                        p { class: "tartan-eyebrow", "Live now" }
                        h2 { {title} }
                        p { class: "tartan-muted", {subtitle} }
                        TagList { tags }
                        if !metrics.is_empty() {
                            MetricStrip { metrics }
                        }
                    }
                    div { class: "tartan-metrics",
                        if let Some(join) = data.join_url {
                            a { class: "tartan-button", href: join, "Join game" }
                        }
                        if let Some(setup) = data.setup_url {
                            a { class: "tartan-button", href: setup, "Open Setup" }
                        }
                    }
                }
            }
        }
        other => {
            let message = match other {
                LiveGame::Unreachable => "Foundry is not answering. Check back in a moment.",
                _ if data.role == Role::Player => {
                    "Your GM has not launched a world yet. Check back soon."
                }
                _ => "Launch a world from Setup, then share the join link with your players.",
            };
            rsx! {
                div {
                    EmptyState { heading: "No game is live right now".to_string(), message: message.to_string() }
                    if let Some(setup) = data.setup_url {
                        a { class: "tartan-button", href: setup, "Open Setup" }
                    }
                }
            }
        }
    }
}

/// One lobby button press, mapped to the matching server function.
enum SessionCall {
    Start,
    Setup(String),
    Stop(String),
}

#[component]
fn SessionCard(data: ConsoleData, refresh: EventHandler<()>) -> Element {
    let error = use_signal(|| None::<String>);
    let body = match (&data.session, data.role) {
        (None, _) if data.unmanaged_world => rsx! {
            p { class: "tartan-muted",
                "A world is running outside any session. An operator must stop it in Foundry before a session can start."
            }
        },
        (None, Role::Player) => rsx! {
            p { class: "tartan-muted", "No session is open right now. Check back soon." }
        },
        (None, _) => rsx! {
            p { class: "tartan-muted",
                "A session reserves the one GM slot and opens Foundry Setup for the holder."
            }
            button {
                class: "tartan-button",
                r#type: "button",
                onclick: move |_| apply(SessionCall::Start, error, refresh),
                "Start session"
            }
        },
        (Some(session), _) => {
            let is_holder = session.is_holder;
            let is_admin = data.role == Role::Admin;
            let phase = session.phase;
            let holder = session.holder.name.clone();
            let setup_id = session.id.clone();
            let stop_id = session.id.clone();
            let play_setup = data.play_setup_url.clone();
            let join_url = data.join_url.clone();
            // The live-game card already links to the join URL when Foundry
            // reports a running world; this is the lease-driven fallback.
            let session_join =
                phase == SessionPhase::Running && !matches!(&data.game, LiveGame::Live(_));
            rsx! {
                p { class: "tartan-muted", "Session held by {holder}." }
                p { class: "tartan-tag", {phase_label(phase)} }
                div { class: "tartan-metrics",
                    if let Some(setup) = play_setup {
                        a { class: "tartan-button", href: setup, "Open Foundry Setup" }
                    }
                    if session_join {
                        if let Some(join) = join_url {
                            a { class: "tartan-button", href: join, "Join game" }
                        }
                    }
                    if is_holder && phase == SessionPhase::Running {
                        button {
                            class: "tartan-button",
                            r#type: "button",
                            onclick: move |_| apply(SessionCall::Setup(setup_id.clone()), error, refresh),
                            "Return to Setup"
                        }
                    }
                    if (is_holder || is_admin) && phase != SessionPhase::Ending {
                        button {
                            class: "tartan-button",
                            r#type: "button",
                            onclick: move |_| apply(SessionCall::Stop(stop_id.clone()), error, refresh),
                            "End session"
                        }
                    }
                }
            }
        }
    };
    rsx! {
        article { class: "tartan-card",
            p { class: "tartan-eyebrow", "Game session" }
            h2 { "One table at a time" }
            if let Some(message) = error() {
                FeedbackBanner { feedback: Feedback { kind: FeedbackKind::Error, message } }
            }
            {body}
        }
    }
}

/// Buttons share one path: run the server function, reload the view on
/// success, or show why it was refused.
fn apply(call: SessionCall, error: Signal<Option<String>>, refresh: EventHandler<()>) {
    let mut error = error;
    spawn(async move {
        let result = match call {
            SessionCall::Start => start_session().await,
            SessionCall::Setup(id) => return_to_setup(id).await,
            SessionCall::Stop(id) => end_session(id).await,
        };
        let code = match result {
            Ok(None) => {
                refresh.call(());
                return;
            }
            Ok(Some(code)) => code,
            Err(_) => "unavailable".into(),
        };
        error.set(Some(session_error_message(&code).to_owned()));
    });
}

fn phase_label(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::Setup => "The GM is in Foundry Setup.",
        SessionPhase::Running => "The world is running.",
        SessionPhase::Ending => "The session is ending.",
    }
}

fn session_error_message(code: &str) -> &'static str {
    match code {
        "session_in_progress" => "Another GM already holds the session.",
        "unmanaged_world" => {
            "A world is already running outside a session; an operator must stop it in Foundry first."
        }
        "permission_denied" => "Only the session holder or an admin can do that.",
        "session_not_found" => "That session has already ended.",
        "session_phase_conflict" => "The session is not in a state that allows that.",
        "foundry_unavailable" => "Foundry did not answer; try again in a moment.",
        "lease_not_configured" => "Session controls are not configured on this server.",
        _ => "That did not work; try again in a moment.",
    }
}

#[component]
fn InviteCard(join_url: Option<String>) -> Element {
    rsx! {
        article { class: "tartan-card",
            p { class: "tartan-eyebrow", "Invite players" }
            h2 { "Share the join link" }
            if let Some(url) = join_url {
                // ponytail: select-all on click, no clipboard JS; add a Copy button if the shared UI grows one.
                p { class: "tartan-tag", style: "user-select: all", {url} }
            }
            p { class: "tartan-muted",
                "Create each player's Foundry user inside your world before they join. Their Foundry user is separate from their sign-in."
            }
        }
    }
}

// The SSR renderer ships with the server feature only.
#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    fn data(role: Role, game: LiveGame) -> ConsoleData {
        ConsoleData {
            name: "Robin".into(),
            email: Some("robin@example.test".into()),
            role,
            game,
            join_url: Some("https://play.example.test/join".into()),
            setup_url: (role != Role::Player).then(|| "https://play.example.test/setup".into()),
            play_setup_url: None,
            world_state: driver::WorldState::WorldStopped,
            database: true,
            session: None,
            can_start_session: role != Role::Player,
            unmanaged_world: false,
        }
    }

    fn held(role: Role, phase: SessionPhase, is_holder: bool) -> ConsoleData {
        ConsoleData {
            session: Some(SessionView {
                id: "0b6f4c2e-0000-4000-8000-000000000000".into(),
                phase,
                holder: SessionHolder {
                    name: "Piotr".into(),
                },
                world_id: Some("canary".into()),
                started_at: "2026-11-12T18:00:00+00:00".into(),
                is_holder,
            }),
            can_start_session: false,
            ..data(role, LiveGame::None)
        }
    }

    fn live() -> LiveGame {
        LiveGame::Live(GameStatus {
            active: true,
            version: Some("13.351".into()),
            world: Some("Daggerheart".into()),
            system: Some("Daggerheart".into()),
            system_version: Some("1.6.4".into()),
            users: Some(4),
            users_active: Some(2),
        })
    }

    fn render(view: ConsoleView) -> String {
        // `EventHandler::new` needs a Dioxus runtime, which a component has.
        #[component]
        fn Harness(view: ConsoleView) -> Element {
            let refresh = EventHandler::new(|()| ());
            rsx! {
                Lobby { view, refresh }
            }
        }
        dioxus::ssr::render_element(rsx! {
            Harness { view }
        })
    }

    #[test]
    fn signed_out_view_offers_sign_in_and_a_main_landmark() {
        let html = render(ConsoleView::SignedOut);
        assert!(html.contains("href=\"/api/auth/login\""));
        assert!(html.contains("id=\"main\""));
        assert!(html.contains("href=\"#main\""));
        assert!(!html.contains("/api/auth/logout"));
    }

    #[test]
    fn denied_view_offers_sign_out() {
        let html = render(ConsoleView::Denied {
            name: "Robin".into(),
        });
        assert!(html.contains("action=\"/api/auth/logout\""));
        assert!(html.contains("Access not provisioned"));
    }

    #[test]
    fn player_view_joins_the_game_without_setup() {
        let html = render(ConsoleView::Console(Box::new(data(Role::Player, live()))));
        assert!(html.contains("Join game"));
        assert!(!html.contains("Open Setup"));
        assert!(html.contains("action=\"/api/auth/logout\""));
    }

    #[test]
    fn gm_view_shows_the_invite_card() {
        let html = render(ConsoleView::Console(Box::new(data(
            Role::Gm,
            LiveGame::None,
        ))));
        assert!(html.contains("Share the join link"));
        assert!(html.contains("Launch a world from Setup"));
    }

    #[test]
    fn admin_view_reads_the_broker_state() {
        let html = render(ConsoleView::Console(Box::new(data(
            Role::Admin,
            LiveGame::None,
        ))));
        assert!(html.contains("Broker state"));
        assert!(html.contains("World stopped"));
        assert!(!html.contains("WorldStopped"));
    }

    #[test]
    fn a_gm_without_a_session_can_start_one() {
        let html = render(ConsoleView::Console(Box::new(data(
            Role::Gm,
            LiveGame::None,
        ))));
        assert!(html.contains("Start session"));
    }

    #[test]
    fn a_player_cannot_start_a_session() {
        let html = render(ConsoleView::Console(Box::new(data(
            Role::Player,
            LiveGame::None,
        ))));
        assert!(!html.contains("Start session"));
    }

    #[test]
    fn an_unmanaged_world_blocks_starting() {
        let mut console = data(Role::Gm, live());
        console.can_start_session = false;
        console.unmanaged_world = true;
        let html = render(ConsoleView::Console(Box::new(console)));
        assert!(!html.contains("Start session"));
        assert!(html.contains("outside any session"));
    }

    #[test]
    fn the_holder_can_return_to_setup_and_end_a_running_session() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Gm,
            SessionPhase::Running,
            true,
        ))));
        assert!(html.contains("Session held by Piotr"));
        assert!(html.contains("Return to Setup"));
        assert!(html.contains("End session"));
    }

    #[test]
    fn the_holder_in_setup_can_only_end_the_session() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Gm,
            SessionPhase::Setup,
            true,
        ))));
        assert!(!html.contains("Return to Setup"));
        assert!(html.contains("End session"));
    }

    #[test]
    fn a_non_holder_gm_only_sees_who_holds_the_session() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Gm,
            SessionPhase::Running,
            false,
        ))));
        assert!(html.contains("Session held by Piotr"));
        assert!(!html.contains("Return to Setup"));
        assert!(!html.contains("End session"));
    }

    #[test]
    fn an_admin_may_end_a_session_they_do_not_hold() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Admin,
            SessionPhase::Running,
            false,
        ))));
        assert!(html.contains("End session"));
        assert!(!html.contains("Return to Setup"));
    }

    #[test]
    fn the_holder_in_setup_gets_the_play_host_setup_link() {
        let mut console = held(Role::Gm, SessionPhase::Setup, true);
        console.play_setup_url = Some("https://play.example.test/_circle/setup".into());
        let html = render(ConsoleView::Console(Box::new(console)));
        assert!(html.contains("Open Foundry Setup"));
        assert!(html.contains("https://play.example.test/_circle/setup"));
    }

    #[test]
    fn a_non_holder_in_setup_gets_no_setup_link() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Gm,
            SessionPhase::Setup,
            false,
        ))));
        assert!(!html.contains("Open Foundry Setup"));
    }

    #[test]
    fn a_running_session_offers_join_to_everyone() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Player,
            SessionPhase::Running,
            false,
        ))));
        assert!(html.contains("Join game"));
        assert!(html.contains("https://play.example.test/join"));
        // A live world already offers the same link on the game card; the
        // session card must not duplicate it.
        let console = ConsoleData {
            game: live(),
            ..held(Role::Player, SessionPhase::Running, false)
        };
        let html = render(ConsoleView::Console(Box::new(console)));
        assert_eq!(html.matches("Join game").count(), 1);
    }

    #[test]
    fn a_player_sees_the_holder_without_controls() {
        let html = render(ConsoleView::Console(Box::new(held(
            Role::Player,
            SessionPhase::Running,
            false,
        ))));
        assert!(html.contains("Session held by Piotr"));
        assert!(!html.contains("End session"));
    }
}
