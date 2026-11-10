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
    pub world_state: driver::WorldState,
    pub database: bool,
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

/// Dioxus console root.  The server resolves the caller's role and the live
/// game; this only renders what it was told, so nothing here is authorization.
#[component]
pub fn App() -> Element {
    let view = use_server_future(console_view)?;
    let view = match &*view.read() {
        Some(Ok(view)) => view.clone(),
        _ => ConsoleView::SignedOut,
    };
    rsx! {
        Lobby { view }
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
fn Lobby(view: ConsoleView) -> Element {
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
                            span { class: "lobby-account__name", "{name}" }
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

#[cfg(test)]
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
            world_state: driver::WorldState::WorldStopped,
            database: true,
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
        dioxus::ssr::render_element(rsx! {
            Lobby { view }
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
}
