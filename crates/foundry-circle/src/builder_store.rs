//! The character builder's control-plane half: SQL, authorization and the
//! error codes the `builder_ui` server functions hand back.
//!
//! Drafts are control-plane state until approval; Foundry owns the Actor after
//! that, so nothing here writes to a world.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{AppState, Caller, auth, console_actor, console_caller, session};
use crate::{
    Role, adoption,
    builder::{
        AbilityMethod, BuilderData, BuilderView, CampaignRules, CampaignView, Catalog, CatalogItem,
        CharactersData, CharactersView, Decision, DraftChoices, DraftPatch, DraftStatus,
        DraftSummary, PackInfo, PartySummary, Pending, Redirect, Rolls, Scores, Slot,
        catalog_skills, class_details, classes, create_args, decisions, equipment_plans,
        granted_traits, origin_details, origin_options, party_summary, pending, prune_picks,
        roll_sets, submit_refusal, totals, validate_asi, validate_detail, validate_equipment,
        validate_note, validate_pick, validate_scores,
    },
    driver::{CatalogPacks, FoundryDriver, WorldError, WorldOp, WorldState},
};

/// An at most five drafts per player and world rule.
const DRAFT_LIMIT: i64 = 5;

/// One page of a pack's documents; the module has no server-side paging.
const CATALOG_PAGE: u32 = 50;

