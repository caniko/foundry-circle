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

    #[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    pub struct WorldSnapshot {
        pub state: WorldState,
        pub id: Option<String>,
        pub epoch: u64,
        pub foundry_version: Option<String>,
        pub system_id: Option<String>,
        pub system_version: Option<String>,
        pub is_gm: bool,
    }

    impl WorldSnapshot {
        pub const fn starting() -> Self {
            Self {
                state: WorldState::Starting,
                id: None,
                epoch: 0,
                foundry_version: None,
                system_id: None,
                system_version: None,
                is_gm: false,
            }
        }
    }

    /// The narrow seam used by the API layer.  Foundry Circle only exposes
    /// metadata gathered by the adapter; Foundry remains document-authoritative.
    pub trait FoundryDriver: Send + Sync {
        fn snapshot(&self) -> WorldSnapshot;

        fn world_state(&self) -> WorldState {
            self.snapshot().state
        }
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
        fn snapshot(&self) -> WorldSnapshot {
            WorldSnapshot {
                state: self.state,
                ..WorldSnapshot::starting()
            }
        }
    }

    #[cfg(feature = "server")]
    mod live {
        use super::{FoundryDriver, WorldSnapshot, WorldState};
        use chromiumoxide::{
            browser::{Browser, BrowserConfig},
            page::Page,
        };
        use futures_util::StreamExt;
        use serde::Deserialize;
        use std::{
            env,
            path::PathBuf,
            sync::{Arc, RwLock},
            time::{Duration, Instant},
        };
        use tokio::time::sleep;
        use uuid::Uuid;

        const POLL_INTERVAL: Duration = Duration::from_secs(2);
        const SESSION_TIMEOUT: Duration = Duration::from_secs(60);

        #[derive(Debug, thiserror::Error)]
        pub enum ConfigError {
            #[error("{0} is not set")]
            Missing(&'static str),
            #[error("{0} is empty")]
            Empty(&'static str),
        }

        #[derive(Clone, Debug)]
        pub struct DriverConfig {
            pub base_url: String,
            pub api_user: String,
            pub password_file: PathBuf,
            pub world_id: String,
            pub foundry_version: String,
            pub system_id: String,
            pub system_version: String,
            pub chromium: PathBuf,
            pub profile_dir: PathBuf,
        }

        impl DriverConfig {
            pub fn from_env() -> Result<Self, ConfigError> {
                let required = |name| {
                    let value = env::var(name).map_err(|_| ConfigError::Missing(name))?;
                    if value.trim().is_empty() {
                        return Err(ConfigError::Empty(name));
                    }
                    Ok(value)
                };
                Ok(Self {
                    base_url: required("FOUNDRY_WORLD_BASE_URL")?
                        .trim_end_matches('/')
                        .into(),
                    api_user: required("FOUNDRY_WORLD_API_USER")?,
                    password_file: required("FOUNDRY_WORLD_API_PASSWORD_FILE")?.into(),
                    world_id: required("FOUNDRY_WORLD_ID")?,
                    foundry_version: required("FOUNDRY_EXPECTED_FOUNDRY_VERSION")?,
                    system_id: required("FOUNDRY_EXPECTED_SYSTEM_ID")?,
                    system_version: required("FOUNDRY_EXPECTED_SYSTEM_VERSION")?,
                    chromium: required("FOUNDRY_CHROMIUM")?.into(),
                    profile_dir: env::var_os("FOUNDRY_BROWSER_PROFILE_DIR")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| "/run/foundry-circle/browser".into()),
                })
            }
        }

        #[derive(Debug, thiserror::Error)]
        enum Failure {
            #[error("world is stopped")]
            WorldStopped,
            #[error("browser session failed")]
            Browser,
            #[error("world authentication failed")]
            Authentication,
            #[error("world contract mismatch")]
            Contract,
        }

        #[derive(Debug, Deserialize)]
        struct JoinPageState {
            has_form: bool,
            no_world: bool,
            game_ready: bool,
        }

        #[derive(Debug, Deserialize)]
        struct GameState {
            ready: bool,
            world_id: Option<String>,
            foundry_version: Option<String>,
            system_id: Option<String>,
            system_version: Option<String>,
            is_gm: bool,
            no_canvas: bool,
            no_world: bool,
        }

        pub struct LiveFoundryDriver {
            snapshot: Arc<RwLock<WorldSnapshot>>,
        }

        impl LiveFoundryDriver {
            pub fn start(config: DriverConfig) -> Arc<Self> {
                let driver = Arc::new(Self {
                    snapshot: Arc::new(RwLock::new(WorldSnapshot::starting())),
                });
                let task_driver = Arc::clone(&driver);
                tokio::spawn(async move { supervise(task_driver, config).await });
                driver
            }

            fn set_snapshot(&self, snapshot: WorldSnapshot) {
                *self.snapshot.write().expect("Foundry driver lock poisoned") = snapshot;
            }
        }

        impl FoundryDriver for LiveFoundryDriver {
            fn snapshot(&self) -> WorldSnapshot {
                self.snapshot
                    .read()
                    .expect("Foundry driver lock poisoned")
                    .clone()
            }
        }

        async fn supervise(driver: Arc<LiveFoundryDriver>, config: DriverConfig) {
            let mut backoff = 5;
            loop {
                driver.set_snapshot(WorldSnapshot::starting());
                match run_session(&driver, &config).await {
                    Err(Failure::WorldStopped) => {
                        driver.set_snapshot(WorldSnapshot {
                            state: WorldState::WorldStopped,
                            ..WorldSnapshot::starting()
                        });
                        backoff = 15;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Foundry browser session ended");
                        driver.set_snapshot(WorldSnapshot {
                            state: WorldState::Error,
                            ..WorldSnapshot::starting()
                        });
                    }
                    Ok(()) => backoff = 5,
                }
                sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }

        async fn run_session(
            driver: &LiveFoundryDriver,
            config: &DriverConfig,
        ) -> Result<(), Failure> {
            let password = std::fs::read_to_string(&config.password_file)
                .map_err(|_| Failure::Authentication)?
                .trim()
                .to_owned();
            if password.is_empty() {
                return Err(Failure::Authentication);
            }

            std::fs::create_dir_all(&config.profile_dir).map_err(|_| Failure::Browser)?;
            let profile = config.profile_dir.join(Uuid::new_v4().to_string());
            std::fs::create_dir_all(&profile).map_err(|_| Failure::Browser)?;
            let browser_config = BrowserConfig::builder()
                .chrome_executable(&config.chromium)
                .no_sandbox()
                .arg("--disable-dev-shm-usage")
                .arg("--disable-gpu")
                .user_data_dir(&profile)
                .build()
                .map_err(|_| Failure::Browser)?;
            let (mut browser, mut handler) = Browser::launch(browser_config)
                .await
                .map_err(|_| Failure::Browser)?;
            let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
            let result = session(driver, config, &browser, &password).await;
            let _ = browser.close().await;
            handler_task.abort();
            let _ = std::fs::remove_dir_all(profile);
            result
        }

        async fn session(
            driver: &LiveFoundryDriver,
            config: &DriverConfig,
            browser: &Browser,
            password: &str,
        ) -> Result<(), Failure> {
            let page = browser
                .new_page(format!("{}/join", config.base_url))
                .await
                .map_err(|_| Failure::Browser)?;
            wait_for_join(&page).await?;
            select_user(&page, &config.api_user).await?;
            page.find_element("input[name='password']")
                .await
                .map_err(|_| Failure::Authentication)?
                .click()
                .await
                .map_err(|_| Failure::Authentication)?
                .type_str(password)
                .await
                .map_err(|_| Failure::Authentication)?
                .press_key("Enter")
                .await
                .map_err(|_| Failure::Authentication)?;

            let deadline = Instant::now() + SESSION_TIMEOUT;
            let mut reloaded_for_no_canvas = false;
            while Instant::now() < deadline {
                let state = evaluate::<GameState>(&page, GAME_STATE).await?;
                if state.no_world {
                    return Err(Failure::WorldStopped);
                }
                if state.ready {
                    let snapshot = validate_state(&state, config)?;
                    if !state.no_canvas && !reloaded_for_no_canvas {
                        evaluate::<bool>(&page, SET_NO_CANVAS).await?;
                        page.reload().await.map_err(|_| Failure::Browser)?;
                        reloaded_for_no_canvas = true;
                        continue;
                    }
                    driver.set_snapshot(snapshot);
                    loop {
                        sleep(Duration::from_secs(10)).await;
                        let state = evaluate::<GameState>(&page, GAME_STATE).await?;
                        if state.no_world {
                            return Err(Failure::WorldStopped);
                        }
                        if !state.ready {
                            return Err(Failure::Browser);
                        }
                        driver.set_snapshot(validate_state(&state, config)?);
                    }
                }
                sleep(POLL_INTERVAL).await;
            }
            Err(Failure::Authentication)
        }

        async fn wait_for_join(page: &Page) -> Result<(), Failure> {
            let deadline = Instant::now() + SESSION_TIMEOUT;
            while Instant::now() < deadline {
                let state = evaluate::<JoinPageState>(page, JOIN_PAGE_STATE).await?;
                if state.no_world {
                    return Err(Failure::WorldStopped);
                }
                if state.game_ready {
                    return Ok(());
                }
                if state.has_form {
                    return Ok(());
                }
                sleep(POLL_INTERVAL).await;
            }
            Err(Failure::Authentication)
        }

        async fn select_user(page: &Page, expected: &str) -> Result<(), Failure> {
            let expected = serde_json::to_string(expected).map_err(|_| Failure::Authentication)?;
            let script = format!(
                "() => {{ const expected = {expected}; const select = document.querySelector(\"select[name='userid']\"); if (!select) return false; const option = [...select.options].find(item => item.textContent.trim() === expected || item.value === expected); if (!option) return false; select.value = option.value; select.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }}"
            );
            if evaluate::<bool>(page, &script).await? {
                Ok(())
            } else {
                Err(Failure::Authentication)
            }
        }

        fn validate_state(
            state: &GameState,
            config: &DriverConfig,
        ) -> Result<WorldSnapshot, Failure> {
            if !state.is_gm
                || state.world_id.as_deref() != Some(config.world_id.as_str())
                || state.foundry_version.as_deref() != Some(config.foundry_version.as_str())
                || state.system_id.as_deref() != Some(config.system_id.as_str())
                || state.system_version.as_deref() != Some(config.system_version.as_str())
            {
                return Err(Failure::Contract);
            }
            Ok(WorldSnapshot {
                state: WorldState::Ready,
                id: state.world_id.clone(),
                epoch: 0,
                foundry_version: state.foundry_version.clone(),
                system_id: state.system_id.clone(),
                system_version: state.system_version.clone(),
                is_gm: state.is_gm,
            })
        }

        async fn evaluate<T: serde::de::DeserializeOwned>(
            page: &Page,
            script: &str,
        ) -> Result<T, Failure> {
            page.evaluate(script)
                .await
                .map_err(|_| Failure::Browser)?
                .into_value()
                .map_err(|_| Failure::Browser)
        }

        const JOIN_PAGE_STATE: &str = r#"() => {
            const body = (document.body?.innerText || '').toLowerCase();
            return {
                has_form: !!document.querySelector("select[name='userid']") && !!document.querySelector("input[name='password']"),
                no_world: body.includes('no active game session'),
                game_ready: !!window.game?.ready,
            };
        }"#;

        const GAME_STATE: &str = r#"() => {
            const body = (document.body?.innerText || '').toLowerCase();
            return {
                ready: !!window.game?.ready,
                world_id: window.game?.world?.id ?? null,
                foundry_version: window.game?.version ?? null,
                system_id: window.game?.system?.id ?? null,
                system_version: window.game?.system?.version ?? null,
                is_gm: !!window.game?.user?.isGM,
                no_canvas: window.game?.settings?.get?.('core', 'noCanvas') === true,
                no_world: body.includes('no active game session'),
            };
        }"#;

        const SET_NO_CANVAS: &str = r#"async function() {
            if (!window.game?.settings?.set) return false;
            await window.game.settings.set('core', 'noCanvas', true);
            return true;
        }"#;

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn contract_validation_rejects_non_gm_and_mismatches() {
                let state = GameState {
                    ready: true,
                    world_id: Some("canary".into()),
                    foundry_version: Some("13.351".into()),
                    system_id: Some("daggerheart".into()),
                    system_version: Some("1.6.4".into()),
                    is_gm: false,
                    no_canvas: true,
                    no_world: false,
                };
                let config = DriverConfig {
                    base_url: "https://vtt.example".into(),
                    api_user: "api".into(),
                    password_file: "/run/credentials/password".into(),
                    world_id: "canary".into(),
                    foundry_version: "13.351".into(),
                    system_id: "daggerheart".into(),
                    system_version: "1.6.4".into(),
                    chromium: "/bin/chromium".into(),
                    profile_dir: "/tmp/foundry".into(),
                };
                assert!(validate_state(&state, &config).is_err());
            }
        }
    }

    #[cfg(feature = "server")]
    pub use live::{ConfigError, DriverConfig, LiveFoundryDriver};
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
