use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::builder::{PackInfo, Redirect, Scores};

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
    /// The world's Item packs, from the companion module's `catalogPacks`.
    CatalogPacks,
    /// Enable the installed companion module in the current world.
    ModuleEnable,
    /// One page of a pack's items, from the companion module's `catalogPage`.
    CatalogPage {
        pack: String,
        after: Option<String>,
        limit: u32,
    },
    /// One character from an approved draft, through the companion module's
    /// `createCharacter`: the one world write the broker performs.
    CharacterCreate {
        args: Value,
    },
}

/// A `CatalogPacks` reply: the world's Item packs and the compendium uuid
/// redirects it applies. A module older than `uuidRedirects()` sends no
/// redirects, and the catalog still syncs against the documents it has.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPacks {
    pub packs: Vec<PackInfo>,
    #[serde(default)]
    pub redirects: Vec<Redirect>,
}

/// A broker-managed Foundry account operation. The password is only ever a
/// fixed-script argument; this type deliberately has no `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub enum AccountOp {
    /// The names of the world's users, for collision-free naming.
    UserNames,
    /// Find by id, else by name when `reuse_by_name`, else create; then set
    /// the role. Adoption reuses by name (retry-safe); a player's first entry
    /// never adopts a stranger's same-named user.
    EnsureUser {
        user_id: Option<String>,
        name: String,
        role: u8,
        reuse_by_name: bool,
    },
    /// Set a user's password; Foundry deletes that user's HTTP sessions.
    SetPassword { user_id: String, password: String },
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

impl WorldError {
    /// The code `sync_catalog` reports for a failed catalog call; the companion
    /// module's two refusals keep their own names so the GM can act on them.
    pub fn catalog_code(&self) -> &'static str {
        match self {
            Self::Unsupported(code) if code == "module_missing" => "module_missing",
            Self::Unsupported(code) if code == "module_inactive" => "module_inactive",
            Self::Unsupported(code) if code == "module_outdated" => "module_outdated",
            _ => "foundry_unavailable",
        }
    }
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

    /// Broker-mapped identities (`/_circle/enter`). Drivers without a live
    /// page answer not ready.
    fn account(
        &self,
        _op: AccountOp,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WorldError>> + Send + '_>> {
        Box::pin(std::future::ready(Err(WorldError::NotReady)))
    }
}

/// Deterministic driver used by unit tests and the initial VM test.
#[derive(Clone)]
pub struct FakeDriver {
    state: WorldState,
    module: std::sync::Arc<std::sync::Mutex<(bool, std::collections::BTreeMap<String, bool>)>>,
    /// The draft ids it has already created an actor for, so a retried
    /// approval answers `created: false` instead of a second actor.
    created: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
    /// Every account operation it was asked for, in order, so a test can read
    /// what entry and approval asked the world to do.
    accounts: std::sync::Arc<std::sync::Mutex<Vec<AccountOp>>>,
}

impl std::fmt::Debug for FakeDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `AccountOp` deliberately has no `Debug`: a password never reaches a
        // log through a driver value.
        formatter
            .debug_struct("FakeDriver")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl FakeDriver {
    pub fn new(state: WorldState) -> Self {
        Self {
            state,
            module: std::sync::Arc::new(std::sync::Mutex::new((
                true,
                std::collections::BTreeMap::from([("foundry-circle".into(), true)]),
            ))),
            created: std::sync::Arc::default(),
            accounts: std::sync::Arc::default(),
        }
    }

    /// The account operations this fake has answered.
    pub fn accounts(&self) -> Vec<AccountOp> {
        self.accounts
            .lock()
            .expect("fake Foundry driver lock poisoned")
            .clone()
    }

    /// One actor per draft id; a second call returns the first one's id.
    fn character_create(&self, args: &Value) -> Result<Value, WorldError> {
        let draft = args["draftId"].as_str().unwrap_or_default().to_owned();
        let manual = args.get("manual").cloned().unwrap_or_else(|| json!([]));
        let mut created = self
            .created
            .lock()
            .expect("fake Foundry driver lock poisoned");
        let fresh = created.insert(draft.clone());
        Ok(json!({
            "id": format!("a{}", draft.replace('-', "")),
            "created": fresh,
            "manual": manual,
        }))
    }
}

/// The user a fake `EnsureUser` creates.
const FAKE_USER: &str = "playercase000001";

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
            validate_op(&op).and_then(|()| match op {
                WorldOp::ModuleEnable => {
                    let mut module = self
                        .module
                        .lock()
                        .expect("fake Foundry driver lock poisoned");
                    if !module.0 {
                        Err(WorldError::Unsupported("module_missing".into()))
                    } else {
                        let enabled = !module.1.get("foundry-circle").copied().unwrap_or(false);
                        module.1.insert("foundry-circle".into(), true);
                        Ok(json!({"enabled": enabled}))
                    }
                }
                op @ (WorldOp::CatalogPacks
                | WorldOp::CatalogPage { .. }
                | WorldOp::CharacterCreate { .. }) => {
                    let module = self
                        .module
                        .lock()
                        .expect("fake Foundry driver lock poisoned");
                    if !module.0 {
                        Err(WorldError::Unsupported("module_missing".into()))
                    } else if !module.1.get("foundry-circle").copied().unwrap_or(false) {
                        Err(WorldError::Unsupported("module_inactive".into()))
                    } else if let WorldOp::CharacterCreate { args } = op {
                        self.character_create(&args)
                    } else {
                        fake_call(op)
                    }
                }
                other => fake_call(other),
            })
        } else {
            Err(WorldError::NotReady)
        };
        Box::pin(std::future::ready(result))
    }

    fn account(
        &self,
        op: AccountOp,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WorldError>> + Send + '_>> {
        let result = if self.state == WorldState::Ready {
            self.accounts
                .lock()
                .expect("fake Foundry driver lock poisoned")
                .push(op.clone());
            match op {
                AccountOp::UserNames => Ok(json!({"names": ["Gamemaster"]})),
                // A user id means the mapping already exists; no id means the
                // world creates the user, as Foundry's own script reports it.
                AccountOp::EnsureUser {
                    user_id,
                    name,
                    role,
                    ..
                } => {
                    let created = user_id.is_none();
                    Ok(json!({
                        "id": user_id.unwrap_or_else(|| FAKE_USER.to_owned()),
                        "name": name,
                        "role": role,
                        "created": created,
                    }))
                }
                AccountOp::SetPassword { user_id, .. } => Ok(json!({"id": user_id})),
            }
        } else {
            Err(WorldError::NotReady)
        };
        Box::pin(std::future::ready(result))
    }
}

const FAKE_ACTOR: &str = "a1";
const FAKE_PACK: &str = "dnd5e.classes24";
const FAKE_CLASS: &str = "phbclcCleric0000";

fn fake_call(op: WorldOp) -> Result<Value, WorldError> {
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
        WorldOp::CatalogPacks => Ok(json!({
            "packs": [{
                "id": FAKE_PACK,
                "label": "Character Classes",
                "packageName": "dnd5e",
                "sourceBook": "SRD 5.2",
                "rulesVersion": "modern",
                "count": 1
            }]
        })),
        WorldOp::CatalogPage { pack, after, limit } => {
            if pack != FAKE_PACK {
                return Err(WorldError::NotFound);
            }
            let items = if after.as_deref().is_none_or(|after| FAKE_CLASS > after) {
                vec![fake_catalog_item()]
            } else {
                Vec::new()
            };
            let items: Vec<_> = items.into_iter().take(limit as usize).collect();
            // The cursor is the pack index id, as the module returns it.
            let next_cursor = (items.len() == limit as usize).then(|| FAKE_CLASS.to_owned());
            Ok(json!({"items": items, "nextCursor": next_cursor}))
        }
        // Answered by `FakeDriver::call`, which keeps the actors it created.
        WorldOp::CharacterCreate { .. } | WorldOp::ModuleEnable => {
            Err(WorldError::Unsupported("character".into()))
        }
    }
}

