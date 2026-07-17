//! Foundry Circle's initial application boundary.
//!
//! The public API is intentionally small while the live Foundry contract is
//! being certified.  Foundry remains the authority for world documents; this
//! crate owns transport, readiness, authorization, and the typed driver seam.

use dioxus::prelude::*;
use tartan_ui_core::Identity;
use tartan_ui_dioxus::{AppShell, DashboardHeader, EmptyState, MetricStrip};

#[cfg(feature = "server")]
pub mod http;

pub mod driver {
    use serde::{Deserialize, Serialize};

    /// The lifecycle state exposed by the readiness endpoint and console.
    #[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum WorldState {
        Starting,
        WorldStopped,
        Ready,
        Error,
    }

    /// The narrow seam used by the API layer.  Browser/CDP integration will
    /// implement this trait after a disposable world is available.
    pub trait FoundryDriver: Send + Sync {
        fn world_state(&self) -> WorldState;
    }

    /// Deterministic driver used by unit tests and the initial VM test.
    #[derive(Debug, Clone, Copy)]
    pub struct FakeDriver {
        state: WorldState,
    }

    impl FakeDriver {
        pub const fn new(state: WorldState) -> Self {
            Self { state }
        }
    }

    impl FoundryDriver for FakeDriver {
        fn world_state(&self) -> WorldState {
            self.state
        }
    }
}

/// Dioxus operator console root.  Product-specific world views will be added
/// behind the same application/service boundary as `/api/v1`.
#[component]
pub fn App() -> Element {
    // The browser identity is supplied by the authenticated `/api/v1/me`
    // response.  Never render a fabricated human identity in the shell.
    let identity: Option<Identity> = None;

    rsx! {
        AppShell { title: "Foundry Circle".to_string(), identity,
            DashboardHeader {
                eyebrow: "Foundry Circle".to_string(),
                heading: "World operator console".to_string(),
                description: "A typed broker for the active Foundry world.".to_string(),
            }
            MetricStrip { metrics: vec![] }
            EmptyState {
                heading: "World contract pending".to_string(),
                message: "Connect a certified Foundry world to enable document and command views.".to_string(),
            }
        }
    }
}
