//! The character builder's control-plane half: SQL, authorization and the
//! error codes the `builder_ui` server functions hand back.
//!
//! Drafts are control-plane state until approval; Foundry owns the Actor after
//! that, so nothing here writes to a world.

use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{AppState, Caller, auth, console_actor, console_caller, session};
use crate::{
    Role,
    builder::{
        AbilityMethod, BuilderData, BuilderView, CampaignRules, CampaignView, CharactersData,
        CharactersView, DraftChoices, DraftPatch, DraftStatus, DraftSummary, Rolls, Scores,
        pending, roll_sets, validate_scores,
    },
};

/// An at most five drafts per player and world rule.
const DRAFT_LIMIT: i64 = 5;

/// The signed-in access-group member behind a builder mutation.
/// Goes through `console_actor`, so it carries the lobby's exact-`Origin` check.
fn member() -> Result<(AppState, auth::Principal, Role), &'static str> {
    let (state, principal) = console_actor().ok_or("not_signed_in")?;
    let auth = state.auth.as_ref().ok_or("not_signed_in")?;
    let role = principal.role(&auth.admin_group, auth.gm_group.as_deref());
    Ok((state, principal, role))
}

/// One draft row with the campaign rules it is built against.
struct DraftRow {
    subject: String,
    name: String,
    status: DraftStatus,
    choices: DraftChoices,
    rolls: Option<Rolls>,
    dm_note: Option<String>,
    open: bool,
    rules: CampaignRules,
}

/// The console's characters page for the caller.
pub async fn characters_view() -> CharactersView {
    let (state, principal, role) = match console_caller() {
        Caller::SignedOut => return CharactersView::SignedOut,
        Caller::Denied { name } => return CharactersView::Denied { name },
        Caller::Member {
            state,
            principal,
            role,
            ..
        } => (state, principal, role),
    };
    let Some(pool) = state.database.pool().await else {
        // The lobby degrades to empty lists on a database outage; so does this.
        return CharactersView::Ready(Box::new(CharactersData {
            role,
            campaigns: Vec::new(),
            mine: Vec::new(),
            party: Vec::new(),
        }));
    };
    let campaigns = match campaigns(&pool, role).await {
        Ok(campaigns) => campaigns,
        Err(error) => {
            tracing::warn!(%error, "failed to list Foundry Circle campaigns");
            Vec::new()
        }
    };
    let mine = match summaries(&pool, Some(&principal.subject)).await {
        Ok(mine) => mine,
        Err(error) => {
            tracing::warn!(%error, "failed to list Foundry Circle character drafts");
            Vec::new()
        }
    };
    let party = if role == Role::Player {
        Vec::new()
    } else {
        summaries(&pool, None).await.unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to list the Foundry Circle party");
            Vec::new()
        })
    };
    CharactersView::Ready(Box::new(CharactersData {
        role,
        campaigns,
        mine,
        party,
    }))
}

/// One adopted world, with its rules or the defaults before a GM saved any.
async fn campaigns(pool: &PgPool, role: Role) -> Result<Vec<CampaignView>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT a.world_id, COALESCE(r.open, false) AS open, r.rules
           FROM adopted_worlds a
           LEFT JOIN campaign_rules r ON r.world_id = a.world_id
          WHERE $1 OR COALESCE(r.open, false)
          ORDER BY a.world_id",
    )
    .bind(role != Role::Player)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| CampaignView {
            world_id: row.try_get("world_id").unwrap_or_default(),
            open: row.try_get("open").unwrap_or(false),
            rules: row
                .try_get::<Option<Value>, _>("rules")
                .ok()
                .flatten()
                .and_then(|rules| serde_json::from_value(rules).ok())
                .unwrap_or_default(),
        })
        .collect())
}