/// One class as the companion module emits it.
fn fake_catalog_item() -> Value {
    json!({
        "uuid": format!("Compendium.{FAKE_PACK}.Item.{FAKE_CLASS}"),
        "name": "Cleric",
        "type": "class",
        "img": "systems/dnd5e/icons/classes/cleric.webp",
        "text": "Cleric.",
        "system": {"identifier": "cleric", "levels": 1},
        "traits": {
            "2wUNkQzS4vL9nR6D": {
                "grants": [],
                "choices": [{
                    "count": 2,
                    "options": [{"key": "skills:ins", "label": "Insight"}]
                }]
            }
        }
    })
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
        WorldOp::CatalogPacks | WorldOp::ModuleEnable => {}
        WorldOp::CatalogPage { pack, after, limit } => {
            if !(1..=50).contains(limit) {
                return Err(WorldError::Invalid("limit must be 1..=50".into()));
            }
            let valid = (1..=100).contains(&pack.len())
                && pack
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
            if !valid {
                return Err(WorldError::Invalid(
                    "pack must be 1..=100 characters of [A-Za-z0-9._-]".into(),
                ));
            }
            if let Some(after) = after {
                id_check(after)?;
            }
        }
        WorldOp::CharacterCreate { args } => character_create_check(args)?,
    }
    Ok(())
}

/// Bound every field of a `CharacterCreate` argument before the module sees it:
/// the module trusts its argument, so this is the one place its shape is
/// checked. Nothing here names a document the world must hold; the module
/// refuses an unresolvable uuid itself.
fn character_create_check(args: &Value) -> Result<(), WorldError> {
    text_check(args.get("name"), "name", 1, 100)?;
    let draft = text_check(args.get("draftId"), "draftId", 1, 64)?;
    if !draft
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid("draftId must be a UUID"));
    }
    let owner = args
        .get("ownerUserId")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("ownerUserId must be a string"))?;
    id_check(owner)?;
    let level = args
        .get("level")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("level must be a number"))?;
    if !(1..=20).contains(&level) {
        return Err(invalid("level must be 1..=20"));
    }
    let abilities = args
        .get("abilities")
        .and_then(Value::as_object)
        .filter(|abilities| abilities.len() == Scores::LABELS.len())
        .ok_or_else(|| invalid("abilities must hold exactly the six scores"))?;
    for (key, _) in Scores::LABELS {
        let value = abilities
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("abilities must hold str..cha as numbers"))?;
        if !(1..=30).contains(&value) {
            return Err(invalid("an ability must be 1..=30"));
        }
    }
    let roots = args
        .get("roots")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("roots must be an array"))?;
    if !(1..=3).contains(&roots.len()) {
        return Err(invalid("roots must hold 1..=3 documents"));
    }
    for root in roots {
        uuid_check(root.get("uuid"), "a root uuid")?;
        if !matches!(
            root.get("kind").and_then(Value::as_str),
            Some("species" | "background" | "class")
        ) {
            return Err(invalid("a root kind must be species, background or class"));
        }
    }
    if let Some(choices) = args.get("choices") {
        let choices = choices
            .as_object()
            .ok_or_else(|| invalid("choices must be an object"))?;
        if choices.len() > 200 {
            return Err(invalid("choices must hold at most 200 answers"));
        }
        for (key, data) in choices {
            // `<compendium uuid>#<advancement id>#<level>`.
            let valid = (1..=300).contains(&key.len())
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'#' | b'-'));
            if !valid {
                return Err(invalid(
                    "a choice key must be <uuid>#<advancement id>#<level>",
                ));
            }
            // Every apply-data shape is small; one bound covers Trait,
            // ItemChoice, Size and the ability bonuses.
            let size = serde_json::to_string(data).unwrap_or_default().len();
            if !data.is_object() || size > 4000 {
                return Err(invalid(
                    "a choice answer must be an object of at most 4000 bytes",
                ));
            }
        }
    }
    let items = args
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if items.len() > 100 {
        return Err(invalid("items must hold at most 100 documents"));
    }
    for item in items {
        uuid_check(item.get("uuid"), "an item uuid")?;
        let quantity = item
            .get("quantity")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("an item needs a quantity"))?;
        if !(1..=999).contains(&quantity) {
            return Err(invalid("an item quantity must be 1..=999"));
        }
    }
    if let Some(gp) = args.get("gp").filter(|gp| !gp.is_null()) {
        let gp = gp.as_u64().ok_or_else(|| invalid("gp must be a number"))?;
        if gp > 1_000_000 {
            return Err(invalid("gp must be at most 1000000"));
        }
    }
    for (key, max) in [("wealth", 20), ("manual", 100)] {
        let list = args
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if list.len() > max {
            return Err(invalid("too many entries"));
        }
        for entry in list {
            text_check(Some(entry), key, 1, 2000)?;
        }
    }
    text_check(args.get("biography"), "biography", 0, 2000)?;
    if let Some(details) = args.get("details") {
        let details = details
            .as_object()
            .ok_or_else(|| invalid("details must be an object"))?;
        if details.len() > 32 {
            return Err(invalid("details must hold at most 32 fields"));
        }
        for (key, value) in details {
            if key.len() > 32 {
                return Err(invalid("a detail key must be at most 32 characters"));
            }
            text_check(Some(value), key, 0, 2000)?;
        }
    }
    Ok(())
}

fn invalid(message: &str) -> WorldError {
    WorldError::Invalid(message.to_owned())
}

fn text_check<'a>(
    value: Option<&'a Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<&'a str, WorldError> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))?;
    if !(min..=max).contains(&text.chars().count()) {
        return Err(invalid(&format!("{key} must be {min}..={max} characters")));
    }
    Ok(text)
}