/// A saved `sources` list no longer than this; a world never has more packs.
const SOURCE_LIMIT: usize = 200;

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
    world_id: String,
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
            coverage: PartySummary::default(),
            ready_world: None,
            can_approve: false,
            play_url: super::join(&state.console.play_url, "/_circle/enter"),
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
        Ok((mine, _)) => mine.into_iter().map(|(summary, _)| summary).collect(),
        Err(error) => {
            tracing::warn!(%error, "failed to list Foundry Circle character drafts");
            Vec::new()
        }
    };
    let mut party = Vec::new();
    let mut coverage = PartySummary::default();
    if role != Role::Player {
        match summaries(&pool, None).await {
            Ok((rows, skills)) => {
                coverage = party_summary(&rows, &skills);
                party = rows.into_iter().map(|(summary, _)| summary).collect();
            }
            Err(error) => tracing::warn!(%error, "failed to list the Foundry Circle party"),
        }
    }
    let snapshot = state.driver.snapshot();
    let ready_world = (snapshot.state == WorldState::Ready)
        .then_some(snapshot.id)
        .flatten();
    // A GM approves through the session they hold; an admin through either.
    let holding = session::open(&pool)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to read the open Foundry Circle session");
            None
        })
        .is_some_and(|open| {
            open.phase == crate::SessionPhase::Running && open.holder == principal.subject
        });
    let can_approve =
        ready_world.is_some() && (role == Role::Admin || (role == Role::Gm && holding));
    CharactersView::Ready(Box::new(CharactersData {
        role,
        campaigns,
        mine,
        party,
        coverage,
        ready_world,
        can_approve,
        play_url: super::join(&state.console.play_url, "/_circle/enter"),
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
    // ponytail: one catalog query per adopted world; batch if a host ever runs
    // dozens of them.
    let mut campaigns = Vec::with_capacity(rows.len());
    for row in &rows {
        let world_id: String = row.try_get("world_id")?;
        let (packs, synced_at) = catalog_rows(pool, &world_id).await?;
        campaigns.push(CampaignView {
            world_id,
            open: row.try_get("open").unwrap_or(false),
            rules: row
                .try_get::<Option<Value>, _>("rules")
                .ok()
                .flatten()
                .and_then(|rules| serde_json::from_value(rules).ok())
                .unwrap_or_default(),
            packs,
            synced_at,
        });
    }
    Ok(campaigns)
}

/// What the last sync stored for one world: its packs and when it last ran.
async fn catalog_rows(
    pool: &PgPool,
    world_id: &str,
) -> Result<(Vec<PackInfo>, Option<String>), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT pack_id, label, source_book, rules_version,
                jsonb_array_length(items) AS count,
                to_char(synced_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') || ' UTC' AS synced_at
           FROM builder_catalog
          WHERE world_id = $1
          ORDER BY label, pack_id",
    )
    .bind(world_id)
    .fetch_all(pool)
    .await?;
    let mut synced_at: Option<String> = None;
    let mut packs = Vec::with_capacity(rows.len());
    for row in &rows {
        let at: String = row.try_get("synced_at")?;
        synced_at = Some(synced_at.map_or_else(|| at.clone(), |known| known.max(at.clone())));
        packs.push(PackInfo {
            id: row.try_get("pack_id")?,
            label: row.try_get("label")?,
            source_book: row.try_get("source_book")?,
            rules_version: row.try_get("rules_version")?,
            count: row.try_get::<i32, _>("count").unwrap_or_default().max(0) as u32,
        });
    }
    Ok((packs, synced_at))
}

/// Drafts as the characters page lists them; `only` narrows to one player. Each
/// draft's open steps are counted from its world's real catalog and rules, by the
/// same functions the builder page calls.
/// The listed drafts with their decisions, plus every skill the listed worlds'
/// rules content names (the set party coverage is measured against).
async fn summaries(
    pool: &PgPool,
    only: Option<&str>,
) -> Result<(Vec<(DraftSummary, Vec<Decision>)>, BTreeMap<String, String>), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT d.id, d.name, d.status, d.choices, d.rolls, d.world_id, r.rules,
                COALESCE(p.display_name, p.email, p.subject) AS player
           FROM character_drafts d
           JOIN principals p ON p.subject = d.subject
           LEFT JOIN campaign_rules r ON r.world_id = d.world_id
          WHERE $1::text IS NULL OR d.subject = $1
          ORDER BY d.name, d.created_at",
    )
    .bind(only)
    .fetch_all(pool)
    .await?;
    // One catalog read per world, however many of its drafts are listed.
    let mut worlds: BTreeMap<String, (CampaignRules, Catalog)> = BTreeMap::new();
    let mut summaries = Vec::with_capacity(rows.len());
    for row in &rows {
        let Ok(world_id) = row.try_get::<String, _>("world_id") else {
            continue;
        };
        if !worlds.contains_key(&world_id) {
            let rules: CampaignRules = row
                .try_get::<Value, _>("rules")
                .ok()
                .and_then(|rules| serde_json::from_value(rules).ok())
                .unwrap_or_default();
            let catalog = catalog(pool, &world_id, &rules.sources)
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to read a world's rules content");
                    Catalog::default()
                });
            worlds.insert(world_id.clone(), (rules, catalog));
        }
        let (rules, catalog) = &worlds[&world_id];
        let Ok(name) = row.try_get::<String, _>("name") else {
            continue;
        };
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
        let (Ok(id), Ok(status), Ok(player)) = (
            row.try_get::<Uuid, _>("id"),
            row.try_get::<String, _>("status"),
            row.try_get::<String, _>("player"),
        ) else {
            continue;
        };
        let Some(status) = DraftStatus::from_db(&status) else {
            continue;
        };
        let (found, to_do) = draft_state(catalog, rules, &choices, &name, rolls.as_ref());
        summaries.push((
            DraftSummary {
                id: id.to_string(),
                name,
                status,
                player,
                pending: to_do.len() as u32,
                class_name: catalog
                    .root(&choices, Slot::Class)
                    .map(|item| item.name.clone()),
                species_name: catalog
                    .root(&choices, Slot::Species)
                    .map(|item| item.name.clone()),
                background_name: catalog
                    .root(&choices, Slot::Background)
                    .map(|item| item.name.clone()),
                totals: totals(&found, &choices, None),
                choices,
                rolls,
            },
            found,
        ));
    }
    let skills = worlds
        .values()
        .flat_map(|(_, catalog)| catalog_skills(catalog))
        .collect();
    Ok((summaries, skills))
}

/// One draft's decisions and open steps, from the functions the builder page
/// calls: the board's count and the player's to-do list cannot disagree.
fn draft_state(
    catalog: &Catalog,
    rules: &CampaignRules,
    choices: &DraftChoices,
    name: &str,
    rolls: Option<&Rolls>,
) -> (Vec<Decision>, Vec<Pending>) {
    let decisions = decisions(catalog, choices, rules.starting_level);
    let to_do = pending(Some(catalog), rules, choices, name, rolls, &decisions);
    (decisions, to_do)
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
    let catalog = match catalog(pool, &draft.world_id, &draft.rules.sources).await {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(%error, "failed to read a world's rules content");
            Catalog::default()
        }
    };
    let level = draft.rules.starting_level;
    let (decisions, to_do) = draft_state(
        &catalog,
        &draft.rules,
        &draft.choices,
        &draft.name,
        draft.rolls.as_ref(),
    );
    let equipment = equipment_plans(&catalog, &draft.choices, &draft.rules);
    BuilderView::Ready(Box::new(BuilderData {
        id: id.to_string(),
        editable: draft.status == DraftStatus::Draft && draft.open && draft.subject == subject,
        name: draft.name.clone(),
        status: draft.status,
        rules: draft.rules,
        rolls: draft.rolls.clone(),
        pending: to_do,
        dm_note: draft.dm_note,
        srd_attribution: catalog
            .packs
            .iter()
            .any(|pack| pack.source_book.as_deref() == Some("SRD 5.2")),
        totals: totals(&decisions, &draft.choices, None),
        granted: granted_traits(&decisions, &draft.choices)
            .into_iter()
            .collect(),
        class: class_details(&catalog, &draft.choices, level),
        classes: classes(&catalog),
        species: origin_options(&catalog, Slot::Species),
        backgrounds: origin_options(&catalog, Slot::Background),
        species_details: origin_details(&catalog, &draft.choices, level, Slot::Species),
        background_details: origin_details(&catalog, &draft.choices, level, Slot::Background),
        equipment,
        choices: draft.choices,
    }))
}

