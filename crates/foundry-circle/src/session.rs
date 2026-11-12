//! The one-GM session lease (G3).
//!
//! One `game_session` row is open at most; the schema's partial unique index
//! enforces it. `reconcile` is a pure decision table over what Foundry reports,
//! and the operations below are the only writers. Every transition is audited.
//!
//! Foundry keeps sessions in memory, so a restart drops them all. Ending a
//! lease therefore stops the world, restarts Foundry, and only closes the row
//! once the next poll still reports no active world.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::http::header;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::adoption;
use crate::driver::{DriverConfig, LiveFoundryDriver};
use crate::http::DatabaseState;
use crate::{GameStatus, Role, SessionHolder, SessionPhase, SessionView};

/// A holder absent for this long loses the lease (D3).
const ABSENCE_MINUTES: i64 = 30;
const POLL_SECONDS: u64 = 5;

/// What the loopback `/api/status` poll says about Foundry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundryObservation {
    /// No answer within the poll timeout: a state, never a failure.
    Unreachable,
    /// Foundry answers and no world is launched.
    Idle,
    /// A world is launched; its id is optional because the payload varies.
    Active { world: Option<String> },
}

/// One step the reconciler may take. Nothing here touches Foundry by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Foundry has a world and the lease owns it: `running` with this world.
    Running { world: Option<String> },
    /// The lease lives, Foundry stopped the world: back to `setup`.
    Setup,
    /// `ending` with a world still up: ask Foundry to stop it.
    Shutdown,
    /// `ending` and idle: Foundry must restart before the lease closes.
    Restart,
    /// `ending`, restarted, and still idle: close the lease.
    End,
    /// The absence limit passed: begin stopping (D3).
    AbsentStop,
    /// A world runs with no lease: report only, never act.
    Unmanaged,
}

/// The pure lease decision table. `restarted` records that this session's
/// Foundry restart already succeeded, so the following idle poll may end it.
pub fn reconcile(
    open: Option<&OpenSession>,
    observed: &FoundryObservation,
    restarted: bool,
    now: DateTime<Utc>,
) -> Vec<Action> {
    // Never end a lease because Foundry failed to answer.
    if *observed == FoundryObservation::Unreachable {
        return Vec::new();
    }
    let Some(session) = open else {
        return match observed {
            FoundryObservation::Active { .. } => vec![Action::Unmanaged],
            _ => Vec::new(),
        };
    };
    if session.phase == SessionPhase::Ending {
        return match observed {
            // The world must be gone before Foundry restarts.
            FoundryObservation::Active { .. } => vec![Action::Shutdown],
            FoundryObservation::Idle if restarted => vec![Action::End],
            FoundryObservation::Idle => vec![Action::Restart],
            FoundryObservation::Unreachable => Vec::new(),
        };
    }
    if now - session.last_seen > chrono::Duration::minutes(ABSENCE_MINUTES) {
        return vec![Action::AbsentStop];
    }
    match (session.phase, observed) {
        (SessionPhase::Setup, FoundryObservation::Active { world }) => vec![Action::Running {
            world: world.clone(),
        }],
        (SessionPhase::Running, FoundryObservation::Active { world })
            if *world != session.world_id =>
        {
            vec![Action::Running {
                world: world.clone(),
            }]
        }
        (SessionPhase::Running, FoundryObservation::Idle) => vec![Action::Setup],
        _ => Vec::new(),
    }
}

/// The open lease plus the display name of its holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSession {
    pub id: Uuid,
    pub holder: String,
    pub holder_name: String,
    pub phase: SessionPhase,
    pub world_id: Option<String>,
    /// The running world has completed adoption (G5).
    pub adopted: bool,
    pub started_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// Who is asking. The role itself is resolved from the OIDC groups elsewhere.
#[derive(Debug, Clone, Copy)]
pub struct Caller<'a> {
    pub subject: &'a str,
    pub is_admin: bool,
}

/// Only a GM or an admin may open the one session.
pub fn may_start(role: Role) -> bool {
    role != Role::Player
}

/// The holder or an admin may drive (or read through) an open lease.
pub const fn may_operate(is_holder: bool, is_admin: bool) -> bool {
    is_holder || is_admin
}

/// The broker's Foundry-facing half of the lease: status polls, the admin-key
/// shutdown, and the restart that drops every Foundry session.
#[derive(Clone)]
pub struct Lease {
    config: Arc<LeaseConfig>,
    /// Session whose restart already succeeded; the next idle poll ends it.
    restarted: Arc<Mutex<Option<Uuid>>>,
    /// The broker's own admin session for adoption, keyed by lease id. It is
    /// never relayed to a browser or to the steady-state driver.
    bootstrap: Arc<Mutex<HashMap<Uuid, String>>>,
    /// The last adoption attempt, so a failing world does not start a
    /// Chromium every poll.
    adoption_attempt: Arc<Mutex<Option<(Uuid, Instant)>>>,
}

