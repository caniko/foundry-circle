//! The character builder pages: the characters board and one draft's sheet.
//!
//! The pages are pure components over the views `builder_store` returns, so the
//! SSR tests need neither a server nor a router.

use dioxus::prelude::*;
use tartan_ui_core::{Feedback, FeedbackKind, FilterOption, ResourceLink};
use tartan_ui_dioxus::{
    CardGrid, DashboardHeader, EmptyState, FeedbackBanner, Field, Panel, ResourceCard, SelectField,
};

use crate::{
    ConsoleShell, Role, Route,
    builder::{
        AbilityMethod, AbilityRoll, BuilderView, CampaignRules, CampaignView, CharactersView,
        DraftPatch, DraftSummary, POINT_BUY_BUDGET, Pending, Rolls, Scores, modifier,
        point_buy_cost,
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
            CharactersBoard { view, create, save, error: error() }
        }
    }
}

/// `/api/console/characters/{id}`: one draft's sheet.
#[component]
pub fn BuilderPage(id: String) -> Element {
    let load = id.clone();
    let view = use_server_future(move || {
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
    let roll = EventHandler::new(move |()| {
        if busy() {
            return;
        }
        busy.set(true);
        let id = id.clone();
        spawn(async move {
            match roll_abilities(id).await {
                Ok(Ok(view)) => fresh.set(Some(view)),
                Ok(Err(code)) => error.set(Some(code_message(&code).into())),
                Err(_) => error.set(Some(UNAVAILABLE.into())),
            }
            busy.set(false);
        });
    });
    let mut view = match (fresh(), &*view.read()) {
        (Some(view), _) => view,
        (None, Some(Ok(view))) => view.clone(),
        _ => BuilderView::Missing,
    };
    if let (true, BuilderView::Ready(data)) = (busy(), &mut view) {
        data.editable = false;
    }
    rsx! {
        ConsoleShell { account: None,
            BuilderSheet { view, patch, roll, error: error() }
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
        _ => UNAVAILABLE,
    }
}

/// The characters board: my drafts, and for GMs the rules and the party.
#[component]
fn CharactersBoard(
    view: CharactersView,
    create: EventHandler<String>,
    save: EventHandler<(String, bool, CampaignRules)>,
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
                        description: Some("Open character creation and choose the ability methods the players may use.".to_string()),
                        for campaign in &data.campaigns {
                            CampaignRulesForm { campaign: campaign.clone(), save }
                        }
                    }
                    Panel {
                        title: Some("Party".to_string()),
                        description: Some("Every draft in your worlds, with its honest rolls.".to_string()),
                        if data.party.is_empty() {
                            p { class: "tartan-muted", "Nobody has started a character yet." }
                        } else {
                            PartyTable { drafts: data.party.clone() }
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

/// One campaign's open toggle and allowed methods.
#[component]
fn CampaignRulesForm(
    campaign: CampaignView,
    save: EventHandler<(String, bool, CampaignRules)>,
) -> Element {
    let mut open = use_signal(|| campaign.open);
    let mut methods = use_signal(|| campaign.rules.ability_methods.clone());
    let world_id = campaign.world_id.clone();
    let rules = campaign.rules.clone();
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

/// Name, player, method, scores and the dice behind each roll.
#[component]
fn PartyTable(drafts: Vec<DraftSummary>) -> Element {
    rsx! {
        table { class: "builder-party",
            thead {
                tr {
                    th { "Character" }
                    th { "Player" }
                    th { "Method" }
                    th { "Scores" }
                    th { "Rolls" }
                }
            }
            tbody {
                for draft in drafts {
                    tr { key: "{draft.id}",
                        td { a { href: "/api/console/characters/{draft.id}", {draft_title(&draft)} } }
                        td { {draft.player.clone()} }
                        td { {method_label(draft.choices.method)} }
                        td { {scores_line(draft.choices.base)} }
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
                    AbilityScores { method, base, rolls: data.rolls.clone(), editable, patch }
                }
            }
            if !data.pending.is_empty() {
                PendingList { pending: data.pending.clone() }
            }
        div { class: "builder-sticky",
            span { "{data.pending.len()} to do" }
            a { class: "tartan-button", href: "/api/console/characters", "Back to characters" }
        }
    }
}

/// Six rows: pool selects for array and roll, number inputs for point buy and
/// manual, each with its modifier.
#[component]
fn AbilityScores(
    method: AbilityMethod,
    base: Scores,
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
    pool: Vec<u8>,
    editable: bool,
    patch: EventHandler<DraftPatch>,
) -> Element {
    let value = base.get(&field).unwrap_or(0);
    let modifier = modifier(value);
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
            span { class: "builder-ability__modifier", "{modifier}" }
        }
    }
}

/// The one "to do" list; the phones scroll to the sticky bar's count.
#[component]
fn PendingList(pending: Vec<Pending>) -> Element {
    rsx! {
        ul { class: "builder-pending", aria_label: "To do",
            for item in pending {
                li { key: "{item.key}", "{item.label}" }
            }
        }
    }
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
    use crate::builder::{BuilderData, CharactersData, DraftStatus};

    fn campaign(open: bool) -> CampaignView {
        CampaignView {
            world_id: "canary".into(),
            open,
            rules: CampaignRules::default(),
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
            },
            rolls,
            pending: 0,
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
            },
            rolls: None,
            pending: Vec::new(),
            dm_note: None,
        }
    }

    fn render_board(view: CharactersView) -> String {
        #[component]
        fn Harness(view: CharactersView) -> Element {
            let create = EventHandler::new(|_: String| ());
            let save = EventHandler::new(|_: (String, bool, CampaignRules)| ());
            rsx! {
                CharactersBoard { view, create, save, error: None }
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
            rsx! {
                BuilderSheet { view, patch, roll, error: None }
            }
        }
        dioxus::ssr::render_element(rsx! {
            Harness { view }
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
    fn locked_draft_renders_read_only() {
        let mut data = builder_data(CampaignRules::default());
        data.status = DraftStatus::Submitted;
        data.editable = false;
        let html = render_sheet(BuilderView::Ready(Box::new(data)));
        assert!(html.contains("Submitted"));
        assert!(html.contains("read-only"));
        assert!(html.contains("disabled"));
    }
}