fn uuid_check(uuid: Option<&Value>, what: &str) -> Result<(), WorldError> {
    let uuid = uuid
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{what} must be a string")))?;
    let valid = uuid.len() <= 200
        && uuid.starts_with("Compendium.")
        && uuid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    valid
        .then_some(())
        .ok_or_else(|| invalid(&format!("{what} must be a Compendium. uuid")))
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
    use super::{
        AccountOp, FoundryDriver, WorldError, WorldOp, WorldSnapshot, WorldState, validate_op,
    };
    use axum::http::{HeaderMap, header};
    use chromiumoxide::{
        browser::{Browser, BrowserConfig},
        cdp::browser_protocol::network::CookieParam,
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
        sync::{Mutex, Notify, mpsc, oneshot},
        task::JoinHandle,
        time::sleep,
    };
    use uuid::Uuid;

    const POLL_INTERVAL: Duration = Duration::from_secs(2);
    const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
    const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
    const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
    const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(90);
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
        /// The adopted world's API user; adoption creates it, the driver logs
        /// in as it with its derived password.
        pub api_user: String,
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

    #[derive(Debug, PartialEq, Eq, thiserror::Error)]
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
    struct GameState {
        ready: bool,
        world_id: Option<String>,
        foundry_version: Option<String>,
        system_id: Option<String>,
        system_version: Option<String>,
        is_gm: bool,
        /// The page has a `game.user`: Foundry accepted the cookie.
        authenticated: bool,
        no_canvas: bool,
        no_world: bool,
    }

    /// One live Foundry account, as reported by `getJoinData`.
    #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
    pub(crate) struct FoundryUser {
        pub(crate) id: String,
        pub(crate) name: String,
        pub(crate) role: u8,
    }

    /// Foundry refused a join or an admin takeover.
    #[derive(Debug, thiserror::Error)]
    #[error("Foundry refused the session")]
    pub(crate) struct JoinFailure;

    enum Request {
        World(WorldOp),
        Account(AccountOp),
    }

    type OpSender = mpsc::Sender<(Request, oneshot::Sender<Result<Value, WorldError>>)>;

    #[derive(Clone)]
    struct DriverSession {
        world: String,
        cookie: String,
    }

    pub struct LiveFoundryDriver {
        snapshot: Arc<RwLock<WorldSnapshot>>,
        ops: Mutex<Option<OpSender>>,
        session: Mutex<Option<DriverSession>>,
        wake: Notify,
    }

    impl LiveFoundryDriver {
        pub fn start(config: DriverConfig) -> Arc<Self> {
            let driver = Arc::new(Self {
                snapshot: Arc::new(RwLock::new(WorldSnapshot::starting())),
                ops: Mutex::new(None),
                session: Mutex::new(None),
                wake: Notify::new(),
            });
            let task_driver = Arc::clone(&driver);
            tokio::spawn(async move { supervise(task_driver, config).await });
            driver
        }

        /// Hand the driver a Foundry `session` cookie for this world. The
        /// broker obtains it with the API user's derived password.
        pub async fn set_session(&self, world: &str, cookie: String) {
            *self.session.lock().await = Some(DriverSession {
                world: world.to_owned(),
                cookie,
            });
            self.wake.notify_one();
            tracing::info!(world, "Foundry driver session accepted");
        }

        /// Forget the cookie; the supervisor parks until the next hand-off.
        pub async fn clear_session(&self) {
            if self.session.lock().await.take().is_some() {
                self.wake.notify_one();
                tracing::info!("Foundry driver session cleared");
            }
        }

        /// The world whose cookie the driver holds, never the cookie itself.
        pub async fn session_world(&self) -> Option<String> {
            self.session
                .lock()
                .await
                .as_ref()
                .map(|session| session.world.clone())
        }

        fn set_snapshot(&self, snapshot: WorldSnapshot) {
            *self.snapshot.write().expect("Foundry driver lock poisoned") = snapshot;
        }

        async fn set_ops(&self, ops: Option<OpSender>) {
            *self.ops.lock().await = ops;
        }

        async fn dispatch(&self, request: Request) -> Result<Value, WorldError> {
            if self.snapshot().state != WorldState::Ready {
                return Err(WorldError::NotReady);
            }
            if let Request::World(op) = &request {
                validate_op(op)?;
            }
            let Some(sender) = self.ops.lock().await.clone() else {
                return Err(WorldError::NotReady);
            };
            let timeout = if matches!(request, Request::World(WorldOp::ModuleEnable)) {
                NAVIGATION_TIMEOUT + OPERATION_TIMEOUT
            } else {
                OPERATION_TIMEOUT
            };
            let (reply, response) = oneshot::channel();
            match tokio::time::timeout(OPERATION_TIMEOUT, sender.send((request, reply))).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(WorldError::NotReady),
                Err(_) => return Err(WorldError::Timeout),
            }
            match tokio::time::timeout(timeout, response).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(WorldError::NotReady),
                Err(_) => Err(WorldError::Timeout),
            }
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
            Box::pin(self.dispatch(Request::World(op)))
        }

        fn account(
            &self,
            op: AccountOp,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, WorldError>> + Send + '_>,
        > {
            Box::pin(self.dispatch(Request::Account(op)))
        }
    }

    async fn supervise(driver: Arc<LiveFoundryDriver>, config: DriverConfig) {
        let mut backoff = 5;
        loop {
            let Some(session) = driver.session.lock().await.clone() else {
                driver.set_snapshot(WorldSnapshot {
                    state: WorldState::WorldStopped,
                    ..WorldSnapshot::starting()
                });
                driver.wake.notified().await;
                backoff = 5;
                continue;
            };
            if !world_active(&config.base_url).await {
                driver.set_snapshot(WorldSnapshot {
                    state: WorldState::WorldStopped,
                    ..WorldSnapshot::starting()
                });
                sleep(Duration::from_secs(15)).await;
                continue;
            }
            driver.set_snapshot(WorldSnapshot::starting());
            match run_session(&driver, &config, &session).await {
                Err(Failure::WorldStopped) => {
                    driver.set_snapshot(WorldSnapshot {
                        state: WorldState::WorldStopped,
                        ..WorldSnapshot::starting()
                    });
                    backoff = 15;
                }
                Err(Failure::Authentication) => {
                    tracing::warn!(
                        "Foundry no longer accepts the driver's cookie; a fresh API session is needed"
                    );
                    driver.set_snapshot(WorldSnapshot {
                        state: WorldState::Error,
                        ..WorldSnapshot::starting()
                    });
                    // Drop the dead hand-off so the reconciler reacquires a
                    // fresh API cookie; keeping it would retry an expired
                    // cookie until the world is relaunched.
                    driver.clear_session().await;
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

    async fn run_session(
        driver: &LiveFoundryDriver,
        config: &DriverConfig,
        handoff: &DriverSession,
    ) -> Result<(), Failure> {
        let (browser, handler, profile) = launch(config).await?;
        let result = async {
            install_cookie(&browser, &config.base_url, &handoff.cookie).await?;
            session(driver, config, &browser, &handoff.world).await
        }
        .await;
        close_browser(browser, handler, profile).await;
        result
    }

    async fn session(
        driver: &LiveFoundryDriver,
        config: &DriverConfig,
        browser: &Browser,
        world: &str,
    ) -> Result<(), Failure> {
        let page = browser
            .new_page(format!("{}/game", config.base_url))
            .await
            .map_err(|_| Failure::Browser)?;

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
                let result = serve(driver, config, &page, receiver, epoch, world).await;
                driver.set_ops(None).await;
                return result;
            }
            sleep(POLL_INTERVAL).await;
        }
        Err(Failure::Authentication)
    }

    async fn launch(config: &DriverConfig) -> Result<(Browser, JoinHandle<()>, PathBuf), Failure> {
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
        let (browser, mut handler) = Browser::launch(browser_config)
            .await
            .map_err(|_| Failure::Browser)?;
        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
        Ok((browser, handler_task, profile))
    }

    async fn close_browser(mut browser: Browser, handler: JoinHandle<()>, profile: PathBuf) {
        let _ = browser.close().await;
        handler.abort();
        let _ = std::fs::remove_dir_all(profile);
    }

    /// Set Foundry's `session` cookie for the base URL's origin.
    async fn install_cookie(
        browser: &Browser,
        base_url: &str,
        cookie: &str,
    ) -> Result<(), Failure> {
        let mut param = CookieParam::new("session", cookie);
        param.url = Some(base_url.to_owned());
        browser
            .set_cookies(vec![param])
            .await
            .map_err(|_| Failure::Browser)?;
        Ok(())
    }

    /// A browser page for the broker's one-shot identity work (adoption).
    /// Nothing from this page is ever handed to a browser.
    pub(crate) struct DriverPage {
        browser: Browser,
        handler: JoinHandle<()>,
        profile: PathBuf,
        pub(crate) page: Page,
    }

    impl DriverPage {
        pub(crate) async fn open(config: &DriverConfig, cookie: &str) -> Result<Self, WorldError> {
            let (browser, handler, profile) =
                launch(config).await.map_err(|_| WorldError::Browser)?;
            let opened = async {
                install_cookie(&browser, &config.base_url, cookie)
                    .await
                    .map_err(|_| WorldError::Browser)?;
                browser
                    .new_page(format!("{}/join", config.base_url))
                    .await
                    .map_err(|_| WorldError::Browser)
            };
            let opened = match tokio::time::timeout(NAVIGATION_TIMEOUT, opened).await {
                Ok(opened) => opened,
                Err(_) => Err(WorldError::Timeout),
            };
            match opened {
                Ok(page) => Ok(Self {
                    browser,
                    handler,
                    profile,
                    page,
                }),
                Err(error) => {
                    close_browser(browser, handler, profile).await;
                    Err(error)
                }
            }
        }

        /// Switch this page to another session cookie and open the game.
        pub(crate) async fn goto_game(
            &self,
            config: &DriverConfig,
            cookie: &str,
        ) -> Result<(), WorldError> {
            let open = async {
                install_cookie(&self.browser, &config.base_url, cookie)
                    .await
                    .map_err(|_| WorldError::Browser)?;
                self.page
                    .goto(format!("{}/game", config.base_url))
                    .await
                    .map_err(|_| WorldError::Browser)?;
                wait_for_game(&self.page).await
            };
            match tokio::time::timeout(NAVIGATION_TIMEOUT, open).await {
                Ok(open) => open,
                Err(_) => Err(WorldError::Timeout),
            }
        }

        pub(crate) async fn close(self) {
            close_browser(self.browser, self.handler, self.profile).await;
        }
    }

    pub(crate) async fn wait_for_game(page: &Page) -> Result<(), WorldError> {
        let deadline = Instant::now() + SESSION_TIMEOUT;
        while Instant::now() < deadline {
            match evaluate::<GameState>(page, GAME_STATE).await {
                Ok(state) if state.ready => return Ok(()),
                Ok(state) if state.no_world => return Err(WorldError::NotReady),
                _ => {}
            }
            sleep(POLL_INTERVAL).await;
        }
        Err(WorldError::Timeout)
    }

    /// `POST /join {action: join}` with no cookie: Foundry creates a session
    /// and returns its `session` cookie.
    pub(crate) async fn join_session(
        base_url: &str,
        user_id: &str,
        password: &str,
    ) -> Result<String, JoinFailure> {
        let headers = post_join(
            base_url,
            None,
            json!({"action": "join", "userId": user_id, "password": password}),
        )
        .await?;
        foundry_session_cookie(&headers).ok_or(JoinFailure)
    }

    /// `POST /join {action: loginAs}` on an existing admin cookie.
    pub(crate) async fn login_as(
        base_url: &str,
        cookie: &str,
        user_id: &str,
    ) -> Result<(), JoinFailure> {
        post_join(
            base_url,
            Some(cookie),
            json!({"action": "loginAs", "userId": user_id}),
        )
        .await
        .map(|_| ())
    }

    async fn post_join(
        base_url: &str,
        cookie: Option<&str>,
        payload: Value,
    ) -> Result<HeaderMap, JoinFailure> {
        let mut request = post_client()
            .post(format!("{base_url}/join"))
            .json(&payload);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, format!("session={cookie}"));
        }
        let reply = request.send().await.map_err(|_| JoinFailure)?;
        if !reply.status().is_success() {
            return Err(JoinFailure);
        }
        let headers = reply.headers().clone();
        let body: Value = reply.json().await.map_err(|_| JoinFailure)?;
        if !join_accepted(&body) {
            return Err(JoinFailure);
        }
        Ok(headers)
    }

    /// Foundry answers a successful join with `{status: "success"}`.
    fn join_accepted(body: &Value) -> bool {
        body.get("status").and_then(Value::as_str) == Some("success")
    }

    /// The `session` cookie from a join or admin-login reply; other cookies
    /// are ignored.
    pub(crate) fn foundry_session_cookie(headers: &HeaderMap) -> Option<String> {
        headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value))
            .filter_map(|pair| pair.split_once('='))
            .find_map(|(name, value)| {
                (name.trim() == "session").then(|| value.trim().trim_matches('"').to_owned())
            })
            .filter(|value| valid_cookie_value(value))
    }

    fn valid_cookie_value(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 512
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b';' | b',' | b'"' | b'\\'))
    }

    fn post_client() -> &'static reqwest::Client {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default()
        })
    }

    /// The world's users over the page's socket.io connection. Any socket may
    /// ask for `getJoinData`; the ack carries `db.User.dump()`, passwordless.
    pub(crate) async fn page_users(page: &Page) -> Result<Vec<FoundryUser>, WorldError> {
        let evaluate = async {
            page.evaluate(JOIN_USERS)
                .await
                .map_err(|_| WorldError::Browser)?
                .into_value()
                .map_err(|_| WorldError::Browser)
        };
        let reply: UsersReply = match tokio::time::timeout(NAVIGATION_TIMEOUT, evaluate).await {
            Ok(reply) => reply?,
            Err(_) => return Err(WorldError::Timeout),
        };
        match reply {
            UsersReply::Ok { users } => Ok(users),
            UsersReply::Error { error } if error == "timeout" => Err(WorldError::Timeout),
            UsersReply::Error { .. } => Err(WorldError::Browser),
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(untagged)]
    enum UsersReply {
        Ok { users: Vec<FoundryUser> },
        Error { error: String },
    }

    /// Run one account operation against a given page. Both the steady-state
    /// driver and adoption use this one implementation.
    pub(crate) async fn page_account(page: &Page, op: &AccountOp) -> Result<Value, WorldError> {
        let (script, args) = account_script(op)?;
        let script = script.replace("__ARGS__", &args);
        let evaluate = async {
            page.evaluate(script.as_str())
                .await
                .map_err(|_| WorldError::Browser)?
                .into_value()
                .map_err(|_| WorldError::Browser)
        };
        let reply: Reply = match tokio::time::timeout(OPERATION_TIMEOUT, evaluate).await {
            Ok(reply) => reply?,
            Err(_) => return Err(WorldError::Timeout),
        };
        match reply {
            Reply::Ok { ok } => Ok(ok),
            Reply::Error { error, message } => Err(reply_error(error, message)),
        }
    }

    fn account_script(op: &AccountOp) -> Result<(&'static str, String), WorldError> {
        let (script, args) = match op {
            AccountOp::UserNames => (USER_NAMES, json!({})),
            AccountOp::EnsureUser {
                user_id,
                name,
                role,
                reuse_by_name,
            } => (
                ENSURE_USER,
                json!({
                    "userId": user_id,
                    "name": name,
                    "role": role,
                    "reuseByName": reuse_by_name
                }),
            ),
            AccountOp::SetPassword { user_id, password } => (
                SET_PASSWORD,
                json!({"userId": user_id, "password": password}),
            ),
        };
        Ok((
            script,
            serde_json::to_string(&args).map_err(|_| WorldError::Browser)?,
        ))
    }

    /// Replace every listed user's password. The caller decides who is kept.
    pub(crate) async fn rotate_passwords(
        page: &Page,
        replacements: &[(String, String)],
    ) -> Result<usize, WorldError> {
        let args = serde_json::to_string(&json!({"replacements": replacements}))
            .map_err(|_| WorldError::Browser)?;
        let script = ROTATE_PASSWORDS.replace("__ARGS__", &args);
        let evaluate = async {
            page.evaluate(script.as_str())
                .await
                .map_err(|_| WorldError::Browser)?
                .into_value()
                .map_err(|_| WorldError::Browser)
        };
        let reply: Reply = match tokio::time::timeout(OPERATION_TIMEOUT, evaluate).await {
            Ok(reply) => reply?,
            Err(_) => return Err(WorldError::Timeout),
        };
        match reply {
            Reply::Ok { ok } => {
                let rotated = ok["rotated"].as_u64().unwrap_or_default() as usize;
                // A rotation that skipped a user is a failure, never a
                // partial success the caller could ignore.
                if rotated != replacements.len() {
                    return Err(WorldError::Browser);
                }
                Ok(rotated)
            }
            Reply::Error { error, message } => Err(reply_error(error, message)),
        }
    }

    /// The single serialized executor: health polls, world operations and
    /// account operations share one browser page and never interleave.
    async fn serve(
        driver: &LiveFoundryDriver,
        config: &DriverConfig,
        page: &Page,
        mut ops: mpsc::Receiver<(Request, oneshot::Sender<Result<Value, WorldError>>)>,
        epoch: u64,
        world: &str,
    ) -> Result<(), Failure> {
        let mut health = tokio::time::interval(HEALTH_INTERVAL);
        health.tick().await;
        loop {
            tokio::select! {
                _ = health.tick() => {
                    // The lease dropped or replaced the session: stop serving.
                    if driver.session_world().await.as_deref() != Some(world) {
                        return Err(Failure::WorldStopped);
                    }
                    let state = evaluate::<GameState>(page, GAME_STATE).await?;
                    health_check(&state)?;
                    let mut snapshot = validate_state(&state, config)?;
                    snapshot.epoch = epoch;
                    driver.set_snapshot(snapshot);
                }
                request = ops.recv() => {
                    let Some((request, reply)) = request else { return Ok(()); };
                    if !should_execute(&reply) { continue; }
                    let enabling = matches!(request, Request::World(WorldOp::ModuleEnable));
                    let timeout = if enabling { NAVIGATION_TIMEOUT } else { OPERATION_TIMEOUT };
                    let result = match tokio::time::timeout(timeout, evaluate_request(page, request)).await {
                        Ok(result) => result,
                        Err(_) => Err(WorldError::Timeout),
                    };
                    // A timed-out script may still be running in the page; drop
                    // the session so nothing else runs beside it.
                    let stop = abandoned(&result) || (enabling && matches!(result, Err(WorldError::Browser)));
                    let _ = reply.send(result);
                    if stop {
                        return Err(Failure::Browser);
                    }
                }
            }
        }
    }

    fn should_execute(reply: &oneshot::Sender<Result<Value, WorldError>>) -> bool {
        !reply.is_closed()
    }

    async fn evaluate_request(page: &Page, request: Request) -> Result<Value, WorldError> {
        match request {
            Request::World(op) => evaluate_op(page, op).await,
            Request::Account(op) => page_account(page, &op).await,
        }
    }

    /// Whether a request ended without a usable answer. Both the outer timeout
    /// and an operation's own `WorldError::Timeout` mean its script may still
    /// be running in the page.
    fn abandoned(result: &Result<Value, WorldError>) -> bool {
        matches!(result, Err(WorldError::Timeout))
    }

    /// One health observation: a live GM page keeps serving, a page without a
    /// user lost its cookie, and a ready page that is not a GM lost its role.
    fn health_check(state: &GameState) -> Result<(), Failure> {
        if state.no_world {
            return Err(Failure::WorldStopped);
        }
        if !state.ready {
            return Err(if state.authenticated {
                Failure::Browser
            } else {
                Failure::Authentication
            });
        }
        Ok(())
    }

    fn validate_state(state: &GameState, config: &DriverConfig) -> Result<WorldSnapshot, Failure> {
        // A ready page without GM rights means the cookie is no longer the API
        // user's: ask for a fresh hand-off rather than retrying it forever.
        if !state.is_gm {
            return Err(Failure::Authentication);
        }
        if state.foundry_version.as_deref() != Some(config.foundry_version.as_str())
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
        let enable = matches!(op, WorldOp::ModuleEnable);
        let (script, args) = match op {
            WorldOp::ModuleEnable => (MODULE_ENABLE, json!({})),
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
            WorldOp::CatalogPacks => (CATALOG_PACKS, json!({})),
            WorldOp::CatalogPage { pack, after, limit } => (
                CATALOG_PAGE,
                json!({"pack": pack, "after": after, "limit": limit}),
            ),
            WorldOp::CharacterCreate { args } => (CHARACTER_CREATE, args),
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
            Reply::Ok { ok } => {
                if enable && ok["enabled"] == true {
                    page.reload().await.map_err(|_| WorldError::Browser)?;
                    wait_for_game(page).await?;
                }
                Ok(ok)
            }
            Reply::Error { error, message } => Err(reply_error(error, message)),
        }
    }

    /// One fixed reply-error vocabulary for world and account scripts.
    fn reply_error(error: String, message: Option<String>) -> WorldError {
        match error.as_str() {
            "not_found" => WorldError::NotFound,
            "invalid" => WorldError::Invalid(message.unwrap_or_else(|| "invalid operation".into())),
            "unsupported" => {
                WorldError::Unsupported(message.unwrap_or_else(|| "unsupported".into()))
            }
            _ => WorldError::Browser,
        }
    }

    const LIST_DOCUMENTS: &str = r#"async function () {
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

    const MODULE_ENABLE: &str = r#"async function () {
        const args = __ARGS__;
        const module = game.modules.get("foundry-circle");
        if (!module) return { error: "unsupported", message: "module_missing" };
        if (module.active) return { ok: { enabled: false } };
        const config = foundry.utils.deepClone(game.settings.get("core", "moduleConfiguration"));
        config["foundry-circle"] = true;
        await game.settings.set("core", "moduleConfiguration", config);
        // Foundry 14 client/helpers/client-settings.mjs:249-303 can return the
        // unchanged value when Document.update is cancelled.
        if (game.settings.get("core", "moduleConfiguration")["foundry-circle"] !== true)
            return { error: "update_rejected" };
        return { ok: { enabled: true } };
    }"#;

    /// The companion module's catalog reads: the only caller data is the
    /// argument JSON, and every reply stays inside the fixed error vocabulary.
    const CATALOG_PACKS: &str = r#"async function () {
        const module = game.modules.get("foundry-circle");
        if (!module) return { error: "unsupported", message: "module_missing" };
        if (!module.active) return { error: "unsupported", message: "module_inactive" };
        const api = module.api;
        if (!api) return { error: "unsupported", message: "module_outdated" };
        if (api.apiVersion !== 1) return { error: "unsupported", message: "module_outdated" };
        // A module that predates uuidRedirects() answers no redirects, which is
        // a working world: the content names the packs it can resolve.
        const redirects = typeof api.uuidRedirects === "function" ? await api.uuidRedirects() : [];
        return { ok: { packs: await api.catalogPacks(), redirects } };
    }"#;

    const CATALOG_PAGE: &str = r#"async function () {
        const args = __ARGS__;
        const module = game.modules.get("foundry-circle");
        if (!module) return { error: "unsupported", message: "module_missing" };
        if (!module.active) return { error: "unsupported", message: "module_inactive" };
        const api = module.api;
        if (!api) return { error: "unsupported", message: "module_outdated" };
        if (api.apiVersion !== 1) return { error: "unsupported", message: "module_outdated" };
        const reply = await api.catalogPage({ pack: args.pack, after: args.after, limit: args.limit });
        if (reply?.error) return reply;
        return { ok: reply };
    }"#;

    const CHARACTER_CREATE: &str = r#"async function () {
        const args = __ARGS__;
        const module = game.modules.get("foundry-circle");
        if (!module) return { error: "unsupported", message: "module_missing" };
        if (!module.active) return { error: "unsupported", message: "module_inactive" };
        const api = module.api;
        if (!api) return { error: "unsupported", message: "module_outdated" };
        if (api.apiVersion !== 1) return { error: "unsupported", message: "module_outdated" };
        // A module older than the character write fails closed, never half-way.
        if (typeof api.createCharacter !== "function") {
            return { error: "unsupported", message: "module_outdated" };
        }
        const reply = await api.createCharacter(args);
        if (reply?.error) return reply;
        return { ok: reply };
    }"#;

    const GET_DOCUMENT: &str = r#"async function () {
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

    const CHAT_CREATE: &str = r#"async function () {
        const args = __ARGS__;
        if (!game.messages || typeof ChatMessage === "undefined") {
            return { error: "unsupported", message: "chat is unavailable" };
        }
        const message = await ChatMessage.create({ content: args.content });
        return { ok: { id: message?.id ?? null, created: true } };
    }"#;

    const DICE_ROLL: &str = r#"async function () {
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

    const SCENE_ACTIVATE: &str = r#"async function () {
        const args = __ARGS__;
        const scene = game.scenes?.get(args.id);
        if (!scene) return { error: "not_found", message: "scene not found" };
        await scene.activate();
        return { ok: { id: scene.id, active: true } };
    }"#;

    const COMBAT_UPDATE: &str = r#"async function () {
        const args = __ARGS__;
        // game.combat follows the viewed scene, which a canvas-less session
        // does not have; resolve the active encounter from the collection.
        const scene = game.scenes?.active?.id ?? null;
        const combat = game.combats?.contents.find(c => c.active && (!c.scene || c.scene.id === scene));
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
        return { ok: { action: args.action } };
    }"#;

    const PLAYLIST_CONTROL: &str = r#"async function () {
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

    /// The passwordless user list from `getJoinData`, over the page's socket.
    /// The socket is created with the page's own `session` cookie.
    const JOIN_USERS: &str = r#"async function () {
        const deadline = Date.now() + 10000;
        while (typeof io === "undefined") {
            if (Date.now() > deadline) return { error: "timeout" };
            await new Promise(resolve => setTimeout(resolve, 100));
        }
        return await new Promise(resolve => {
            const socket = io({ path: "/socket.io" });
            let finished = false;
            const finish = value => {
                if (finished) return;
                finished = true;
                clearTimeout(timer);
                socket.close();
                resolve(value);
            };
            const timer = setTimeout(() => finish({ error: "timeout" }), 10000);
            socket.on("connect_error", () => finish({ error: "socket" }));
            socket.on("session", () => {
                socket.emit("getJoinData", data => {
                    const users = Array.isArray(data?.users) ? data.users : [];
                    finish({
                        users: users.map(user => ({
                            id: user._id,
                            name: user.name,
                            role: user.role
                        }))
                    });
                });
            });
        });
    }"#;

    const USER_NAMES: &str = r#"() => {
        if (!window.game?.users) return { error: "unsupported", message: "users are unavailable" };
        return { ok: { names: game.users.contents.map(user => user.name) } };
    }"#;

    const ENSURE_USER: &str = r#"async function () {
        const args = __ARGS__;
        if (typeof User === "undefined") {
            return { error: "unsupported", message: "users are unavailable" };
        }
        let user = args.userId ? game.users.get(args.userId) : null;
        if (!user && args.reuseByName) {
            user = game.users.find(candidate => candidate.name === args.name) ?? null;
        }
        let created = false;
        if (!user) {
            user = await User.create({ name: args.name, role: args.role });
            created = true;
        }
        if (user.role !== args.role) {
            const updated = await user.update({ role: args.role });
            // A cancelled update resolves to undefined: the role write may
            // never have happened.
            if (!updated) return { error: "update_rejected" };
            user = game.users.get(user.id);
            if (!user || user.role !== args.role) return { error: "update_rejected" };
        }
        return { ok: { id: user.id, name: user.name, created } };
    }"#;

    const SET_PASSWORD: &str = r#"async function () {
        const args = __ARGS__;
        const user = game.users.get(args.userId);
        if (!user) return { error: "not_found", message: "user not found" };
        const updated = await user.update({ password: args.password });
        // A cancelled update resolves to undefined: the password is unchanged.
        if (!updated) return { error: "update_rejected" };
        return { ok: { id: user.id } };
    }"#;

    const ROTATE_PASSWORDS: &str = r#"async function () {
        const args = __ARGS__;
        let rotated = 0;
        for (const [id, password] of args.replacements) {
            const user = game.users.get(id);
            if (!user) return { error: "not_found", message: "user not found" };
            const updated = await user.update({ password });
            if (!updated) return { error: "update_rejected" };
            rotated += 1;
        }
        return { ok: { rotated } };
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
            // A cookie Foundry no longer accepts leaves the page without a user,
            // typically on `/join`.
            authenticated: !!window.game?.user,
            no_canvas: window.game?.settings?.get?.('core', 'noCanvas') === true,
            no_world: body.includes('no active game session'),
        };
    }"#;

    const SET_NO_CANVAS: &str = r#"async function () {
        if (!window.game?.settings?.set) return false;
        await window.game.settings.set('core', 'noCanvas', true);
        return true;
    }"#;

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{builder::Redirect, driver::CatalogPacks};

        /// `Page::evaluate` calls a script only when chromiumoxide 0.9's
        /// `is_likely_js_function` sees `function ` with a space
        /// (`src/utils.rs:78`); `async function()` is sent as an expression and
        /// Chromium rejects it with "Function statements require a function name".
        #[test]
        fn every_script_is_called_as_a_function() {
            for script in [
                LIST_DOCUMENTS,
                MODULE_ENABLE,
                CATALOG_PACKS,
                CATALOG_PAGE,
                CHARACTER_CREATE,
                GET_DOCUMENT,
                CHAT_CREATE,
                DICE_ROLL,
                SCENE_ACTIVATE,
                COMBAT_UPDATE,
                PLAYLIST_CONTROL,
                JOIN_USERS,
                ENSURE_USER,
                SET_PASSWORD,
                ROTATE_PASSWORDS,
                SET_NO_CANVAS,
            ] {
                let head = script.trim_start();
                let head = head.strip_prefix("async ").unwrap_or(head).trim_start();
                assert!(
                    head.starts_with("function ") || head.starts_with("() =>"),
                    "{}",
                    &script[..40.min(script.len())]
                );
            }
        }

        #[test]
        fn module_enable_rejects_cancelled_setting_without_reload() {
            let script = MODULE_ENABLE.replace("__ARGS__", "{}");
            let program = format!(
                r#"
                const assert = require('node:assert/strict');
                let reloads = 0;
                const stored = {{ "foundry-circle": false }};
                global.game = {{ modules: new Map([["foundry-circle", {{active: false}}]]),
                    settings: {{get: () => stored, set: async () => stored}} }};
                global.foundry = {{utils: {{deepClone: value => structuredClone(value)}}}};
                (async () => {{
                    const reply = await ({script})();
                    if (reply.ok?.enabled === true) reloads++;
                    assert.deepEqual(reply, {{error: "update_rejected"}});
                    assert.equal(reloads, 0);
                }})().catch(error => {{ console.error(error); process.exitCode = 1; }});
            "#
            );
            assert!(
                std::process::Command::new("node")
                    .arg("-e")
                    .arg(program)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        #[tokio::test]
        async fn abandoned_queued_request_is_skipped() {
            let (reply, response) = oneshot::channel::<Result<Value, WorldError>>();
            drop(response);
            assert!(!should_execute(&reply));
            let (reply, _response) = oneshot::channel::<Result<Value, WorldError>>();
            assert!(should_execute(&reply));
        }

        #[test]
        fn contract_validation_rejects_non_gm_and_mismatches() {
            let state = GameState {
                ready: true,
                world_id: Some("canary".into()),
                foundry_version: Some("13.351".into()),
                system_id: Some("daggerheart".into()),
                system_version: Some("1.6.4".into()),
                is_gm: false,
                authenticated: true,
                no_canvas: true,
                no_world: false,
            };
            let config = DriverConfig {
                base_url: "https://vtt.example".into(),
                api_user: "api".into(),
                foundry_version: "13.351".into(),
                system_id: Some("daggerheart".into()),
                system_version: Some("1.6.4".into()),
                chromium: "/bin/chromium".into(),
                profile_dir: "/tmp/foundry".into(),
            };
            assert_eq!(
                validate_state(&state, &config),
                Err(Failure::Authentication)
            );
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

        #[test]
        fn world_id_is_reported_but_no_longer_pinned() {
            let state = GameState {
                ready: true,
                world_id: Some("another-world".into()),
                foundry_version: Some("14.367".into()),
                system_id: Some("daggerheart".into()),
                system_version: Some("1.6.4".into()),
                is_gm: true,
                authenticated: true,
                no_canvas: true,
                no_world: false,
            };
            let config = DriverConfig {
                base_url: "http://127.0.0.1:8030".into(),
                api_user: "api".into(),
                foundry_version: "14.367".into(),
                system_id: None,
                system_version: None,
                chromium: "/bin/chromium".into(),
                profile_dir: "/tmp/foundry".into(),
            };
            let snapshot = validate_state(&state, &config).unwrap();
            assert_eq!(snapshot.id.as_deref(), Some("another-world"));
        }

        #[test]
        fn a_join_counts_only_on_a_success_body() {
            assert!(join_accepted(
                &json!({"status": "success", "redirect": "/game"})
            ));
            assert!(!join_accepted(&json!({"status": "failed"})));
            assert!(!join_accepted(&json!({})));
        }

        #[test]
        fn account_scripts_refuse_a_cancelled_update() {
            // Foundry's Document.update resolves `undefined` when the write was
            // cancelled; every script must report that as a failure (S2).
            for script in [ENSURE_USER, SET_PASSWORD, ROTATE_PASSWORDS] {
                assert!(
                    script.contains("if (!updated) return { error: \"update_rejected\" };"),
                    "a cancelled update must not report success"
                );
            }
            // A role change is only trusted after reading the user back.
            assert!(ENSURE_USER.contains(
                "if (!user || user.role !== args.role) return { error: \"update_rejected\" };"
            ));
            // Rotation stops on a user it could not update instead of counting
            // a partial success.
            assert!(ROTATE_PASSWORDS.contains("if (!user) return { error: \"not_found\""));
        }

        #[test]
        fn a_page_without_a_session_is_an_authentication_failure() {
            fn page(ready: bool, authenticated: bool, no_world: bool) -> GameState {
                GameState {
                    ready,
                    world_id: None,
                    foundry_version: None,
                    system_id: None,
                    system_version: None,
                    is_gm: false,
                    authenticated,
                    no_canvas: true,
                    no_world,
                }
            }
            // Foundry sent the cookie to `/join`: the hand-off is dead.
            assert_eq!(
                health_check(&page(false, false, false)),
                Err(Failure::Authentication)
            );
            // Still loading with a user: a browser stall, not an auth loss.
            assert_eq!(
                health_check(&page(false, true, false)),
                Err(Failure::Browser)
            );
            assert_eq!(
                health_check(&page(false, false, true)),
                Err(Failure::WorldStopped)
            );
            // A ready page with a non-GM user never serves operations.
            let player = page(true, true, false);
            assert_eq!(health_check(&player), Ok(()));
            let config = DriverConfig {
                base_url: "https://vtt.example".into(),
                api_user: "api".into(),
                foundry_version: "13.351".into(),
                system_id: None,
                system_version: None,
                chromium: "/bin/chromium".into(),
                profile_dir: "/tmp/foundry".into(),
            };
            assert_eq!(
                validate_state(&player, &config),
                Err(Failure::Authentication)
            );
        }

        #[test]
        fn a_timed_out_operation_abandons_the_session() {
            // S4: an inner op timeout leaves its script possibly running, so
            // the executor must tear the browser session down too.
            assert!(abandoned(&Err(WorldError::Timeout)));
            assert!(!abandoned(&Err(WorldError::NotReady)));
            assert!(!abandoned(&Err(WorldError::Browser)));
            assert!(!abandoned(&Ok(json!({"rotated": 1}))));
        }

        #[test]
        fn the_join_session_cookie_is_taken_from_its_own_set_cookie() {
            let mut headers = HeaderMap::new();
            headers.append(header::SET_COOKIE, "other=1; Path=/".parse().unwrap());
            headers.append(
                header::SET_COOKIE,
                "session=abc123; Max-Age=86400; Path=/; HttpOnly; SameSite=Strict"
                    .parse()
                    .unwrap(),
            );
            assert_eq!(foundry_session_cookie(&headers), Some("abc123".into()));
            assert_eq!(foundry_session_cookie(&HeaderMap::new()), None);
        }

        #[test]
        fn catalog_ops_validate_bounds() {
            fn page(pack: &str, after: Option<&str>, limit: u32) -> WorldOp {
                WorldOp::CatalogPage {
                    pack: pack.into(),
                    after: after.map(str::to_owned),
                    limit,
                }
            }
            assert_eq!(validate_op(&WorldOp::CatalogPacks), Ok(()));
            assert_eq!(validate_op(&page("dnd5e.classes24", None, 1)), Ok(()));
            assert_eq!(validate_op(&page("dnd5e.classes24", None, 50)), Ok(()));
            assert_eq!(
                validate_op(&page("dnd5e.classes24", Some("abc123"), 10)),
                Ok(())
            );
            for bad in [
                page("dnd5e.classes24", None, 0),
                page("dnd5e.classes24", None, 51),
                page("dnd5e/classes", None, 10),
                page("dnd5e classes", None, 10),
                page(&"d".repeat(101), None, 10),
                page("dnd5e.classes24", Some("a-b"), 10),
                page("dnd5e.classes24", Some(&"a".repeat(65)), 10),
            ] {
                assert!(
                    matches!(validate_op(&bad), Err(WorldError::Invalid(_))),
                    "{bad:?}"
                );
            }
        }

        #[test]
        fn fake_catalog_page_round_trips_as_catalog_items() {
            let packs = crate::driver::fake_call(WorldOp::CatalogPacks).unwrap();
            let pack: crate::builder::PackInfo =
                serde_json::from_value(packs["packs"][0].clone()).unwrap();
            assert_eq!(pack.id, "dnd5e.classes24");
            assert_eq!(pack.source_book.as_deref(), Some("SRD 5.2"));
            assert_eq!(pack.count, 1);

            let reply = crate::driver::fake_call(WorldOp::CatalogPage {
                pack: pack.id.clone(),
                after: None,
                // A full page yields a cursor; the next one ends the pack.
                limit: 1,
            })
            .unwrap();
            let items: Vec<crate::builder::CatalogItem> =
                serde_json::from_value(reply["items"].clone()).unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].name, "Cleric");
            assert_eq!(items[0].item_type, "class");
            assert_eq!(
                items[0].traits["2wUNkQzS4vL9nR6D"]["choices"][0]["count"],
                2
            );
            // A module with no equipment labels parses to an empty map.
            assert!(items[0].equipment.labels.is_empty());

            // The returned cursor is the pack index id, and the page after it is
            // the end of the pack.
            let cursor = reply["nextCursor"].as_str().unwrap().to_owned();
            let next = crate::driver::fake_call(WorldOp::CatalogPage {
                pack: pack.id,
                after: Some(cursor),
                limit: 50,
            })
            .unwrap();
            assert_eq!(next["items"].as_array().map(Vec::len), Some(0));
            assert!(next["nextCursor"].is_null());
        }

        #[test]
        fn module_errors_map_to_sync_codes() {
            let missing = WorldError::Unsupported("module_missing".into());
            let outdated = WorldError::Unsupported("module_outdated".into());
            assert_eq!(missing.catalog_code(), "module_missing");
            assert_eq!(outdated.catalog_code(), "module_outdated");
            assert_eq!(
                WorldError::Unsupported("unknown collection".into()).catalog_code(),
                "foundry_unavailable"
            );
            assert_eq!(WorldError::NotReady.catalog_code(), "foundry_unavailable");
            assert_eq!(WorldError::NotFound.catalog_code(), "foundry_unavailable");
        }

        #[test]
        fn catalog_scripts_fail_closed_without_the_module() {
            for script in [CATALOG_PACKS, CATALOG_PAGE] {
                assert!(
                    script.contains(r#"game.modules.get("foundry-circle")"#),
                    "a catalog script must read the module API only"
                );
                assert!(script.contains(
                    r#"if (!module) return { error: "unsupported", message: "module_missing" };"#
                ));
                assert!(script.contains(
                    r#"if (api.apiVersion !== 1) return { error: "unsupported", message: "module_outdated" };"#
                ));
            }
            // The page script forwards its one JSON argument and never builds a
            // second query of its own.
            assert!(CATALOG_PAGE.contains("__ARGS__"));
            assert!(CATALOG_PAGE.contains("api.catalogPage({ pack: args.pack"));
            // Both replies use the same `{ ok }` envelope as every other op.
            assert!(CATALOG_PACKS.contains("packs: await api.catalogPacks()"));
            assert!(CATALOG_PAGE.contains("return { ok: reply };"));
            // A module without the redirects function is not an error.
            assert!(CATALOG_PACKS.contains(
                r#"typeof api.uuidRedirects === "function" ? await api.uuidRedirects() : []"#
            ));
        }

        #[test]
        fn module_without_redirects_still_syncs() {
            // The fake module predates uuidRedirects(): its reply carries packs
            // only, and that parses to a complete answer with no redirects.
            let reply = crate::driver::fake_call(WorldOp::CatalogPacks).unwrap();
            assert!(reply.get("redirects").is_none());
            let parsed: CatalogPacks = serde_json::from_value(reply).unwrap();
            assert_eq!(parsed.packs.len(), 1);
            assert_eq!(parsed.packs[0].id, "dnd5e.classes24");
            assert!(parsed.redirects.is_empty());

            // A module that does send them keeps its pairs, still under
            // apiVersion 1.
            let parsed: CatalogPacks = serde_json::from_value(json!({
                "packs": [],
                "redirects": [{
                    "from": "Compendium.dnd-players-handbook.equipment",
                    "to": "Compendium.dnd5e.equipment24"
                }]
            }))
            .unwrap();
            assert_eq!(
                parsed.redirects,
                [Redirect {
                    from: "Compendium.dnd-players-handbook.equipment".into(),
                    to: "Compendium.dnd5e.equipment24".into(),
                }]
            );
            // Anything that is not a compendium pack prefix is dropped before
            // it can reach the table.
            let parsed: CatalogPacks = serde_json::from_value(json!({
                "packs": [],
                "redirects": [{"from": "dnd-players-handbook", "to": "Compendium.dnd5e.equipment24"}]
            }))
            .unwrap();
            assert!(!parsed.redirects[0].valid());
        }

        #[test]
        fn character_create_validates_bounds() {
            fn args() -> Value {
                json!({
                    "draftId": "0b6f4c2e-0000-4000-8000-000000000000",
                    "name": "Al",
                    "ownerUserId": "playeruser000001",
                    "level": 1,
                    "abilities": {"str": 15, "dex": 14, "con": 13, "int": 12, "wis": 10, "cha": 8},
                    "details": {"alignment": "Neutral"},
                    "biography": "",
                    "roots": [{
                        "uuid": "Compendium.dnd5e.classes24.Item.phbclcCleric0000",
                        "kind": "class"
                    }],
                    "choices": {
                        "Compendium.dnd5e.classes24.Item.phbclcCleric0000#2wUNkQzS4vL9nR6D#1":
                            {"chosen": ["skills:his", "skills:ins"]}
                    },
                    "items": [{
                        "uuid": "Compendium.dnd5e.equipment24.Item.phbwepMace000000",
                        "quantity": 1
                    }],
                    "gp": 7,
                    "wealth": [],
                    "manual": ["any holy focus"]
                })
            }
            fn check(args: Value) -> Result<(), WorldError> {
                validate_op(&WorldOp::CharacterCreate { args })
            }
            assert_eq!(check(args()), Ok(()));

            let mut bad = args();
            bad["name"] = json!("");
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["name"] = json!("n".repeat(101));
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["level"] = json!(0);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["level"] = json!(21);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["abilities"]["str"] = json!(31);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["abilities"].as_object_mut().unwrap().remove("cha");
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["roots"] = json!([]);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["roots"] = json!([
                {"uuid": "Actor.a1", "kind": "class"},
                {"uuid": "Actor.a2", "kind": "species"},
                {"uuid": "Actor.a3", "kind": "background"},
                {"uuid": "Actor.a4", "kind": "class"}
            ]);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["roots"][0]["kind"] = json!("subclass");
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["items"] = json!([{"uuid": "Item.mace", "quantity": 1}]);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["items"] = json!([{"uuid": "Compendium.dnd5e.equipment24.Item.phbwepMace000000", "quantity": 0}]);
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["choices"] = json!("chosen");
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["biography"] = json!("b".repeat(2001));
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
            let mut bad = args();
            bad["details"]["appearance"] = json!("a".repeat(2001));
            assert!(matches!(check(bad), Err(WorldError::Invalid(_))));
        }

        #[test]
        fn fake_character_create_is_idempotent_per_draft() {
            use crate::driver::FakeDriver;

            let driver = FakeDriver::new(WorldState::Ready);
            let args = json!({
                "draftId": "0b6f4c2e-0000-4000-8000-000000000000",
                "name": "Al",
                "manual": ["any holy focus"],
            });
            let first = driver.character_create(&args).unwrap();
            let again = driver.character_create(&args).unwrap();
            // A timed-out approval that is retried finds its own actor.
            assert_eq!(first["created"], json!(true));
            assert_eq!(again["created"], json!(false));
            assert_eq!(first["id"], again["id"]);
            assert_eq!(again["manual"], json!(["any holy focus"]));

            let other = driver
                .character_create(&json!({"draftId": "another-draft"}))
                .unwrap();
            assert_eq!(other["created"], json!(true));
            assert_ne!(other["id"], first["id"]);
        }

        #[test]
        fn character_create_fails_closed_without_the_module_function() {
            assert!(
                CHARACTER_CREATE.contains(r#"game.modules.get("foundry-circle")"#),
                "the script must read the module API only"
            );
            assert!(CHARACTER_CREATE.contains(
                r#"if (!module) return { error: "unsupported", message: "module_missing" };"#
            ));
            assert!(CHARACTER_CREATE.contains(
                r#"if (api.apiVersion !== 1) return { error: "unsupported", message: "module_outdated" };"#
            ));
            // A module that predates `createCharacter` is out of date, not missing.
            assert!(CHARACTER_CREATE.contains(
                r#"if (typeof api.createCharacter !== "function") {
            return { error: "unsupported", message: "module_outdated" };
        }"#
            ));
            // The one argument reaches the module whole, as a JSON literal.
            assert!(CHARACTER_CREATE.contains("const args = __ARGS__;"));
            assert!(CHARACTER_CREATE.contains("api.createCharacter(args)"));
            assert!(CHARACTER_CREATE.contains("return { ok: reply };"));
        }

        #[test]
        fn account_arguments_stay_json_encoded() {
            let name = "A \"quoted\"\nname \\ with\tcontrols";
            let (script, args) = account_script(&AccountOp::EnsureUser {
                user_id: None,
                name: name.into(),
                role: 4,
                reuse_by_name: false,
            })
            .unwrap();
            assert!(script.contains("__ARGS__"));
            let decoded: Value = serde_json::from_str(&args).unwrap();
            assert_eq!(decoded["name"], name);
            assert_eq!(decoded["role"], 4);
            assert_eq!(decoded["reuseByName"], false);
            // JSON escapes the newline, so the injected literal never breaks
            // out of its string.
            assert!(!args.contains('\n'));
            assert!(!args.contains('\t'));
        }
    }
}