#[derive(Debug)]
struct LeaseConfig {
    base_url: Option<String>,
    admin_password_file: Option<PathBuf>,
    restart_unit: String,
    systemctl: String,
}

impl Default for Lease {
    fn default() -> Self {
        Self {
            config: Arc::new(LeaseConfig {
                base_url: None,
                admin_password_file: None,
                restart_unit: "foundryvtt.service".into(),
                systemctl: "systemctl".into(),
            }),
            restarted: Arc::new(Mutex::new(None)),
            bootstrap: Arc::new(Mutex::new(HashMap::new())),
            adoption_attempt: Arc::new(Mutex::new(None)),
        }
    }
}

impl Lease {
    /// Read once at startup. An absent admin password file leaves the lease
    /// observing only: the endpoints answer 503 and nothing drives Foundry.
    pub fn from_env(base_url: &str) -> Self {
        let value = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Self {
            config: Arc::new(LeaseConfig {
                base_url: Some(base_url.trim_end_matches('/').to_owned())
                    .filter(|value| !value.is_empty()),
                admin_password_file: value("FOUNDRY_ADMIN_PASSWORD_FILE").map(PathBuf::from),
                restart_unit: value("FOUNDRY_CIRCLE_RESTART_UNIT")
                    .unwrap_or_else(|| "foundryvtt.service".into()),
                systemctl: value("FOUNDRY_CIRCLE_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()),
            }),
            restarted: Arc::new(Mutex::new(None)),
            bootstrap: Arc::new(Mutex::new(HashMap::new())),
            adoption_attempt: Arc::new(Mutex::new(None)),
        }
    }

    /// Driving Foundry needs both an endpoint and a readable admin credential.
    /// Without them the endpoints answer 503 and the reconciler only observes.
    pub fn enabled(&self) -> bool {
        self.config.base_url.is_some() && self.admin_password().is_some()
    }

    /// Read at use time so a rotated credential needs no restart.  The value
    /// never leaves this module: it is only handed to `shutdown`.
    fn admin_password(&self) -> Option<String> {
        let path: &Path = self.config.admin_password_file.as_deref()?;
        let password = std::fs::read_to_string(path).ok()?;
        let password = password.trim();
        (!password.is_empty()).then(|| password.to_owned())
    }

    /// Poll Foundry's public `/api/status`; any failure is an observation.
    pub async fn observe(&self) -> FoundryObservation {
        let Some(base) = self.config.base_url.as_deref() else {
            return FoundryObservation::Unreachable;
        };
        let Ok(response) = client().get(format!("{base}/api/status")).send().await else {
            return FoundryObservation::Unreachable;
        };
        if !response.status().is_success() {
            return FoundryObservation::Unreachable;
        }
        match response.json::<GameStatus>().await {
            Ok(status) => observation(status),
            Err(_) => FoundryObservation::Unreachable,
        }
    }

    /// Stop the launched world with the admin key. Only a 2xx answer whose JSON
    /// says `status: success` counts; anything else leaves the world alone.
    pub async fn shutdown(&self) -> Result<(), ShutdownFailure> {
        let Some(base) = self.config.base_url.as_deref() else {
            return Err(ShutdownFailure);
        };
        let Some(password) = self.admin_password() else {
            tracing::warn!("the Foundry admin credential is not readable; the world cannot stop");
            return Err(ShutdownFailure);
        };
        let reply = client()
            .post(format!("{base}/join"))
            .json(&json!({"action": "shutdown", "adminPassword": password}))
            .send()
            .await
            .map_err(|_| ShutdownFailure)?;
        if !reply.status().is_success() {
            return Err(ShutdownFailure);
        }
        let body: Value = reply.json().await.map_err(|_| ShutdownFailure)?;
        if shutdown_accepted(&body) {
            Ok(())
        } else {
            Err(ShutdownFailure)
        }
    }

    /// Open a fresh admin session: a cookie-less `POST /auth` with the admin
    /// key, redirects disabled. The body is form-encoded, as Foundry's own
    /// login form sends it. `/_circle/setup` calls this once for the holder's
    /// browser and once for the broker's adoption session; the broker keeps
    /// the second cookie in memory and never relays it.
    pub async fn open_setup(&self) -> Result<String, SetupFailure> {
        let Some(base) = self.config.base_url.as_deref() else {
            return Err(SetupFailure);
        };
        let Some(password) = self.admin_password() else {
            tracing::warn!("the Foundry admin credential is not readable; Setup cannot open");
            return Err(SetupFailure);
        };
        let reply = post_client()
            .post(format!("{base}/auth"))
            .form(&[("adminPassword", password.as_str())])
            .send()
            .await
            .map_err(|_| SetupFailure)?;
        let session = crate::driver::foundry_session_cookie(reply.headers()).ok_or(SetupFailure)?;
        let location = reply
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !is_setup_redirect(location) {
            return Err(SetupFailure);
        }
        Ok(session)
    }

    /// Restart Foundry through systemd; the deployment's polkit rule allows
    /// exactly this restart of exactly this unit.
    pub async fn restart(&self) -> bool {
        let status = tokio::process::Command::new(&self.config.systemctl)
            .arg("restart")
            .arg(&self.config.restart_unit)
            .status()
            .await;
        match status {
            Ok(status) if status.success() => {
                tracing::info!(unit = %self.config.restart_unit, "restarted Foundry to drop its sessions");
                true
            }
            Ok(status) => {
                tracing::warn!(unit = %self.config.restart_unit, ?status, "Foundry restart failed");
                false
            }
            Err(error) => {
                tracing::warn!(unit = %self.config.restart_unit, %error, "Foundry restart could not run");
                false
            }
        }
    }

    fn restart_done(&self, id: Uuid) -> bool {
        *self.restarted.lock().expect("lease restart lock poisoned") == Some(id)
    }

    fn mark_restarted(&self, id: Uuid) {
        *self.restarted.lock().expect("lease restart lock poisoned") = Some(id);
    }

    fn clear_restarted(&self) {
        *self.restarted.lock().expect("lease restart lock poisoned") = None;
    }

    /// Keep the broker's own admin session for this lease's adoption.
    pub fn remember_bootstrap(&self, id: Uuid, cookie: String) {
        self.bootstrap
            .lock()
            .expect("lease bootstrap lock poisoned")
            .insert(id, cookie);
    }

    /// The adoption cookie, if this broker process obtained one. A restart
    /// loses it: the world must return to Setup and relaunch to adopt.
    pub fn bootstrap_cookie(&self, id: Uuid) -> Option<String> {
        self.bootstrap
            .lock()
            .expect("lease bootstrap lock poisoned")
            .get(&id)
            .cloned()
    }

    pub fn forget_bootstrap(&self, id: Uuid) {
        self.bootstrap
            .lock()
            .expect("lease bootstrap lock poisoned")
            .remove(&id);
    }

    /// At most one adoption attempt every 30 s per lease.
    pub fn adoption_due(&self, id: Uuid) -> bool {
        let mut attempt = self
            .adoption_attempt
            .lock()
            .expect("lease adoption lock poisoned");
        match *attempt {
            Some((attempted, at)) if attempted == id && at.elapsed() < Duration::from_secs(30) => {
                false
            }
            _ => {
                *attempt = Some((id, Instant::now()));
                true
            }
        }
    }

    pub fn clear_adoption_attempt(&self) {
        *self
            .adoption_attempt
            .lock()
            .expect("lease adoption lock poisoned") = None;
    }
}

/// Foundry refused the shutdown or did not answer.
#[derive(Debug, thiserror::Error)]
#[error("Foundry did not stop the world")]
pub struct ShutdownFailure;

/// Foundry refused the admin login or did not answer.
#[derive(Debug, thiserror::Error)]
#[error("Foundry did not open Setup")]
pub struct SetupFailure;

/// A refused lease operation, with the code the JSON API reports.
#[derive(Debug)]
pub enum LeaseError {
    /// No admin credential and no Foundry endpoint are configured.
    NotConfigured,
    /// Another lease is already open.
    SessionInProgress,
    /// A world runs with no lease, so starting would orphan it.
    UnmanagedWorld,
    NotFound,
    Forbidden,
    /// The lease is not in a phase that allows this operation.
    Conflict,
    Foundry,
    Database(sqlx::Error),
}

impl LeaseError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "lease_not_configured",
            Self::SessionInProgress => "session_in_progress",
            Self::UnmanagedWorld => "unmanaged_world",
            Self::NotFound => "session_not_found",
            Self::Forbidden => "permission_denied",
            Self::Conflict => "session_phase_conflict",
            Self::Foundry => "foundry_unavailable",
            Self::Database(_) => "database_unavailable",
        }
    }

    pub const fn status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Self::NotConfigured | Self::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::SessionInProgress | Self::UnmanagedWorld | Self::Conflict => StatusCode::CONFLICT,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Foundry => StatusCode::BAD_GATEWAY,
        }
    }
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(source) => write!(formatter, "database: {source}"),
            other => formatter.write_str(other.code()),
        }
    }
}