/// Every synced document of the world, the enabled sources' among them: the
/// class step lists only the enabled packs' documents, while a kit's items and
/// a feature a chosen document grants resolve in the world's other packs too.
pub(crate) async fn catalog(
    executor: impl sqlx::PgExecutor<'_>,
    world_id: &str,
    sources: &[String],
) -> Result<Catalog, sqlx::Error> {
    // ponytail: every pack of the world on each read; narrow it to the packs
    // kits and grants name if a large licensed world ever makes this slow.
    let rows = sqlx::query(
        "SELECT b.pack_id, b.label, b.source_book, b.rules_version,
                jsonb_array_length(b.items) AS count, b.items,
                b.pack_id = ANY($2) AS enabled, r.redirects
           FROM builder_catalog b
           CROSS JOIN (
                SELECT COALESCE(jsonb_object_agg(from_prefix, to_prefix), '{}'::jsonb) AS redirects
                  FROM builder_redirects WHERE world_id = $1
           ) r
          WHERE b.world_id = $1
          ORDER BY b.label, b.pack_id",
    )
    .bind(world_id)
    .bind(sources)
    .fetch_all(executor)
    .await?;
    let redirects: Vec<Redirect> = rows
        .first()
        .and_then(|row| row.try_get::<Value, _>("redirects").ok())
        .and_then(|value| {
            serde_json::from_value::<std::collections::BTreeMap<String, String>>(value).ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|(from, to)| Redirect { from, to })
        .collect();
    let mut catalog = Catalog {
        redirects,
        ..Catalog::default()
    };
    for row in &rows {
        if row.try_get("enabled").unwrap_or(false) {
            catalog.packs.push(PackInfo {
                id: row.try_get("pack_id")?,
                label: row.try_get("label")?,
                source_book: row.try_get("source_book")?,
                rules_version: row.try_get("rules_version")?,
                count: row.try_get::<i32, _>("count").unwrap_or_default().max(0) as u32,
            });
        }
        let items: Vec<CatalogItem> = row
            .try_get::<Value, _>("items")
            .ok()
            .and_then(|items| serde_json::from_value(items).ok())
            .unwrap_or_default();
        catalog.items.extend(items);
    }
    Ok(catalog)
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
        world_id: row.try_get("world_id")?,
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
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => return Some(database_code(error)),
    };
    let mut rules = sanitize_rules(rules);
    rules.sources = match known_sources(&mut transaction, world_id, rules.sources).await {
        Ok(sources) => sources,
        Err(error) => return Some(database_code(error)),
    };
    let value = serde_json::to_value(rules).ok()?;
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
        sources: rules.sources,
    }
}

/// The requested sources that the world's catalog actually holds; anything
/// else a caller sends is dropped rather than stored.
async fn known_sources(
    transaction: &mut Transaction<'_, Postgres>,
    world_id: &str,
    requested: Vec<String>,
) -> Result<Vec<String>, sqlx::Error> {
    let mut wanted: Vec<String> = Vec::new();
    for source in requested.into_iter().take(SOURCE_LIMIT) {
        if !wanted.contains(&source) {
            wanted.push(source);
        }
    }
    if wanted.is_empty() {
        return Ok(wanted);
    }
    let known: Vec<String> = sqlx::query_scalar(
        "SELECT pack_id FROM builder_catalog WHERE world_id = $1 AND pack_id = ANY($2)",
    )
    .bind(world_id)
    .bind(&wanted)
    .fetch_all(&mut **transaction)
    .await?;
    Ok(wanted
        .into_iter()
        .filter(|source| known.contains(source))
        .collect())
}