#[cfg(feature = "server")]
pub use live::{ConfigError, DriverConfig, LiveFoundryDriver};

#[cfg(feature = "server")]
pub(crate) use live::{
    DriverPage, FoundryUser, foundry_session_cookie, join_session, login_as, page_account,
    page_users, rotate_passwords,
};

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn module_enable_preserves_other_modules_and_is_idempotent() {
        let driver = FakeDriver::new(WorldState::Ready);
        {
            let mut module = driver.module.lock().unwrap();
            module.1.insert("foundry-circle".into(), false);
            module.1.insert("other".into(), true);
        }
        assert_eq!(
            driver.call(WorldOp::ModuleEnable).await.unwrap(),
            json!({"enabled": true})
        );
        assert_eq!(
            driver.call(WorldOp::ModuleEnable).await.unwrap(),
            json!({"enabled": false})
        );
        assert_eq!(driver.module.lock().unwrap().1.get("other"), Some(&true));
    }

    #[tokio::test]
    async fn catalog_reports_inactive_module_separately() {
        let driver = FakeDriver::new(WorldState::Ready);
        driver
            .module
            .lock()
            .unwrap()
            .1
            .insert("foundry-circle".into(), false);
        assert_eq!(
            driver
                .call(WorldOp::CatalogPacks)
                .await
                .unwrap_err()
                .catalog_code(),
            "module_inactive"
        );
        driver.module.lock().unwrap().0 = false;
        assert_eq!(
            driver
                .call(WorldOp::CatalogPacks)
                .await
                .unwrap_err()
                .catalog_code(),
            "module_missing"
        );
        assert_eq!(
            driver
                .call(WorldOp::ModuleEnable)
                .await
                .unwrap_err()
                .catalog_code(),
            "module_missing"
        );
    }
}