impl From<sqlx::Error> for LeaseError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// The open lease, if any; the table has at most one.
pub async fn open(pool: &PgPool) -> Result<Option<OpenSession>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT s.id, s.holder, COALESCE(p.display_name, p.email, p.subject) AS holder_name,
                s.phase, s.world_id, s.started_at, s.last_seen,
                EXISTS (SELECT 1 FROM adopted_worlds a WHERE a.world_id = s.world_id) AS adopted
           FROM game_session s
           JOIN principals p ON p.subject = s.holder
          WHERE s.ended_at IS NULL",
    )
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_session).transpose()
}

/// The API and console rendering of one lease.
pub fn view(session: &OpenSession, viewer: &str) -> SessionView {
    SessionView {
        id: session.id.to_string(),
        phase: session.phase,
        holder: SessionHolder {
            name: session.holder_name.clone(),
        },
        world_id: session.world_id.clone(),
        started_at: session.started_at.to_rfc3339(),
        is_holder: session.holder == viewer,
    }
}

/// Open a lease in `setup`. A world launched outside a lease must be stopped
/// by an operator first, so a start cannot adopt it by accident. Callers check
/// `may_start` before this; the row itself has no role column.
pub async fn start(pool: &PgPool, lease: &Lease, subject: &str) -> Result<OpenSession, LeaseError> {
    if !lease.enabled() {
        return Err(LeaseError::NotConfigured);
    }
    if open(pool).await?.is_some() {
        return Err(LeaseError::SessionInProgress);
    }
    // Only an explicit idle answer proves no world runs outside a lease; an
    // unreachable Foundry may still host one.
    match lease.observe().await {
        FoundryObservation::Idle => {}
        FoundryObservation::Active { .. } => return Err(LeaseError::UnmanagedWorld),
        FoundryObservation::Unreachable => return Err(LeaseError::Foundry),
    }
    let id = Uuid::new_v4();
    let mut transaction = pool.begin().await?;
    match sqlx::query("INSERT INTO game_session (id, holder, phase) VALUES ($1, $2, 'setup')")
        .bind(id)
        .bind(subject)
        .execute(&mut *transaction)
        .await
    {
        Ok(_) => {}
        Err(error) if is_unique_violation(&error) => return Err(LeaseError::SessionInProgress),
        Err(error) => return Err(error.into()),
    }
    insert_audit(
        &mut *transaction,
        Some(subject),
        "session.start",
        "ok",
        json!({"sessionId": id}),
    )
    .await?;
    transaction.commit().await?;
    open(pool).await?.ok_or(LeaseError::NotFound)
}