/// Snapshot the running world's Item packs into the catalog cache. Foundry
/// stays the authority: the rows are a cache of its documents, keyed by uuid.
pub async fn sync_catalog(world_id: &str) -> Result<Vec<PackInfo>, String> {
    let (state, principal, role) = member().map_err(str::to_owned)?;
    if role == Role::Player {
        return Err("permission_denied".into());
    }
    let Some(pool) = state.database.pool().await else {
        return Err("database_unavailable".into());
    };
    sync_catalog_for(&state, &pool, &principal.subject, world_id).await
}

pub(crate) async fn sync_catalog_for(
    state: &AppState,
    pool: &PgPool,
    subject: &str,
    world_id: &str,
) -> Result<Vec<PackInfo>, String> {
    let snapshot = state.driver.snapshot();
    if snapshot.state != WorldState::Ready {
        return Err("world_not_ready".into());
    }
    if snapshot.id.as_deref() != Some(world_id) {
        return Err("world_mismatch".into());
    }
    if snapshot.system_id.as_deref() != Some("dnd5e") {
        return Err("not_dnd5e".into());
    }
    let system_version = snapshot.system_version.unwrap_or_default();
    let mut enable_attempted = false;
    let reply = catalog_sync_call(
        state.driver.as_ref(),
        pool,
        subject,
        world_id,
        &mut enable_attempted,
        WorldOp::CatalogPacks,
        "the catalog pack list",
    )
    .await?;
    let reply: CatalogPacks = serde_json::from_value(reply).map_err(|error| {
        tracing::warn!(%error, "the catalog pack list did not parse");
        "foundry_unavailable".to_owned()
    })?;
    // Only compendium pack prefixes are stored; a module is not trusted to send
    // anything else. A module older than `uuidRedirects()` sends none.
    let redirects: Vec<Redirect> = reply
        .redirects
        .into_iter()
        .filter(Redirect::valid)
        .collect();
    let packs = reply.packs;

    let mut synced: Vec<(PackInfo, Vec<CatalogItem>)> = Vec::with_capacity(packs.len());
    for pack in packs {
        let mut items = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let reply = catalog_sync_call(
                state.driver.as_ref(),
                pool,
                subject,
                world_id,
                &mut enable_attempted,
                WorldOp::CatalogPage {
                    pack: pack.id.clone(),
                    after: after.clone(),
                    limit: CATALOG_PAGE,
                },
                "a catalog page",
            )
            .await?;
            let page: CatalogPage = serde_json::from_value(reply).map_err(|error| {
                tracing::warn!(%error, pack = %pack.id, "a catalog page did not parse");
                "foundry_unavailable".to_owned()
            })?;
            items.extend(page.items);
            match page.next_cursor {
                // A cursor that does not advance would page forever.
                Some(cursor) if Some(&cursor) != after.as_ref() => after = Some(cursor),
                Some(_) => return Err("foundry_unavailable".into()),
                None => break,
            }
        }
        synced.push((pack, items));
    }

    let mut transaction = pool.begin().await.map_err(database_code)?;
    let first: i64 = sqlx::query_scalar("SELECT count(*) FROM builder_catalog WHERE world_id = $1")
        .bind(world_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_code)?;
    for (pack, items) in &synced {
        let items = serde_json::to_value(items).map_err(|error| error.to_string())?;
        sqlx::query(
            "INSERT INTO builder_catalog
                    (world_id, pack_id, label, source_book, rules_version, system_version, items, synced_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())
             ON CONFLICT (world_id, pack_id) DO UPDATE
                SET label = EXCLUDED.label, source_book = EXCLUDED.source_book,
                    rules_version = EXCLUDED.rules_version, system_version = EXCLUDED.system_version,
                    items = EXCLUDED.items, synced_at = now()",
        )
        .bind(world_id)
        .bind(&pack.id)
        .bind(&pack.label)
        .bind(&pack.source_book)
        .bind(&pack.rules_version)
        .bind(&system_version)
        .bind(items)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    }
    // A pack the world no longer offers leaves the cache, and with it every
    // saved source the GM could no longer justify.
    let ids: Vec<String> = synced.iter().map(|(pack, _)| pack.id.clone()).collect();
    sqlx::query("DELETE FROM builder_catalog WHERE world_id = $1 AND pack_id <> ALL($2)")
        .bind(world_id)
        .bind(&ids)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    // The redirects belong to the same snapshot as the documents they rewrite.
    sqlx::query("DELETE FROM builder_redirects WHERE world_id = $1")
        .bind(world_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    for redirect in &redirects {
        sqlx::query(
            "INSERT INTO builder_redirects (world_id, from_prefix, to_prefix)
             VALUES ($1, $2, $3) ON CONFLICT (world_id, from_prefix)
                DO UPDATE SET to_prefix = EXCLUDED.to_prefix",
        )
        .bind(world_id)
        .bind(&redirect.from)
        .bind(&redirect.to)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    }
    if first == 0 {
        // The first sync proposes the 2024 rules; a GM may still turn them off.
        let modern: Vec<String> = synced
            .iter()
            .filter(|(pack, _)| pack.rules_version.as_deref() == Some("modern"))
            .map(|(pack, _)| pack.id.clone())
            .collect();
        // Syncing before the rules were ever saved is the usual order, so the
        // defaults need a (closed) campaign row to land in.
        sqlx::query(
            "INSERT INTO campaign_rules (world_id, open, rules, updated_by)
             VALUES ($1, false, $2, $3) ON CONFLICT (world_id) DO NOTHING",
        )
        .bind(world_id)
        .bind(serde_json::to_value(CampaignRules::default()).map_err(|error| error.to_string())?)
        .bind(subject)
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
        sqlx::query(
            "UPDATE campaign_rules SET rules = jsonb_set(rules, '{sources}', $2), updated_at = now()
              WHERE world_id = $1 AND COALESCE(jsonb_array_length(rules->'sources'), 0) = 0",
        )
        .bind(world_id)
        .bind(json!(modern))
        .execute(&mut *transaction)
        .await
        .map_err(database_code)?;
    }
    let mut info: Vec<PackInfo> = synced
        .into_iter()
        .map(|(pack, items)| PackInfo {
            count: items.len() as u32,
            ..pack
        })
        .collect();
    info.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
    audit(
        &mut transaction,
        subject,
        "catalog.sync",
        json!({
            "worldId": world_id,
            "packs": info.len(),
            "items": info.iter().map(|pack| pack.count).sum::<u32>(),
        }),
    )
    .await;
    transaction.commit().await.map_err(database_code)?;
    Ok(info)
}