/// Drafts as the characters page lists them; `only` narrows to one player.
async fn summaries(pool: &PgPool, only: Option<&str>) -> Result<Vec<DraftSummary>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT d.id, d.name, d.status, d.choices, d.rolls,
                COALESCE(p.display_name, p.email, p.subject) AS player
           FROM character_drafts d
           JOIN principals p ON p.subject = d.subject
          WHERE $1::text IS NULL OR d.subject = $1
          ORDER BY d.name, d.created_at",
    )
    .bind(only)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let choices: DraftChoices = row
                .try_get::<Value, _>("choices")
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            let rolls = row
                .try_get::<Option<Value>, _>("rolls")
                .ok()
                .flatten()
                .and_then(|value| serde_json::from_value(value).ok());
            let name: String = row.try_get("name").ok()?;
            Some(DraftSummary {
                id: row.try_get::<Uuid, _>("id").ok()?.to_string(),
                name: name.clone(),
                status: DraftStatus::from_db(row.try_get::<String, _>("status").ok()?.as_str())?,
                player: row.try_get("player").ok()?,
                pending: pending(&choices, &name, rolls.as_ref()).len() as u32,
                choices,
                rolls,
            })
        })
        .collect())
}

/// One draft's builder page, or `Missing` when it is not the caller's to see.
pub async fn builder_view(id: &str) -> BuilderView {
    let Caller::Member {
        state,
        principal,
        role,
        ..
    } = console_caller()
    else {
        return BuilderView::Missing;
    };
    let Some(pool) = state.database.pool().await else {
        return BuilderView::Missing;
    };
    let Ok(id) = Uuid::parse_str(id) else {
        return BuilderView::Missing;
    };
    read_ready(&pool, id, &principal.subject, role).await
}

/// `read_ready` after a commit: reload what the player now sees.
async fn read_ready(pool: &PgPool, id: Uuid, subject: &str, role: Role) -> BuilderView {
    let draft = match draft_row(pool, id, false).await {
        Ok(Some(draft)) => draft,
        Ok(None) => return BuilderView::Missing,
        Err(error) => {
            tracing::warn!(%error, "failed to read a Foundry Circle character draft");
            return BuilderView::Missing;
        }
    };
    if role == Role::Player && draft.subject != subject {
        return BuilderView::Missing;
    }
    BuilderView::Ready(Box::new(BuilderData {
        id: id.to_string(),
        editable: draft.status == DraftStatus::Draft && draft.open && draft.subject == subject,
        name: draft.name.clone(),
        status: draft.status,
        rules: draft.rules,
        rolls: draft.rolls.clone(),
        pending: pending(&draft.choices, &draft.name, draft.rolls.as_ref()),
        dm_note: draft.dm_note,
        choices: draft.choices,
    }))
}

/// The draft row plus its campaign's rules; `lock` serialises the mutations.
async fn draft_row(
    executor: impl sqlx::PgExecutor<'_>,
    id: Uuid,
    lock: bool,
) -> Result<Option<DraftRow>, sqlx::Error> {
    let sql = format!(
        "SELECT d.world_id, d.subject, d.name, d.status, d.choices, d.rolls, d.dm_note,
                r.open, r.rules
           FROM character_drafts d
           JOIN campaign_rules r ON r.world_id = d.world_id
          WHERE d.id = $1{}",
        if lock { " FOR UPDATE OF d" } else { "" }
    );
    let row = sqlx::query(&sql).bind(id).fetch_optional(executor).await?;
    let Some(row) = row else { return Ok(None) };
    let status: String = row.try_get("status")?;
    Ok(Some(DraftRow {
        subject: row.try_get("subject")?,
        name: row.try_get("name")?,
        status: DraftStatus::from_db(&status).ok_or_else(|| {
            sqlx::Error::Decode(format!("unknown character draft status {status}").into())
        })?,
        choices: row
            .try_get::<Value, _>("choices")
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        rolls: row
            .try_get::<Option<Value>, _>("rolls")
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_value(value).ok()),
        dm_note: row.try_get("dm_note")?,
        open: row.try_get("open")?,
        rules: row
            .try_get::<Value, _>("rules")
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
    }))
}