/// Stop the world with the admin key and keep the lease in `setup`.
pub async fn return_to_setup(
    pool: &PgPool,
    lease: &Lease,
    id: Uuid,
    caller: Caller<'_>,
) -> Result<OpenSession, LeaseError> {
    if !lease.enabled() {
        return Err(LeaseError::NotConfigured);
    }
    let mut transaction = pool.begin().await?;
    let session = lock(&mut transaction, id)
        .await?
        .ok_or(LeaseError::NotFound)?;
    if !may_operate(session.holder == caller.subject, caller.is_admin) {
        return Err(LeaseError::Forbidden);
    }
    if session.phase != SessionPhase::Running {
        return Err(LeaseError::Conflict);
    }
    // Foundry stops the world first: a failure leaves the lease untouched.
    lease.shutdown().await.map_err(|_| LeaseError::Foundry)?;
    sqlx::query(
        "UPDATE game_session SET phase = 'setup', world_id = NULL, last_seen = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    insert_audit(
        &mut *transaction,
        Some(caller.subject),
        "session.setup",
        "ok",
        json!({"sessionId": id}),
    )
    .await?;
    transaction.commit().await?;
    open(pool).await?.ok_or(LeaseError::NotFound)
}

/// Begin ending the lease. The reconciler finishes it: restart Foundry, then
/// close the row. An admin uses the same call as the holder.
pub async fn stop(
    pool: &PgPool,
    lease: &Lease,
    id: Uuid,
    caller: Caller<'_>,
    reason: &str,
) -> Result<OpenSession, LeaseError> {
    if !lease.enabled() {
        return Err(LeaseError::NotConfigured);
    }
    let mut transaction = pool.begin().await?;
    let session = lock(&mut transaction, id)
        .await?
        .ok_or(LeaseError::NotFound)?;
    if !may_operate(session.holder == caller.subject, caller.is_admin) {
        return Err(LeaseError::Forbidden);
    }
    if session.phase != SessionPhase::Ending {
        sqlx::query("UPDATE game_session SET phase = 'ending', end_reason = $2, last_seen = now() WHERE id = $1")
            .bind(id)
            .bind(reason)
            .execute(&mut *transaction)
            .await?;
        insert_audit(
            &mut *transaction,
            Some(caller.subject),
            "session.stop",
            "ok",
            json!({"sessionId": id, "reason": reason}),
        )
        .await?;
    }
    transaction.commit().await?;
    // The reconciler alone drives Foundry for an ending lease (within one
    // poll), so no request can act on a lease that has since been replaced.
    open(pool).await?.ok_or(LeaseError::NotFound)
}

