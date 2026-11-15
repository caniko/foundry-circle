//! The character builder pages: the characters board and one draft's sheet.
//!
//! The pages are pure components over the views `builder_store` returns, so the
//! SSR tests need neither a server nor a router.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use tartan_ui_core::{Feedback, FeedbackKind, FilterChoice, FilterOption, ResourceLink};
use tartan_ui_dioxus::{
    CardGrid, CheckboxFilterGroup, DashboardHeader, DetailDisclosure, EmptyState, FeedbackBanner,
    Field, Panel, ResourceCard, SelectField, TagList,
};

use crate::{
    ConsoleShell, Role, Route,
    builder::{
        AbilityMethod, AbilityRoll, BuilderData, BuilderView, CampaignRules, CampaignView,
        CharactersView, ClassDetails, ClassOption, DETAIL_LABELS, DETAIL_MAX, Decision,
        DecisionKind, DraftPatch, DraftStatus, DraftSummary, EquipmentPlan, NOTE_MAX, Opt,
        OriginDetails, POINT_BUY_BUDGET, PackInfo, PartySummary, Pending, Rolls, SRD_ATTRIBUTION,
        Scores, Slot, Step, TraitMode, ability_label, hit_points, modifier, point_buy_cost,
        size_label, trait_options, validate_asi,
    },
};

/// The characters page's data; the viewer's name only appears when denied.
#[server]
async fn characters_view() -> Result<CharactersView, ServerFnError> {
    Ok(crate::http::builder_store::characters_view().await)
}

#[server]
async fn builder_view(id: String) -> Result<BuilderView, ServerFnError> {
    Ok(crate::http::builder_store::builder_view(&id).await)
}

#[server]
async fn save_rules(
    world_id: String,
    open: bool,
    rules: CampaignRules,
) -> Result<Option<String>, ServerFnError> {
    Ok(crate::http::builder_store::save_rules(&world_id, open, rules).await)
}

#[server]
async fn sync_catalog(world_id: String) -> Result<Result<Vec<PackInfo>, String>, ServerFnError> {
    Ok(crate::http::builder_store::sync_catalog(&world_id).await)
}

#[server]
async fn create_draft(world_id: String) -> Result<Result<String, String>, ServerFnError> {
    Ok(crate::http::builder_store::create_draft(&world_id).await)
}

#[server]
async fn update_draft(
    id: String,
    patch: DraftPatch,
) -> Result<Result<BuilderView, String>, ServerFnError> {
    Ok(crate::http::builder_store::update_draft(&id, patch).await)
}

#[server]
async fn roll_abilities(id: String) -> Result<Result<BuilderView, String>, ServerFnError> {
    Ok(crate::http::builder_store::roll_abilities(&id).await)
}

#[server]
async fn submit_draft(id: String) -> Result<Option<String>, ServerFnError> {
    Ok(crate::http::builder_store::submit_draft(&id).await)
}

#[server]
async fn request_changes(id: String, note: String) -> Result<Option<String>, ServerFnError> {
    Ok(crate::http::builder_store::request_changes(&id, &note).await)
}

#[server]
async fn approve_draft(id: String) -> Result<Option<String>, ServerFnError> {
    Ok(crate::http::builder_store::approve_draft(&id).await)
}

// Slice one ships these two server functions; the board controls that call
// them arrive with the DM board.
#[server]
async fn grant_reroll(id: String) -> Result<Option<String>, ServerFnError> {
    Ok(crate::http::builder_store::grant_reroll(&id).await)
}

#[server]
async fn delete_draft(id: String) -> Result<Option<String>, ServerFnError> {
    Ok(crate::http::builder_store::delete_draft(&id).await)
}