/// A GM or admin opens, closes and edits a world's session-zero rules.
pub async fn save_rules(world_id: &str, open: bool, rules: CampaignRules) -> Option<String> {
    let (state, principal, role) = match member() {
        Ok(member) => member,
        Err(code) => return Some(code.into()),
    };
    if role == Role::Player {
        return Some("permission_denied".into());
    }
    let Some(pool) = state.database.pool().await else {
        return Some("database_unavailable".into());
    };
    let rules = sanitize_rules(rules);
    let value = serde_json::to_value(rules).ok()?;
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => return Some(database_code(error)),
    };
    // The SELECT makes an unknown or unadopted world a refusal, not a row.
    let written = sqlx::query(
        "INSERT INTO campaign_rules (world_id, open, rules, updated_by)
         SELECT $1, $2, $3, $4 FROM adopted_worlds WHERE world_id = $1
         ON CONFLICT (world_id) DO UPDATE
            SET open = EXCLUDED.open, rules = EXCLUDED.rules,
                updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(world_id)
    .bind(open)
    .bind(&value)
    .bind(&principal.subject)
    .execute(&mut *transaction)
    .await;
    let written = match written {
        Ok(written) => written.rows_affected(),
        Err(error) => return Some(database_code(error)),
    };
    if written == 0 {
        return Some("campaign_closed".into());
    }
    if let Err(error) = session::insert_audit_for(
        &mut *transaction,
        "character_builder",
        Some(&principal.subject),
        "campaign.rules",
        "ok",
        json!({"worldId": world_id, "open": open, "abilityMethods": value["abilityMethods"]}),
    )
    .await
    {
        tracing::warn!(%error, "failed to write the campaign rules audit entry");
    }
    match transaction.commit().await {
        Ok(()) => None,
        Err(error) => Some(database_code(error)),
    }
}

/// The client's rules are re-validated here: known methods only, nothing newer
/// than slice one, and never an empty method list (it would block every player).
fn sanitize_rules(rules: CampaignRules) -> CampaignRules {
    let mut methods = Vec::new();
    for method in rules.ability_methods {
        if !methods.contains(&method) {
            methods.push(method);
        }
    }
    if methods.is_empty() {
        methods = CampaignRules::default().ability_methods;
    }
    CampaignRules {
        ability_methods: methods,
        starting_level: 1,
        equipment: rules.equipment,
        // ponytail: sources arrive with the catalog sync; slice one persists none.
        sources: Vec::new(),
    }
}

/// Any access-group member may start a draft in an open campaign.
pub async fn create_draft(world_id: &str) -> Result<String, String> {
    let (state, principal, _) = member().map_err(str::to_owned)?;
    let Some(pool) = state.database.pool().await else {
        return Err("database_unavailable".into());
    };
    let mut transaction = pool.begin().await.map_err(database_code)?;
    // The row lock serialises creates per world, so the limit holds under races.
    let open: Option<bool> =
        sqlx::query_scalar("SELECT open FROM campaign_rules WHERE world_id = $1 FOR UPDATE")
            .bind(world_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_code)?;
    if open != Some(true) {
        return Err("campaign_closed".into());
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM character_drafts WHERE world_id = $1 AND subject = $2",
    )
    .bind(world_id)
    .bind(&principal.subject)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_code)?;
    if count >= DRAFT_LIMIT {
        return Err("draft_limit".into());
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO character_drafts (id, world_id, subject) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(world_id)
        .bind(&principal.subject)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.create",
        json!({"draftId": id, "worldId": world_id}),
    )
    .await;
    transaction.commit().await.map_err(database_code)?;
    Ok(id.to_string())
}