/// Record holder activity; called by every authenticated broker request.
/// Best effort and detached: `/_circle/enter` holds the lease row lock across
/// its Foundry round trips, and presence must never stall the gate behind it.
pub async fn touch(pool: &PgPool, subject: &str) {
    let (pool, subject) = (pool.clone(), subject.to_owned());
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE game_session SET last_seen = now() WHERE ended_at IS NULL AND holder = $1",
        )
        .bind(&subject)
        .execute(&pool)
        .await
        {
            tracing::warn!(%error, "failed to record Foundry Circle session activity");
        }
    });
}

/// Drive the lease from the same `/api/status` poll the lobby uses. Runs
/// whenever a database is available; without a credential it only observes.
pub fn spawn_reconciler(
    database: DatabaseState,
    lease: Lease,
    driver: Arc<LiveFoundryDriver>,
    config: DriverConfig,
) {
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(Duration::from_secs(POLL_SECONDS));
        loop {
            poll.tick().await;
            if let Some(pool) = database.pool().await {
                reconcile_once(&pool, &lease, &driver, &config).await;
            }
        }
    });
}

async fn reconcile_once(
    pool: &PgPool,
    lease: &Lease,
    driver: &LiveFoundryDriver,
    config: &DriverConfig,
) {
    let open = match open(pool).await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, "failed to read the open Foundry Circle session");
            return;
        }
    };
    let observed = lease.observe().await;
    let restarted = open
        .as_ref()
        .is_some_and(|session| lease.restart_done(session.id));
    let mut actions = reconcile(open.as_ref(), &observed, restarted, Utc::now());
    if actions.contains(&Action::Unmanaged) {
        // Reported by the API and the console; the reconciler never acts on it.
        tracing::debug!("a Foundry world is active with no open session");
        actions.retain(|action| *action != Action::Unmanaged);
    }
    if !lease.enabled() {
        if !actions.is_empty() {
            tracing::debug!(
                ?actions,
                "the session lease is not configured; observing only"
            );
        }
        return;
    }
    let Some(session) = open else {
        driver.clear_session().await;
        return;
    };
    for action in actions {
        if let Err(error) = apply(pool, lease, &session, action).await {
            tracing::warn!(%error, "failed to apply a session lease action");
        }
    }
    drive(pool, lease, driver, config).await;
}

/// Keep the live driver in step with the lease: adopt an unadopted running
/// world once, then log the driver in as the adopted world's API user. Only a
/// `running` lease with an adopted world ever holds a driver session.
async fn drive(pool: &PgPool, lease: &Lease, driver: &LiveFoundryDriver, config: &DriverConfig) {
    let open = match open(pool).await {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, "failed to read the open Foundry Circle session");
            return;
        }
    };
    let Some(session) = open else {
        driver.clear_session().await;
        lease.clear_adoption_attempt();
        return;
    };
    if session.phase != SessionPhase::Running {
        driver.clear_session().await;
        lease.clear_adoption_attempt();
        return;
    }
    let Some(world) = session.world_id.as_deref() else {
        driver.clear_session().await;
        return;
    };
    if !session.adopted {
        driver.clear_session().await;
        if lease.adoption_due(session.id) {
            match adoption::adopt(pool, lease, config, driver, &session, world).await {
                Ok(()) => lease.clear_adoption_attempt(),
                Err(error) => tracing::debug!(?error, world, "world adoption is pending"),
            }
        }
        return;
    }
    if let Err(error) = adoption::prepare_driver(pool, driver, config, world).await {
        tracing::warn!(?error, world, "the Foundry driver session is not ready");
    }
}