/// `/api/console/characters`: what the caller may build.
#[component]
pub fn CharactersPage() -> Element {
    let view = use_server_future(characters_view)?;
    let mut error = use_signal(|| None::<String>);
    let navigator = use_navigator();
    let mut reload = view;
    let create = EventHandler::new(move |world_id: String| {
        spawn(async move {
            match create_draft(world_id).await {
                Ok(Ok(id)) => {
                    navigator.push(Route::Builder { id });
                }
                Ok(Err(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
        });
    });
    let save = EventHandler::new(
        move |(world_id, open, rules): (String, bool, CampaignRules)| {
            spawn(async move {
                match save_rules(world_id, open, rules).await {
                    Ok(None) => reload.restart(),
                    Ok(Some(code)) => error.set(Some(code_message(&code).into())),
                    Err(_) => error.set(Some(UNAVAILABLE.into())),
                }
            });
        },
    );
    let sync = EventHandler::new(move |world_id: String| {
        spawn(async move {
            match sync_catalog(world_id).await {
                Ok(Ok(_)) => reload.restart(),
                Ok(Err(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
        });
    });
    let ask = EventHandler::new(move |(id, note): (String, String)| {
        spawn(async move {
            match request_changes(id, note).await {
                Ok(None) => reload.restart(),
                Ok(Some(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
        });
    });
    let approve = EventHandler::new(move |id: String| {
        spawn(async move {
            match approve_draft(id).await {
                Ok(None) => reload.restart(),
                Ok(Some(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
        });
    });
    let view = match &*view.read() {
        Some(Ok(view)) => view.clone(),
        _ => CharactersView::SignedOut,
    };
    let account = match &view {
        CharactersView::Denied { name } => Some(name.clone()),
        _ => None,
    };
    rsx! {
        ConsoleShell { account,
            CharactersBoard { view, create, save, sync, ask, approve, error: error() }
        }
    }
}

/// `/api/console/characters/{id}`: one draft's sheet.
#[component]
pub fn BuilderPage(id: String) -> Element {
    let load = id.clone();
    let mut reload = use_server_future(move || {
        let id = load.clone();
        async move { builder_view(id).await }
    })?;
    let mut error = use_signal(|| None::<String>);
    // Each edit sends whole scores, so the sheet is read-only while one is in
    // flight and then shows the view the server returned.
    let mut busy = use_signal(|| false);
    let mut fresh = use_signal(|| None::<BuilderView>);
    let patch_id = id.clone();
    let patch = EventHandler::new(move |patch: DraftPatch| {
        if busy() {
            return;
        }
        busy.set(true);
        let id = patch_id.clone();
        spawn(async move {
            match update_draft(id, patch).await {
                Ok(Ok(view)) => fresh.set(Some(view)),
                Ok(Err(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
            busy.set(false);
        });
    });
    let roll_id = id.clone();
    let roll = EventHandler::new(move |()| {
        if busy() {
            return;
        }
        busy.set(true);
        let id = roll_id.clone();
        spawn(async move {
            match roll_abilities(id).await {
                Ok(Ok(view)) => fresh.set(Some(view)),
                Ok(Err(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
            busy.set(false);
        });
    });
    // Submitting shows the server's own answer: the closed draft, its status and
    // whatever note the DM left last time.
    let submit_id = id.clone();
    let submit = EventHandler::new(move |()| {
        if busy() {
            return;
        }
        busy.set(true);
        let id = submit_id.clone();
        spawn(async move {
            match submit_draft(id).await {
                Ok(None) => {
                    fresh.set(None);
                    reload.restart();
                }
                Ok(Some(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
            busy.set(false);
        });
    });
    let mut view = match (fresh(), &*reload.read()) {
        (Some(view), _) => view,
        (None, Some(Ok(view))) => view.clone(),
        _ => BuilderView::Missing,
    };
    if let (true, BuilderView::Ready(data)) = (busy(), &mut view) {
        data.editable = false;
    }
    rsx! {
        ConsoleShell { account: None,
            BuilderSheet { view, patch, roll, submit, error: error() }
        }
    }
}

const UNAVAILABLE: &str = "That did not work; try again in a moment.";

/// The refusal codes the builder server functions hand back.
fn code_message(code: &str) -> &'static str {
    match code {
        "not_signed_in" => "Sign in again to continue.",
        "permission_denied" => "That draft is not yours to change.",
        "database_unavailable" => "The control plane database did not answer.",
        "campaign_closed" => "The GM has not opened character creation for this world.",
        "draft_not_found" => "That draft no longer exists.",
        "draft_locked" => "This draft is no longer editable.",
        "draft_limit" => "You already have five characters in this world.",
        "method_not_allowed" => "The GM does not allow that ability method.",
        "invalid_scores" => "Those ability scores do not follow the method.",
        "already_rolled" => "These abilities are already rolled.",
        "invalid_name" => "Give the character a name of one to a hundred characters.",
        "not_offered" => "The campaign's rules content does not hold that option.",
        "unknown_decision" => "That choice is not open on this draft; reload the page.",
        "pick_count" => "Choose exactly as many options as the feature asks for.",
        "pick_not_offered" => "That option is not one this feature offers.",
        "pick_duplicate" => "Choose each option once.",
        "pick_unsupported" => "The builder cannot resolve that choice yet.",
        "asi_points" => "Assign every ability bonus point the background grants.",
        "asi_cap" => "That ability cannot take more than the background allows.",
        "asi_locked" => "The background does not improve that ability.",
        "asi_max" => "No ability score may rise above 20.",
        "invalid_detail" => "That sheet field is not one the builder stores, or it is too long.",
        "draft_incomplete" => "Finish everything on the to do list before submitting.",
        "not_submitted" => "That draft is not waiting for review.",
        "draft_not_submitted" => "Only a submitted draft can be approved.",
        "invalid_draft" => "That draft is missing its class, species or background.",
        "foundry_rejected" => "Foundry refused to create the character; nothing was written.",
        "invalid_note" => "Write a note of one to a thousand characters.",
        "world_not_ready" => "Start the session and open the world, then try again.",
        "world_mismatch" => "The running world is not this campaign's world.",
        "not_dnd5e" => "The running world does not use the dnd5e system.",
        "module_missing" => "Install the Foundry Circle module, then sync again.",
        "module_inactive" => {
            "The Foundry Circle module is inactive; sync rules content and try again."
        }
        "module_outdated" => "The world runs an older Foundry Circle module; update it first.",
        "foundry_unavailable" => "Foundry did not answer the catalog read; try again.",
        _ => UNAVAILABLE,
    }
}

/// The characters board: my drafts, and for GMs the rules and the party.
#[component]
fn CharactersBoard(
    view: CharactersView,
    create: EventHandler<String>,
    save: EventHandler<(String, bool, CampaignRules)>,
    sync: EventHandler<String>,
    ask: EventHandler<(String, String)>,
    approve: EventHandler<String>,
    error: Option<String>,
) -> Element {
    let content = match view {
        CharactersView::SignedOut => rsx! {
            DashboardHeader {
                eyebrow: "Character builder".to_string(),
                heading: "Sign in to build a character".to_string(),
                description: "Use your Rauthy account to see your drafts.".to_string(),
            }
        },
        CharactersView::Denied { name } => rsx! {
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
        CharactersView::Ready(data) => {
            let role = data.role;
            let description = if role == Role::Player {
                "Build a character between sessions; your GM reviews it before it reaches Foundry."
            } else {
                "Open session zero for a world, then review what the party is building."
            };
            rsx! {
                DashboardHeader {
                    eyebrow: "Character builder".to_string(),
                    heading: "Characters".to_string(),
                    description: description.to_string(),
                }
                section { aria_label: "My characters",
                    h2 { "My characters" }
                    CardGrid {
                        for campaign in data.campaigns.iter().filter(|campaign| campaign.open) {
                            article { class: "tartan-card", key: "{campaign.world_id}",
                                p { class: "tartan-eyebrow", "New character" }
                                h2 { {campaign.world_id.clone()} }
                                p { class: "tartan-muted", {method_summary(&campaign.rules)} }
                                button {
                                    class: "tartan-button",
                                    r#type: "button",
                                    onclick: {
                                        let world_id = campaign.world_id.clone();
                                        move |_| create.call(world_id.clone())
                                    },
                                    "New character"
                                }
                            }
                        }
                        for draft in &data.mine {
                            ResourceCard {
                                resource: draft_link(draft),
                                action_label: "Open".to_string(),
                                p { class: "tartan-tag", {draft.status.label()} }
                                p { class: "tartan-muted", {draft_line(draft)} }
                            }
                        }
                    }
                    if data.mine.is_empty() {
                        EmptyState {
                            heading: "No characters yet".to_string(),
                            message: "Start a draft in an open campaign and the GM will see it here.".to_string(),
                        }
                    }
                }
                if role != Role::Player {
                    Panel {
                        title: Some("Session zero".to_string()),
                        description: Some("Sync the world's rules content, open character creation and choose what the players may use.".to_string()),
                        for campaign in &data.campaigns {
                            // The form's signals start from these props, so any
                            // server-side change to the rules (a sync's default
                            // sources, another tab's save) remounts it instead of
                            // leaving stale choices to be saved back.
                            CampaignRulesForm {
                                key: "{campaign.world_id}|{campaign.open}|{serde_json::to_string(&campaign.rules).unwrap_or_default()}",
                                campaign: campaign.clone(),
                                ready: data.ready_world.as_deref() == Some(campaign.world_id.as_str()),
                                save,
                                sync,
                            }
                        }
                    }
                    Panel {
                        title: Some("Party".to_string()),
                        description: Some("Every draft in your worlds, with its honest rolls and what it still owes.".to_string()),
                        if data.party.is_empty() {
                            p { class: "tartan-muted", "Nobody has started a character yet." }
                        } else {
                            PartyTable {
                                drafts: data.party.clone(),
                                can_approve: data.can_approve,
                                play_url: data.play_url.clone(),
                                ask,
                                approve,
                            }
                            PartySkills { coverage: data.coverage.clone() }
                        }
                    }
                }
            }
        }
    };
    rsx! {
        if let Some(message) = error {
            FeedbackBanner { feedback: Feedback { kind: FeedbackKind::Error, message } }
        }
        {content}
    }
}

/// One campaign's open toggle, allowed methods, sources and sync button.
#[component]
fn CampaignRulesForm(
    campaign: CampaignView,
    ready: bool,
    save: EventHandler<(String, bool, CampaignRules)>,
    sync: EventHandler<String>,
) -> Element {
    let mut open = use_signal(|| campaign.open);
    let mut methods = use_signal(|| campaign.rules.ability_methods.clone());
    let mut sources = use_signal(|| campaign.rules.sources.clone());
    let world_id = campaign.world_id.clone();
    let rules = campaign.rules.clone();
    let choices: Vec<FilterChoice> = campaign
        .packs
        .iter()
        .map(|pack| FilterChoice {
            value: pack.id.clone(),
            label: pack.line(),
            count: Some(u64::from(pack.count)),
            checked: sources().contains(&pack.id),
        })
        .collect();
    rsx! {
        fieldset { class: "builder-form", key: "{campaign.world_id}",
            legend { {campaign.world_id.clone()} }
            label {
                input {
                    r#type: "checkbox",
                    name: "open",
                    checked: open(),
                    onchange: move |event: FormEvent| open.set(event.checked()),
                }
                " Open character creation"
            }
            fieldset {
                legend { "Ability methods" }
                for method in AbilityMethod::ALL {
                    label { key: "{method.key()}",
                        input {
                            r#type: "checkbox",
                            name: "method",
                            value: "{method.key()}",
                            checked: methods().contains(&method),
                            onchange: move |event: FormEvent| {
                                let mut chosen = methods();
                                if event.checked() {
                                    if !chosen.contains(&method) {
                                        chosen.push(method);
                                    }
                                } else {
                                    chosen.retain(|allowed| *allowed != method);
                                }
                                methods.set(chosen);
                            },
                        }
                        " {method.label()}"
                    }
                }
            }
            if choices.is_empty() {
                p { class: "tartan-muted",
                    "No rules content yet. Sync while this world is running to list its packs here."
                }
            } else {
                CheckboxFilterGroup {
                    legend: "Sources".to_string(),
                    name: "source".to_string(),
                    options: choices,
                    on_change: move |(value, checked): (String, bool)| {
                        let mut chosen = sources();
                        if checked {
                            if !chosen.contains(&value) {
                                chosen.push(value);
                            }
                        } else {
                            chosen.retain(|source| *source != value);
                        }
                        sources.set(chosen);
                    },
                }
            }
            div { class: "builder-sync",
                button {
                    class: "tartan-button",
                    r#type: "button",
                    disabled: !ready,
                    onclick: {
                        let world_id = world_id.clone();
                        move |_| sync.call(world_id.clone())
                    },
                    "Sync rules content"
                }
                match &campaign.synced_at {
                    Some(at) => rsx! { span { class: "tartan-muted", "Last sync: {at}" } },
                    None => rsx! { span { class: "tartan-muted", "Never synced." } },
                }
            }
            button {
                class: "tartan-button",
                r#type: "button",
                onclick: {
                    let world_id = world_id.clone();
                    let rules = rules.clone();
                    move |_| {
                        save.call((
                            world_id.clone(),
                            open(),
                            CampaignRules {
                                ability_methods: methods(),
                                sources: sources(),
                                ..rules.clone()
                            },
                        ));
                    }
                },
                "Save rules"
            }
        }
    }
}

/// The DM board: every draft with its status, what it still owes, its class,
/// species, background, totals and the dice behind its rolls; a submitted draft
/// can be sent back with a note or approved into the world, and an approved one
/// links to where it is played.
#[component]
fn PartyTable(
    drafts: Vec<DraftSummary>,
    can_approve: bool,
    play_url: Option<String>,
    ask: EventHandler<(String, String)>,
    approve: EventHandler<String>,
) -> Element {
    rsx! {
        table { class: "builder-party",
            thead {
                tr {
                    th { "Character" }
                    th { "Player" }
                    th { "Status" }
                    th { "To do" }
                    th { "Class" }
                    th { "Species" }
                    th { "Background" }
                    th { "Method" }
                    th { "Scores" }
                    th { "Rolls" }
                    th { "Review" }
                }
            }
            tbody {
                for draft in drafts {
                    tr { key: "{draft.id}",
                        td { a { href: "/api/console/characters/{draft.id}", {draft_title(&draft)} } }
                        td { {draft.player.clone()} }
                        td { span { class: "tartan-tag", {draft.status.label()} } }
                        td { {draft.pending.to_string()} }
                        td { {draft.class_name.clone().unwrap_or_else(|| "–".into())} }
                        td { {draft.species_name.clone().unwrap_or_else(|| "–".into())} }
                        td { {draft.background_name.clone().unwrap_or_else(|| "–".into())} }
                        td { {method_label(draft.choices.method)} }
                        td { {scores_line(draft.choices.base.map(|_| draft.totals))} }
                        td {
                            if let Some(rolls) = &draft.rolls {
                                for (index, set) in rolls.sets.iter().enumerate() {
                                    span { class: "builder-dice", key: "{index}",
                                        {dice_line(set)}
                                        " = {set.total}"
                                    }
                                }
                            }
                        }
                        td {
                            ChangeRequest {
                                id: draft.id.clone(),
                                name: draft_title(&draft),
                                submitted: draft.status == DraftStatus::Submitted,
                                ask,
                            }
                            // Approve is the one world write: only an admin or
                            // the session-holding GM while the world is ready.
                            if can_approve && draft.status == DraftStatus::Submitted {
                                button {
                                    id: "approve-{draft.id}",
                                    class: "tartan-button",
                                    r#type: "button",
                                    // The server refuses an unfinished draft too.
                                    disabled: draft.pending > 0,
                                    onclick: {
                                        let id = draft.id.clone();
                                        move |_| approve.call(id.clone())
                                    },
                                    "Approve"
                                }
                            }
                            if draft.status == DraftStatus::Approved
                                && let Some(url) = &play_url {
                                a {
                                    id: "play-{draft.id}",
                                    class: "tartan-button",
                                    href: "{url}",
                                    "Play in Foundry"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A submitted draft's "request changes" note; a draft that is not waiting for
/// review has the form disabled rather than hidden, so the board stays one shape.
#[component]
fn ChangeRequest(
    id: String,
    name: String,
    submitted: bool,
    ask: EventHandler<(String, String)>,
) -> Element {
    let mut note = use_signal(String::new);
    let field = format!("note-{id}");
    rsx! {
        div { class: "builder-note",
            label { r#for: "{field}", class: "tartan-muted", "Request changes to {name}" }
            textarea {
                id: "{field}",
                name: "{field}",
                maxlength: "{NOTE_MAX}",
                disabled: !submitted,
                value: "{note}",
                oninput: move |event: FormEvent| note.set(event.value()),
            }
            button {
                class: "tartan-button",
                r#type: "button",
                disabled: !submitted,
                onclick: {
                    let id = id.clone();
                    move |_| ask.call((id.clone(), note()))
                },
                "Request changes"
            }
        }
    }
}

/// The skill coverage table: one row per skill somebody holds, then the skills
/// nobody in the party has.
#[component]
fn PartySkills(coverage: PartySummary) -> Element {
    rsx! {
        h3 { "Skill coverage" }
        if coverage.skills.is_empty() && coverage.uncovered.is_empty() {
            p { class: "tartan-muted", "No skills chosen yet." }
        }
        table { class: "builder-party",
            thead {
                tr {
                    th { "Skill" }
                    th { "Characters" }
                }
            }
            tbody {
                for (skill, names) in coverage.skills.clone() {
                    tr { key: "{skill}",
                        td { "{skill}" }
                        td { {names.join(", ")} }
                    }
                }
                for skill in coverage.uncovered.clone() {
                    tr { key: "uncovered-{skill}", class: "tartan-muted",
                        td { "{skill}" }
                        td { "nobody" }
                    }
                }
            }
        }
    }
}

/// One draft's builder: name, method, six ability rows and the "to do" list.
#[component]
fn BuilderSheet(
    view: BuilderView,
    patch: EventHandler<DraftPatch>,
    roll: EventHandler<()>,
    submit: EventHandler<()>,
    error: Option<String>,
) -> Element {
    let BuilderView::Ready(data) = view else {
        return rsx! {
            EmptyState {
                heading: "Draft not found".to_string(),
                message: "This draft does not exist, or it belongs to another player.".to_string(),
                actions: rsx! {
                    a { class: "tartan-button", href: "/api/console/characters", "Back to characters" }
                },
            }
        };
    };
    let editable = data.editable;
    let name = data.name.clone();
    let method = data.choices.method;
    let base = data.choices.base;
    let allowed = data.rules.ability_methods.clone();
    let mut options = Vec::new();
    if method.is_none() {
        options.push(FilterOption {
            value: String::new(),
            label: "Choose a method".into(),
            count: None,
        });
    }
    for allowed in &allowed {
        options.push(FilterOption {
            value: allowed.key().into(),
            label: allowed.label().into(),
            count: None,
        });
    }
    let on_method = move |key: String| match AbilityMethod::from_key(&key) {
        Some(AbilityMethod::Roll) => roll.call(()),
        Some(chosen) => patch.call(DraftPatch::Abilities {
            method: chosen,
            base: starter(chosen),
        }),
        None => {}
    };
    let counts: Vec<(Step, usize)> = Step::ALL
        .into_iter()
        .map(|step| {
            let count = data
                .pending
                .iter()
                .filter(|pending| pending.step == step)
                .count();
            (step, count)
        })
        .collect();
    let scores = data.totals;
    rsx! {
        DashboardHeader {
                eyebrow: "Character builder".to_string(),
                heading: if name.is_empty() { "Unnamed character".to_string() } else { name.clone() },
                description: format!("{} · {}", data.status.label(), method_label(method)),
            }
            if let Some(message) = error {
                FeedbackBanner { feedback: Feedback { kind: FeedbackKind::Error, message } }
            }
            if !editable {
                p { class: "tartan-muted",
                    "This draft is read-only: it is {data.status.label().to_lowercase()} or the campaign is closed."
                }
            }
            if let Some(note) = &data.dm_note {
                FeedbackBanner { feedback: Feedback { kind: FeedbackKind::Info, message: format!("DM note: {note}") } }
            }
            nav { class: "builder-steps", aria_label: "Steps",
                for (step, count) in counts {
                    a { key: "{step.anchor()}", class: "builder-step", href: "#{step.anchor()}",
                        {step.label()}
                        if count > 0 {
                            span { class: "builder-step__count", "{count}" }
                        }
                    }
                }
            }
            section { id: "class", class: "builder-step-body", aria_label: "Class",
                h2 { "Class" }
                ClassStep {
                    classes: data.classes.clone(),
                    selected: data.choices.class.clone(),
                    class: data.class.clone(),
                    picks: data.choices.picks.clone(),
                    granted: data.granted.clone(),
                    editable,
                    patch,
                }
            }
            section { id: "origin", class: "builder-step-body", aria_label: "Origin",
                h2 { "Origin" }
                OriginStep {
                    species: data.species.clone(),
                    backgrounds: data.backgrounds.clone(),
                    selected_species: data.choices.species.clone(),
                    selected_background: data.choices.background.clone(),
                    species_details: data.species_details.clone(),
                    background_details: data.background_details.clone(),
                    picks: data.choices.picks.clone(),
                    asi: data.choices.asi.clone(),
                    // The ASI field judges its own answer against every other bonus.
                    base: base.map(|_| data.totals),
                    granted: data.granted.clone(),
                    editable,
                    patch,
                }
            }
            div { id: "abilities",
                SelectField {
                    id: "draft-method".to_string(),
                    label: "Ability method".to_string(),
                    value: method.map(|method| method.key().to_owned()).unwrap_or_default(),
                    options,
                    on_change: on_method,
                }
                if editable && method == Some(AbilityMethod::Roll) && data.rolls.is_none() {
                    // A granted reroll clears the dice; the select is already on
                    // "Roll", so it cannot fire the change again.
                    p { class: "tartan-muted", "Your GM granted a reroll." }
                    button {
                        class: "tartan-button",
                        r#type: "button",
                        onclick: move |_| roll.call(()),
                        "Roll abilities"
                    }
                }
                if let Some(base) = base {
                    if let Some(method) = method {
                        AbilityScores { method, base, totals: scores, rolls: data.rolls.clone(), editable, patch }
                    }
                }
            }
            section { id: "equipment", class: "builder-step-body", aria_label: "Equipment",
                h2 { "Equipment" }
                EquipmentStep {
                    plans: data.equipment.clone(),
                    chosen: data.choices.equipment.clone(),
                    editable,
                    patch,
                }
            }
            div { id: "details",
                Field { id: "draft-name".to_string(), label: "Character name".to_string(),
                    input {
                        id: "draft-name",
                        r#type: "text",
                        value: "{name}",
                        maxlength: "100",
                        disabled: !editable,
                        onchange: move |event: FormEvent| patch.call(DraftPatch::Name(event.value())),
                    }
                }
                DetailsStep { details: data.choices.details.clone(), editable, patch }
            }
            section { id: "review", class: "builder-step-body", aria_label: "Review",
                h2 { "Review" }
                ReviewStep { data: data.as_ref().clone(), submit }
            }
        div { class: "builder-sticky",
            span { "{data.pending.len()} to do" }
            a { class: "tartan-button", href: "/api/console/characters", "Back to characters" }
        }
        if data.srd_attribution {
            p { class: "builder-attribution", "{SRD_ATTRIBUTION}" }
        }
    }
}

/// Six rows: pool selects for array and roll, number inputs for point buy and
/// manual, each showing what the backgrounds add and the total score.
#[component]
fn AbilityScores(
    method: AbilityMethod,
    base: Scores,
    totals: Scores,
    rolls: Option<Rolls>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let pool: Vec<u8> = match method {
        AbilityMethod::StandardArray => vec![15, 14, 13, 12, 10, 8],
        AbilityMethod::Roll => rolls
            .as_ref()
            .map(|rolls| rolls.sets.map(|set| set.total).to_vec())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let points_left = POINT_BUY_BUDGET as i16
        - base
            .values()
            .iter()
            .filter_map(|value| point_buy_cost(*value))
            .map(i16::from)
            .sum::<i16>();
    rsx! {
        section { class: "builder-abilities", aria_label: "Ability scores",
            if method == AbilityMethod::PointBuy {
                p { class: "builder-points", "{points_left} points left" }
            }
            for (field, label) in Scores::LABELS {
                AbilityRow {
                    field: field.to_string(),
                    label: label.to_string(),
                    method,
                    base,
                    total: totals.get(field).unwrap_or(0),
                    pool: pool.clone(),
                    editable,
                    patch,
                }
            }
        }
    }
}

#[component]
fn AbilityRow(
    field: String,
    label: String,
    method: AbilityMethod,
    base: Scores,
    total: u8,
    pool: Vec<u8>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let value = base.get(&field).unwrap_or(0);
    // The bonuses the background grants; the modifier reads the total score.
    let bonus = total.saturating_sub(value);
    let modifier = modifier(total);
    let modifier = if modifier >= 0 {
        format!("+{modifier}")
    } else {
        modifier.to_string()
    };
    let key = field.clone();
    let control = match method {
        AbilityMethod::PointBuy | AbilityMethod::Manual => {
            let (min, max) = if method == AbilityMethod::PointBuy {
                (8, 15)
            } else {
                (3, 18)
            };
            let field = field.clone();
            rsx! {
                input {
                    class: "builder-ability__score",
                    r#type: "number",
                    value: "{value}",
                    min: "{min}",
                    max: "{max}",
                    disabled: !editable,
                    onchange: move |event: FormEvent| {
                        if let Ok(chosen) = event.value().parse::<u8>() {
                            let mut next = base;
                            next.set(&field, chosen.clamp(min, max));
                            patch.call(DraftPatch::Abilities { method, base: next });
                        }
                    },
                }
            }
        }
        _ => rsx! {
            select {
                class: "builder-ability__score",
                disabled: !editable,
                onchange: move |event: FormEvent| {
                    if let Ok(chosen) = event.value().parse::<u8>() {
                        let mut next = base;
                        // Swapping keeps the pool a permutation instead of
                        // asking the player to empty a slot first.
                        if let Some(other) = Scores::LABELS
                            .iter()
                            .map(|(key, _)| *key)
                            .find(|other| *other != key && next.get(other) == Some(chosen))
                        {
                            next.set(other, value);
                        }
                        next.set(&key, chosen);
                        patch.call(DraftPatch::Abilities { method, base: next });
                    }
                },
                for option in pool {
                    option { value: "{option}", selected: option == value, "{option}" }
                }
            }
        },
    };
    rsx! {
        div { class: "builder-ability",
            span { class: "builder-ability__label", "{label}" }
            {control}
            span { class: "builder-ability__math", "{value} + {bonus} = {total}" }
            span { class: "builder-ability__modifier", "({modifier})" }
        }
    }
}

/// The one "to do" list; each open decision links to the step that answers it.
#[component]
fn PendingList(pending: Vec<Pending>) -> Element {
    rsx! {
        ul { class: "builder-pending", aria_label: "To do",
            for item in pending {
                li { key: "{item.key}",
                    a { href: "#{item.step.anchor()}", "{item.label}" }
                }
            }
        }
    }
}

/// The details step's three groups of optional sheet fields.
const DETAIL_GROUPS: [(&str, &[&str]); 3] = [
    (
        "About you",
        &["alignment", "faith", "gender", "age", "height", "weight"],
    ),
    ("Appearance", &["eyes", "hair", "skin", "appearance"]),
    ("Story", &["trait", "ideal", "bond", "flaw", "backstory"]),
];

/// The optional sheet fields: three native disclosure groups of textareas.
#[component]
fn DetailsStep(
    details: BTreeMap<String, String>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    rsx! {
        p { class: "tartan-muted", "All of this is optional; your DM reads it with the rest of the draft." }
        for (group, fields) in DETAIL_GROUPS {
            details { class: "builder-details", open: true, key: "{group}",
                summary { "{group}" }
                for field in fields.iter() {
                    DetailField {
                        key: "{field}",
                        field: (*field).to_string(),
                        value: details.get(*field).cloned().unwrap_or_default(),
                        editable,
                        patch,
                    }
                }
            }
        }
    }
}

/// One sheet field, with the label the actor's own field carries.
#[component]
fn DetailField(
    field: String,
    value: String,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let label = DETAIL_LABELS
        .iter()
        .find(|(key, _)| *key == field.as_str())
        .map_or_else(|| field.clone(), |(_, label)| (*label).to_owned());
    let id = format!("detail-{field}");
    let key = field.clone();
    rsx! {
        Field { id: id.clone(), label,
            textarea {
                id: "{id}",
                name: "{id}",
                maxlength: "{DETAIL_MAX}",
                disabled: !editable,
                value: "{value}",
                onchange: move |event: FormEvent| patch.call(DraftPatch::Detail {
                    key: key.clone(),
                    value: event.value(),
                }),
            }
        }
    }
}

/// The review step: the sheet in brief, the to-do list and the submit button.
#[component]
fn ReviewStep(data: BuilderData, submit: EventHandler<()>) -> Element {
    // The labels the decisions hold for the trait keys the draft already has.
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut features = Vec::new();
    for details in [
        data.class.as_ref().map(|class| &class.decisions),
        data.species_details
            .as_ref()
            .map(|origin| &origin.decisions),
        data.background_details
            .as_ref()
            .map(|origin| &origin.decisions),
    ]
    .into_iter()
    .flatten()
    {
        for decision in details {
            for option in trait_options(decision) {
                labels
                    .entry(option.value.clone())
                    .or_insert_with(|| option.label.clone());
            }
        }
    }
    if let Some(class) = &data.class {
        features.extend(class.features.iter().cloned());
    }
    for origin in [&data.species_details, &data.background_details]
        .into_iter()
        .flatten()
    {
        features.extend(origin.features.iter().cloned());
    }
    let proficiencies: Vec<String> = data
        .granted
        .iter()
        .map(|key| labels.get(key).cloned().unwrap_or_else(|| key.clone()))
        .collect();
    let rows: Vec<(String, &str, u8, String)> = Scores::LABELS
        .iter()
        .map(|(field, label)| {
            let total = data.totals.get(field).unwrap_or(0);
            let modifier = modifier(total);
            let modifier = if modifier >= 0 {
                format!("+{modifier}")
            } else {
                modifier.to_string()
            };
            (field.to_string(), *label, total, modifier)
        })
        .collect();
    let hit_point_total = data.class.as_ref().map(|class| {
        (
            class.hit_die.clone(),
            hit_points(&class.hit_die, data.totals.con),
        )
    });
    rsx! {
        p { class: "tartan-muted",
            "Level 1"
            if let Some(class) = &data.class {
                " · {class.name} ({class.source})"
            }
            if let Some(origin) = &data.species_details {
                " · {origin.name}"
            }
            if let Some(origin) = &data.background_details {
                " · {origin.name}"
            }
        }
        div { class: "builder-abilities",
            for (key, label, total, modifier) in rows {
                div { class: "builder-ability", key: "{key}",
                    span { class: "builder-ability__label", "{label}" }
                    span { class: "builder-ability__math", "{total}" }
                    span { class: "builder-ability__modifier", "({modifier})" }
                }
            }
        }
        if let Some((hit_die, hit_points)) = hit_point_total {
            p { "Hit points {hit_points} (level 1: the {hit_die} maximum plus your Constitution modifier)" }
        }
        h3 { "Proficiencies" }
        if proficiencies.is_empty() {
            p { class: "tartan-muted", "Nothing yet; choose a class and an origin." }
        } else {
            TagList { tags: proficiencies }
        }
        h3 { "Features" }
        for feature in features {
            DetailDisclosure { key: "{feature.name}", label: feature.name.clone(),
                p { {feature.text.clone()} }
            }
        }
        h3 { "Equipment" }
        if data.equipment.is_empty() {
            p { class: "tartan-muted", "Nothing yet." }
        }
        for plan in &data.equipment {
            p { key: "{plan.owner}",
                "{plan.owner_name}: "
                match plan.chosen(&data.choices) {
                    Some(option) => rsx! { "{option.label}" },
                    None => rsx! { "not chosen yet" },
                }
            }
        }
        h3 { "To do" }
        if data.pending.is_empty() {
            p { class: "tartan-muted", "Everything is answered." }
        } else {
            PendingList { pending: data.pending.clone() }
        }
        if data.class.as_ref().is_some_and(|class| class.caster) {
            p { class: "tartan-muted", "Pick your spells in Foundry once your GM approves this character." }
        }
        if data.status == DraftStatus::Draft {
            button {
                id: "draft-submit",
                class: "tartan-button",
                r#type: "button",
                disabled: !data.editable || !data.pending.is_empty(),
                onclick: move |_| submit.call(()),
                "Submit for review"
            }
        } else {
            p { class: "tartan-muted",
                "{data.status.label()} — your DM reviews it, and any note they leave shows at the top of this page."
            }
        }
    }
}

/// The class's and the background's starting equipment: one radio per option,
/// the kit's contents in the option's line, and what the DM hands out.
#[component]
fn EquipmentStep(
    plans: Vec<EquipmentPlan>,
    chosen: BTreeMap<String, String>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    rsx! {
        if plans.is_empty() {
            p { class: "tartan-muted",
                "No starting equipment yet. Choose a class or a background that offers some."
            }
        }
        for plan in plans {
            section { key: "{plan.owner}", class: "builder-equipment",
                h3 { "{plan.owner_name}" }
                fieldset { class: "builder-decision",
                    legend { "Starting equipment" }
                    for option in plan.options {
                        label { key: "{option.id}",
                            input {
                                r#type: "radio",
                                name: "equipment-{plan.owner}",
                                value: "{option.id}",
                                checked: chosen.get(&plan.owner) == Some(&option.id),
                                disabled: !editable,
                                onchange: {
                                    let (owner, id) = (plan.owner.clone(), option.id.clone());
                                    move |_| {
                                        patch.call(DraftPatch::Equipment {
                                            owner: owner.clone(),
                                            option: id.clone(),
                                        })
                                    }
                                },
                            }
                            " {option.label}"
                        }
                        if !option.manual.is_empty() {
                            p { class: "tartan-muted",
                                "Ask your DM for: {option.manual.join(\", \")}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The class step: the sources' classes, then the chosen one's features and
/// the choices they open.
#[component]
fn ClassStep(
    classes: Vec<ClassOption>,
    selected: Option<String>,
    class: Option<ClassDetails>,
    picks: BTreeMap<String, Vec<String>>,
    granted: Vec<String>,
    editable: bool,
    /// Seeds the search box; empty unless a page links here filtered.
    #[props(default)]
    search: String,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let mut query = use_signal(|| search);
    let needle = query().trim().to_lowercase();
    let offered = !classes.is_empty();
    // ponytail: the filter runs in the page; a hundred classes fit in one render.
    let mut groups: Vec<(String, Vec<ClassOption>)> = Vec::new();
    for class in classes
        .into_iter()
        .filter(|class| needle.is_empty() || class.name.to_lowercase().contains(&needle))
    {
        match groups
            .iter_mut()
            .find(|(source, _)| *source == class.source)
        {
            Some((_, group)) => group.push(class),
            None => groups.push((class.source.clone(), vec![class])),
        }
    }
    let chosen = class;
    rsx! {
        if !offered {
            p { class: "tartan-muted",
                "No classes here yet. The GM syncs the world's rules content and enables a pack with classes."
            }
        } else {
            // The search stays whether or not it matches something.
            Field { id: "class-search".to_string(), label: "Search classes".to_string(),
                input {
                    id: "class-search",
                    r#type: "search",
                    value: "{query}",
                    disabled: !editable,
                    oninput: move |event: FormEvent| query.set(event.value()),
                }
            }
            if groups.is_empty() {
                p { class: "tartan-muted", "No classes match." }
            }
            for (source, options) in groups {
                fieldset { class: "builder-classes", key: "{source}",
                    legend { "{source}" }
                    for class in options {
                        label { key: "{class.uuid}",
                            input {
                                r#type: "radio",
                                name: "class",
                                value: "{class.uuid}",
                                checked: selected.as_deref() == Some(class.uuid.as_str()),
                                disabled: !editable,
                                onchange: {
                                    let uuid = class.uuid.clone();
                                    move |_| patch.call(DraftPatch::Select {
                                        slot: Slot::Class,
                                        uuid: Some(uuid.clone()),
                                    })
                                },
                            }
                            " {class.name}"
                        }
                    }
                }
            }
        }
        if let Some(class) = chosen {
            p { class: "tartan-muted",
                "Hit die {class.hit_die} · Primary ability {class.primary_ability} · {class.source}"
            }
            for feature in class.features {
                DetailDisclosure { key: "{feature.name}", label: feature.name.clone(),
                    p { {feature.text.clone()} }
                }
            }
            for decision in class.decisions.into_iter().filter(|decision| !decision.is_automatic()) {
                DecisionField {
                    key: "{decision.key}",
                    picks: picks.get(&decision.key).cloned().unwrap_or_default(),
                    granted: granted.clone(),
                    editable,
                    decision,
                    patch,
                }
            }
        }
    }
}

/// The origin step: the sources' species and backgrounds, then the chosen
/// roots' features and the choices they open.
#[component]
fn OriginStep(
    species: Vec<ClassOption>,
    backgrounds: Vec<ClassOption>,
    selected_species: Option<String>,
    selected_background: Option<String>,
    species_details: Option<OriginDetails>,
    background_details: Option<OriginDetails>,
    picks: BTreeMap<String, Vec<String>>,
    asi: BTreeMap<String, BTreeMap<String, u8>>,
    base: Option<Scores>,
    granted: Vec<String>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    rsx! {
        h3 { "Species" }
        OriginList {
            name: "species".to_string(),
            missing: "No species here yet. The GM syncs the world's rules content and enables a pack with species.".to_string(),
            options: species,
            selected: selected_species,
            slot: Slot::Species,
            editable,
            patch,
        }
        if let Some(details) = species_details {
            OriginRoot { details, picks: picks.clone(), asi: asi.clone(), base, granted: granted.clone(), editable, patch }
        }
        h3 { "Background" }
        OriginList {
            name: "background".to_string(),
            missing: "No backgrounds here yet. The GM syncs the world's rules content and enables a pack with backgrounds.".to_string(),
            options: backgrounds,
            selected: selected_background,
            slot: Slot::Background,
            editable,
            patch,
        }
        if let Some(details) = background_details {
            OriginRoot { details, picks, asi, base, granted, editable, patch }
        }
    }
}

/// One origin slot's radio list, grouped under its source.
#[component]
fn OriginList(
    name: String,
    missing: String,
    options: Vec<ClassOption>,
    selected: Option<String>,
    slot: Slot,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let mut groups: Vec<(String, Vec<ClassOption>)> = Vec::new();
    for option in options {
        match groups
            .iter_mut()
            .find(|(source, _)| *source == option.source)
        {
            Some((_, group)) => group.push(option),
            None => groups.push((option.source.clone(), vec![option])),
        }
    }
    rsx! {
        if groups.is_empty() {
            p { class: "tartan-muted", "{missing}" }
        }
        for (source, options) in groups {
            fieldset { class: "builder-classes", key: "{name}-{source}",
                legend { "{source}" }
                for option in options {
                    label { key: "{option.uuid}",
                        input {
                            r#type: "radio",
                            name: "{name}",
                            value: "{option.uuid}",
                            checked: selected.as_deref() == Some(option.uuid.as_str()),
                            disabled: !editable,
                            onchange: {
                                let uuid = option.uuid.clone();
                                move |_| patch.call(DraftPatch::Select {
                                    slot,
                                    uuid: Some(uuid.clone()),
                                })
                            },
                        }
                        " {option.name}"
                    }
                }
            }
        }
    }
}

/// One chosen origin root: its features and the decisions they open.
#[component]
fn OriginRoot(
    details: OriginDetails,
    picks: BTreeMap<String, Vec<String>>,
    asi: BTreeMap<String, BTreeMap<String, u8>>,
    base: Option<Scores>,
    granted: Vec<String>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let OriginDetails {
        name,
        source,
        features,
        decisions,
        ..
    } = details;
    rsx! {
        p { class: "tartan-muted", "{name} · {source}" }
        for feature in features {
            DetailDisclosure { key: "{feature.name}", label: feature.name.clone(),
                p { {feature.text.clone()} }
            }
        }
        for decision in decisions.into_iter().filter(|decision| !decision.is_automatic()) {
            DecisionField {
                key: "{decision.key}",
                picks: picks.get(&decision.key).cloned().unwrap_or_default(),
                deltas: asi.get(&decision.key).cloned().unwrap_or_default(),
                base,
                granted: granted.clone(),
                editable,
                decision,
                patch,
            }
        }
    }
}

/// One decision as a native checkbox or radio group that stops at its count.
#[component]
fn DecisionField(
    decision: Decision,
    picks: Vec<String>,
    #[props(default)] deltas: BTreeMap<String, u8>,
    #[props(default)] base: Option<Scores>,
    granted: Vec<String>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    // An ability bonus is answered into its own map, with its own rule.
    if let DecisionKind::Asi { .. } = &decision.kind {
        return rsx! {
            AsiField { decision, deltas, base, editable, patch }
        };
    }
    let pools: Vec<(u8, Vec<Opt>)> = match &decision.kind {
        DecisionKind::Trait { choices, .. } => choices
            .iter()
            .map(|pool| (pool.count, pool.options.clone()))
            .collect(),
        DecisionKind::Items { count, options } => vec![(*count, options.clone())],
        DecisionKind::Size { options } => vec![(
            1,
            options
                .iter()
                .map(|size| Opt {
                    value: size.clone(),
                    label: size_label(size).to_owned(),
                })
                .collect(),
        )],
        _ => Vec::new(),
    };
    // A pick the content no longer offers is not sent back.
    let offered: Vec<String> = match &decision.kind {
        DecisionKind::Trait { choices, .. } => choices
            .iter()
            .flat_map(|pool| pool.options.iter().map(|option| option.value.clone()))
            .collect(),
        DecisionKind::Items { options, .. } => {
            options.iter().map(|option| option.value.clone()).collect()
        }
        DecisionKind::Size { options } => options.clone(),
        DecisionKind::Asi { .. } | DecisionKind::Unsupported { .. } => Vec::new(),
    };
    let key = decision.key.clone();
    // dnd5e's own one-liner for what a non-default trait mode can still take.
    let mode_hint = match &decision.kind {
        DecisionKind::Trait { mode, .. } => mode.hint(),
        _ => None,
    };
    let unsupported = match &decision.kind {
        DecisionKind::Unsupported { .. } => Some("The builder cannot resolve this choice yet."),
        DecisionKind::Trait { .. } | DecisionKind::Items { .. } if pools.is_empty() => {
            Some("The rules content did not resolve this choice.")
        }
        _ => None,
    };
    // A non-default Trait mode's offers are the ones it can still take, so none
    // of them is "already had".
    let granted = match &decision.kind {
        DecisionKind::Trait { mode, .. } if *mode != TraitMode::Default => Vec::new(),
        _ => granted,
    };
    rsx! {
        fieldset { class: "builder-decision",
            legend { "{decision.title}" }
            if !decision.hint.is_empty() {
                p { class: "tartan-muted", "{decision.hint}" }
            }
            if let Some(hint) = mode_hint {
                p { class: "tartan-muted", "{hint}" }
            }
            if let Some(message) = unsupported {
                p { class: "tartan-muted", "{message}" }
            }
            for (index, (count, options)) in pools.into_iter().enumerate() {
                DecisionPool {
                    key: "{key}#{index}",
                    name: "{key}#{index}",
                    decision: key.clone(),
                    count,
                    options,
                    picks: picks.clone(),
                    offered: offered.clone(),
                    granted: granted.clone(),
                    editable,
                    patch,
                }
            }
        }
    }
}

/// One pool of a decision: one radio per option at count one, checkboxes above.
#[component]
fn DecisionPool(
    decision: String,
    name: String,
    count: u8,
    options: Vec<Opt>,
    picks: Vec<String>,
    offered: Vec<String>,
    granted: Vec<String>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let single = count == 1;
    // Everything this pool already holds, so a checkbox group stops at count.
    let taken = options
        .iter()
        .filter(|option| picks.contains(&option.value))
        .count();
    let full = !single && taken >= usize::from(count);
    let all = options.clone();
    rsx! {
        if count > 1 {
            p { class: "tartan-muted", "Choose {count}" }
        }
        for option in options {
            label { key: "{option.value}",
                input {
                    r#type: if single { "radio" } else { "checkbox" },
                    name: "{name}",
                    value: "{option.value}",
                    checked: picks.contains(&option.value),
                    disabled: !editable
                        || (full && !picks.contains(&option.value))
                        || (granted.contains(&option.value) && !picks.contains(&option.value)),
                    onchange: {
                        let value = option.value.clone();
                        let all = all.clone();
                        let offered = offered.clone();
                        let decision = decision.clone();
                        let picks = picks.clone();
                        move |event: FormEvent| {
                            patch.call(DraftPatch::Pick {
                                key: decision.clone(),
                                values: toggle(&picks, &offered, &all, count, &value, event.checked()),
                            })
                        }
                    },
                }
                " {option.label}"
                if granted.contains(&option.value) && !picks.contains(&option.value) {
                    span { class: "tartan-muted", " already have" }
                }
            }
        }
    }
}

/// The pick list one change produces: a radio replaces its pool's value, a
/// checkbox adds or drops its own, and a value the content no longer offers is
/// dropped rather than sent back.
fn toggle(
    picks: &[String],
    offered: &[String],
    options: &[Opt],
    count: u8,
    value: &str,
    checked: bool,
) -> Vec<String> {
    let mut next: Vec<String> = picks
        .iter()
        .filter(|current| offered.iter().any(|option| option == *current))
        .cloned()
        .collect();
    if count == 1 {
        next.retain(|current| !options.iter().any(|option| &option.value == current));
    }
    if checked {
        if !next.iter().any(|current| current == value) {
            next.push(value.to_owned());
        }
    } else {
        next.retain(|current| current != value);
    }
    next
}

/// A background's ability bonuses: the 2024 presets over the open abilities,
/// then one select per open ability that keeps the total on the points granted.
#[component]
fn AsiField(
    decision: Decision,
    deltas: BTreeMap<String, u8>,
    base: Option<Scores>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let DecisionKind::Asi {
        points, cap, open, ..
    } = &decision.kind
    else {
        return rsx! {
            p { class: "tartan-muted", "The rules content did not resolve this choice." }
        };
    };
    let (points, cap, open) = (*points, *cap, open.clone());
    // `base` arrives as the draft's totals; take this decision's own bonus back
    // out, so it is judged against the base plus every other bonus.
    let mut base = base.unwrap_or_default();
    if let DecisionKind::Asi { fixed, .. } = &decision.kind {
        for (key, delta) in fixed.iter().chain(&deltas) {
            base.set(key, base.get(key).unwrap_or(0).saturating_sub(*delta));
        }
    }
    let key = decision.key.clone();
    let two_one = preset(&open, points, cap, false);
    let ones = preset(&open, points, cap, true);
    let abilities: Vec<(String, String)> = open
        .iter()
        .map(|field| {
            let label = ability_label(field).map_or(field.clone(), str::to_owned);
            (field.clone(), label)
        })
        .collect();
    // Only a complete, legal assignment is ever sent: the same rule the server
    // re-runs in `update_draft`.
    let send = move |next: BTreeMap<String, u8>,
                     decision: Decision,
                     patch: EventHandler<DraftPatch>,
                     key: String| {
        if validate_asi(&decision, &next, &base).is_ok() {
            patch.call(DraftPatch::Asi { key, deltas: next });
        }
    };
    rsx! {
        fieldset { class: "builder-decision",
            legend { "{decision.title}" }
            if !decision.hint.is_empty() {
                p { class: "tartan-muted", "{decision.hint}" }
            }
            p { class: "tartan-muted", "{points} points to assign, at most {cap} on one ability." }
            for (value, label, chosen) in [
                ("twoOne", "+2 / +1", two_one),
                ("ones", "+1 / +1 / +1", ones),
            ] {
                label { key: "{value}",
                    input {
                        r#type: "radio",
                        name: "{key}",
                        value: "{value}",
                        checked: deltas == chosen,
                        disabled: !editable,
                        onchange: {
                            let next = chosen.clone();
                            let decision = decision.clone();
                            let key = key.clone();
                            move |_| send(next.clone(), decision.clone(), patch, key.clone())
                        },
                    }
                    " {label}"
                }
            }
            div { class: "builder-asi",
                for (field, label) in abilities {
                    label { key: "{field}",
                        {label}
                        select {
                            disabled: !editable,
                            onchange: {
                                let field = field.clone();
                                let deltas = deltas.clone();
                                let open = open.clone();
                                let decision = decision.clone();
                                let key = key.clone();
                                move |event: FormEvent| {
                                    let Ok(to) = event.value().parse::<u8>() else {
                                        return;
                                    };
                                    let next = rebalance(&deltas, &open, &field, to, points, cap);
                                    send(next, decision.clone(), patch, key.clone());
                                }
                            },
                            for value in 0..=cap.min(points) {
                                option {
                                    value: "{value}",
                                    selected: deltas.get(&field).copied().unwrap_or(0) == value,
                                    "{value}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The whole answer one ability change produces: the changed value stays and
/// the others give way, or top up, so the total is exactly `points`.
fn rebalance(
    deltas: &BTreeMap<String, u8>,
    open: &[String],
    changed: &str,
    to: u8,
    points: u8,
    cap: u8,
) -> BTreeMap<String, u8> {
    let mut next: BTreeMap<String, u8> = open
        .iter()
        .map(|key| {
            let delta = deltas.get(key).copied().unwrap_or(0).min(cap);
            (key.clone(), delta)
        })
        .collect();
    next.insert(changed.to_owned(), to.min(cap).min(points));
    let sum = |map: &BTreeMap<String, u8>| map.values().map(|delta| u16::from(*delta)).sum::<u16>();
    let budget = u16::from(points);
    for key in open.iter().rev().filter(|key| key.as_str() != changed) {
        if sum(&next) <= budget {
            break;
        }
        let over = u8::try_from(sum(&next) - budget).unwrap_or(u8::MAX);
        if let Some(delta) = next.get_mut(key) {
            *delta = delta.saturating_sub(over);
        }
    }
    for key in open.iter().filter(|key| key.as_str() != changed) {
        if sum(&next) >= budget {
            break;
        }
        let short = u8::try_from(budget - sum(&next)).unwrap_or(u8::MAX);
        let room = short.min(cap.saturating_sub(next.get(key).copied().unwrap_or(0)));
        if let Some(delta) = next.get_mut(key) {
            *delta += room;
        }
    }
    next.retain(|_, delta| *delta > 0);
    next
}

/// The two 2024 presets: the cap on the first open abilities then what is left,
/// or one point on each open ability in turn.
fn preset(open: &[String], points: u8, cap: u8, ones: bool) -> BTreeMap<String, u8> {
    let mut deltas = BTreeMap::new();
    let mut left = points;
    for key in open {
        if left == 0 {
            break;
        }
        let give = if ones { 1 } else { left.min(cap) }.min(left);
        deltas.insert(key.clone(), give);
        left -= give;
    }
    deltas
}

/// A fresh valid assignment when the player switches method.
fn starter(method: AbilityMethod) -> Scores {
    let value = match method {
        AbilityMethod::StandardArray => {
            return Scores {
                str: 15,
                dex: 14,
                con: 13,
                int: 12,
                wis: 10,
                cha: 8,
            };
        }
        AbilityMethod::PointBuy => 8,
        _ => 10,
    };
    Scores {
        str: value,
        dex: value,
        con: value,
        int: value,
        wis: value,
        cha: value,
    }
}

fn draft_link(draft: &DraftSummary) -> ResourceLink {
    ResourceLink {
        key: draft.id.clone(),
        title: draft_title(draft),
        subtitle: Some(draft_line(draft)),
        href: format!("/api/console/characters/{}", draft.id),
        kind: None,
    }
}

fn draft_title(draft: &DraftSummary) -> String {
    if draft.name.trim().is_empty() {
        format!("Unnamed ({} draft)", draft.player)
    } else {
        draft.name.clone()
    }
}

fn draft_line(draft: &DraftSummary) -> String {
    let mut line = method_label(draft.choices.method).to_owned();
    if let Some(base) = draft.choices.base {
        line.push_str(" · ");
        line.push_str(&scores_line(Some(base)));
    }
    if draft.pending > 0 {
        line.push_str(&format!(" · {} to do", draft.pending));
    }
    line
}

fn method_label(method: Option<AbilityMethod>) -> &'static str {
    method.map_or("No method yet", AbilityMethod::label)
}

fn method_summary(rules: &CampaignRules) -> String {
    let methods = rules
        .ability_methods
        .iter()
        .map(|method| method.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!("Methods: {methods}")
}

fn scores_line(scores: Option<Scores>) -> String {
    match scores {
        Some(scores) => scores
            .values()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(" "),
        None => "–".into(),
    }
}

fn dice_line(set: &AbilityRoll) -> String {
    set.dice
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use crate::builder::{BuilderData, CharactersData, DraftStatus, Pool};

    fn campaign(open: bool) -> CampaignView {
        CampaignView {
            world_id: "canary".into(),
            open,
            rules: CampaignRules::default(),
            packs: Vec::new(),
            synced_at: None,
        }
    }

    fn pack() -> PackInfo {
        PackInfo {
            id: "dnd5e.classes24".into(),
            label: "Character Classes".into(),
            source_book: Some("SRD 5.2".into()),
            rules_version: Some("modern".into()),
            count: 12,
        }
    }

    fn summary(name: &str, method: AbilityMethod, rolls: Option<Rolls>) -> DraftSummary {
        let base = match method {
            AbilityMethod::Roll => Some(Scores {
                str: 13,
                dex: 13,
                con: 13,
                int: 13,
                wis: 13,
                cha: 13,
            }),
            _ => Some(Scores {
                str: 15,
                dex: 14,
                con: 13,
                int: 12,
                wis: 10,
                cha: 8,
            }),
        };
        DraftSummary {
            id: "0b6f4c2e-0000-4000-8000-000000000000".into(),
            name: name.into(),
            status: DraftStatus::Draft,
            player: "Piotr".into(),
            choices: crate::builder::DraftChoices {
                method: Some(method),
                base,
                ..crate::builder::DraftChoices::default()
            },
            rolls,
            pending: 0,
            class_name: None,
            species_name: None,
            background_name: None,
            totals: Scores::default(),
        }
    }

    fn rolls() -> Rolls {
        Rolls {
            rolled_at: "2026-11-15T10:00:00+00:00".into(),
            sets: [AbilityRoll {
                dice: [4, 3, 1, 6],
                total: 13,
            }; 6],
        }
    }

    fn data(role: Role) -> CharactersData {
        CharactersData {
            role,
            campaigns: vec![campaign(true)],
            mine: vec![summary("Al", AbilityMethod::StandardArray, None)],
            party: Vec::new(),
            coverage: PartySummary::default(),
            ready_world: Some("canary".into()),
            can_approve: false,
            play_url: None,
        }
    }

    fn builder_data(rules: CampaignRules) -> BuilderData {
        BuilderData {
            id: "0b6f4c2e-0000-4000-8000-000000000000".into(),
            name: "Al".into(),
            status: DraftStatus::Draft,
            editable: true,
            rules,
            choices: crate::builder::DraftChoices {
                method: Some(AbilityMethod::StandardArray),
                base: Some(Scores {
                    str: 15,
                    dex: 14,
                    con: 13,
                    int: 12,
                    wis: 10,
                    cha: 8,
                }),
                ..crate::builder::DraftChoices::default()
            },
            rolls: None,
            pending: Vec::new(),
            dm_note: None,
            srd_attribution: false,
            classes: Vec::new(),
            class: None,
            species: Vec::new(),
            backgrounds: Vec::new(),
            species_details: None,
            background_details: None,
            equipment: Vec::new(),
            granted: Vec::new(),
            totals: Scores::default(),
        }
    }

    fn cleric(uuid: &str) -> ClassOption {
        ClassOption {
            uuid: uuid.into(),
            name: "Cleric".into(),
            source: "Character Classes".into(),
        }
    }

    fn cleric_details(uuid: &str) -> ClassDetails {
        ClassDetails {
            uuid: uuid.into(),
            name: "Cleric".into(),
            source: "Character Classes".into(),
            hit_die: "d8".into(),
            primary_ability: "Wisdom".into(),
            caster: true,
            features: vec![crate::builder::Feature {
                name: "Divine Order".into(),
                text: "You have dedicated yourself to one sacred role.".into(),
            }],
            decisions: vec![Decision {
                key: format!("{uuid}#2wUNkQzS4vL9nR6D#1"),
                owner_name: "Cleric".into(),
                title: "Skill Proficiencies".into(),
                hint: String::new(),
                step: Step::Class,
                kind: DecisionKind::Trait {
                    mode: crate::builder::TraitMode::Default,
                    grants: Vec::new(),
                    choices: vec![crate::builder::Pool {
                        count: 2,
                        options: vec![
                            Opt {
                                value: "skills:his".into(),
                                label: "History".into(),
                            },
                            Opt {
                                value: "skills:ins".into(),
                                label: "Insight".into(),
                            },
                        ],
                    }],
                },
            }],
        }
    }

    fn render_board(view: CharactersView) -> String {
        #[component]
        fn Harness(view: CharactersView) -> Element {
            let create = EventHandler::new(|_: String| ());
            let save = EventHandler::new(|_: (String, bool, CampaignRules)| ());
            let sync = EventHandler::new(|_: String| ());
            let ask = EventHandler::new(|_: (String, String)| ());
            let approve = EventHandler::new(|_: String| ());
            rsx! {
                CharactersBoard { view, create, save, sync, ask, approve, error: None }
            }
        }
        dioxus::ssr::render_element(rsx! {
            Harness { view }
        })
    }

    fn render_sheet(view: BuilderView) -> String {
        #[component]
        fn Harness(view: BuilderView) -> Element {
            let patch = EventHandler::new(|_: DraftPatch| ());
            let roll = EventHandler::new(|()| ());
            let submit = EventHandler::new(|()| ());
            rsx! {
                BuilderSheet { view, patch, roll, submit, error: None }
            }
        }
        dioxus::ssr::render_element(rsx! {
            Harness { view }
        })
    }

    fn render_class_step(classes: Vec<ClassOption>, search: &str) -> String {
        #[component]
        fn Harness(classes: Vec<ClassOption>, search: String) -> Element {
            let patch = EventHandler::new(|_: DraftPatch| ());
            rsx! {
                ClassStep {
                    classes,
                    selected: None,
                    class: None,
                    picks: BTreeMap::new(),
                    granted: Vec::new(),
                    editable: true,
                    search,
                    patch,
                }
            }
        }
        dioxus::ssr::render_element(rsx! {
            Harness { classes, search: search.to_string() }
        })
    }

    /// The class step for one chosen class and the trait keys the draft holds.
    fn render_class(class: ClassDetails, granted: Vec<String>) -> String {
        #[component]
        fn Harness(class: ClassDetails, granted: Vec<String>) -> Element {
            let patch = EventHandler::new(|_: DraftPatch| ());
            let option = ClassOption {
                uuid: class.uuid.clone(),
                name: class.name.clone(),
                source: class.source.clone(),
            };
            rsx! {
                ClassStep {
                    classes: vec![option],
                    selected: Some(class.uuid.clone()),
                    class: Some(class),
                    picks: BTreeMap::new(),
                    granted,
                    editable: true,
                    patch,
                }
            }
        }
        dioxus::ssr::render_element(rsx! {
            Harness { class, granted }
        })
    }

    #[test]
    fn player_sees_only_own_drafts_and_no_rules_form() {
        let html = render_board(CharactersView::Ready(Box::new(data(Role::Player))));
        assert!(html.contains("My characters"));
        assert!(html.contains("Al"));
        assert!(html.contains("New character"));
        assert!(!html.contains("Session zero"));
        assert!(!html.contains("Save rules"));
        assert!(!html.contains("Party"));
    }

    #[test]
    fn gm_sees_rules_and_party_rolls() {
        let mut data = data(Role::Gm);
        data.party = vec![summary("Bryn", AbilityMethod::Roll, Some(rolls()))];
        let html = render_board(CharactersView::Ready(Box::new(data)));
        assert!(html.contains("Session zero"));
        assert!(html.contains("Save rules"));
        assert!(html.contains("canary"));
        assert!(html.contains("Bryn"));
        assert!(html.contains("Roll (server)"));
        assert!(html.contains("4 3 1 6"));
    }

    #[test]
    fn builder_offers_only_allowed_methods() {
        let rules = CampaignRules {
            ability_methods: vec![AbilityMethod::StandardArray, AbilityMethod::PointBuy],
            ..CampaignRules::default()
        };
        let html = render_sheet(BuilderView::Ready(Box::new(builder_data(rules))));
        assert!(html.contains("value=\"standardArray\""));
        assert!(html.contains("value=\"pointBuy\""));
        assert!(!html.contains("value=\"roll\""));
        assert!(!html.contains("value=\"manual\""));
    }

    #[test]
    fn gm_sees_sync_and_source_toggles() {
        let mut data = data(Role::Gm);
        data.campaigns[0].packs = vec![pack()];
        data.campaigns[0].synced_at = Some("2026-11-15 18:20 UTC".into());
        let html = render_board(CharactersView::Ready(Box::new(data)));
        assert!(html.contains("Sync rules content"));
        assert!(html.contains("Last sync: 2026-11-15 18:20 UTC"));
        assert!(html.contains("name=\"source\""));
        assert!(html.contains("value=\"dnd5e.classes24\""));
        assert!(html.contains("Character Classes · SRD 5.2 (12)"));
    }

    #[test]
    fn srd_attribution_is_shown_for_srd_sources() {
        let mut data = builder_data(CampaignRules::default());
        data.srd_attribution = true;
        let html = render_sheet(BuilderView::Ready(Box::new(data)));
        assert!(html.contains("System Reference Document 5.2.1"));
        assert!(html.contains("Creative Commons Attribution 4.0"));

        let html = render_sheet(BuilderView::Ready(Box::new(builder_data(
            CampaignRules::default(),
        ))));
        assert!(!html.contains("System Reference Document"));
    }

    #[test]
    fn locked_draft_renders_read_only() {
        let mut data = builder_data(CampaignRules::default());
        data.status = DraftStatus::Submitted;
        data.editable = false;
        let html = render_sheet(BuilderView::Ready(Box::new(data)));
        assert!(html.contains("Submitted"));
        assert!(html.contains("read-only"));
        assert!(html.contains("disabled"));
    }

    #[test]
    fn class_step_lists_enabled_sources_only() {
        let uuid = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
        let mut data = builder_data(CampaignRules::default());
        // The server hands over only the classes of the campaign's sources.
        data.classes = vec![cleric(uuid)];
        data.choices.class = Some(uuid.into());
        data.class = Some(cleric_details(uuid));
        let html = render_sheet(BuilderView::Ready(Box::new(data)));

        // One radio per offered class, grouped under its source.
        assert_eq!(html.matches("name=\"class\"").count(), 1);
        assert!(html.contains("Character Classes"));
        assert!(html.contains(&format!("value=\"{uuid}\"")));
        assert!(html.contains("checked"));

        // The chosen class shows its numbers and a disclosure per feature.
        assert!(html.contains("d8"));
        assert!(html.contains("Wisdom"));
        assert!(html.contains("<summary>Divine Order</summary>"));

        // Its two-skill pool is a checkbox group.
        assert!(html.contains("type=\"checkbox\""));
        assert!(html.contains("value=\"skills:ins\""));
        assert!(html.contains("Choose 2"));
    }

    /// A class whose only decision is a two-skill Trait in `mode`.
    fn expertise_class(mode: TraitMode) -> ClassDetails {
        let uuid = "Compendium.dnd5e.classes24.Item.phbrgeRogue00000";
        ClassDetails {
            uuid: uuid.into(),
            name: "Rogue".into(),
            source: "Character Classes".into(),
            hit_die: "d8".into(),
            primary_ability: "Dexterity".into(),
            caster: false,
            features: Vec::new(),
            decisions: vec![Decision {
                key: format!("{uuid}#4doj2kFD8sIYiX8X#1"),
                owner_name: "Expertise".into(),
                title: "Expertise".into(),
                hint: String::new(),
                step: Step::Class,
                kind: DecisionKind::Trait {
                    mode,
                    grants: Vec::new(),
                    choices: vec![Pool {
                        count: 2,
                        options: [
                            ("skills:ins", "Insight"),
                            ("skills:prc", "Perception"),
                            ("skills:slt", "Sleight of Hand"),
                            ("skills:ste", "Stealth"),
                        ]
                        .map(|(value, label)| Opt {
                            value: value.into(),
                            label: label.into(),
                        })
                        .to_vec(),
                    }],
                },
            }],
        }
    }

    #[test]
    fn expertise_options_are_never_already_had() {
        // The walker offers an Expertise decision only the skills the draft is
        // proficient with, so every offer must stay pickable.
        let granted = ["skills:ins", "skills:prc", "skills:ste"]
            .map(str::to_owned)
            .to_vec();
        let html = render_class(expertise_class(TraitMode::Expertise), granted);
        assert!(html.contains("Gain expertise in a trait"));
        for value in ["skills:ins", "skills:prc", "skills:slt", "skills:ste"] {
            assert!(
                html.contains(&format!("value=\"{value}\"")),
                "{value} missing"
            );
        }
        assert!(!html.contains("disabled"), "{html}");
        // The mode's own hint says "already have", so match the marker itself.
        assert!(!html.contains("> already have</span>"), "{html}");

        // A default-mode pool does offer traits the draft already has, and says
        // so instead of letting the player take the same one twice.
        let html = render_class(
            expertise_class(TraitMode::Default),
            ["skills:ins".to_owned()].to_vec(),
        );
        assert!(html.contains("> already have</span>"), "{html}");
        assert!(html.contains("disabled"), "{html}");
    }

    #[test]
    fn class_search_survives_an_empty_filter() {
        // A search that matches nothing keeps its box and says so; the empty
        // catalog keeps its own message instead.
        let uuid = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
        let html = render_class_step(vec![cleric(uuid)], "wizard");
        assert!(html.contains("No classes match"));
        assert!(html.contains("id=\"class-search\""));
        assert!(html.contains("value=\"wizard\""));
        assert!(!html.contains("No classes here yet"));
        assert!(!html.contains(&format!("value=\"{uuid}\"")));

        let html = render_class_step(Vec::new(), "wizard");
        assert!(html.contains("No classes here yet"));
        assert!(!html.contains("No classes match"));
        assert!(!html.contains("id=\"class-search\""));
    }

    #[test]
    fn equipment_step_shows_both_options() {
        let uuid = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
        let mut data = builder_data(CampaignRules::default());
        data.choices.class = Some(uuid.into());
        data.class = Some(cleric_details(uuid));
        // The Cleric's one kit and the gold that replaces it.
        data.equipment = vec![EquipmentPlan {
            owner: uuid.into(),
            owner_name: "Cleric".into(),
            options: vec![
                crate::builder::EquipmentOption {
                    id: "kit".into(),
                    label: "Kit: Chain Shirt, Shield, Mace, any holy focus, 7 GP".into(),
                    items: Vec::new(),
                    gp: 7,
                    wealth: None,
                    manual: vec!["any holy focus".into()],
                },
                crate::builder::EquipmentOption {
                    id: "gold".into(),
                    label: "110 GP".into(),
                    items: Vec::new(),
                    gp: 0,
                    wealth: Some("110".into()),
                    manual: Vec::new(),
                },
            ],
        }];
        let html = render_sheet(BuilderView::Ready(Box::new(data)));

        // One radio per option, under the owner's own name, and the manual line
        // says what the DM hands out.
        assert!(html.contains("Kit: Chain Shirt, Shield, Mace, any holy focus, 7 GP"));
        assert!(html.contains("110 GP"));
        assert!(html.contains(&format!("name=\"equipment-{uuid}\"")));
        assert_eq!(html.matches("type=\"radio\"").count(), 2);
        assert!(html.contains("Ask your DM for: any holy focus"));
        assert!(!html.contains("checked"), "{html}");
    }

    #[test]
    fn todo_list_links_each_open_decision() {
        let uuid = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
        let mut data = builder_data(CampaignRules::default());
        data.choices.class = Some(uuid.into());
        data.class = Some(cleric_details(uuid));
        data.pending = vec![
            Pending {
                step: Step::Class,
                key: format!("{uuid}#2wUNkQzS4vL9nR6D#1"),
                label: "Skill Proficiencies".into(),
            },
            Pending {
                step: Step::Class,
                key: "Compendium.dnd5e.classes24.Item.phbclcDivineOrde#V8GfnoZYTe1PLXPI#1".into(),
                label: "Divine Order Choice".into(),
            },
        ];
        let html = render_sheet(BuilderView::Ready(Box::new(data)));

        // The nav links the class step and counts what is open there; the
        // "to do" list links each decision to the same anchor.
        assert_eq!(html.matches("href=\"#class\"").count(), 3);
        assert!(html.contains("<span class=\"builder-step__count\">2</span>"));
        assert!(html.contains("Skill Proficiencies"));
        assert!(html.contains("Divine Order Choice"));
        assert!(html.contains("2 to do"));
    }

    #[test]
    fn ability_rows_show_base_bonus_total() {
        let mut data = builder_data(CampaignRules::default());
        // The Acolyte's +2 Intelligence and +1 Wisdom, on the standard array.
        data.choices.asi.insert(
            "Compendium.dnd5e.origins24.Item.phbbgAcolyte0000#kDI6zoUamHzFTvHG#0".into(),
            BTreeMap::from([("int".to_owned(), 2), ("wis".to_owned(), 1)]),
        );
        // The server computes totals (fixed and assigned bonuses) for the page.
        let base = data.choices.base.expect("standard array");
        data.totals = Scores {
            int: base.int + 2,
            wis: base.wis + 1,
            ..base
        };
        let html = render_sheet(BuilderView::Ready(Box::new(data)));

        // Every row reads base + bonus = total, and the modifier follows the
        // total rather than the base.
        assert!(html.contains("15 + 0 = 15"), "{html}");
        assert!(html.contains("12 + 2 = 14"), "{html}");
        assert!(html.contains("10 + 1 = 11"), "{html}");
        assert!(html.contains("(+2)"), "{html}");
    }

    /// Whether the review step's submit button is disabled: the rendered
    /// opening tag alone, so a disabled field elsewhere cannot answer for it.
    fn submit_disabled(html: &str) -> bool {
        let tag = html
            .split("<button")
            .find(|chunk| chunk.contains("draft-submit"))
            .expect("a submit button");
        tag[..tag.find('>').expect("the tag ends")].contains("disabled")
    }

    #[test]
    fn review_shows_summary_and_blocks_submit_while_pending() {
        let uuid = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
        let mut data = builder_data(CampaignRules::default());
        data.choices.class = Some(uuid.into());
        data.class = Some(cleric_details(uuid));
        // The cleric's pool labelled Insight, so the review names the skill the
        // draft holds rather than its key.
        data.granted = vec!["skills:ins".into()];
        data.totals = Scores {
            str: 15,
            dex: 14,
            con: 13,
            int: 12,
            wis: 10,
            cha: 8,
        };
        data.equipment = vec![EquipmentPlan {
            owner: uuid.into(),
            owner_name: "Cleric".into(),
            options: vec![crate::builder::EquipmentOption {
                id: "kit".into(),
                label: "Kit: Chain Shirt, Shield, Mace, 7 GP".into(),
                items: Vec::new(),
                gp: 7,
                wealth: None,
                manual: Vec::new(),
            }],
        }];
        data.choices.equipment.insert(uuid.into(), "kit".into());
        data.choices
            .details
            .insert("backstory".into(), "Raised in the temple.".into());
        data.pending = vec![Pending {
            step: Step::Class,
            key: "class-skills".into(),
            label: "Skill Proficiencies".into(),
        }];
        let html = render_sheet(BuilderView::Ready(Box::new(data.clone())));

        // The sheet in brief: totals, level-one hit points, proficiencies,
        // features, the equipment answer and the open work.
        assert!(
            html.contains("Hit points 9 (level 1: the d8 maximum"),
            "{html}"
        );
        assert!(html.contains("(+2)"), "{html}");
        assert!(html.contains("Insight"), "{html}");
        assert!(html.contains("<summary>Divine Order</summary>"), "{html}");
        assert!(
            html.contains("Cleric: Kit: Chain Shirt, Shield, Mace, 7 GP"),
            "{html}"
        );
        assert!(html.contains("Skill Proficiencies"), "{html}");
        // A caster picks spells in Foundry until the builder has a spell step.
        assert!(html.contains("Pick your spells in Foundry"), "{html}");
        // The optional details are three disclosures of textareas.
        assert!(html.contains("id=\"detail-backstory\""), "{html}");
        assert!(html.contains("Raised in the temple."), "{html}");
        assert!(html.contains("About you"), "{html}");
        // Nothing is submitted while a choice is open.
        assert!(html.contains("Submit for review"), "{html}");
        assert!(submit_disabled(&html), "{html}");

        // Answering the last choice enables it.
        data.pending.clear();
        let html = render_sheet(BuilderView::Ready(Box::new(data)));
        assert!(!submit_disabled(&html), "{html}");
    }

    #[test]
    fn dm_board_shows_status_pending_and_note_form() {
        let mut data = data(Role::Gm);
        let mut draft = summary("Bryn", AbilityMethod::Roll, Some(rolls()));
        draft.status = DraftStatus::Submitted;
        draft.pending = 3;
        draft.class_name = Some("Cleric".into());
        draft.species_name = Some("Human".into());
        draft.background_name = Some("Acolyte".into());
        data.party = vec![draft];
        data.coverage = PartySummary {
            skills: vec![("Insight".into(), vec!["Bryn".into()])],
            uncovered: vec!["Perception".into()],
        };
        let html = render_board(CharactersView::Ready(Box::new(data)));

        // One row per draft with what the DM reviews: status, open work, the
        // roots' names, the scores and the honest dice.
        assert!(html.contains("Submitted"), "{html}");
        assert!(html.contains("<td>3</td>"), "{html}");
        assert!(html.contains("Cleric"), "{html}");
        assert!(html.contains("Human"), "{html}");
        assert!(html.contains("Acolyte"), "{html}");
        assert!(html.contains("4 3 1 6"), "{html}");
        // The note form, enabled for a submitted draft.
        assert!(html.contains("Request changes to Bryn"), "{html}");
        assert!(
            html.contains("note-0b6f4c2e-0000-4000-8000-000000000000"),
            "{html}"
        );
        assert!(html.contains("maxlength=\"1000\""), "{html}");
        let tag = html
            .split("<button")
            .find(|chunk| chunk.contains("Request changes"))
            .expect("a request button");
        assert!(
            !tag[..tag.find('>').expect("tag ends")].contains("disabled"),
            "{html}"
        );

        // The coverage table names what the party has and what nobody has.
        assert!(html.contains("Skill coverage"), "{html}");
        assert!(html.contains("Perception"), "{html}");
        assert!(html.contains("nobody"), "{html}");
    }

    #[test]
    fn approve_is_offered_only_when_allowed() {
        let submitted = |name: &str| {
            let mut draft = summary(name, AbilityMethod::StandardArray, None);
            draft.status = DraftStatus::Submitted;
            draft
        };
        // An admin, or the GM holding the running session, while a world is ready.
        let mut data = data(Role::Gm);
        data.party = vec![submitted("Bryn")];
        data.can_approve = true;
        let html = render_board(CharactersView::Ready(Box::new(data.clone())));
        assert!(html.contains("id=\"approve-0b6f4c2e-0000-4000-8000-000000000000\""));
        assert!(html.contains(">Approve</button>"), "{html}");

        // The session is held by another GM, or no world is ready.
        let mut held = data.clone();
        held.can_approve = false;
        let html = render_board(CharactersView::Ready(Box::new(held)));
        assert!(!html.contains("Approve"), "{html}");

        // Only a submitted draft may be approved.
        let mut pending = data;
        pending.party[0].status = DraftStatus::Draft;
        let html = render_board(CharactersView::Ready(Box::new(pending)));
        assert!(!html.contains("id=\"approve-"), "{html}");
    }

    #[test]
    fn approved_draft_links_to_the_play_host() {
        let mut data = data(Role::Gm);
        let mut draft = summary("Bryn", AbilityMethod::Roll, Some(rolls()));
        draft.status = DraftStatus::Approved;
        data.party = vec![draft];
        data.play_url = Some("https://play.example.test/_circle/enter".into());
        let html = render_board(CharactersView::Ready(Box::new(data.clone())));
        assert!(
            html.contains("href=\"https://play.example.test/_circle/enter\""),
            "{html}"
        );
        assert!(html.contains("Play in Foundry"), "{html}");

        // A draft that has not reached Foundry has nothing to open there.
        let mut unapproved = data;
        unapproved.party[0].status = DraftStatus::Submitted;
        let html = render_board(CharactersView::Ready(Box::new(unapproved)));
        assert!(!html.contains("Play in Foundry"), "{html}");
    }

    #[test]
    fn player_sees_the_dm_note() {
        let mut data = builder_data(CampaignRules::default());
        data.status = DraftStatus::Submitted;
        data.editable = false;
        data.dm_note = Some("Pick a deity that fits the campaign.".into());
        let html = render_sheet(BuilderView::Ready(Box::new(data)));

        assert!(
            html.contains("DM note: Pick a deity that fits the campaign."),
            "{html}"
        );
        assert!(html.contains("Submitted"), "{html}");
        assert!(html.contains("your DM reviews it"), "{html}");
        // A submitted draft offers no second submit.
        assert!(!html.contains("Submit for review"), "{html}");
    }

    #[test]
    fn picking_keeps_a_pool_within_its_count() {
        let options = vec![
            Opt {
                value: "skills:his".into(),
                label: "History".into(),
            },
            Opt {
                value: "skills:ins".into(),
                label: "Insight".into(),
            },
        ];
        let offered = vec![
            "skills:his".to_owned(),
            "skills:ins".to_owned(),
            "skills:rel".to_owned(),
        ];

        // A checkbox adds and drops its own value only.
        let one = toggle(&[], &offered, &options, 2, "skills:ins", true);
        assert_eq!(one, ["skills:ins"]);
        let two = toggle(&one, &offered, &options, 2, "skills:rel", true);
        assert_eq!(two, ["skills:ins", "skills:rel"]);
        assert_eq!(
            toggle(&two, &offered, &options, 2, "skills:rel", false),
            ["skills:ins"]
        );

        // A radio replaces this pool's value and leaves another pool's alone.
        assert_eq!(
            toggle(&two, &offered, &options, 1, "skills:his", true),
            ["skills:rel", "skills:his"]
        );

        // A pick the content no longer offers is dropped by the next change.
        assert_eq!(
            toggle(
                &["skills:med".to_owned()],
                &offered,
                &options,
                2,
                "skills:his",
                true
            ),
            ["skills:his"]
        );
    }
}