/// The player edits their own draft until it is submitted.
pub async fn update_draft(id: &str, patch: DraftPatch) -> Result<BuilderView, String> {
    let (state, principal, role) = member().map_err(str::to_owned)?;
    let Some(pool) = state.database.pool().await else {
        return Err("database_unavailable".into());
    };
    let id = Uuid::parse_str(id).map_err(|_| "draft_not_found".to_owned())?;
    let mut transaction = pool.begin().await.map_err(database_code)?;
    let draft = locked_own_draft(&mut transaction, id, &principal.subject).await?;
    let (name, choices) = match patch {
        DraftPatch::Name(name) => {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 100 {
                return Err("invalid_name".into());
            }
            (name.to_owned(), draft.choices)
        }
        DraftPatch::Abilities { method, base } => {
            validate_scores(method, &base, draft.rolls.as_ref(), &draft.rules)
                .map_err(str::to_owned)?;
            let mut choices = draft.choices;
            choices.method = Some(method);
            choices.base = Some(base);
            (draft.name, choices)
        }
    };
    sqlx::query(
        "UPDATE character_drafts SET name = $2, choices = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(serde_json::to_value(&choices).map_err(|error| error.to_string())?)
    .execute(&mut *transaction)
    .await
    .map_err(database_code)?;
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.update",
        json!({"draftId": id, "choices": choices}),
    )
    .await;
    transaction.commit().await.map_err(database_code)?;
    Ok(read_ready(&pool, id, &principal.subject, role).await)
}

/// Roll the one honest set for a draft, then use it as the assignment.
pub async fn roll_abilities(id: &str) -> Result<BuilderView, String> {
    let (state, principal, role) = member().map_err(str::to_owned)?;
    let Some(pool) = state.database.pool().await else {
        return Err("database_unavailable".into());
    };
    let id = Uuid::parse_str(id).map_err(|_| "draft_not_found".to_owned())?;
    let mut transaction = pool.begin().await.map_err(database_code)?;
    let draft = locked_own_draft(&mut transaction, id, &principal.subject).await?;
    if !draft.rules.ability_methods.contains(&AbilityMethod::Roll) {
        return Err("method_not_allowed".into());
    }
    let rolls = match draft.rolls {
        Some(rolls) => rolls,
        None => {
            let Some(rolls) = os_rolls() else {
                // No code exists for an OS RNG failure; nothing was written.
                tracing::error!("the OS random source failed; no abilities were rolled");
                return Err("database_unavailable".into());
            };
            let written = sqlx::query(
                "UPDATE character_drafts SET rolls = $2, updated_at = now() WHERE id = $1 AND rolls IS NULL",
            )
            .bind(id)
            .bind(serde_json::to_value(&rolls).map_err(|error| error.to_string())?)
            .execute(&mut *transaction)
            .await
            .map_err(database_code)?
            .rows_affected();
            if written == 0 {
                // Another request wrote first; its dice are the draft's dice.
                let stored: Option<Value> =
                    sqlx::query_scalar("SELECT rolls FROM character_drafts WHERE id = $1")
                        .bind(id)
                        .fetch_one(&mut *transaction)
                        .await
                        .map_err(database_code)?;
                stored
                    .and_then(|value| serde_json::from_value(value).ok())
                    .ok_or_else(|| "database_unavailable".to_owned())?
            } else {
                rolls
            }
        }
    };
    let totals = rolls.sets.map(|set| set.total);
    let mut choices = draft.choices;
    choices.method = Some(AbilityMethod::Roll);
    choices.base = Some(Scores {
        str: totals[0],
        dex: totals[1],
        con: totals[2],
        int: totals[3],
        wis: totals[4],
        cha: totals[5],
    });
    sqlx::query("UPDATE character_drafts SET choices = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(serde_json::to_value(&choices).map_err(|error| error.to_string())?)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.roll",
        json!({"draftId": id, "sets": rolls.sets}),
    )
    .await;
    transaction.commit().await.map_err(database_code)?;
    Ok(read_ready(&pool, id, &principal.subject, role).await)
}

