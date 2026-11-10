use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

/// A typed operation on the live world. Foundry remains document-authoritative;
/// callers never supply JavaScript.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorldOp {
    ListDocuments {
        collection: String,
        limit: u32,
        after: Option<String>,
    },
    GetDocument {
        collection: String,
        id: String,
    },
    ChatCreate {
        content: String,
    },
    DiceRoll {
        formula: String,
    },
    SceneActivate {
        id: String,
    },
    CombatUpdate {
        action: String,
    },
    PlaylistControl {
        id: String,
        action: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorldError {
    #[error("world is not ready")]
    NotReady,
    #[error("{0}")]
    Unsupported(String),
    #[error("document not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("browser operation timed out")]
    Timeout,
    #[error("browser failure")]
    Browser,
}

/// The narrow seam used by the API layer. Foundry Circle only exposes
/// metadata gathered by the adapter; Foundry remains document-authoritative.
pub trait FoundryDriver: Send + Sync {
    fn snapshot(&self) -> WorldSnapshot;

    fn world_state(&self) -> WorldState {
        self.snapshot().state
    }

    /// Drivers that only report state answer every operation as not ready.
    fn call(
        &self,
        _op: WorldOp,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WorldError>> + Send + '_>> {
        Box::pin(std::future::ready(Err(WorldError::NotReady)))
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

    fn call(
        &self,
        op: WorldOp,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WorldError>> + Send + '_>> {
        let result = if self.state == WorldState::Ready {
            fake_call(op)
        } else {
            Err(WorldError::NotReady)
        };
        Box::pin(std::future::ready(result))
    }
}

const FAKE_ACTOR: &str = "a1";

fn fake_call(op: WorldOp) -> Result<Value, WorldError> {
    validate_op(&op)?;
    match op {
        WorldOp::ListDocuments {
            collection,
            limit,
            after,
        } => {
            let mut items = Vec::new();
            if collection == "actors" && after.as_deref().is_none_or(|after| FAKE_ACTOR > after) {
                items.push(fake_document("actors", FAKE_ACTOR)?);
            }
            let items: Vec<_> = items.into_iter().take(limit as usize).collect();
            let next_cursor = if items.len() == limit as usize {
                items
                    .last()
                    .and_then(|doc| doc["id"].as_str())
                    .map(str::to_owned)
            } else {
                None
            };
            Ok(json!({"items": items, "nextCursor": next_cursor}))
        }
        WorldOp::GetDocument { collection, id } => fake_document(&collection, &id),
        WorldOp::ChatCreate { .. } => Ok(json!({"created": true})),
        WorldOp::DiceRoll { formula } => Ok(json!({"formula": formula, "total": 0, "result": "0"})),
        WorldOp::SceneActivate { id } => Ok(json!({"id": id, "active": true})),
        WorldOp::CombatUpdate { action } => Ok(json!({"action": action})),
        WorldOp::PlaylistControl { id, action } => Ok(json!({"id": id, "action": action})),
    }
}

fn fake_document(collection: &str, id: &str) -> Result<Value, WorldError> {
    if collection != "actors" || id != FAKE_ACTOR {
        return Err(WorldError::NotFound);
    }
    Ok(json!({
        "documentType": "Actor",
        "id": FAKE_ACTOR,
        "uuid": "Actor.a1",
        "name": "Fixture Actor",
        "type": "character",
        "source": {"_id": FAKE_ACTOR, "name": "Fixture Actor", "type": "character"},
    }))
}

/// Bound checks done before a call can reach the browser.
pub(crate) fn validate_op(op: &WorldOp) -> Result<(), WorldError> {
    match op {
        WorldOp::ListDocuments {
            collection,
            limit,
            after,
        } => {
            collection_check(collection)?;
            if !(1..=200).contains(limit) {
                return Err(WorldError::Invalid("limit must be 1..=200".into()));
            }
            if let Some(after) = after {
                id_check(after)?;
            }
        }
        WorldOp::GetDocument { collection, id } => {
            collection_check(collection)?;
            id_check(id)?;
        }
        WorldOp::ChatCreate { content } => {
            const MAX: usize = 4000;
            if !(1..=MAX).contains(&content.chars().count()) {
                return Err(WorldError::Invalid(format!(
                    "content must be 1..={MAX} characters"
                )));
            }
        }
        WorldOp::DiceRoll { formula } => {
            const MAX: usize = 200;
            if !(1..=MAX).contains(&formula.chars().count()) {
                return Err(WorldError::Invalid(format!(
                    "formula must be 1..={MAX} characters"
                )));
            }
        }
        WorldOp::SceneActivate { id } => id_check(id)?,
        WorldOp::CombatUpdate { action } => {
            if !matches!(
                action.as_str(),
                "start" | "next-turn" | "previous-turn" | "next-round" | "end"
            ) {
                return Err(WorldError::Invalid(
                    "action must be start, next-turn, previous-turn, next-round, or end".into(),
                ));
            }
        }
        WorldOp::PlaylistControl { id, action } => {
            id_check(id)?;
            if !matches!(action.as_str(), "play" | "stop") {
                return Err(WorldError::Invalid("action must be play or stop".into()));
            }
        }
    }
    Ok(())
}

fn collection_check(collection: &str) -> Result<(), WorldError> {
    matches!(
        collection,
        "actors" | "scenes" | "messages" | "playlists" | "tables"
    )
    .then_some(())
    .ok_or_else(|| WorldError::Unsupported(collection.to_owned()))
}

fn id_check(id: &str) -> Result<(), WorldError> {
    let valid = (1..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric());
    valid
        .then_some(())
        .ok_or_else(|| WorldError::Invalid("id must be 1..=64 alphanumeric characters".into()))
}

#[cfg(feature = "server")]
mod live {
    use super::{FoundryDriver, WorldError, WorldOp, WorldSnapshot, WorldState, validate_op};
    use chromiumoxide::{
        browser::{Browser, BrowserConfig},
        page::Page,
    };
    use futures_util::StreamExt;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::{
        env,
        path::PathBuf,
        sync::{
            Arc, RwLock,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    };
    use tokio::{
        sync::{Mutex, mpsc, oneshot},
        time::sleep,
    };
    use uuid::Uuid;

    const POLL_INTERVAL: Duration = Duration::from_secs(2);
    const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
    const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
    const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
    const CHROMIUM_ARGS: [&str; 1] = ["disable-gpu"];

    /// Incremented whenever a new browser session first becomes ready.
    static EPOCH: AtomicU64 = AtomicU64::new(0);

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
        /// The world's game system is chosen in Foundry's setup UI; when
        /// unset, any system the live world reports is accepted.
        pub system_id: Option<String>,
        pub system_version: Option<String>,
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
            let optional = |name| env::var(name).ok().filter(|value| !value.trim().is_empty());
            Ok(Self {
                base_url: required("FOUNDRY_WORLD_BASE_URL")?
                    .trim_end_matches('/')
                    .into(),
                api_user: required("FOUNDRY_WORLD_API_USER")?,
                password_file: required("FOUNDRY_WORLD_API_PASSWORD_FILE")?.into(),
                world_id: required("FOUNDRY_WORLD_ID")?,
                foundry_version: required("FOUNDRY_EXPECTED_FOUNDRY_VERSION")?,
                system_id: optional("FOUNDRY_EXPECTED_SYSTEM_ID"),
                system_version: optional("FOUNDRY_EXPECTED_SYSTEM_VERSION"),
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

    type OpSender = mpsc::Sender<(WorldOp, oneshot::Sender<Result<Value, WorldError>>)>;

    pub struct LiveFoundryDriver {
        snapshot: Arc<RwLock<WorldSnapshot>>,
        ops: Mutex<Option<OpSender>>,
    }

    impl LiveFoundryDriver {
        pub fn start(config: DriverConfig) -> Arc<Self> {
            let driver = Arc::new(Self {
                snapshot: Arc::new(RwLock::new(WorldSnapshot::starting())),
                ops: Mutex::new(None),
            });
            let task_driver = Arc::clone(&driver);
            tokio::spawn(async move { supervise(task_driver, config).await });
            driver
        }

        fn set_snapshot(&self, snapshot: WorldSnapshot) {
            *self.snapshot.write().expect("Foundry driver lock poisoned") = snapshot;
        }

        async fn set_ops(&self, ops: Option<OpSender>) {
            *self.ops.lock().await = ops;
        }
    }

    impl FoundryDriver for LiveFoundryDriver {
        fn snapshot(&self) -> WorldSnapshot {
            self.snapshot
                .read()
                .expect("Foundry driver lock poisoned")
                .clone()
        }

        fn call(
            &self,
            op: WorldOp,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, WorldError>> + Send + '_>,
        > {
            Box::pin(async move {
                if self.snapshot().state != WorldState::Ready {
                    return Err(WorldError::NotReady);
                }
                validate_op(&op)?;
                let Some(sender) = self.ops.lock().await.clone() else {
                    return Err(WorldError::NotReady);
                };
                let (reply, response) = oneshot::channel();
                match tokio::time::timeout(OPERATION_TIMEOUT, sender.send((op, reply))).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => return Err(WorldError::NotReady),
                    Err(_) => return Err(WorldError::Timeout),
                }
                match tokio::time::timeout(OPERATION_TIMEOUT, response).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err(WorldError::NotReady),
                    Err(_) => Err(WorldError::Timeout),
                }
            })
        }
    }

    async fn supervise(driver: Arc<LiveFoundryDriver>, config: DriverConfig) {
        let mut backoff = 5;
        loop {
            if !world_active(&config.base_url).await {
                driver.set_snapshot(WorldSnapshot {
                    state: WorldState::WorldStopped,
                    ..WorldSnapshot::starting()
                });
                sleep(Duration::from_secs(15)).await;
                continue;
            }
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

    /// True unless Foundry answers `/api/status` with `active: false`. Request
    /// failures fall through to the browser, which reports its own error.
    async fn world_active(base_url: &str) -> bool {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap_or_default()
        });
        let response = client.get(format!("{base_url}/api/status")).send().await;
        match response {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(status) => status
                        .get("active")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    Err(_) => true,
                }
            }
            _ => true,
        }
    }

    async fn run_session(driver: &LiveFoundryDriver, config: &DriverConfig) -> Result<(), Failure> {
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
            .args(CHROMIUM_ARGS)
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
                let mut snapshot = validate_state(&state, config)?;
                if !state.no_canvas && !reloaded_for_no_canvas {
                    evaluate::<bool>(&page, SET_NO_CANVAS).await?;
                    page.reload().await.map_err(|_| Failure::Browser)?;
                    reloaded_for_no_canvas = true;
                    continue;
                }
                let epoch = EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
                snapshot.epoch = epoch;
                driver.set_snapshot(snapshot);
                let (sender, receiver) = mpsc::channel(16);
                driver.set_ops(Some(sender)).await;
                let result = serve(driver, config, &page, receiver, epoch).await;
                driver.set_ops(None).await;
                return result;
            }
            sleep(POLL_INTERVAL).await;
        }
        Err(Failure::Authentication)
    }

    /// The single serialized executor: health polls and typed world operations
    /// share one browser page and never interleave.
    async fn serve(
        driver: &LiveFoundryDriver,
        config: &DriverConfig,
        page: &Page,
        mut ops: mpsc::Receiver<(WorldOp, oneshot::Sender<Result<Value, WorldError>>)>,
        epoch: u64,
    ) -> Result<(), Failure> {
        let mut health = tokio::time::interval(HEALTH_INTERVAL);
        health.tick().await;
        loop {
            tokio::select! {
                _ = health.tick() => {
                    let state = evaluate::<GameState>(page, GAME_STATE).await?;
                    if state.no_world {
                        return Err(Failure::WorldStopped);
                    }
                    if !state.ready {
                        return Err(Failure::Browser);
                    }
                    let mut snapshot = validate_state(&state, config)?;
                    snapshot.epoch = epoch;
                    driver.set_snapshot(snapshot);
                }
                op = ops.recv() => {
                    let Some((op, reply)) = op else { return Ok(()); };
                    let result = match tokio::time::timeout(OPERATION_TIMEOUT, evaluate_op(page, op)).await {
                        Ok(result) => result,
                        Err(_) => Err(WorldError::Timeout),
                    };
                    let _ = reply.send(result);
                }
            }
        }
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

    fn validate_state(state: &GameState, config: &DriverConfig) -> Result<WorldSnapshot, Failure> {
        if !state.is_gm
            || state.world_id.as_deref() != Some(config.world_id.as_str())
            || state.foundry_version.as_deref() != Some(config.foundry_version.as_str())
            || !expected(config.system_id.as_ref(), state.system_id.as_ref())
            || !expected(
                config.system_version.as_ref(),
                state.system_version.as_ref(),
            )
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

    /// An undeclared expectation accepts whatever the live world reports.
    fn expected(declared: Option<&String>, live: Option<&String>) -> bool {
        declared.is_none() || declared == live
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

    #[derive(Debug, Deserialize)]
    #[serde(untagged)]
    enum Reply {
        Ok {
            ok: Value,
        },
        Error {
            error: String,
            #[serde(default)]
            message: Option<String>,
        },
    }

    /// One fixed function per operation; the only caller data is a JSON literal
    /// produced by `serde_json::to_string`.
    async fn evaluate_op(page: &Page, op: WorldOp) -> Result<Value, WorldError> {
        let (script, args) = match op {
            WorldOp::ListDocuments {
                collection,
                limit,
                after,
            } => (
                LIST_DOCUMENTS,
                json!({"collection": collection, "limit": limit, "after": after}),
            ),
            WorldOp::GetDocument { collection, id } => {
                (GET_DOCUMENT, json!({"collection": collection, "id": id}))
            }
            WorldOp::ChatCreate { content } => (CHAT_CREATE, json!({"content": content})),
            WorldOp::DiceRoll { formula } => (DICE_ROLL, json!({"formula": formula})),
            WorldOp::SceneActivate { id } => (SCENE_ACTIVATE, json!({"id": id})),
            WorldOp::CombatUpdate { action } => (COMBAT_UPDATE, json!({"action": action})),
            WorldOp::PlaylistControl { id, action } => {
                (PLAYLIST_CONTROL, json!({"id": id, "action": action}))
            }
        };
        let args = serde_json::to_string(&args).map_err(|_| WorldError::Browser)?;
        let script = script.replace("__ARGS__", &args);
        let reply: Reply = page
            .evaluate(script.as_str())
            .await
            .map_err(|_| WorldError::Browser)?
            .into_value()
            .map_err(|_| WorldError::Browser)?;
        match reply {
            Reply::Ok { ok } => Ok(ok),
            Reply::Error { error, message } => Err(match error.as_str() {
                "not_found" => WorldError::NotFound,
                "invalid" => {
                    WorldError::Invalid(message.unwrap_or_else(|| "invalid operation".into()))
                }
                "unsupported" => {
                    WorldError::Unsupported(message.unwrap_or_else(|| "unsupported".into()))
                }
                _ => WorldError::Browser,
            }),
        }
    }

    const LIST_DOCUMENTS: &str = r#"async function() {
        const args = __ARGS__;
        const collection = game[args.collection];
        if (!collection) return { error: "unsupported", message: "unknown collection" };
        const after = args.after;
        const docs = collection.contents
            .filter(doc => after === null || doc.id > after)
            .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
            .slice(0, args.limit)
            .map(doc => ({
                documentType: doc.documentName,
                id: doc.id,
                uuid: doc.uuid,
                name: doc.name ?? null,
                type: doc.type ?? null,
                source: doc.toObject()
            }));
        return {
            ok: {
                items: docs,
                nextCursor: docs.length === args.limit ? docs[docs.length - 1].id : null
            }
        };
    }"#;

    const GET_DOCUMENT: &str = r#"async function() {
        const args = __ARGS__;
        const collection = game[args.collection];
        if (!collection) return { error: "unsupported", message: "unknown collection" };
        const doc = collection.get(args.id);
        if (!doc) return { error: "not_found", message: "document not found" };
        return {
            ok: {
                documentType: doc.documentName,
                id: doc.id,
                uuid: doc.uuid,
                name: doc.name ?? null,
                type: doc.type ?? null,
                source: doc.toObject()
            }
        };
    }"#;

    const CHAT_CREATE: &str = r#"async function() {
        const args = __ARGS__;
        if (!game.messages || typeof ChatMessage === "undefined") {
            return { error: "unsupported", message: "chat is unavailable" };
        }
        const message = await ChatMessage.create({ content: args.content });
        return { ok: { id: message?.id ?? null, created: true } };
    }"#;

    const DICE_ROLL: &str = r#"async function() {
        const args = __ARGS__;
        if (typeof Roll === "undefined") {
            return { error: "unsupported", message: "rolls are unavailable" };
        }
        if (!Roll.validate(args.formula)) {
            return { error: "invalid", message: "invalid dice formula" };
        }
        const roll = await new Roll(args.formula).evaluate();
        return { ok: { formula: roll.formula, total: roll.total, result: roll.result } };
    }"#;

    const SCENE_ACTIVATE: &str = r#"async function() {
        const args = __ARGS__;
        const scene = game.scenes?.get(args.id);
        if (!scene) return { error: "not_found", message: "scene not found" };
        await scene.activate();
        return { ok: { id: scene.id, active: true } };
    }"#;

    const COMBAT_UPDATE: &str = r#"async function() {
        const args = __ARGS__;
        const combat = game.combat;
        if (!combat) return { error: "not_found", message: "no active combat" };
        const actions = {
            start: () => combat.startCombat(),
            "next-turn": () => combat.nextTurn(),
            "previous-turn": () => combat.previousTurn(),
            "next-round": () => combat.nextRound(),
            // endCombat() opens a confirmation dialog nobody can answer here.
            end: () => combat.delete()
        };
        const action = actions[args.action];
        if (!action) return { error: "invalid", message: "invalid combat action" };
        await action();
        return { ok: { action: args.action, active: !!game.combat } };
    }"#;

    const PLAYLIST_CONTROL: &str = r#"async function() {
        const args = __ARGS__;
        const playlist = game.playlists?.get(args.id);
        if (!playlist) return { error: "not_found", message: "playlist not found" };
        if (args.action === "play") {
            await playlist.playAll();
        } else if (args.action === "stop") {
            await playlist.stopAll();
        } else {
            return { error: "invalid", message: "invalid playlist action" };
        }
        return { ok: { id: playlist.id, action: args.action } };
    }"#;

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
                system_id: Some("daggerheart".into()),
                system_version: Some("1.6.4".into()),
                chromium: "/bin/chromium".into(),
                profile_dir: "/tmp/foundry".into(),
            };
            assert!(validate_state(&state, &config).is_err());
            let gm = GameState {
                is_gm: true,
                system_id: Some("pf2e".into()),
                system_version: Some("7.0.0".into()),
                ..state
            };
            assert!(validate_state(&gm, &config).is_err());
            let gui_selected = DriverConfig {
                system_id: None,
                system_version: None,
                ..config
            };
            let snapshot = validate_state(&gm, &gui_selected).unwrap();
            assert_eq!(snapshot.system_id.as_deref(), Some("pf2e"));
        }

        #[test]
        fn chromiumoxide_args_are_switch_names_without_prefixes() {
            assert!(CHROMIUM_ARGS.iter().all(|arg| !arg.starts_with("--")));
        }
    }
}

#[cfg(feature = "server")]
pub use live::{ConfigError, DriverConfig, LiveFoundryDriver};