/// Apply one action, conditioned on this session and the phase it was decided
/// against: a late decision never resurrects a session that is ending.
async fn apply(
    pool: &PgPool,
    lease: &Lease,
    session: &OpenSession,
    action: Action,
) -> Result<(), LeaseError> {
    let id = session.id;
    match action {
        Action::Running { world } => {
            let mut transaction = pool.begin().await?;
            let updated = sqlx::query(
                "UPDATE game_session SET phase = 'running', world_id = $2
                  WHERE id = $1 AND ended_at IS NULL AND phase IN ('setup', 'running')",
            )
            .bind(id)
            .bind(&world)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            if updated == 1 {
                insert_audit(
                    &mut *transaction,
                    Some(&session.holder),
                    "session.running",
                    "ok",
                    json!({"sessionId": id, "worldId": world}),
                )
                .await?;
            }
            transaction.commit().await?;
        }
        Action::Setup => {
            let mut transaction = pool.begin().await?;
            let updated = sqlx::query(
                "UPDATE game_session SET phase = 'setup', world_id = NULL
                  WHERE id = $1 AND ended_at IS NULL AND phase = 'running'",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            if updated == 1 {
                insert_audit(
                    &mut *transaction,
                    Some(&session.holder),
                    "session.setup",
                    "ok",
                    json!({"sessionId": id, "reason": "foundry_idle"}),
                )
                .await?;
            }
            transaction.commit().await?;
        }
        Action::Shutdown => {
            let outcome = match lease.shutdown().await {
                Ok(()) => "ok",
                Err(error) => {
                    tracing::warn!(%error, "Foundry refused to stop the world");
                    "failed"
                }
            };
            record(
                pool,
                Some(&session.holder),
                "session.shutdown",
                outcome,
                json!({"sessionId": id}),
            )
            .await;
        }
        Action::Restart => {
            let restarted = lease.restart().await;
            if restarted {
                lease.mark_restarted(id);
            }
            record(
                pool,
                Some(&session.holder),
                "session.restart",
                if restarted { "ok" } else { "failed" },
                json!({"sessionId": id}),
            )
            .await;
        }
        Action::End => {
            let mut transaction = pool.begin().await?;
            let updated = sqlx::query(
                "UPDATE game_session SET ended_at = now()
                  WHERE id = $1 AND ended_at IS NULL AND phase = 'ending'",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            if updated == 1 {
                insert_audit(
                    &mut *transaction,
                    Some(&session.holder),
                    "session.ended",
                    "ok",
                    json!({"sessionId": id}),
                )
                .await?;
            }
            transaction.commit().await?;
            lease.clear_restarted();
            lease.forget_bootstrap(id);
            lease.clear_adoption_attempt();
        }
        Action::AbsentStop => {
            let mut transaction = pool.begin().await?;
            let updated = sqlx::query(
                "UPDATE game_session SET phase = 'ending', end_reason = 'absence'
                  WHERE id = $1 AND ended_at IS NULL AND phase IN ('setup', 'running')
                    AND last_seen < now() - make_interval(mins => $2)",
            )
            .bind(id)
            .bind(ABSENCE_MINUTES as i32)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            if updated == 1 {
                insert_audit(
                    &mut *transaction,
                    Some(&session.holder),
                    "session.absent",
                    "ok",
                    json!({"sessionId": id}),
                )
                .await?;
            }
            transaction.commit().await?;
        }
        // Reported by the reconciler before it filters this action out.
        Action::Unmanaged => {}
    }
    Ok(())
}

async fn lock(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<OpenSession>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT s.id, s.holder, COALESCE(p.display_name, p.email, p.subject) AS holder_name,
                s.phase, s.world_id, s.started_at, s.last_seen,
                EXISTS (SELECT 1 FROM adopted_worlds a WHERE a.world_id = s.world_id) AS adopted
           FROM game_session s
           JOIN principals p ON p.subject = s.holder
          WHERE s.id = $1 AND s.ended_at IS NULL
          FOR UPDATE OF s",
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref().map(row_session).transpose()
}

fn row_session(row: &sqlx::postgres::PgRow) -> Result<OpenSession, sqlx::Error> {
    let phase: String = row.try_get("phase")?;
    Ok(OpenSession {
        id: row.try_get("id")?,
        holder: row.try_get("holder")?,
        holder_name: row.try_get("holder_name")?,
        phase: SessionPhase::from_db(&phase)
            .ok_or_else(|| sqlx::Error::Decode(format!("unknown session phase {phase}").into()))?,
        world_id: row.try_get("world_id")?,
        adopted: row.try_get("adopted")?,
        started_at: row.try_get("started_at")?,
        last_seen: row.try_get("last_seen")?,
    })
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some("23505"))
}

/// Best-effort audit for a transition that already happened on its own.
async fn record(pool: &PgPool, subject: Option<&str>, action: &str, outcome: &str, details: Value) {
    if let Err(error) = insert_audit(pool, subject, action, outcome, details).await {
        tracing::warn!(%error, "failed to write the Foundry Circle session audit entry");
    }
}

/// Shared audit writer for the lease and the play-host gate.
pub(crate) async fn insert_audit(
    executor: impl sqlx::PgExecutor<'_>,
    subject: Option<&str>,
    action: &str,
    outcome: &str,
    details: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_log (subject, action, resource, outcome, details) VALUES ($1, $2, 'game_session', $3, $4)",
    )
    .bind(subject)
    .bind(action)
    .bind(outcome)
    .bind(details)
    .execute(executor)
    .await
    .map(|_| ())
}