/// One page of a pack's documents, in the module's own reply shape.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogPage {
    items: Vec<CatalogItem>,
    next_cursor: Option<String>,
}

/// Retry one inactive-module read after enabling, at most once per sync.
async fn catalog_sync_call(
    driver: &dyn FoundryDriver,
    pool: &sqlx::PgPool,
    subject: &str,
    world_id: &str,
    attempted: &mut bool,
    op: WorldOp,
    what: &str,
) -> Result<Value, String> {
    let result = catalog_call(driver, op.clone(), what).await;
    if !should_enable(&result, *attempted) {
        return result;
    }
    *attempted = true;
    let enabled = catalog_call(driver, WorldOp::ModuleEnable, "the module enable").await?;
    let mut transaction = pool.begin().await.map_err(database_code)?;
    audit(
        &mut transaction,
        subject,
        "catalog.module_enable",
        json!({"worldId": world_id, "enabled": enabled["enabled"]}),
    )
    .await;
    transaction.commit().await.map_err(database_code)?;
    catalog_call(driver, op, what).await
}

fn should_enable(result: &Result<Value, String>, attempted: bool) -> bool {
    !attempted && matches!(result, Err(code) if code == "module_inactive")
}

/// One catalog driver call; the module's refusals keep their own codes.
async fn catalog_call(
    driver: &dyn FoundryDriver,
    op: WorldOp,
    what: &str,
) -> Result<Value, String> {
    match driver.call(op).await {
        Ok(reply) => Ok(reply),
        Err(error) => {
            let code = error.catalog_code();
            if code == "foundry_unavailable" {
                tracing::warn!(%error, what, "the catalog sync driver call failed");
            }
            Err(code.to_owned())
        }
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
    let catalog = catalog(&mut *transaction, &draft.world_id, &draft.rules.sources)
        .await
        .map_err(database_code)?;
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
        DraftPatch::Select { slot, uuid } => {
            // Only a document an enabled source offers may fill a slot.
            if let Some(uuid) = &uuid
                && !catalog.offers(uuid, slot.item_type())
            {
                return Err("not_offered".into());
            }
            let mut choices = draft.choices;
            choices.select(slot, uuid);
            // Only what the roots still open stays; a nested decision's key
            // carries its granted feature, not the replaced root.
            prune_picks(&catalog, &mut choices, draft.rules.starting_level);
            (draft.name, choices)
        }
        DraftPatch::Pick { key, values } => {
            let mut choices = draft.choices;
            // The list is bounded before any decision walks it.
            if values.len() > 32 || values.iter().any(|value| value.len() > 200) {
                return Err("pick_not_offered".into());
            }
            let level = draft.rules.starting_level;
            let decisions = decisions(&catalog, &choices, level);
            let decision = decisions
                .iter()
                .find(|decision| decision.key == key)
                .ok_or_else(|| "unknown_decision".to_owned())?;
            validate_pick(decision, &values).map_err(str::to_owned)?;
            choices.picks.insert(key, values);
            // A dropped item takes its sub-decisions' answers with it.
            prune_picks(&catalog, &mut choices, level);
            (draft.name, choices)
        }
        DraftPatch::Asi { key, deltas } => {
            let mut choices = draft.choices;
            if deltas.len() > Scores::LABELS.len() {
                return Err("asi_locked".into());
            }
            let level = draft.rules.starting_level;
            let decisions = decisions(&catalog, &choices, level);
            let decision = decisions
                .iter()
                .find(|decision| decision.key == key)
                .ok_or_else(|| "unknown_decision".to_owned())?;
            validate_asi(decision, &deltas, &totals(&decisions, &choices, Some(&key)))
                .map_err(str::to_owned)?;
            choices.asi.insert(key, deltas);
            prune_picks(&catalog, &mut choices, level);
            (draft.name, choices)
        }
        DraftPatch::Equipment { owner, option } => {
            // Bounded before any plan walks the content.
            if owner.len() > 200 || option.len() > 32 {
                return Err("not_offered".into());
            }
            let mut choices = draft.choices;
            let plans = equipment_plans(&catalog, &choices, &draft.rules);
            validate_equipment(&plans, &owner, &option).map_err(str::to_owned)?;
            choices.equipment.insert(owner, option);
            (draft.name, choices)
        }
        DraftPatch::Detail { key, value } => {
            let value = validate_detail(&key, &value).map_err(str::to_owned)?;
            let mut choices = draft.choices;
            // An emptied field is no field; the sheet stores only what it shows.
            if value.is_empty() {
                choices.details.remove(&key);
            } else {
                choices.details.insert(key, value);
            }
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

/// The player hands their own finished draft to the DM. The to-do list is
/// recomputed here from the world's real catalog and campaign rules, so a page
/// that went stale cannot submit an unfinished character.
pub async fn submit_draft(id: &str) -> Option<String> {
    let (state, principal, _) = match member() {
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
    // Own, still a draft and in an open campaign, with the row locked.
    let draft = match locked_own_draft(&mut transaction, id, &principal.subject).await {
        Ok(draft) => draft,
        Err(code) => return Some(code),
    };
    let catalog = match catalog(&mut *transaction, &draft.world_id, &draft.rules.sources).await {
        Ok(catalog) => catalog,
        Err(error) => return Some(database_code(error)),
    };
    let (_, to_do) = draft_state(
        &catalog,
        &draft.rules,
        &draft.choices,
        &draft.name,
        draft.rolls.as_ref(),
    );
    if let Err(code) = submit_refusal(&to_do) {
        return Some(code.into());
    }
    if let Err(error) = sqlx::query(
        "UPDATE character_drafts SET status = 'submitted', updated_at = now()
          WHERE id = $1 AND status = 'draft'",
    )
    .bind(id)
    .execute(&mut *transaction)
    .await
    {
        return Some(database_code(error));
    }
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.submit",
        json!({"draftId": id, "subject": draft.subject}),
    )
    .await;
    match transaction.commit().await {
        Ok(()) => None,
        Err(error) => Some(database_code(error)),
    }
}

/// Approve one submitted draft: create its Actor in Foundry through dnd5e's own
/// advancement code, assign it to the player, and record the id. The row stays
/// locked for the whole approval, so two approvals cannot both create.
pub async fn approve_draft(id: &str) -> Option<String> {
    let (state, principal, role) = match member() {
        Ok(member) => member,
        Err(code) => return Some(code.into()),
    };
    let Some(pool) = state.database.pool().await else {
        return Some("database_unavailable".into());
    };
    let draft_id = match Uuid::parse_str(id) {
        Ok(id) => id,
        Err(_) => return Some("draft_not_found".into()),
    };
    match approve(&state, &pool, draft_id, &principal, role).await {
        Ok(()) => None,
        Err(code) => {
            // Every outcome is audited, refusals included.
            if let Err(error) = session::insert_audit_for(
                &pool,
                "character_draft",
                Some(&principal.subject),
                "character_draft.approve",
                "refused",
                json!({"draftId": draft_id, "code": code}),
            )
            .await
            {
                tracing::warn!(%error, "failed to write the approval refusal audit entry");
            }
            Some(code)
        }
    }
}

/// Everything approval requires, checked under the draft row lock and the
/// running session's lease row lock (held until the caller commits, as
/// `adoption::enter` holds it, so stop/setup transitions cannot change the holder
/// or world mid-write): an admin, or that session's GM; a submitted draft; and
/// the driver serving the draft's world, up.
async fn approval_checks(
    transaction: &mut Transaction<'_, Postgres>,
    state: &AppState,
    id: Uuid,
    principal: &auth::Principal,
    role: Role,
) -> Result<DraftRow, String> {
    let draft = draft_row(&mut **transaction, id, true)
        .await
        .map_err(database_code)?
        .ok_or_else(|| "draft_not_found".to_owned())?;
    let holder: Option<String> = sqlx::query_scalar(
        "SELECT holder FROM game_session
          WHERE ended_at IS NULL AND phase = 'running' AND world_id = $1
          FOR UPDATE",
    )
    .bind(&draft.world_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_code)?;
    let allowed = role == Role::Admin
        || (role == Role::Gm && holder.as_deref() == Some(principal.subject.as_str()));
    if !allowed {
        return Err("permission_denied".into());
    }
    if draft.status != DraftStatus::Submitted {
        return Err("draft_not_submitted".into());
    }
    let snapshot = state.driver.snapshot();
    if snapshot.state != WorldState::Ready {
        return Err("world_not_ready".into());
    }
    if snapshot.id.as_deref() != Some(draft.world_id.as_str()) {
        return Err("world_mismatch".into());
    }
    Ok(draft)
}

/// The approval itself, under the locked draft row. Foundry is the authority:
/// nothing is written until it has created the actor, and the row only records
/// what Foundry answered.
pub(crate) async fn approve(
    state: &AppState,
    pool: &PgPool,
    id: Uuid,
    principal: &auth::Principal,
    role: Role,
) -> Result<(), String> {
    // Two phases, one connection at a time (the pool is small, and a second
    // connection taken under the session lock can starve it). Phase one checks
    // everything under the locks and commits the owner's mapping, so a later
    // timed-out create never orphans a new user; phase two relocks, rechecks and
    // writes.
    let mut transaction = pool.begin().await.map_err(database_code)?;
    let draft = approval_checks(&mut transaction, state, id, principal, role).await?;
    // A4: a player who never entered has no Foundry user yet; approval makes it
    // through the same function entry uses.
    player_user(&mut transaction, state.driver.as_ref(), &draft).await?;
    transaction.commit().await.map_err(database_code)?;

    let mut transaction = pool.begin().await.map_err(database_code)?;
    let draft = approval_checks(&mut transaction, state, id, principal, role).await?;
    let owner = adoption::mapped_user(&mut *transaction, &draft.subject, &draft.world_id)
        .await
        .map_err(database_code)?
        .ok_or_else(|| "foundry_rejected".to_owned())?;
    let catalog = catalog(&mut *transaction, &draft.world_id, &draft.rules.sources)
        .await
        .map_err(database_code)?;
    let args = create_args(
        &catalog,
        &draft.rules,
        &draft.choices,
        &draft.name,
        draft.rolls.as_ref(),
        &id.to_string(),
        &owner,
    )?;
    let reply = state
        .driver
        .call(WorldOp::CharacterCreate { args })
        .await
        .map_err(|error| approval_code(&error))?;
    let actor_id = reply["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "foundry_rejected".to_owned())?
        .to_owned();
    sqlx::query(
        "UPDATE character_drafts SET status = 'approved', actor_id = $2, updated_at = now()
          WHERE id = $1 AND status = 'submitted'",
    )
    .bind(id)
    .bind(&actor_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_code)?;
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.approve",
        json!({
            "draftId": id,
            "subject": draft.subject,
            "actorId": actor_id,
            "foundryUserId": owner,
            "created": reply["created"],
            "manual": reply["manual"],
        }),
    )
    .await;
    transaction.commit().await.map_err(database_code)?;

    // Best effort: the world's chat says who joined. A failed post is not a
    // failed character.
    let class = catalog
        .root(&draft.choices, Slot::Class)
        .map_or_else(|| "adventurer".to_owned(), |item| item.name.clone());
    let content = approval_message(
        &draft.name,
        &class,
        draft.rules.starting_level,
        &draft.choices,
        draft.rolls.as_ref(),
    );
    if let Err(error) = state.driver.call(WorldOp::ChatCreate { content }).await {
        tracing::warn!(%error, "the approval chat message was not posted");
    }
    Ok(())
}

/// The draft player's Foundry user id. An existing mapping is used as it is (its
/// role is never touched, so a GM approving their own character stays a GM).
/// Only an unmapped player gets a new player-role user; `approve` commits that
/// mapping in its first phase, before the actor is written, so a timed-out create
/// never leaves an actor owned by a user the broker forgot. Entry sets the
/// password on the player's first join, not here.
async fn player_user(
    transaction: &mut Transaction<'_, Postgres>,
    driver: &dyn FoundryDriver,
    draft: &DraftRow,
) -> Result<String, String> {
    if let Some(user_id) =
        adoption::mapped_user(&mut **transaction, &draft.subject, &draft.world_id)
            .await
            .map_err(database_code)?
    {
        return Ok(user_id);
    }
    let display_name: Option<String> = sqlx::query_scalar(
        "SELECT COALESCE(display_name, email, subject) FROM principals WHERE subject = $1",
    )
    .bind(&draft.subject)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_code)?;
    adoption::ensure_mapped_user(
        transaction,
        driver,
        &draft.subject,
        display_name.as_deref().unwrap_or(&draft.subject),
        &draft.world_id,
        adoption::PLAYER_ROLE,
        None,
    )
    .await
    .map_err(|error| match error {
        adoption::EnterError::Database => "database_unavailable".to_owned(),
        _ => "foundry_rejected".to_owned(),
    })
}

/// The code a failed character write reports: the module's own two refusals keep
/// their names, and everything else is the world refusing it.
fn approval_code(error: &WorldError) -> String {
    match error {
        WorldError::Unsupported(code)
            if matches!(
                code.as_str(),
                "module_missing" | "module_inactive" | "module_outdated"
            ) =>
        {
            code.clone()
        }
        WorldError::NotReady => "world_not_ready".into(),
        _ => "foundry_rejected".into(),
    }
}

/// The chat line an approval posts: the character, its class and level, the
/// ability method with its scores, and the dice behind a roll.
fn approval_message(
    name: &str,
    class: &str,
    level: u8,
    choices: &DraftChoices,
    rolls: Option<&Rolls>,
) -> String {
    let method = choices.method.map_or("chosen", AbilityMethod::label);
    let mut message =
        format!("{name} the {class} (level {level}) is ready to play. Abilities: {method}");
    if let Some(base) = choices.base {
        let scores: Vec<String> = base.values().iter().map(u8::to_string).collect();
        message.push_str(&format!(". {}", scores.join(" ")));
    }
    if let Some(rolls) = rolls {
        let sets: Vec<String> = rolls
            .sets
            .iter()
            .map(|set| {
                let dice: Vec<String> = set.dice.iter().map(u8::to_string).collect();
                format!("{} = {}", dice.join(" "), set.total)
            })
            .collect();
        message.push_str(&format!(". Rolls: {}", sets.join(", ")));
    }
    message.push('.');
    message
}

/// A GM or admin sends a submitted draft back with a note the player can read.
pub async fn request_changes(id: &str, note: &str) -> Option<String> {
    let (state, principal, role) = match member() {
        Ok(member) => member,
        Err(code) => return Some(code.into()),
    };
    if role == Role::Player {
        return Some("permission_denied".into());
    }
    let note = match validate_note(note) {
        Ok(note) => note,
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
    // A GM reads every adopted world's drafts, as the party board lists them, so
    // this keeps the same scope as `grant_reroll`.
    let draft = match draft_row(&mut *transaction, id, true).await {
        Ok(Some(draft)) => draft,
        Ok(None) => return Some("draft_not_found".into()),
        Err(error) => return Some(database_code(error)),
    };
    if draft.status != DraftStatus::Submitted {
        return Some("not_submitted".into());
    }
    if let Err(error) = sqlx::query(
        "UPDATE character_drafts SET status = 'draft', dm_note = $2, updated_at = now()
          WHERE id = $1",
    )
    .bind(id)
    .bind(&note)
    .execute(&mut *transaction)
    .await
    {
        return Some(database_code(error));
    }
    audit(
        &mut transaction,
        &principal.subject,
        "character_draft.request_changes",
        json!({"draftId": id, "subject": draft.subject}),
    )
    .await;
    match transaction.commit().await {
        Ok(()) => None,
        Err(error) => Some(database_code(error)),
    }
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

#[cfg(test)]
mod module_enable_tests {
    use super::*;
    #[test]
    fn retries_only_inactive_once() {
        assert!(should_enable(&Err("module_inactive".into()), false));
        assert!(!should_enable(&Err("module_inactive".into()), true));
        assert!(!should_enable(&Err("module_missing".into()), false));
        assert!(!should_enable(&Ok(json!({})), false));
    }
}
