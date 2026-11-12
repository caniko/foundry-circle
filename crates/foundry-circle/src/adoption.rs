//! World adoption (G5) and the broker-mapped identities behind
//! `/_circle/enter`.
//!
//! One adoption per world: the broker takes over a throwaway bootstrap admin
//! session, ensures its own GM-role API user with a derived password, switches
//! to a fresh non-admin session, and rotates every other user's password so
//! nothing but the broker can still log in with a password a person knows.
//!
//! Nothing here stores or logs a password, a cookie or the HMAC secret.

use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    driver::{
        self, AccountOp, DriverConfig, DriverPage, FoundryDriver, FoundryUser, LiveFoundryDriver,
        WorldError,
    },
    http::session::{self, OpenSession},
};

/// Foundry's `USER_ROLES.GAMEMASTER`; the lease holder, and the API user.
pub(crate) const GM_ROLE: u8 = 4;
/// Foundry's `USER_ROLES.PLAYER`; everyone else.
pub(crate) const PLAYER_ROLE: u8 = 1;

type HmacSha256 = Hmac<Sha256>;

/// `hex(HMAC-SHA256(secret, world_id || 0x00 || foundry_user_id))`.
pub(crate) fn derive_password(secret: &[u8], world_id: &str, user_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(world_id.as_bytes());
    mac.update(&[0]);
    mac.update(user_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Read at use time so a rotated secret needs no restart. A missing or empty
/// credential disables entry and adoption with a 503.
pub(crate) fn password_secret() -> Option<Vec<u8>> {
    let path = std::env::var("FOUNDRY_CIRCLE_PASSWORD_SECRET_FILE").ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let secret = std::fs::read_to_string(path).ok()?;
    let secret = secret.trim();
    (!secret.is_empty()).then(|| secret.as_bytes().to_vec())
}

/// A password for a Foundry user the broker locks out. Two v4 UUIDs give 32
/// bytes from the OS RNG without adding `rand`.
pub(crate) fn random_password() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// The lease holder is the only GM this session; everyone else enters as a
/// player.
pub(crate) const fn member_role(is_holder: bool) -> u8 {
    if is_holder { GM_ROLE } else { PLAYER_ROLE }
}

/// The role for one entry, decided from the locked lease row's holder. It is
/// never carried over from a read made before the browser work.
pub(crate) fn holder_role(holder: &str, subject: &str) -> u8 {
    member_role(holder == subject)
}

/// Prefer the world's `Gamemaster`, then any other GM-role user.
pub(crate) fn pick_gm(users: &[FoundryUser]) -> Option<&FoundryUser> {
    users
        .iter()
        .find(|user| user.role == GM_ROLE && user.name == "Gamemaster")
        .or_else(|| users.iter().find(|user| user.role == GM_ROLE))
}

/// The principal's display name, with Foundry's unique-name rule applied:
/// `Name`, `Name (2)`, `Name (3)`, ...
pub(crate) fn unique_name(desired: &str, existing: &[String]) -> String {
    let desired = desired.trim();
    let base = if desired.is_empty() {
        "Player"
    } else {
        desired
    };
    if !existing.iter().any(|name| name == base) {
        return base.to_owned();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base} ({suffix})");
        if !existing.iter().any(|name| name == &candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Users whose passwords adoption rotates: everyone except the API user and
/// users this world already maps.
pub(crate) fn rotation_targets(users: &[FoundryUser], keep: &[String]) -> Vec<String> {
    users
        .iter()
        .filter(|user| !keep.contains(&user.id))
        .map(|user| user.id.clone())
        .collect()
}

/// The adopted world's API user id.
pub(crate) async fn adopted_api_user(
    pool: &PgPool,
    world: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT api_user_id FROM adopted_worlds WHERE world_id = $1")
        .bind(world)
        .fetch_optional(pool)
        .await
}

/// The principal's mapped Foundry user in this world.
pub(crate) async fn mapped_user(
    executor: impl sqlx::PgExecutor<'_>,
    subject: &str,
    world: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT user_id FROM foundry_users WHERE subject = $1 AND world_id = $2")
        .bind(subject)
        .bind(world)
        .fetch_optional(executor)
        .await
}

async fn remember_user(
    executor: impl sqlx::PgExecutor<'_>,
    subject: &str,
    world: &str,
    user_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO foundry_users (subject, world_id, user_id) VALUES ($1, $2, $3)
         ON CONFLICT (subject, world_id) DO UPDATE SET user_id = EXCLUDED.user_id",
    )
    .bind(subject)
    .bind(world)
    .bind(user_id)
    .execute(executor)
    .await
    .map(|_| ())
}

async fn mapped_users(pool: &PgPool, world: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT user_id FROM foundry_users WHERE world_id = $1")
        .bind(world)
        .fetch_all(pool)
        .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdoptionError {
    /// No bootstrap admin cookie: the broker restarted, or Setup was never
    /// opened. The world must return to Setup and relaunch.
    MissingBootstrap,
    MissingSecret,
    NoGm,
    Driver,
    Foundry,
    Database,
}

impl AdoptionError {
    const fn outcome(self) -> &'static str {
        match self {
            Self::MissingBootstrap => "missing_bootstrap",
            Self::MissingSecret => "missing_secret",
            Self::NoGm => "no_gm",
            Self::Driver => "driver_error",
            Self::Foundry => "foundry_error",
            Self::Database => "database_error",
        }
    }
}

impl From<WorldError> for AdoptionError {
    fn from(_: WorldError) -> Self {
        Self::Driver
    }
}

/// Adopt the running world. Called by the reconciler once per poll while the
/// world is running and not yet adopted. Retrying is safe: the API user is
/// reused by name.
pub(crate) async fn adopt(
    pool: &PgPool,
    lease: &session::Lease,
    config: &DriverConfig,
    driver: &LiveFoundryDriver,
    open: &OpenSession,
    world: &str,
) -> Result<(), AdoptionError> {
    let Some(bootstrap) = lease.bootstrap_cookie(open.id) else {
        tracing::warn!(
            world,
            "world adoption has no bootstrap admin session; return to Setup and relaunch"
        );
        return Err(AdoptionError::MissingBootstrap);
    };
    let Some(secret) = password_secret() else {
        tracing::warn!("the derived-password secret is not readable; adoption is deferred");
        return Err(AdoptionError::MissingSecret);
    };
    let page = DriverPage::open(config, &bootstrap)
        .await
        .map_err(|_| AdoptionError::Driver)?;
    let result = adopt_with_page(
        pool, config, driver, open, world, &secret, &bootstrap, &page,
    )
    .await;
    page.close().await;
    if result.is_ok() {
        lease.forget_bootstrap(open.id);
    } else {
        record_subject(
            pool,
            &open.holder,
            "adoption",
            result.err().map_or("ok", AdoptionError::outcome),
            json!({"worldId": world}),
        )
        .await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn adopt_with_page(
    pool: &PgPool,
    config: &DriverConfig,
    driver: &LiveFoundryDriver,
    open: &OpenSession,
    world: &str,
    secret: &[u8],
    bootstrap: &str,
    page: &DriverPage,
) -> Result<(), AdoptionError> {
    let users = match driver::page_users(&page.page).await {
        Ok(users) => users,
        Err(error) => {
            tracing::warn!(%error, "adoption could not read the world's users");
            record(pool, open, "adoption.users", "failed", json!({})).await;
            return Err(error.into());
        }
    };
    record(
        pool,
        open,
        "adoption.users",
        "ok",
        json!({"users": users.len()}),
    )
    .await;

    let Some(gm) = pick_gm(&users) else {
        record(pool, open, "adoption.gm", "failed", json!({})).await;
        return Err(AdoptionError::NoGm);
    };
    let gm_id = gm.id.clone();
    record(
        pool,
        open,
        "adoption.gm",
        "ok",
        json!({"foundryUserId": gm_id}),
    )
    .await;

    if driver::login_as(&config.base_url, bootstrap, &gm_id)
        .await
        .is_err()
    {
        record(pool, open, "adoption.login_as", "failed", json!({})).await;
        return Err(AdoptionError::Foundry);
    }
    record(pool, open, "adoption.login_as", "ok", json!({})).await;

    if let Err(error) = page.goto_game(config, bootstrap).await {
        tracing::warn!(%error, "adoption could not open the game as the bootstrap GM");
        record(pool, open, "adoption.game", "failed", json!({})).await;
        return Err(error.into());
    }
    record(pool, open, "adoption.game", "ok", json!({})).await;

    let ensured = match driver::page_account(
        &page.page,
        &AccountOp::EnsureUser {
            user_id: None,
            name: config.api_user.clone(),
            role: GM_ROLE,
            reuse_by_name: true,
        },
    )
    .await
    {
        Ok(ensured) => ensured,
        Err(error) => {
            tracing::warn!(%error, "adoption could not ensure the API user");
            record(pool, open, "adoption.api_user", "failed", json!({})).await;
            return Err(error.into());
        }
    };
    let Some(api_id) = ensured["id"].as_str().map(str::to_owned) else {
        record(pool, open, "adoption.api_user", "failed", json!({})).await;
        return Err(AdoptionError::Driver);
    };
    record(
        pool,
        open,
        "adoption.api_user",
        "ok",
        json!({
            "foundryUserId": api_id,
            "created": ensured["created"].as_bool().unwrap_or(false),
        }),
    )
    .await;

    let password = derive_password(secret, world, &api_id);
    if let Err(error) = driver::page_account(
        &page.page,
        &AccountOp::SetPassword {
            user_id: api_id.clone(),
            password: password.clone(),
        },
    )
    .await
    {
        tracing::warn!(%error, "adoption could not set the API user's password");
        record(
            pool,
            open,
            "adoption.api_password",
            "failed",
            json!({"foundryUserId": api_id}),
        )
        .await;
        return Err(error.into());
    }
    record(
        pool,
        open,
        "adoption.api_password",
        "ok",
        json!({"foundryUserId": api_id}),
    )
    .await;

    let api_cookie = match driver::join_session(&config.base_url, &api_id, &password).await {
        Ok(cookie) => cookie,
        Err(_) => {
            record(
                pool,
                open,
                "adoption.api_session",
                "failed",
                json!({"foundryUserId": api_id}),
            )
            .await;
            return Err(AdoptionError::Foundry);
        }
    };
    record(
        pool,
        open,
        "adoption.api_session",
        "ok",
        json!({"foundryUserId": api_id}),
    )
    .await;

    if let Err(error) = page.goto_game(config, &api_cookie).await {
        tracing::warn!(%error, "adoption could not switch to the API session");
        record(pool, open, "adoption.api_game", "failed", json!({})).await;
        return Err(error.into());
    }
    record(pool, open, "adoption.api_game", "ok", json!({})).await;

    let mut keep = match mapped_users(pool, world).await {
        Ok(mapped) => mapped,
        Err(error) => {
            tracing::warn!(%error, "adoption could not read the mapped users");
            record(pool, open, "adoption.passwords", "failed", json!({})).await;
            return Err(AdoptionError::Database);
        }
    };
    keep.push(api_id.clone());
    let replacements: Vec<(String, String)> = rotation_targets(&users, &keep)
        .into_iter()
        .map(|id| (id, random_password()))
        .collect();
    match driver::rotate_passwords(&page.page, &replacements).await {
        Ok(rotated) => {
            record(
                pool,
                open,
                "adoption.passwords",
                "ok",
                json!({"rotated": rotated}),
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(%error, "adoption could not rotate the remaining passwords");
            record(pool, open, "adoption.passwords", "failed", json!({})).await;
            return Err(error.into());
        }
    }

    if let Err(error) = sqlx::query(
        "INSERT INTO adopted_worlds (world_id, api_user_id) VALUES ($1, $2)
         ON CONFLICT (world_id) DO UPDATE SET api_user_id = EXCLUDED.api_user_id",
    )
    .bind(world)
    .bind(&api_id)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, "adoption could not record the adopted world");
        record(pool, open, "adoption.complete", "failed", json!({})).await;
        return Err(AdoptionError::Database);
    }
    record(
        pool,
        open,
        "adoption.complete",
        "ok",
        json!({"worldId": world, "foundryUserId": api_id}),
    )
    .await;

    driver.set_session(world, api_cookie).await;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriverError {
    NotAdopted,
    MissingSecret,
    Foundry,
    Database,
}

/// Keep the steady-state driver logged in as the adopted world's API user.
pub(crate) async fn prepare_driver(
    pool: &PgPool,
    driver: &LiveFoundryDriver,
    config: &DriverConfig,
    world: &str,
) -> Result<(), DriverError> {
    // The driver retries its own page with backoff; only a missing hand-off
    // needs a new Foundry session. Re-joining on every error would leak one
    // Foundry session per poll.
    if driver.session_world().await.as_deref() == Some(world) {
        return Ok(());
    }
    let Some(api_id) = adopted_api_user(pool, world)
        .await
        .map_err(|_| DriverError::Database)?
    else {
        return Err(DriverError::NotAdopted);
    };
    let Some(secret) = password_secret() else {
        return Err(DriverError::MissingSecret);
    };
    let password = derive_password(&secret, world, &api_id);
    let cookie = driver::join_session(&config.base_url, &api_id, &password)
        .await
        .map_err(|_| DriverError::Foundry)?;
    driver.set_session(world, cookie).await;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnterError {
    MissingSecret,
    /// The driver has no live page: the world is starting.
    NotReady,
    Foundry,
    Database,
}

impl EnterError {
    pub(crate) const fn status(self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Self::MissingSecret | Self::NotReady => StatusCode::SERVICE_UNAVAILABLE,
            Self::Foundry => StatusCode::BAD_GATEWAY,
            Self::Database => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::MissingSecret => "password_secret_not_configured",
            Self::NotReady => "world_not_ready",
            Self::Foundry => "foundry_unavailable",
            Self::Database => "database_unavailable",
        }
    }
}

fn enter_error(error: WorldError) -> EnterError {
    match error {
        WorldError::NotReady => EnterError::NotReady,
        _ => EnterError::Foundry,
    }
}

/// Ensure this principal's Foundry user and return a fresh `session` cookie
/// plus whether the caller holds the lease.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn enter(
    pool: &PgPool,
    driver: &dyn FoundryDriver,
    base_url: &str,
    session_id: Uuid,
    subject: &str,
    display_name: &str,
    world: &str,
) -> Result<(String, bool), EnterError> {
    let Some(secret) = password_secret() else {
        return Err(EnterError::MissingSecret);
    };
    // The lease and the live page must still be talking about the same world.
    if driver.snapshot().id.as_deref() != Some(world) {
        return Err(EnterError::NotReady);
    }
    // The whole entry runs under the open lease row, so a request delayed
    // across another holder's launch cannot be promoted after the world
    // changed hands: the role comes from this locked row, not from an earlier
    // read. Stop/return-to-setup/reconcile transitions wait on the same row.
    let mut transaction = pool.begin().await.map_err(|_| EnterError::Database)?;
    let holder: Option<String> = sqlx::query_scalar(
        "SELECT holder FROM game_session
          WHERE id = $1 AND ended_at IS NULL AND phase = 'running' AND world_id = $2
          FOR UPDATE",
    )
    .bind(session_id)
    .bind(world)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| EnterError::Database)?;
    // Gone or changed while this request waited: touch nothing in Foundry.
    let Some(holder) = holder else {
        return Err(EnterError::NotReady);
    };
    let role = holder_role(&holder, subject);
    // Same truth as the role, taken from the one locked row: the caller only
    // uses it to record presence.
    let is_holder = role == GM_ROLE;
    let mapped = mapped_user(&mut *transaction, subject, world)
        .await
        .map_err(|_| EnterError::Database)?;
    // A same-named user created between the name read and the create is a
    // race, not a reason to adopt their account: reread once and retry.
    let mut ensured = None;
    for attempt in 0..2 {
        let names = match driver.account(AccountOp::UserNames).await {
            Ok(value) => value
                .get("names")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            Err(error) => return Err(enter_error(error)),
        };
        let name = unique_name(display_name, &names);
        match driver
            .account(AccountOp::EnsureUser {
                user_id: mapped.clone(),
                name,
                role,
                reuse_by_name: false,
            })
            .await
        {
            Ok(value) => {
                ensured = Some(value);
                break;
            }
            Err(error) if attempt == 0 => {
                tracing::debug!(%error, "retrying Foundry user creation after a name race");
            }
            Err(error) => return Err(enter_error(error)),
        }
    }
    let Some(ensured) = ensured else {
        return Err(EnterError::Foundry);
    };
    let Some(user_id) = ensured["id"].as_str().map(str::to_owned) else {
        return Err(EnterError::Foundry);
    };
    let password = derive_password(&secret, world, &user_id);
    driver
        .account(AccountOp::SetPassword {
            user_id: user_id.clone(),
            password: password.clone(),
        })
        .await
        .map_err(enter_error)?;
    if mapped.as_deref() != Some(user_id.as_str()) {
        remember_user(&mut *transaction, subject, world, &user_id)
            .await
            .map_err(|_| EnterError::Database)?;
    }
    let cookie = driver::join_session(base_url, &user_id, &password)
        .await
        .map_err(|_| EnterError::Foundry)?;
    // Only now release the lease row: the cookie belongs to the world this
    // transaction still owns.
    transaction
        .commit()
        .await
        .map_err(|_| EnterError::Database)?;
    record_subject(
        pool,
        subject,
        "session.enter",
        "ok",
        json!({
            "sessionId": session_id,
            "worldId": world,
            "foundryUserId": user_id,
            "role": role,
        }),
    )
    .await;
    Ok((cookie, is_holder))
}

async fn record(pool: &PgPool, open: &OpenSession, action: &str, outcome: &str, details: Value) {
    record_subject(pool, &open.holder, action, outcome, details).await;
}

async fn record_subject(pool: &PgPool, subject: &str, action: &str, outcome: &str, details: Value) {
    if let Err(error) = session::insert_audit(pool, Some(subject), action, outcome, details).await {
        tracing::warn!(%error, "failed to write the Foundry Circle adoption audit entry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, name: &str, role: u8) -> FoundryUser {
        FoundryUser {
            id: id.into(),
            name: name.into(),
            role,
        }
    }

    #[test]
    fn derived_passwords_match_the_openssl_vector() {
        // printf 'canary-world\0AbCdEf0123456789' |
        //   openssl dgst -sha256 -hmac 'foundry-circle-test-secret' -hex
        assert_eq!(
            derive_password(
                b"foundry-circle-test-secret",
                "canary-world",
                "AbCdEf0123456789"
            ),
            "5b55c7b20270b747a32c9dca975c77a1490a4ef745673f29873cf64c38b8ff5c"
        );
        // The zero separator is part of the message.
        assert_ne!(
            derive_password(b"foundry-circle-test-secret", "canary-world", "x"),
            derive_password(b"foundry-circle-test-secret", "canary-world0", "x")
        );
    }

    #[test]
    fn adoption_prefers_the_gamemaster_named_user() {
        let users = vec![
            user("a", "Alice", GM_ROLE),
            user("b", "Gamemaster", GM_ROLE),
            user("c", "Bob", PLAYER_ROLE),
        ];
        assert_eq!(pick_gm(&users).map(|user| user.id.as_str()), Some("b"));
        let first = vec![user("a", "Alice", GM_ROLE), user("b", "Bob", PLAYER_ROLE)];
        assert_eq!(pick_gm(&first).map(|user| user.id.as_str()), Some("a"));
        assert!(pick_gm(&[user("c", "Bob", PLAYER_ROLE)]).is_none());
        assert!(pick_gm(&[]).is_none());
    }

    #[test]
    fn display_names_get_a_suffix_only_when_taken() {
        let existing = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(unique_name("Robin", &existing(&["Alice"])), "Robin");
        assert_eq!(unique_name("Robin", &existing(&["Robin"])), "Robin (2)");
        assert_eq!(
            unique_name("Robin", &existing(&["Robin", "Robin (2)"])),
            "Robin (3)"
        );
        assert_eq!(
            unique_name("Robin", &existing(&["Robin", "Robin (3)"])),
            "Robin (2)"
        );
        assert_eq!(unique_name("  ", &existing(&[])), "Player");
    }

    #[test]
    fn only_the_holder_enters_as_a_gm() {
        assert_eq!(member_role(true), GM_ROLE);
        assert_eq!(member_role(false), PLAYER_ROLE);
    }

    #[test]
    fn entry_role_follows_the_lease_row_it_locked() {
        // S1: the role is derived from the holder of the row locked for this
        // entry, never from a read made before the browser work.
        assert_eq!(holder_role("roy", "roy"), GM_ROLE);
        assert_eq!(holder_role("alice", "roy"), PLAYER_ROLE);
    }

    #[test]
    fn password_rotation_keeps_the_api_and_mapped_users() {
        let users = vec![
            user("api", "foundry-circle-api", GM_ROLE),
            user("holder", "Robin", PLAYER_ROLE),
            user("gm", "Gamemaster", GM_ROLE),
        ];
        assert_eq!(
            rotation_targets(&users, &["api".into(), "holder".into()]),
            vec!["gm".to_owned()]
        );
    }

    #[test]
    fn rotated_passwords_are_32_bytes_of_hex() {
        let password = random_password();
        assert_eq!(password.len(), 64);
        assert!(password.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(random_password(), random_password());
    }
}