/// A GM grants one more roll by clearing the stored dice.
pub async fn grant_reroll(id: &str) -> Option<String> {
    let (state, principal, role) = match member() {
        Ok(member) => member,
        Err(code) => return Some(code.into()),
    };
    if role == Role::Player {
        return Some("permission_denied".into());
    }
    let Some(pool) = state.database.pool().await else {
        return Some("database_unavailable".into());
    };
    let id = match Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return Some("draft_not_found".into()),
    };
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => return Some(database_code(error)),
    };
    let draft = match draft_row(&mut *transaction, id, true).await {
        Ok(Some(draft)) => draft,
        Ok(None) => return Some("draft_not_found".into()),
        Err(error) => return Some(database_code(error)),
    };
    if draft.status != DraftStatus::Draft {
        return Some("draft_locked".into());
    }
    if let Err(error) =
        sqlx::query("UPDATE character_drafts SET rolls = NULL, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *transaction)
            .await
    {
        return Some(database_code(error));
    }
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.reroll",
        json!({"draftId": id, "subject": draft.subject}),
    )
    .await;
    match transaction.commit().await {
        Ok(()) => None,
        Err(error) => Some(database_code(error)),
    }
}

/// The owner, or a GM, removes a draft that has not been submitted.
pub async fn delete_draft(id: &str) -> Option<String> {
    let (state, principal, role) = match member() {
        Ok(member) => member,
        Err(code) => return Some(code.into()),
    };
    let Some(pool) = state.database.pool().await else {
        return Some("database_unavailable".into());
    };
    let id = match Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return Some("draft_not_found".into()),
    };
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => return Some(database_code(error)),
    };
    let draft = match draft_row(&mut *transaction, id, true).await {
        Ok(Some(draft)) => draft,
        Ok(None) => return Some("draft_not_found".into()),
        Err(error) => return Some(database_code(error)),
    };
    if draft.subject != principal.subject && role == Role::Player {
        return Some("permission_denied".into());
    }
    if draft.status != DraftStatus::Draft {
        return Some("draft_locked".into());
    }
    if let Err(error) = sqlx::query("DELETE FROM character_drafts WHERE id = $1")
        .bind(id)
        .execute(&mut *transaction)
        .await
    {
        return Some(database_code(error));
    }
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.delete",
        json!({"draftId": id, "subject": draft.subject}),
    )
    .await;
    match transaction.commit().await {
        Ok(()) => None,
        Err(error) => Some(database_code(error)),
    }
}

/// The locked draft, own and editable, or the code that refuses the change.
async fn locked_own_draft(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    subject: &str,
) -> Result<DraftRow, String> {
    let draft = draft_row(&mut **transaction, id, true)
        .await
        .map_err(database_code)?
        .ok_or_else(|| "draft_not_found".to_owned())?;
    if draft.subject != subject {
        return Err("permission_denied".into());
    }
    if draft.status != DraftStatus::Draft {
        return Err("draft_locked".into());
    }
    if !draft.open {
        return Err("campaign_closed".into());
    }
    Ok(draft)
}

/// Six honest sets from the OS source, written once per draft.
fn os_rolls() -> Option<Rolls> {
    let mut bytes = [0_u8; 512];
    getrandom::fill(&mut bytes).ok()?;
    let mut buffer = bytes.into_iter();
    // ponytail: 512 bytes for 24 dice; the fallback byte only fires on a broken
    // OS random source (~4^-488 per roll), and fails noisily in the audit log.
    let sets = roll_sets(move || buffer.next().unwrap_or(0));
    Some(Rolls {
        rolled_at: chrono::Utc::now().to_rfc3339(),
        sets,
    })
}

/// One audit row; a failure is logged, never fatal to the change it describes.
async fn audit(
    transaction: &mut Transaction<'_, Postgres>,
    subject: &str,
    action: &str,
    details: Value,
) {
    if let Err(error) = session::insert_audit_for(
        &mut **transaction,
        "character_draft",
        Some(subject),
        action,
        "ok",
        details,
    )
    .await
    {
        tracing::warn!(%error, "failed to write the character builder audit entry");
    }
}

/// The refusal code for a database failure, logged with the detail kept here.
fn database_code(error: sqlx::Error) -> String {
    tracing::warn!(%error, "Foundry Circle character builder database operation failed");
    "database_unavailable".into()
}