/// The status payload mapping, separate from the request so it stays testable.
fn observation(status: GameStatus) -> FoundryObservation {
    if status.active {
        FoundryObservation::Active {
            world: status.world,
        }
    } else {
        FoundryObservation::Idle
    }
}

/// Foundry answers a shutdown with `status: success`; anything else, including
/// a wrong-password error body, is a failure.
fn shutdown_accepted(body: &Value) -> bool {
    body.get("status").and_then(Value::as_str) == Some("success")
}

/// A successful admin login redirects to Setup; a refusal goes back to
/// `/auth`. The path may be absolute.
fn is_setup_redirect(location: &str) -> bool {
    let path = location.split(['?', '#']).next().unwrap_or(location);
    path.trim_end_matches('/').ends_with("/setup")
}

/// The same short-timeout client pattern as the lobby's `/api/status` read.
fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default()
    })
}

/// The admin login must not follow Foundry's redirect: its reply carries the
/// cookie and the Location the decision is made from.
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn session_at(
        phase: SessionPhase,
        world: Option<&str>,
        last_seen_minutes: i64,
        now: DateTime<Utc>,
    ) -> OpenSession {
        OpenSession {
            id: Uuid::nil(),
            holder: "gm".into(),
            holder_name: "GM".into(),
            phase,
            world_id: world.map(str::to_owned),
            adopted: false,
            started_at: now,
            last_seen: now - chrono::Duration::minutes(last_seen_minutes),
        }
    }

    fn session(phase: SessionPhase, world: Option<&str>, last_seen_minutes: i64) -> OpenSession {
        session_at(phase, world, last_seen_minutes, Utc::now())
    }

    fn active(world: Option<&str>) -> FoundryObservation {
        FoundryObservation::Active {
            world: world.map(str::to_owned),
        }
    }

    fn actions(open: &OpenSession, observed: &FoundryObservation) -> Vec<Action> {
        reconcile(Some(open), observed, false, Utc::now())
    }

    #[test]
    fn setup_with_a_launched_world_becomes_running() {
        assert_eq!(
            actions(
                &session(SessionPhase::Setup, None, 0),
                &active(Some("canary"))
            ),
            vec![Action::Running {
                world: Some("canary".into())
            }]
        );
        assert_eq!(
            actions(&session(SessionPhase::Setup, None, 0), &active(None)),
            vec![Action::Running { world: None }]
        );
        assert!(
            actions(
                &session(SessionPhase::Setup, None, 0),
                &FoundryObservation::Idle
            )
            .is_empty()
        );
    }

    #[test]
    fn running_follows_the_world_and_returns_to_setup_when_it_stops() {
        assert_eq!(
            actions(
                &session(SessionPhase::Running, Some("canary"), 0),
                &FoundryObservation::Idle
            ),
            vec![Action::Setup]
        );
        assert_eq!(
            actions(
                &session(SessionPhase::Running, Some("canary"), 0),
                &active(Some("other"))
            ),
            vec![Action::Running {
                world: Some("other".into())
            }]
        );
        assert!(
            actions(
                &session(SessionPhase::Running, Some("canary"), 0),
                &active(Some("canary"))
            )
            .is_empty()
        );
    }

    #[test]
    fn ending_stops_the_world_before_restarting_and_only_then_ends() {
        let ending = session(SessionPhase::Ending, Some("canary"), 0);
        assert_eq!(
            actions(&ending, &active(Some("canary"))),
            vec![Action::Shutdown]
        );
        assert_eq!(
            reconcile(Some(&ending), &FoundryObservation::Idle, false, Utc::now()),
            vec![Action::Restart]
        );
        assert_eq!(
            reconcile(Some(&ending), &FoundryObservation::Idle, true, Utc::now()),
            vec![Action::End]
        );
    }

    #[test]
    fn absence_begins_a_stop_for_every_open_phase() {
        let now = Utc::now();
        for phase in [SessionPhase::Setup, SessionPhase::Running] {
            let stale = session_at(phase, None, ABSENCE_MINUTES + 1, now);
            assert_eq!(
                reconcile(Some(&stale), &FoundryObservation::Idle, false, now),
                vec![Action::AbsentStop]
            );
        }
        // Exactly at the limit the holder is still there.
        let fresh = session_at(SessionPhase::Setup, None, ABSENCE_MINUTES, now);
        assert!(reconcile(Some(&fresh), &FoundryObservation::Idle, false, now).is_empty());
        // Ending is already stopping; absence changes nothing.
        let ending = session_at(SessionPhase::Ending, None, ABSENCE_MINUTES + 1, now);
        assert_eq!(
            reconcile(Some(&ending), &FoundryObservation::Idle, true, now),
            vec![Action::End]
        );
    }

    #[test]
    fn unreachable_foundry_never_ends_a_lease() {
        for phase in [
            SessionPhase::Setup,
            SessionPhase::Running,
            SessionPhase::Ending,
        ] {
            let open = session(phase, Some("canary"), ABSENCE_MINUTES + 1);
            assert!(
                reconcile(
                    Some(&open),
                    &FoundryObservation::Unreachable,
                    true,
                    Utc::now()
                )
                .is_empty(),
                "{phase:?}"
            );
        }
        assert!(reconcile(None, &FoundryObservation::Unreachable, false, Utc::now()).is_empty());
    }

    #[test]
    fn a_world_without_a_lease_is_only_reported() {
        assert_eq!(
            reconcile(None, &active(Some("canary")), false, Utc::now()),
            vec![Action::Unmanaged]
        );
        assert!(reconcile(None, &FoundryObservation::Idle, false, Utc::now()).is_empty());
    }

    #[test]
    fn only_a_gm_or_admin_may_open_the_session() {
        assert!(!may_start(Role::Player));
        assert!(may_start(Role::Gm));
        assert!(may_start(Role::Admin));
    }

    #[test]
    fn only_the_holder_or_an_admin_may_drive_the_lease() {
        assert!(
            !may_operate(false, false),
            "a GM without the lease is refused"
        );
        assert!(may_operate(true, false), "the holder may drive it");
        assert!(may_operate(false, true), "an admin may override");
    }

    #[test]
    fn a_lease_without_a_credential_is_not_enabled() {
        assert!(!Lease::default().enabled());
    }

    #[test]
    fn a_status_payload_maps_to_an_observation() {
        assert_eq!(
            observation(GameStatus {
                active: false,
                ..GameStatus::default()
            }),
            FoundryObservation::Idle
        );
        assert_eq!(
            observation(GameStatus {
                active: true,
                world: Some("canary".into()),
                ..GameStatus::default()
            }),
            FoundryObservation::Active {
                world: Some("canary".into())
            }
        );
    }

    #[test]
    fn a_shutdown_counts_only_on_a_success_body() {
        assert!(shutdown_accepted(&json!({"status": "success"})));
        assert!(!shutdown_accepted(
            &json!({"status": "error", "message": "invalid admin password"})
        ));
        assert!(!shutdown_accepted(&json!({})));
    }

    fn set_cookies(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::SET_COOKIE, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn the_admin_login_session_is_taken_from_its_own_set_cookie() {
        use crate::driver::foundry_session_cookie;
        // Other attributes and other Set-Cookie headers do not matter.
        assert_eq!(
            foundry_session_cookie(&set_cookies(&[
                "other=1; Path=/",
                "session=abc123; Path=/; HttpOnly; SameSite=Strict",
            ])),
            Some("abc123".into())
        );
        assert_eq!(
            foundry_session_cookie(&set_cookies(&["session=\"quoted\"; Path=/"])),
            Some("quoted".into())
        );
        // A missing cookie or a malformed value is a failure.
        assert_eq!(
            foundry_session_cookie(&set_cookies(&["path=/; HttpOnly"])),
            None
        );
        assert_eq!(
            foundry_session_cookie(&set_cookies(&["session=; Path=/"])),
            None
        );
        // Header values cannot carry CR/LF; a space or non-ASCII value still
        // must not reach our own Set-Cookie.
        assert_eq!(
            foundry_session_cookie(&set_cookies(&["session=a b; Path=/"])),
            None
        );
        assert_eq!(
            foundry_session_cookie(&set_cookies(&["session=é; Path=/"])),
            None
        );
        assert_eq!(foundry_session_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn an_admin_login_counts_only_on_a_setup_redirect() {
        assert!(is_setup_redirect("/setup"));
        assert!(is_setup_redirect("https://foundry.example/setup"));
        assert!(is_setup_redirect("/setup?x=1"));
        assert!(is_setup_redirect("/setup/"));
        assert!(!is_setup_redirect("/auth"));
        assert!(!is_setup_redirect("/setupx"));
        assert!(!is_setup_redirect("/setup/../join"));
        assert!(!is_setup_redirect(""));
    }

    #[test]
    fn session_view_reports_the_api_shape() {
        let open = OpenSession {
            world_id: Some("canary".into()),
            phase: SessionPhase::Running,
            ..session(SessionPhase::Running, Some("canary"), 0)
        };
        let value = serde_json::to_value(view(&open, &open.holder)).unwrap();
        assert_eq!(value["id"], open.id.to_string());
        assert_eq!(value["phase"], "running");
        assert_eq!(value["holder"]["name"], "GM");
        assert_eq!(value["worldId"], "canary");
        assert_eq!(value["isHolder"], true);
        assert!(value["startedAt"].is_string());
        assert!(!view(&open, "someone-else").is_holder);
    }
}
