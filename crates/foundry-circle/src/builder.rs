//! Session-zero rules and the pure half of the character builder.
//!
//! Everything here is I/O-free so the wasm bundle and the server agree on the
//! rules; `builder_store` owns the SQL and `builder_ui` the pages.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Role;

/// The one ability-score method the rules offer for a draft.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AbilityMethod {
    StandardArray,
    PointBuy,
    Roll,
    Manual,
}

impl AbilityMethod {
    pub const ALL: [Self; 4] = [
        Self::StandardArray,
        Self::PointBuy,
        Self::Roll,
        Self::Manual,
    ];

    /// The value a native select submits; the same spelling serde uses.
    pub fn key(self) -> &'static str {
        match self {
            Self::StandardArray => "standardArray",
            Self::PointBuy => "pointBuy",
            Self::Roll => "roll",
            Self::Manual => "manual",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::StandardArray => "Standard array",
            Self::PointBuy => "Point buy",
            Self::Roll => "Roll (server)",
            Self::Manual => "Manual",
        }
    }
}

/// What a class or background hands out at level one.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EquipmentMode {
    Kit,
    Gold,
    #[default]
    Either,
}

/// One campaign's session-zero rules; one row per adopted world.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CampaignRules {
    /// Methods the players may pick from; empty selections fall back to the
    /// default three.
    pub ability_methods: Vec<AbilityMethod>,
    /// 1 until starting levels above one ship.
    pub starting_level: u8,
    pub equipment: EquipmentMode,
    /// Enabled pack ids; filled by the catalog sync.
    pub sources: Vec<String>,
}

impl Default for CampaignRules {
    fn default() -> Self {
        Self {
            ability_methods: vec![
                AbilityMethod::StandardArray,
                AbilityMethod::PointBuy,
                AbilityMethod::Roll,
            ],
            starting_level: 1,
            equipment: EquipmentMode::Either,
            sources: Vec::new(),
        }
    }
}

/// The six scores in book order.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Scores {
    pub str: u8,
    pub dex: u8,
    pub con: u8,
    pub int: u8,
    pub wis: u8,
    pub cha: u8,
}

impl Scores {
    /// Field key and the label the pages show, in book order.
    pub const LABELS: [(&'static str, &'static str); 6] = [
        ("str", "Strength"),
        ("dex", "Dexterity"),
        ("con", "Constitution"),
        ("int", "Intelligence"),
        ("wis", "Wisdom"),
        ("cha", "Charisma"),
    ];

    pub fn values(self) -> [u8; 6] {
        [self.str, self.dex, self.con, self.int, self.wis, self.cha]
    }

    pub fn get(self, key: &str) -> Option<u8> {
        Some(match key {
            "str" => self.str,
            "dex" => self.dex,
            "con" => self.con,
            "int" => self.int,
            "wis" => self.wis,
            "cha" => self.cha,
            _ => return None,
        })
    }

    pub fn set(&mut self, key: &str, value: u8) {
        match key {
            "str" => self.str = value,
            "dex" => self.dex = value,
            "con" => self.con = value,
            "int" => self.int = value,
            "wis" => self.wis = value,
            "cha" => self.cha = value,
            _ => {}
        }
    }
}

/// One 4d6-drop-lowest set, dice kept so the DM can check the total.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AbilityRoll {
    pub dice: [u8; 4],
    pub total: u8,
}

/// The server's one roll per draft; stored in a column players cannot write.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Rolls {
    pub rolled_at: String,
    pub sets: [AbilityRoll; 6],
}

/// Everything a player has decided so far.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DraftChoices {
    pub method: Option<AbilityMethod>,
    pub base: Option<Scores>,
    /// The chosen compendium uuids.
    pub class: Option<String>,
    pub species: Option<String>,
    pub background: Option<String>,
    /// Decision key → trait keys or item uuids.
    pub picks: BTreeMap<String, Vec<String>>,
    /// Decision key → ability → the bonus the player assigned there.
    pub asi: BTreeMap<String, BTreeMap<String, u8>>,
}

impl DraftChoices {
    /// Point a slot at another document. The picks the old root opened are the
    /// caller's to drop with `prune_picks`: a nested decision's key carries the
    /// granted feature's uuid, not the root's.
    pub fn select(&mut self, slot: Slot, uuid: Option<String>) {
        *slot.field(self) = uuid;
    }
}

/// The three documents a draft roots on.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Slot {
    Class,
    Species,
    Background,
}

impl Slot {
    /// The item type a document must have to fill this slot.
    pub fn item_type(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Species => "race",
            Self::Background => "background",
        }
    }

    fn field(self, choices: &mut DraftChoices) -> &mut Option<String> {
        match self {
            Self::Class => &mut choices.class,
            Self::Species => &mut choices.species,
            Self::Background => &mut choices.background,
        }
    }

    /// The document this slot names, if the draft filled it.
    fn chosen(self, choices: &DraftChoices) -> Option<&str> {
        match self {
            Self::Class => &choices.class,
            Self::Species => &choices.species,
            Self::Background => &choices.background,
        }
        .as_deref()
    }
}

/// One change the builder sends; the server re-validates all of it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DraftPatch {
    Name(String),
    Abilities {
        method: AbilityMethod,
        base: Scores,
    },
    Select {
        slot: Slot,
        uuid: Option<String>,
    },
    Pick {
        key: String,
        values: Vec<String>,
    },
    Asi {
        key: String,
        /// Ability key → the bonus the player assigned there.
        deltas: BTreeMap<String, u8>,
    },
}

/// Where an unfinished draft needs the player.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    Class,
    Origin,
    Abilities,
    Equipment,
    Details,
    Review,
}

impl Step {
    pub const ALL: [Self; 6] = [
        Self::Class,
        Self::Origin,
        Self::Abilities,
        Self::Equipment,
        Self::Details,
        Self::Review,
    ];

    /// The in-page anchor the step nav and the "to do" list link to.
    pub fn anchor(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Origin => "origin",
            Self::Abilities => "abilities",
            Self::Equipment => "equipment",
            Self::Details => "details",
            Self::Review => "review",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Class => "Class",
            Self::Origin => "Origin",
            Self::Abilities => "Abilities",
            Self::Equipment => "Equipment",
            Self::Details => "Details",
            Self::Review => "Review",
        }
    }
}

/// One open item in the "To do" list.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Pending {
    pub step: Step,
    pub key: String,
    pub label: String,
}

/// A draft's lifecycle; only `Draft` rows are editable.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DraftStatus {
    Draft,
    Submitted,
    Approved,
}

impl DraftStatus {
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "submitted" => Some(Self::Submitted),
            "approved" => Some(Self::Approved),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Submitted => "Submitted",
            Self::Approved => "Approved",
        }
    }
}

/// One of the world's Item packs as the companion module lists it. Only packs
/// the world already offers reach this list; nothing is bundled here.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackInfo {
    /// The pack's collection id, for example `dnd5e.classes24`.
    pub id: String,
    pub label: String,
    /// The pack's `flags.dnd5e.sourceBook`, when the system sets one.
    pub source_book: Option<String>,
    pub rules_version: Option<String>,
    /// Documents the catalog holds for this pack.
    pub count: u32,
}

impl PackInfo {
    /// The toggle label: the pack's name and the book behind it.
    pub fn line(&self) -> String {
        match &self.source_book {
            Some(book) => format!("{} · {book}", self.label),
            None => self.label.clone(),
        }
    }
}

/// One compendium document as the module returns it. `system` is the item's
/// stored data without its description; `traits` maps a Trait advancement id to
/// its granted and choosable keys.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    pub uuid: String,
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub img: Option<String>,
    /// The description as plain text.
    pub text: String,
    pub system: serde_json::Value,
    #[serde(default)]
    pub traits: serde_json::Value,
}

/// The enabled sources' cached documents: the class step and every decision the
/// walker resolves read only this. Foundry stays the authority behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// The enabled packs, by label; the item's uuid names which one it is.
    pub packs: Vec<PackInfo>,
    pub items: Vec<CatalogItem>,
}

impl Catalog {
    pub fn get(&self, uuid: &str) -> Option<&CatalogItem> {
        self.items.iter().find(|item| item.uuid == uuid)
    }

    /// The label of the pack a document's uuid names, for grouping by source.
    pub fn source(&self, uuid: &str) -> String {
        let id = pack_id(uuid).unwrap_or_default();
        self.packs
            .iter()
            .find(|pack| pack.id == id)
            .map_or(id, |pack| pack.label.clone())
    }
}

/// The `<package>.<pack>` collection a compendium uuid names.
fn pack_id(uuid: &str) -> Option<String> {
    let mut parts = uuid.split('.');
    if parts.next()? != "Compendium" {
        return None;
    }
    Some(format!("{}.{}", parts.next()?, parts.next()?))
}

/// One class the enabled sources offer, as the class step lists it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClassOption {
    pub uuid: String,
    pub name: String,
    /// The pack's label, which is how the list groups its classes.
    pub source: String,
}

/// The chosen class as its step shows it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClassDetails {
    pub uuid: String,
    pub name: String,
    pub source: String,
    pub hit_die: String,
    pub primary_ability: String,
    /// The features the class grants, with their descriptions.
    pub features: Vec<Feature>,
    pub decisions: Vec<Decision>,
}

/// One chosen origin root (species or background) as its step shows it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OriginDetails {
    pub uuid: String,
    pub name: String,
    pub source: String,
    /// The features the root grants, with their descriptions.
    pub features: Vec<Feature>,
    pub decisions: Vec<Decision>,
}

/// One granted feature: its name and plain-text description.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Feature {
    pub name: String,
    pub text: String,
}

/// One choice a decision offers; `value` is the trait key or the item uuid.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Opt {
    #[serde(alias = "key")]
    pub value: String,
    pub label: String,
}

/// One Trait pool: how many of its options the player picks.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Pool {
    pub count: u8,
    pub options: Vec<Opt>,
}

/// What a Trait advancement does with the keys it takes, as
/// `DND5E.traitModes` names them.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TraitMode {
    /// Gain the trait.
    #[default]
    Default,
    /// Gain expertise in a trait the draft is already proficient with.
    Expertise,
    /// Gain expertise regardless of proficiency.
    ForcedExpertise,
    /// Gain proficiency, or expertise when it is already proficient.
    Upgrade,
    /// Gain mastery with a weapon the draft is proficient with.
    Mastery,
}

impl TraitMode {
    /// The mode an advancement's configuration names; an unknown one is the
    /// system's default.
    fn parse(key: &str) -> Self {
        match key {
            "expertise" => Self::Expertise,
            "forcedExpertise" => Self::ForcedExpertise,
            "upgrade" => Self::Upgrade,
            "mastery" => Self::Mastery,
            _ => Self::Default,
        }
    }

    /// The mode's index in the system's `traitModes`, which orders a Trait
    /// advancement against the others at its level.
    fn order(self) -> u16 {
        match self {
            Self::Default => 0,
            Self::Expertise => 1,
            Self::ForcedExpertise => 2,
            Self::Upgrade => 3,
            Self::Mastery => 4,
        }
    }

    /// Whether the draft can still take this pool key in this mode, from what it
    /// is proficient with and what it already holds expertise in.
    fn offers(
        self,
        option: &Opt,
        proficient: &BTreeSet<String>,
        expert: &BTreeSet<String>,
    ) -> bool {
        match self {
            Self::Default => true,
            Self::Expertise => {
                proficient.contains(&option.value) && !expert.contains(&option.value)
            }
            Self::ForcedExpertise | Self::Upgrade => !expert.contains(&option.value),
            // A mastery key counts when the weapon itself or its category is
            // proficient: `weapon:sim:dagger` or `weapon:sim`.
            Self::Mastery => {
                let category = option
                    .value
                    .rsplit_once(':')
                    .map_or(option.value.as_str(), |(category, _)| category);
                proficient.contains(category) || proficient.contains(&option.value)
            }
        }
    }

    /// The system's own one-line hint for a mode, as its labels file words it.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Expertise => {
                Some("Gain expertise in a trait in which you already have proficiency.")
            }
            Self::ForcedExpertise => {
                Some("Gain expertise in a trait regardless of your previous proficiency level.")
            }
            Self::Upgrade => Some(
                "Gain proficiency in a trait unless you already have it, otherwise gain expertise.",
            ),
            Self::Mastery => {
                Some("Gain mastery with a weapon in which you already have proficiency.")
            }
        }
    }
}

/// What kind of answer a decision wants.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum DecisionKind {
    Trait {
        /// What the advancement does with the keys it takes; the pools already
        /// hold only what it can still offer.
        #[serde(default)]
        mode: TraitMode,
        grants: Vec<Opt>,
        choices: Vec<Pool>,
    },
    Items {
        count: u8,
        options: Vec<Opt>,
    },
    Asi {
        /// How many points the player assigns across `open`.
        points: u8,
        /// The most one ability may take.
        cap: u8,
        /// The abilities the player may improve, in book order.
        open: Vec<String>,
        /// Ability → points the configuration assigns itself; they count toward
        /// the score ceiling but never the player's `points`.
        #[serde(default)]
        fixed: BTreeMap<String, u8>,
        /// The configured score ceiling; 20 when absent.
        max: Option<u8>,
    },
    Size {
        options: Vec<String>,
    },
    Unsupported {
        kind: String,
    },
}

/// One choice a document's advancement opens at the draft's level.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// `<item uuid>#<advancement id>#<level>`; what `DraftPatch::Pick` names.
    pub key: String,
    /// The document that opens it, for example "Divine Order".
    pub owner_name: String,
    pub title: String,
    /// The advancement's own hint, as plain text.
    pub hint: String,
    /// The step of the root that leads here; a nested grant keeps its root's.
    pub step: Step,
    pub kind: DecisionKind,
}

impl Decision {
    /// Nothing to answer: a Trait advancement that only grants something.
    pub fn is_automatic(&self) -> bool {
        matches!(&self.kind, DecisionKind::Trait { choices, .. } if choices.is_empty())
    }
}

/// An advancement's hint is HTML; the pages show text.
///
/// ponytail: tags only; decode entities if a real hint carries one.
fn plain(html: &str) -> String {
    let mut text = String::new();
    let mut inside = false;
    for character in html.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => text.push(character),
            _ => {}
        }
    }
    text.trim().to_owned()
}

/// How deep a chain of granted items may nest before the walker gives up.
const WALK_DEPTH: u8 = 8;

fn advancement_kind(advancement: &Value) -> &str {
    advancement["type"].as_str().unwrap_or_default()
}

/// `CONFIG.DND5E.traits` order, which a Trait advancement sorts by its first
/// represented type after its own type's order (`module/config.mjs`).
const TRAITS: [&str; 11] = [
    "saves",
    "skills",
    "languages",
    "armor",
    "weapon",
    "tool",
    "di",
    "dr",
    "dv",
    "dm",
    "ci",
];

/// The key dnd5e sorts one item's advancements at a level by, from each type's
/// `metadata.order` and `sortingValueForLevel`
/// (`module/documents/advancement/*.mjs`, `item.mjs`): a Trait adds the order
/// of the trait type it represents and of its mode, then every type adds its
/// title.
fn sort_key(advancement: &Value) -> String {
    let kind = advancement_kind(advancement);
    let order = match kind {
        "HitPoints" => 10,
        "AbilityScoreImprovement" => 20,
        "Size" => 25,
        "Trait" => 30,
        "ItemGrant" => 40,
        "ItemChoice" => 50,
        "ModifyItem" => 55,
        "ScaleValue" => 60,
        "Subclass" => 70,
        _ => 100,
    };
    let title = advancement["name"].as_str().unwrap_or_default();
    if kind != "Trait" {
        return format!("{order:04} {title}");
    }
    // The first key the advancement represents, grants before pools: its
    // prefix is the trait type.
    let configuration = &advancement["configuration"];
    let trait_type = configuration["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            configuration["choices"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|choice| choice["pool"].as_array()?.first()),
        )
        .find_map(Value::as_str)
        .and_then(|key| key.split(':').next())
        .unwrap_or_default();
    let trait_order = TRAITS
        .iter()
        .position(|name| *name == trait_type)
        .unwrap_or(TRAITS.len()) as u16;
    let mode = TraitMode::parse(configuration["mode"].as_str().unwrap_or_default());
    format!("{order:04} {:04} {title}", trait_order + mode.order() * 100)
}

/// The levels an advancement applies at, as dnd5e's `levels` getter reports
/// them: ItemChoice keys its own, Size is always level 0, the rest carry one.
fn advancement_levels(advancement: &Value) -> Vec<u8> {
    match advancement_kind(advancement) {
        "ItemChoice" => advancement["configuration"]["choices"]
            .as_object()
            .map(|choices| choices.keys().filter_map(|key| key.parse().ok()).collect())
            .unwrap_or_default(),
        "Size" => vec![0],
        _ => advancement["level"]
            .as_u64()
            .and_then(|level| u8::try_from(level).ok())
            .map(|level| vec![level])
            .unwrap_or_default(),
    }
}

/// A Trait advancement's labelled grants and pools, as the module resolved
/// them; a document the module could not resolve reads as empty.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TraitData {
    grants: Vec<Opt>,
    choices: Vec<Pool>,
}

fn trait_data(item: &CatalogItem, id: &str) -> TraitData {
    serde_json::from_value(item.traits.get(id).cloned().unwrap_or_default()).unwrap_or_default()
}

/// An ItemChoice's offers: the pool's documents the catalog holds plus, for a
/// dropping choice, every document whose type matches its restriction. An
/// offer whose prerequisites the draft does not meet is not offered.
fn item_choice_options(
    catalog: &Catalog,
    advancement: &Value,
    level: u8,
    held: &BTreeSet<String>,
) -> Vec<Opt> {
    let configuration = &advancement["configuration"];
    let mut options: Vec<Opt> = Vec::new();
    for entry in configuration["pool"].as_array().into_iter().flatten() {
        if let Some(item) = entry["uuid"].as_str().and_then(|uuid| catalog.get(uuid)) {
            push_option(&mut options, item, level, held);
        }
    }
    let restriction = &configuration["restriction"];
    let subtype = restriction["subtype"].as_str().unwrap_or_default();
    if configuration["allowDrops"].as_bool().unwrap_or(false) && !subtype.is_empty() {
        let value = restriction["type"].as_str().unwrap_or_default();
        for item in &catalog.items {
            let kind = &item.system["type"];
            if kind["value"].as_str() == Some(value) && kind["subtype"].as_str() == Some(subtype) {
                push_option(&mut options, item, level, held);
            }
        }
    }
    options
}

fn push_option(options: &mut Vec<Opt>, item: &CatalogItem, level: u8, held: &BTreeSet<String>) {
    if prerequisite_met(item, level, held)
        && !options.iter().any(|option| option.value == item.uuid)
    {
        options.push(Opt {
            value: item.uuid.clone(),
            label: item.name.clone(),
        });
    }
}

/// Whether an item's feat prerequisites hold at `level`: its level is at most
/// the character's, and when it names required identifiers the draft holds at
/// least one of them, since the system's warning reads them as a disjunction.
/// `held` carries each item's identifier both plain and `type:identifier`.
fn prerequisite_met(item: &CatalogItem, level: u8, held: &BTreeSet<String>) -> bool {
    let prerequisites = &item.system["prerequisites"];
    if prerequisites["level"]
        .as_u64()
        .is_some_and(|required| required > u64::from(level))
    {
        return false;
    }
    let required: Vec<&str> = prerequisites["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    required.is_empty() || required.iter().any(|identifier| held.contains(*identifier))
}

/// What one root's walk found: the items it grants and the choices it opens.
#[derive(Default)]
struct Walk {
    features: Vec<Feature>,
    decisions: Vec<Decision>,
    /// Trait keys the draft holds proficiently and at expertise, plus the
    /// identifiers of the items it holds: the state dnd5e applies one
    /// advancement at a time and later advancements read.
    proficient: BTreeSet<String>,
    expert: BTreeSet<String>,
    held: BTreeSet<String>,
}

impl Walk {
    /// Note an item as held, in the two forms a prerequisite can name it.
    fn hold(&mut self, item: &CatalogItem) {
        if let Some(identifier) = item.system["identifier"]
            .as_str()
            .filter(|identifier| !identifier.is_empty())
        {
            self.held.insert(identifier.to_owned());
            self.held.insert(format!("{}:{identifier}", item.item_type));
        }
    }

    /// Fold one Trait advancement's grants and answers into what the draft
    /// holds; the system applies every mode but default as expertise.
    fn record_traits(&mut self, mode: TraitMode, grants: &[Opt], chosen: &[String]) {
        let keys = grants
            .iter()
            .map(|option| option.value.clone())
            .chain(chosen.iter().cloned());
        match mode {
            TraitMode::Default => self.proficient.extend(keys),
            _ => self.expert.extend(keys),
        }
    }
}

/// Walk one root item and, after it, every item its advancements grant or pick
/// at this level: dnd5e applies a level's root steps first, then synthesises
/// steps for the items they added, in the order they were added
/// (`advancement-manager.mjs`).
fn walk(
    catalog: &Catalog,
    root: &CatalogItem,
    choices: &DraftChoices,
    level: u8,
    step: Step,
    found: &mut Walk,
) {
    let mut queue = VecDeque::from([(root, 0)]);
    while let Some((item, depth)) = queue.pop_front() {
        if depth >= WALK_DEPTH {
            continue;
        }
        found.hold(item);
        let mut advancements: Vec<(&String, &Value)> = item.system["advancement"]
            .as_object()
            .into_iter()
            .flatten()
            .collect();
        advancements.sort_by_cached_key(|(_, advancement)| sort_key(advancement));
        for (id, advancement) in advancements {
            // A secondary-class advancement belongs to a multiclass dip, not here.
            if advancement["classRestriction"].as_str() == Some("secondary") {
                continue;
            }
            // ponytail: spell ItemChoices are slice 8's data; skipping them keeps a
            // spell picker from arriving as an unanswerable step.
            let configuration = &advancement["configuration"];
            if advancement_kind(advancement) == "ItemChoice"
                && (configuration["type"].as_str() == Some("spell")
                    || configuration["restriction"]["type"].as_str() == Some("spell"))
            {
                continue;
            }
            let Some(at) = advancement_levels(advancement)
                .into_iter()
                .filter(|at| *at <= level)
                .max()
            else {
                continue;
            };
            let key = format!("{}#{id}#{at}", item.uuid);
            let mut title = advancement["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let hint = plain(advancement["hint"].as_str().unwrap_or_default());
            let mut chosen: Vec<&String> = Vec::new();
            let kind = match advancement_kind(advancement) {
                "Trait" => {
                    let data = trait_data(item, id);
                    let mode = TraitMode::parse(configuration["mode"].as_str().unwrap_or_default());
                    let configured = configuration["choices"]
                        .as_array()
                        .is_some_and(|choices| !choices.is_empty());
                    let kind = if configured && data.choices.is_empty() {
                        // Nothing resolved the pools, so do not offer an empty choice.
                        DecisionKind::Unsupported {
                            kind: "Trait".into(),
                        }
                    } else {
                        DecisionKind::Trait {
                            mode,
                            grants: data.grants.clone(),
                            choices: data
                                .choices
                                .iter()
                                .map(|pool| Pool {
                                    count: pool.count,
                                    options: pool
                                        .options
                                        .iter()
                                        .filter(|option| {
                                            mode.offers(option, &found.proficient, &found.expert)
                                        })
                                        .cloned()
                                        .collect(),
                                })
                                .collect(),
                        }
                    };
                    // The offers above are what this advancement can still take, so
                    // its own answers count only for the advancements after it.
                    let answers: &[String] = choices.picks.get(&key).map_or(&[], Vec::as_slice);
                    found.record_traits(mode, &data.grants, answers);
                    Some(kind)
                }
                "ItemChoice" => {
                    let count = configuration["choices"][at.to_string()]["count"]
                        .as_u64()
                        .and_then(|count| u8::try_from(count).ok())
                        .unwrap_or(0);
                    let options = item_choice_options(catalog, advancement, level, &found.held);
                    chosen = choices
                        .picks
                        .get(&key)
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>();
                    Some(if options.len() < usize::from(count) {
                        // The enabled sources cannot satisfy this pool yet.
                        DecisionKind::Unsupported {
                            kind: "ItemChoice".into(),
                        }
                    } else {
                        DecisionKind::Items { count, options }
                    })
                }
                "ItemGrant" => {
                    if !configuration["optional"].as_bool().unwrap_or(false) {
                        for entry in configuration["items"].as_array().into_iter().flatten() {
                            if entry["optional"].as_bool().unwrap_or(false) {
                                continue;
                            }
                            let Some(granted) =
                                entry["uuid"].as_str().and_then(|uuid| catalog.get(uuid))
                            else {
                                continue;
                            };
                            found.features.push(Feature {
                                name: granted.name.clone(),
                                text: granted.text.clone(),
                            });
                            queue.push_back((granted, depth + 1));
                        }
                    }
                    None
                }
                "Size" => {
                    let options: Vec<String> = advancement["configuration"]["sizes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|size| size.as_str().map(str::to_owned))
                        .collect();
                    // One size is no choice; the actor takes it by default.
                    if options.len() > 1 {
                        title = Some("Size".into());
                        Some(DecisionKind::Size { options })
                    } else {
                        None
                    }
                }
                "AbilityScoreImprovement" if at == 0 => {
                    let locked: Vec<&str> = configuration["locked"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    // A fixed bonus is pre-assigned: it counts toward the score
                    // ceiling, not the player's points (`ability-score-improvement.mjs`).
                    let fixed = configuration["fixed"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter_map(|(key, value)| {
                            let value =
                                value.as_u64().and_then(|value| u8::try_from(value).ok())?;
                            (value > 0).then(|| (key.clone(), value))
                        })
                        .collect();
                    Some(DecisionKind::Asi {
                        points: configuration["points"]
                            .as_u64()
                            .map_or(0, |points| u8::try_from(points).unwrap_or(u8::MAX)),
                        // The system writes no cap when an ability is unbounded.
                        cap: configuration["cap"]
                            .as_u64()
                            .map_or(u8::MAX, |cap| u8::try_from(cap).unwrap_or(u8::MAX)),
                        open: Scores::LABELS
                            .iter()
                            .map(|(key, _)| *key)
                            .filter(|key| !locked.contains(key))
                            .map(str::to_owned)
                            .collect(),
                        fixed,
                        max: configuration["max"]
                            .as_u64()
                            .map(|max| u8::try_from(max).unwrap_or(u8::MAX)),
                    })
                }
                "Subclass" | "AbilityScoreImprovement" => Some(DecisionKind::Unsupported {
                    kind: advancement_kind(advancement).into(),
                }),
                "ScaleValue" | "HitPoints" | "ModifyItem" => None,
                other => Some(DecisionKind::Unsupported { kind: other.into() }),
            };
            if let Some(kind) = kind {
                found.decisions.push(Decision {
                    key,
                    owner_name: item.name.clone(),
                    title: title.unwrap_or_else(|| item.name.clone()),
                    hint,
                    step,
                    kind,
                });
            }
            for uuid in chosen {
                if let Some(item) = catalog.get(uuid) {
                    found.features.push(Feature {
                        name: item.name.clone(),
                        text: item.text.clone(),
                    });
                    queue.push_back((item, depth + 1));
                }
            }
        }
    }
}

/// Every decision the draft's chosen roots open at `level`.
pub fn decisions(catalog: &Catalog, choices: &DraftChoices, level: u8) -> Vec<Decision> {
    let mut found = Walk::default();
    // dnd5e applies level 0 (species, then background, with their grants) before
    // level 1 (the class), so Expertise sees background skills. ponytail: each root
    // walks all its levels at once; interleave by level when slice 9 adds levels.
    for (root, step) in [
        (&choices.species, Step::Origin),
        (&choices.background, Step::Origin),
        (&choices.class, Step::Class),
    ] {
        if let Some(item) = root.as_deref().and_then(|uuid| catalog.get(uuid)) {
            walk(catalog, item, choices, level, step, &mut found);
        }
    }
    found.decisions
}

/// Drop every answer the draft's roots no longer open: a replaced root's own and
/// nested decisions, and the sub-decisions of an item it no longer picks. The
/// walker produces keys only for the current roots, so this is the one rule,
/// and it covers the ability bonuses beside the picks.
pub fn prune_picks(catalog: &Catalog, choices: &mut DraftChoices, level: u8) {
    let keys: BTreeSet<String> = decisions(catalog, choices, level)
        .into_iter()
        .map(|decision| decision.key)
        .collect();
    choices.picks.retain(|key, _| keys.contains(key));
    choices.asi.retain(|key, _| keys.contains(key));
}

/// The documents of one item type the enabled sources offer, by source then name.
fn offered(catalog: &Catalog, item_type: &str) -> Vec<ClassOption> {
    let mut options: Vec<ClassOption> = catalog
        .items
        .iter()
        .filter(|item| item.item_type == item_type)
        .map(|item| ClassOption {
            uuid: item.uuid.clone(),
            name: item.name.clone(),
            source: catalog.source(&item.uuid),
        })
        .collect();
    options.sort_by(|a, b| a.source.cmp(&b.source).then(a.name.cmp(&b.name)));
    options
}

/// The classes the enabled sources offer, by source then name.
pub fn classes(catalog: &Catalog) -> Vec<ClassOption> {
    offered(catalog, Slot::Class.item_type())
}

/// The species or backgrounds the enabled sources offer, by source then name.
pub fn origin_options(catalog: &Catalog, slot: Slot) -> Vec<ClassOption> {
    offered(catalog, slot.item_type())
}

/// The chosen class as its step shows it: hit die, primary ability, features
/// and the decisions they open.
pub fn class_details(catalog: &Catalog, choices: &DraftChoices, level: u8) -> Option<ClassDetails> {
    let class = choices
        .class
        .as_deref()
        .and_then(|uuid| catalog.get(uuid))?;
    // Walk the origin first, as `decisions` does, so class offers (Expertise)
    // see background proficiencies; then keep only what the class opened.
    let mut found = Walk::default();
    for root in [&choices.species, &choices.background] {
        if let Some(item) = root.as_deref().and_then(|uuid| catalog.get(uuid)) {
            walk(catalog, item, choices, level, Step::Origin, &mut found);
        }
    }
    let (features_from, decisions_from) = (found.features.len(), found.decisions.len());
    walk(catalog, class, choices, level, Step::Class, &mut found);
    let primary_ability = class.system["primaryAbility"]["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(ability_label)
        .collect::<Vec<_>>()
        .join(", ");
    Some(ClassDetails {
        uuid: class.uuid.clone(),
        name: class.name.clone(),
        source: catalog.source(&class.uuid),
        hit_die: class.system["hd"]["denomination"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        primary_ability,
        features: found.features.split_off(features_from),
        decisions: found.decisions.split_off(decisions_from),
    })
}

/// The label of an ability key, as the pages word it.
pub fn ability_label(key: &str) -> Option<&'static str> {
    Scores::LABELS
        .iter()
        .find(|(field, _)| *field == key)
        .map(|(_, label)| *label)
}

/// The label of a size key, as `CONFIG.DND5E.actorSizes` words it.
pub fn size_label(key: &str) -> &str {
    match key {
        "tiny" => "Tiny",
        "sm" => "Small",
        "med" => "Medium",
        "lg" => "Large",
        "huge" => "Huge",
        "grg" => "Gargantuan",
        other => other,
    }
}

/// One chosen origin root's features and decisions, from the same shared walk
/// `decisions` makes: the origin roots walk in dnd5e's order (species, then
/// background) so later choices see what earlier grants made the draft hold,
/// and each root keeps its own part.
pub fn origin_details(
    catalog: &Catalog,
    choices: &DraftChoices,
    level: u8,
    slot: Slot,
) -> Option<OriginDetails> {
    let mut found = Walk::default();
    for root in [Slot::Species, Slot::Background] {
        let Some(item) = root.chosen(choices).and_then(|uuid| catalog.get(uuid)) else {
            continue;
        };
        let (features_from, decisions_from) = (found.features.len(), found.decisions.len());
        walk(catalog, item, choices, level, Step::Origin, &mut found);
        if root == slot {
            return Some(OriginDetails {
                uuid: item.uuid.clone(),
                name: item.name.clone(),
                source: catalog.source(&item.uuid),
                features: found.features.split_off(features_from),
                decisions: found.decisions.split_off(decisions_from),
            });
        }
    }
    None
}

/// What a draft may store for one decision: offered values only, each once,
/// and never more than it asks for. A shorter list is an unfinished answer, so
/// `is_answered` is what closes the decision.
pub fn validate_pick(decision: &Decision, values: &[String]) -> Result<(), &'static str> {
    if values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
    {
        return Err("pick_duplicate");
    }
    match &decision.kind {
        DecisionKind::Trait { choices, .. } => {
            if values.iter().any(|value| {
                !choices
                    .iter()
                    .any(|pool| pool.options.iter().any(|option| &option.value == value))
            }) {
                return Err("pick_not_offered");
            }
            for pool in choices {
                let taken = values
                    .iter()
                    .filter(|value| pool.options.iter().any(|option| &option.value == *value))
                    .count();
                if taken > usize::from(pool.count) {
                    return Err("pick_count");
                }
            }
            Ok(())
        }
        DecisionKind::Items { count, options } => {
            if values
                .iter()
                .any(|value| !options.iter().any(|option| &option.value == value))
            {
                return Err("pick_not_offered");
            }
            if values.len() > usize::from(*count) {
                return Err("pick_count");
            }
            Ok(())
        }
        DecisionKind::Size { options } => {
            if !values.iter().all(|value| options.contains(value)) {
                return Err("pick_not_offered");
            }
            if values.len() > 1 {
                return Err("pick_count");
            }
            Ok(())
        }
        DecisionKind::Asi { .. } | DecisionKind::Unsupported { .. } => Err("pick_unsupported"),
    }
}

/// Whether a decision is answered: every pool holds its count and, for a size
/// or an item choice, exactly what it asks for. An ability bonus answers into
/// `DraftChoices::asi`, so only `pending` and `validate_asi` can judge it.
pub fn is_answered(decision: &Decision, values: &[String]) -> bool {
    validate_pick(decision, values).is_ok()
        && match &decision.kind {
            DecisionKind::Trait { choices, .. } => choices.iter().all(|pool| {
                values
                    .iter()
                    .filter(|value| pool.options.iter().any(|option| &option.value == *value))
                    .count()
                    == usize::from(pool.count)
            }),
            DecisionKind::Items { count, .. } => values.len() == usize::from(*count),
            DecisionKind::Size { .. } => values.len() == 1,
            DecisionKind::Asi { .. } | DecisionKind::Unsupported { .. } => false,
        }
}

/// What a draft may store for one ability bonus: only the abilities the
/// advancement leaves open, never more than `cap` on one of them, exactly
/// `points` in all, and no score above 20 or the configured ceiling. The UI and
/// `update_draft` share this one rule.
pub fn validate_asi(
    decision: &Decision,
    deltas: &BTreeMap<String, u8>,
    base: &Scores,
) -> Result<(), &'static str> {
    let DecisionKind::Asi {
        points,
        cap,
        open,
        fixed,
        max,
    } = &decision.kind
    else {
        return Err("unknown_decision");
    };
    if deltas.keys().any(|key| !open.contains(key)) {
        return Err("asi_locked");
    }
    if deltas.values().any(|delta| delta > cap) {
        return Err("asi_cap");
    }
    let assigned: u16 = deltas.values().map(|delta| u16::from(*delta)).sum();
    if assigned != u16::from(*points) {
        return Err("asi_points");
    }
    let ceiling = max.unwrap_or(20);
    // A fixed bonus may land on a locked ability, so check all six.
    for (key, _) in Scores::LABELS {
        let score = base
            .get(key)
            .unwrap_or(0)
            .saturating_add(fixed.get(key).copied().unwrap_or(0))
            .saturating_add(deltas.get(key).copied().unwrap_or(0));
        if score > ceiling {
            return Err("asi_max");
        }
    }
    Ok(())
}

/// The base scores plus every ability bonus the draft's decisions grant: each
/// configuration's `fixed` bonus and the player's answered assignment. `except`
/// leaves one decision out, which is the context that decision is judged in.
pub fn totals(decisions: &[Decision], choices: &DraftChoices, except: Option<&str>) -> Scores {
    let mut totals = choices.base.unwrap_or_default();
    for decision in decisions {
        let DecisionKind::Asi { fixed, .. } = &decision.kind else {
            continue;
        };
        if except == Some(decision.key.as_str()) {
            continue;
        }
        let answered = choices.asi.get(&decision.key).into_iter().flatten();
        for (key, delta) in fixed.iter().chain(answered) {
            let current = totals.get(key).unwrap_or(0);
            totals.set(key, current.saturating_add(*delta));
        }
    }
    totals
}

/// Every trait key the draft already holds: what its decisions grant plus what
/// the player picked.
pub fn granted_traits(decisions: &[Decision], choices: &DraftChoices) -> BTreeSet<String> {
    let mut granted = BTreeSet::new();
    for decision in decisions {
        if let DecisionKind::Trait { grants, .. } = &decision.kind {
            granted.extend(grants.iter().map(|option| option.value.clone()));
            granted.extend(
                choices
                    .picks
                    .get(&decision.key)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
    }
    granted
}

fn decision_label(decision: &Decision) -> String {
    match &decision.kind {
        DecisionKind::Unsupported { .. } => format!("{} (not supported yet)", decision.title),
        _ => decision.title.clone(),
    }
}

/// The attribution the SRD 5.2.1 licence requires wherever its content shows.
pub const SRD_ATTRIBUTION: &str = "This work includes material from the System Reference Document 5.2.1 (\"SRD 5.2.1\") by Wizards of the Coast LLC, available at https://www.dndbeyond.com/srd. The SRD 5.2.1 is licensed under the Creative Commons Attribution 4.0 International License, available at https://creativecommons.org/licenses/by/4.0/legalcode.";

/// One adopted world's session-zero rules as the console lists them.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CampaignView {
    pub world_id: String,
    pub open: bool,
    pub rules: CampaignRules,
    /// The catalog packs the last sync stored, ordered by label.
    pub packs: Vec<PackInfo>,
    /// When the catalog was last synced, as shown to the GM.
    pub synced_at: Option<String>,
}

/// One draft as the characters page lists it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DraftSummary {
    pub id: String,
    pub name: String,
    pub status: DraftStatus,
    /// The player's display name; the session holder sees it on the party board.
    pub player: String,
    pub choices: DraftChoices,
    pub rolls: Option<Rolls>,
    pub pending: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CharactersData {
    pub role: Role,
    pub campaigns: Vec<CampaignView>,
    pub mine: Vec<DraftSummary>,
    /// Empty for players.
    pub party: Vec<DraftSummary>,
    /// The world the driver reports Ready; only it can sync its catalog.
    pub ready_world: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum CharactersView {
    SignedOut,
    Denied { name: String },
    Ready(Box<CharactersData>),
}

/// One draft as its builder page shows it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuilderData {
    pub id: String,
    pub name: String,
    pub status: DraftStatus,
    /// The player may change it: their own draft, still open, campaign open.
    pub editable: bool,
    pub rules: CampaignRules,
    pub choices: DraftChoices,
    pub rolls: Option<Rolls>,
    pub pending: Vec<Pending>,
    pub dm_note: Option<String>,
    /// An enabled source is SRD 5.2 content, so the page must credit it.
    pub srd_attribution: bool,
    /// The classes the enabled sources offer.
    pub classes: Vec<ClassOption>,
    /// The chosen class's features and the decisions they open.
    pub class: Option<ClassDetails>,
    /// The species and backgrounds the enabled sources offer.
    pub species: Vec<ClassOption>,
    pub backgrounds: Vec<ClassOption>,
    /// The chosen origin roots' features and the decisions they open.
    pub species_details: Option<OriginDetails>,
    pub background_details: Option<OriginDetails>,
    /// The trait keys the draft already holds, for "already have" marks.
    pub granted: Vec<String>,
    /// Base scores plus every ability bonus, fixed and assigned.
    pub totals: Scores,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum BuilderView {
    /// No such draft, or not this caller's to see.
    Missing,
    Ready(Box<BuilderData>),
}

/// The point-buy budget the SRD uses.
pub const POINT_BUY_BUDGET: u8 = 27;

/// What one score costs on the point-buy table; `None` outside 8..=15.
pub fn point_buy_cost(score: u8) -> Option<u8> {
    Some(match score {
        8..=13 => score - 8,
        14 => 7,
        15 => 9,
        _ => return None,
    })
}

/// One die from a random byte, rejecting the biased tail (252..=255).
pub fn d6(byte: u8) -> Option<u8> {
    (byte < 252).then(|| byte % 6 + 1)
}

/// Six 4d6-drop-lowest sets; `next` yields raw bytes, rejections rolled again.
pub fn roll_sets(mut next: impl FnMut() -> u8) -> [AbilityRoll; 6] {
    let mut die = || loop {
        if let Some(value) = d6(next()) {
            return value;
        }
    };
    std::array::from_fn(|_| {
        let mut dice = [0_u8; 4];
        for value in &mut dice {
            *value = die();
        }
        // Dropping one of the tied minima is the same as subtracting the min.
        let total = dice.iter().sum::<u8>() - dice.iter().min().copied().unwrap_or(0);
        AbilityRoll { dice, total }
    })
}

/// The 5e modifier for a score.
pub fn modifier(score: u8) -> i8 {
    (i8::try_from(score).unwrap_or(i8::MAX) - 10).div_euclid(2)
}

/// The exact rule checks for one method; error codes match the server fns.
pub fn validate_scores(
    method: AbilityMethod,
    scores: &Scores,
    rolls: Option<&Rolls>,
    rules: &CampaignRules,
) -> Result<(), &'static str> {
    if !rules.ability_methods.contains(&method) {
        return Err("method_not_allowed");
    }
    let values = scores.values();
    let valid = match method {
        AbilityMethod::StandardArray => {
            let mut sorted = values;
            sorted.sort_unstable();
            sorted == [8, 10, 12, 13, 14, 15]
        }
        AbilityMethod::PointBuy => {
            values.iter().all(|value| (8..=15).contains(value))
                && values
                    .iter()
                    .filter_map(|value| point_buy_cost(*value))
                    .sum::<u8>()
                    <= POINT_BUY_BUDGET
        }
        AbilityMethod::Roll => rolls.is_some_and(|rolls| {
            let mut have = values;
            have.sort_unstable();
            let mut want = rolls.sets.map(|set| set.total);
            want.sort_unstable();
            have == want
        }),
        AbilityMethod::Manual => values.iter().all(|value| (3..=18).contains(value)),
    };
    if valid { Ok(()) } else { Err("invalid_scores") }
}

/// The open steps of one draft, in the order the player should visit them.
/// `catalog` is what tells a chosen root the enabled sources still offer; a
/// caller without one cannot see a root fall out of the sources.
pub fn pending(
    catalog: Option<&Catalog>,
    choices: &DraftChoices,
    name: &str,
    rolls: Option<&Rolls>,
    decisions: &[Decision],
) -> Vec<Pending> {
    let mut pending = Vec::new();
    if name.trim().is_empty() {
        pending.push(Pending {
            step: Step::Details,
            key: "name".into(),
            label: "Name your character".into(),
        });
    }
    let offered = match (choices.class.as_deref(), catalog) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(uuid), Some(catalog)) => catalog
            .get(uuid)
            .is_some_and(|item| item.item_type == Slot::Class.item_type()),
    };
    if !offered {
        pending.push(Pending {
            step: Step::Class,
            key: "class".into(),
            label: "Choose a class".into(),
        });
    }
    for (slot, key, label) in [
        (Slot::Species, "species", "Choose a species"),
        (Slot::Background, "background", "Choose a background"),
    ] {
        let offered = match (slot.chosen(choices), catalog) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(uuid), Some(catalog)) => catalog
                .get(uuid)
                .is_some_and(|item| item.item_type == slot.item_type()),
        };
        if !offered {
            pending.push(Pending {
                step: Step::Origin,
                key: key.into(),
                label: label.into(),
            });
        }
    }
    let abilities = match (choices.method, choices.base) {
        (None, _) => Some(("method", "Choose an ability method")),
        (Some(AbilityMethod::Roll), _) if rolls.is_none() => Some(("roll", "Roll your abilities")),
        (Some(_), None) => Some(("abilities", "Assign your ability scores")),
        (Some(_), Some(_)) => None,
    };
    if let Some((key, label)) = abilities {
        pending.push(Pending {
            step: Step::Abilities,
            key: key.into(),
            label: label.into(),
        });
    }
    for decision in decisions {
        let values: &[String] = choices.picks.get(&decision.key).map_or(&[], Vec::as_slice);
        let answered = match &decision.kind {
            DecisionKind::Asi { .. } => {
                let empty = BTreeMap::new();
                let deltas = choices.asi.get(&decision.key).unwrap_or(&empty);
                let context = totals(decisions, choices, Some(&decision.key));
                validate_asi(decision, deltas, &context).is_ok()
            }
            _ => is_answered(decision, values),
        };
        if !answered {
            pending.push(Pending {
                step: decision.step,
                key: decision.key.clone(),
                label: decision_label(decision),
            });
        }
    }
    pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    const CLERIC: &str = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
    const DIVINE_ORDER: &str = "Compendium.dnd5e.classes24.Item.phbclcDivineOrde";
    const ACOLYTE: &str = "Compendium.dnd5e.origins24.Item.phbbgAcolyte0000";
    const HUMAN: &str = "Compendium.dnd5e.origins24.Item.phbspHuman000000";
    const VERSATILE: &str = "Compendium.dnd5e.origins24.Item.phbsptVersatile0";
    const MAGIC_INITIATE: &str = "Compendium.dnd5e.feats24.Item.phbftMagicInitia";
    const SKILLED: &str = "Compendium.dnd5e.feats24.Item.phbftSkilled0000";
    const PROTECTOR: &str = "Compendium.dnd5e.classes24.Item.phbDivineOrderPr";
    const THAUMATURGE: &str = "Compendium.dnd5e.classes24.Item.phbDivineOrderTh";
    const BARD: &str = "Compendium.dnd5e.classes24.Item.phbbrdBard000000";
    const ROGUE: &str = "Compendium.dnd5e.classes24.Item.phbrgeRogue00000";
    const WARLOCK: &str = "Compendium.dnd5e.classes24.Item.phbwlkWarlock000";
    const PACT_OF_THE_BLADE: &str = "Compendium.dnd5e.classes24.Item.phbinvPactBlade0";
    const THIRSTING_BLADE: &str = "Compendium.dnd5e.classes24.Item.phbinvThirstingB";

    /// The transcribed packs as one campaign's enabled sources.
    fn catalog() -> Catalog {
        let mut catalog = Catalog::default();
        for (id, label, file) in [
            (
                "dnd5e.classes24",
                "Character Classes",
                include_str!("../tests/fixtures/catalog/classes24.json"),
            ),
            (
                "dnd5e.origins24",
                "Character Origins",
                include_str!("../tests/fixtures/catalog/origins24.json"),
            ),
            (
                "dnd5e.feats24",
                "Feats",
                include_str!("../tests/fixtures/catalog/feats24.json"),
            ),
        ] {
            let items: Vec<CatalogItem> = serde_json::from_str(file).expect("fixture parses");
            catalog.packs.push(PackInfo {
                id: id.into(),
                label: label.into(),
                source_book: Some("SRD 5.2".into()),
                rules_version: Some("modern".into()),
                count: items.len() as u32,
            });
            catalog.items.extend(items);
        }
        catalog
    }

    fn with_class(class: &str) -> DraftChoices {
        DraftChoices {
            class: Some(class.into()),
            ..DraftChoices::default()
        }
    }

    fn decision<'a>(decisions: &'a [Decision], title: &str) -> &'a Decision {
        decisions
            .iter()
            .find(|decision| decision.title == title)
            .unwrap_or_else(|| panic!("{title} in {decisions:?}"))
    }

    fn scores(str: u8, dex: u8, con: u8, int: u8, wis: u8, cha: u8) -> Scores {
        Scores {
            str,
            dex,
            con,
            int,
            wis,
            cha,
        }
    }

    fn rolled(totals: [u8; 6]) -> Rolls {
        Rolls {
            rolled_at: "2026-11-15T10:00:00+00:00".into(),
            sets: totals.map(|total| AbilityRoll {
                dice: [6, 6, 6, 1],
                total,
            }),
        }
    }

    #[test]
    fn point_buy_costs_match_srd() {
        let table = [
            (8, 0),
            (9, 1),
            (10, 2),
            (11, 3),
            (12, 4),
            (13, 5),
            (14, 7),
            (15, 9),
        ];
        for (score, cost) in table {
            assert_eq!(point_buy_cost(score), Some(cost), "{score}");
        }
        assert_eq!(point_buy_cost(16), None);
        assert_eq!(point_buy_cost(7), None);
    }

    #[test]
    fn scores_validate_per_method() {
        let rules = CampaignRules::default();
        let array = scores(15, 14, 13, 12, 10, 8);
        assert_eq!(
            validate_scores(AbilityMethod::StandardArray, &array, None, &rules),
            Ok(())
        );
        assert_eq!(
            validate_scores(
                AbilityMethod::StandardArray,
                &scores(15, 15, 13, 12, 10, 8),
                None,
                &rules
            ),
            Err("invalid_scores")
        );

        assert_eq!(
            validate_scores(
                AbilityMethod::PointBuy,
                &scores(15, 15, 15, 8, 8, 8),
                None,
                &rules
            ),
            Ok(())
        );
        assert_eq!(
            validate_scores(
                AbilityMethod::PointBuy,
                &scores(15, 15, 15, 9, 8, 8),
                None,
                &rules
            ),
            Err("invalid_scores")
        );

        let rolls = rolled([15, 14, 13, 12, 10, 8]);
        let async_ = scores(14, 15, 10, 13, 12, 8);
        assert_eq!(
            validate_scores(AbilityMethod::Roll, &async_, Some(&rolls), &rules),
            Ok(())
        );
        assert_eq!(
            validate_scores(AbilityMethod::Roll, &async_, None, &rules),
            Err("invalid_scores")
        );
        assert_eq!(
            validate_scores(
                AbilityMethod::Roll,
                &scores(14, 15, 10, 13, 12, 9),
                Some(&rolls),
                &rules
            ),
            Err("invalid_scores")
        );

        assert_eq!(
            validate_scores(
                AbilityMethod::Manual,
                &scores(18, 3, 10, 10, 10, 10),
                None,
                &rules
            ),
            Err("method_not_allowed")
        );
    }

    #[test]
    fn roll_sets_drop_the_lowest_die() {
        // Bytes 0..=5 are die faces 1..=6; keep the stream on that band.
        let mut stream = [3, 2, 0, 5].into_iter().cycle();
        let sets = roll_sets(move || stream.next().unwrap());
        for set in sets {
            assert_eq!(set.dice, [4, 3, 1, 6]);
            assert_eq!(set.total, 13);
        }

        // 252 is rejected and drawn again, so the first set is four 5s.
        let mut stream = [252, 4, 4, 4, 4]
            .into_iter()
            .chain([5, 5, 5, 5].into_iter().cycle());
        let sets = roll_sets(move || stream.next().unwrap());
        assert_eq!(sets[0].dice, [5, 5, 5, 5]);
        assert_eq!(sets[0].total, 15);
        assert_eq!(sets[1].dice, [6, 6, 6, 6]);
    }

    #[test]
    fn d6_rejects_biased_bytes() {
        assert_eq!(d6(0), Some(1));
        assert_eq!(d6(5), Some(6));
        assert_eq!(d6(251), Some(6));
        assert_eq!(d6(252), None);
        assert_eq!(d6(255), None);
    }

    #[test]
    fn catalog_fixture_parses() {
        // C3 replaces these with a live export; until then they are transcribed
        // from the SRD 5.2 pack sources (see the fixtures' README).
        let classes: Vec<CatalogItem> =
            serde_json::from_str(include_str!("../tests/fixtures/catalog/classes24.json"))
                .expect("classes24 fixture parses");
        let origins: Vec<CatalogItem> =
            serde_json::from_str(include_str!("../tests/fixtures/catalog/origins24.json"))
                .expect("origins24 fixture parses");
        assert_eq!(
            origins
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Acolyte", "Human", "Versatile"]
        );

        let cleric = &classes[0];
        assert_eq!(cleric.name, "Cleric");
        assert_eq!(cleric.item_type, "class");
        assert_eq!(
            cleric.uuid,
            "Compendium.dnd5e.classes24.Item.phbclcCleric0000"
        );
        // The module emits plain text and strips the description from `system`.
        assert!(!cleric.text.is_empty() && cleric.text.chars().count() <= 4000);
        assert!(!cleric.text.contains('<') && !cleric.text.contains('@'));
        assert!(cleric.system.get("description").is_none());
        assert_eq!(cleric.system["identifier"], "cleric");

        let skills = &cleric.traits["2wUNkQzS4vL9nR6D"];
        assert_eq!(skills["choices"][0]["count"], 2);
        assert_eq!(skills["choices"][0]["options"][1]["label"], "Insight");
        assert_eq!(skills["grants"].as_array().map(Vec::len), Some(0));
        let armor = &cleric.traits["Z9K5kHYMLvroIMpT"];
        assert_eq!(armor["grants"][2]["key"], "armor:med");

        // A wildcard pool arrives expanded, so the builder never resolves one.
        let acolyte = &origins[0];
        let languages = &acolyte.traits["McRHkSL3WnG2Po7g"];
        let options = languages["choices"][0]["options"]
            .as_array()
            .expect("options");
        assert_eq!(options.len(), 10);
        assert!(
            options
                .iter()
                .all(|option| !option["key"].as_str().unwrap_or_default().ends_with('*'))
        );

        // A species with no Trait advancement carries an empty map.
        assert_eq!(origins[1].traits, serde_json::json!({}));
        assert_eq!(
            origins[1].system["advancement"].as_object().map(Map::len),
            Some(2)
        );

        // The origin feat the Human's Versatile pool can offer.
        let feats: Vec<CatalogItem> =
            serde_json::from_str(include_str!("../tests/fixtures/catalog/feats24.json"))
                .expect("feats24 fixture parses");
        assert_eq!(feats[0].name, "Magic Initiate");
        assert_eq!(feats[0].system["type"]["subtype"], "origin");
    }

    #[test]
    fn pending_lists_name_abilities_and_class() {
        let catalog = catalog();
        let empty = pending(Some(&catalog), &DraftChoices::default(), "  ", None, &[]);
        assert_eq!(empty.len(), 5, "{empty:?}");
        assert_eq!(empty[0].step, Step::Details);
        assert_eq!(empty[0].key, "name");
        assert_eq!(empty[1].step, Step::Class);
        assert_eq!(empty[1].key, "class");
        assert_eq!(empty[2].step, Step::Origin);
        assert_eq!(empty[2].key, "species");
        assert_eq!(empty[3].key, "background");
        assert_eq!(empty[4].step, Step::Abilities);
        assert_eq!(empty[4].key, "method");

        let rolling = pending(
            Some(&catalog),
            &DraftChoices {
                method: Some(AbilityMethod::Roll),
                base: None,
                ..DraftChoices::default()
            },
            "Al",
            None,
            &[],
        );
        assert_eq!(rolling.len(), 4, "{rolling:?}");
        assert_eq!(rolling[3].key, "roll");

        let done = pending(
            Some(&catalog),
            &DraftChoices {
                method: Some(AbilityMethod::StandardArray),
                base: Some(scores(15, 14, 13, 12, 10, 8)),
                class: Some(CLERIC.into()),
                species: Some(HUMAN.into()),
                background: Some(ACOLYTE.into()),
                ..DraftChoices::default()
            },
            "Al",
            None,
            &[],
        );
        assert!(done.is_empty(), "{done:?}");
        assert_eq!(modifier(8), -1);
        assert_eq!(modifier(15), 2);

        // A class the enabled sources no longer offer opens the step again,
        // instead of losing every decision it made in silence.
        let stale = DraftChoices {
            method: Some(AbilityMethod::StandardArray),
            base: Some(scores(15, 14, 13, 12, 10, 8)),
            class: Some("Compendium.dnd5e.classes24.Item.phbclcGone0000".into()),
            ..DraftChoices::default()
        };
        let lost = pending(Some(&catalog), &stale, "Al", None, &[]);
        assert_eq!(lost.len(), 3, "{lost:?}");
        assert!(
            lost.iter()
                .any(|item| item.step == Step::Class && item.label == "Choose a class")
        );

        // A class's open decisions join the same list, on the class step.
        let classed = DraftChoices {
            method: Some(AbilityMethod::StandardArray),
            base: Some(scores(15, 14, 13, 12, 10, 8)),
            class: Some(CLERIC.into()),
            ..DraftChoices::default()
        };
        let to_do = pending(
            Some(&catalog),
            &classed,
            "Al",
            None,
            &decisions(&catalog, &classed, 1),
        );
        assert_eq!(
            to_do.iter().filter(|item| item.step == Step::Class).count(),
            2,
            "{to_do:?}"
        );
        assert!(to_do.iter().any(|item| item.key == "species"));
        assert!(to_do.iter().any(|item| item.key == "background"));
        assert!(to_do.iter().any(|item| item.label == "Divine Order Choice"));
        assert!(to_do.iter().any(|item| item.label == "Skill Proficiencies"));
    }

    #[test]
    fn cleric_level_one_decisions() {
        let catalog = catalog();
        let decisions = decisions(&catalog, &with_class(CLERIC), 1);

        // The class's own skill pool, two of the five.
        let skills = decision(&decisions, "Skill Proficiencies");
        assert_eq!(skills.owner_name, "Cleric");
        assert_eq!(skills.key, format!("{CLERIC}#2wUNkQzS4vL9nR6D#1"));
        let DecisionKind::Trait { choices, .. } = &skills.kind else {
            panic!("{skills:?}")
        };
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].count, 2);
        assert_eq!(
            choices[0]
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            ["History", "Insight", "Medicine", "Perception", "Religion"]
        );

        // Divine Order's choice arrives through the class's ItemGrant.
        let order = decision(&decisions, "Divine Order Choice");
        assert_eq!(order.owner_name, "Divine Order");
        assert_eq!(
            order.key,
            "Compendium.dnd5e.classes24.Item.phbclcDivineOrde#V8GfnoZYTe1PLXPI#1"
        );
        let DecisionKind::Items { count, options } = &order.kind else {
            panic!("{order:?}")
        };
        assert_eq!(*count, 1);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            [PROTECTOR, THAUMATURGE]
        );

        // A Trait that only grants is no choice, but its keys still count as had.
        let armor = decision(&decisions, "Armor Training");
        assert!(armor.is_automatic());
        assert!(granted_traits(&decisions, &with_class(CLERIC)).contains("armor:med"));
    }

    #[test]
    fn restriction_pool_lists_origin_feats_and_drops_missing_uuids() {
        let catalog = catalog();
        let choices = DraftChoices {
            species: Some(HUMAN.into()),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &choices, 1);

        // Versatile's pool names a package the world lacks, so only the
        // restriction's own match survives: the origin feat in the catalog.
        let versatile = decision(&decisions, "Versatile");
        let DecisionKind::Items { count, options } = &versatile.kind else {
            panic!("{versatile:?}")
        };
        assert_eq!(*count, 1);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            [MAGIC_INITIATE, SKILLED]
        );

        // Human picks one of two sizes.
        let size = decision(&decisions, "Size");
        let DecisionKind::Size { options } = &size.kind else {
            panic!("{size:?}")
        };
        assert_eq!(options, &["sm".to_owned(), "med".to_owned()]);
    }

    #[test]
    fn acolyte_asi_allows_two_one_or_three_ones() {
        let catalog = catalog();
        let choices = DraftChoices {
            background: Some(ACOLYTE.into()),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &choices, 1);
        let asi = decision(&decisions, "Background Ability Score Improvement");
        let DecisionKind::Asi {
            points, cap, open, ..
        } = &asi.kind
        else {
            panic!("{asi:?}")
        };
        // Its configuration: three points, two on one ability, locked to the
        // background's Intelligence, Wisdom and Charisma.
        assert_eq!((*points, *cap), (3, 2));
        assert_eq!(
            open,
            &["int".to_owned(), "wis".to_owned(), "cha".to_owned()]
        );

        let base = scores(15, 14, 13, 12, 10, 8);
        let two_one = BTreeMap::from([("int".to_owned(), 2), ("wis".to_owned(), 1)]);
        let three_ones = BTreeMap::from([
            ("int".to_owned(), 1),
            ("wis".to_owned(), 1),
            ("cha".to_owned(), 1),
        ]);
        assert_eq!(validate_asi(asi, &two_one, &base), Ok(()));
        assert_eq!(validate_asi(asi, &three_ones, &base), Ok(()));
        // Every point is spent, and none of them twice on one ability.
        assert_eq!(
            validate_asi(asi, &BTreeMap::from([("int".to_owned(), 2)]), &base),
            Err("asi_points")
        );
        assert_eq!(
            validate_asi(asi, &BTreeMap::from([("int".to_owned(), 3)]), &base),
            Err("asi_cap")
        );
        assert_eq!(
            validate_asi(asi, &BTreeMap::new(), &base),
            Err("asi_points")
        );

        // The answer shows as the total and leaves with its background.
        let mut answered = choices;
        answered.base = Some(base);
        let found = super::decisions(&catalog, &answered, 1);
        // The bonus stays on the to do list until every point is assigned.
        let before = pending(Some(&catalog), &answered, "Al", None, &found);
        assert!(before.iter().any(|item| item.key == asi.key), "{before:?}");
        answered.asi.insert(asi.key.clone(), two_one);
        let found = super::decisions(&catalog, &answered, 1);
        let after = pending(Some(&catalog), &answered, "Al", None, &found);
        assert!(!after.iter().any(|item| item.key == asi.key), "{after:?}");
        let totals = totals(&found, &answered, None);
        assert_eq!((totals.int, totals.wis, totals.dex), (14, 11, 14));
        answered.select(Slot::Background, None);
        prune_picks(&catalog, &mut answered, 1);
        assert!(answered.asi.is_empty(), "{:?}", answered.asi);
    }

    #[test]
    fn asi_rejects_locked_abilities_and_totals_above_twenty() {
        let catalog = catalog();
        let choices = DraftChoices {
            background: Some(ACOLYTE.into()),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &choices, 1);
        let asi = decision(&decisions, "Background Ability Score Improvement");

        // Strength is outside the background's three.
        let base = scores(10, 10, 10, 10, 10, 10);
        assert_eq!(
            validate_asi(
                asi,
                &BTreeMap::from([("str".to_owned(), 2), ("int".to_owned(), 1)]),
                &base
            ),
            Err("asi_locked")
        );

        // 19 + 2 is 21; 18 + 2 is the ceiling of 20.
        let two_one = BTreeMap::from([("int".to_owned(), 2), ("wis".to_owned(), 1)]);
        assert_eq!(
            validate_asi(asi, &two_one, &scores(10, 10, 10, 19, 10, 10)),
            Err("asi_max")
        );
        assert_eq!(
            validate_asi(asi, &two_one, &scores(10, 10, 10, 18, 10, 10)),
            Ok(())
        );

        // A configuration's fixed bonus is pre-assigned: it counts toward the
        // ceiling but never the player's points, and `max` raises the ceiling.
        let mut fixed = asi.clone();
        fixed.kind = DecisionKind::Asi {
            points: 3,
            cap: 2,
            open: ["int", "wis", "cha"].map(str::to_owned).to_vec(),
            fixed: BTreeMap::from([("wis".to_owned(), 1)]),
            max: None,
        };
        assert_eq!(
            validate_asi(&fixed, &two_one, &scores(10, 10, 10, 10, 19, 10)),
            Err("asi_max")
        );
        // Totals carry every decision's fixed bonus, and each decision is judged
        // against the base plus every other bonus: two +2s on a 18 cannot both pass.
        let mut first = asi.clone();
        first.kind = DecisionKind::Asi {
            points: 2,
            cap: 2,
            open: vec!["int".to_owned()],
            fixed: BTreeMap::from([("wis".to_owned(), 1)]),
            max: None,
        };
        let mut second = first.clone();
        second.key = format!("{}-second", first.key);
        let both = [first.clone(), second.clone()];
        let plus_two = BTreeMap::from([("int".to_owned(), 2)]);
        let mut draft = DraftChoices {
            base: Some(scores(10, 10, 10, 16, 10, 10)),
            ..DraftChoices::default()
        };
        draft.asi.insert(first.key.clone(), plus_two.clone());
        let all = totals(&both, &draft, None);
        assert_eq!((all.int, all.wis), (18, 12));
        assert_eq!(
            validate_asi(
                &second,
                &plus_two,
                &totals(&both, &draft, Some(&second.key))
            ),
            Ok(())
        );
        draft.base = Some(scores(10, 10, 10, 18, 10, 10));
        assert_eq!(
            validate_asi(
                &second,
                &plus_two,
                &totals(&both, &draft, Some(&second.key))
            ),
            Err("asi_max")
        );
        fixed.kind = DecisionKind::Asi {
            points: 3,
            cap: 2,
            open: ["int", "wis", "cha"].map(str::to_owned).to_vec(),
            fixed: BTreeMap::new(),
            max: Some(22),
        };
        assert_eq!(
            validate_asi(&fixed, &two_one, &scores(10, 10, 10, 20, 10, 10)),
            Ok(())
        );
    }

    #[test]
    fn human_needs_size_and_an_origin_feat() {
        let catalog = catalog();
        let human = DraftChoices {
            method: Some(AbilityMethod::StandardArray),
            base: Some(scores(15, 14, 13, 12, 10, 8)),
            class: Some(CLERIC.into()),
            species: Some(HUMAN.into()),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &human, 1);
        let to_do = pending(Some(&catalog), &human, "Al", None, &decisions);
        // The species fills its slot; its own two choices stay open, and the
        // background is still unpicked.
        assert!(!to_do.iter().any(|item| item.key == "species"), "{to_do:?}");
        assert!(to_do.iter().any(|item| item.key == "background"));
        assert!(
            to_do
                .iter()
                .any(|item| item.key == decision(&decisions, "Size").key
                    && item.step == Step::Origin),
            "{to_do:?}"
        );
        assert!(
            to_do
                .iter()
                .any(|item| item.label == "Versatile" && item.step == Step::Origin),
            "{to_do:?}"
        );

        // Answering both closes them.
        let size = decision(&decisions, "Size");
        let versatile = decision(&decisions, "Versatile");
        let mut answered = human;
        answered
            .picks
            .insert(size.key.clone(), vec!["med".to_owned()]);
        answered
            .picks
            .insert(versatile.key.clone(), vec![SKILLED.to_owned()]);
        let found = super::decisions(&catalog, &answered, 1);
        let to_do = pending(Some(&catalog), &answered, "Al", None, &found);
        assert!(
            !to_do
                .iter()
                .any(|item| item.label == "Size" || item.label == "Versatile"),
            "{to_do:?}"
        );
    }

    #[test]
    fn chosen_feat_decisions_appear_and_vanish_with_the_pick() {
        let catalog = catalog();
        let versatile = format!("{VERSATILE}#KB8IQLwyuL6SOFnv#0");
        let skilled = format!("{SKILLED}#Mm4MAeZK89zGqmyH#0");
        let mut choices = DraftChoices {
            species: Some(HUMAN.into()),
            picks: BTreeMap::from([(versatile.clone(), vec![SKILLED.to_owned()])]),
            ..DraftChoices::default()
        };

        // Skilled's own trait choice arrives through the Versatile pick.
        let found = decisions(&catalog, &choices, 1);
        let feat = found
            .iter()
            .find(|decision| decision.key == skilled)
            .unwrap_or_else(|| panic!("{found:?}"));
        assert_eq!(feat.owner_name, "Skilled");
        let DecisionKind::Trait { choices: pools, .. } = &feat.kind else {
            panic!("{feat:?}")
        };
        assert_eq!(pools[0].count, 3);
        assert_eq!(
            validate_pick(
                feat,
                &[
                    "skills:ins".to_owned(),
                    "skills:per".to_owned(),
                    "skills:ste".to_owned()
                ]
            ),
            Ok(())
        );

        // Another feat takes them away with it.
        choices
            .picks
            .insert(versatile, vec![MAGIC_INITIATE.to_owned()]);
        prune_picks(&catalog, &mut choices, 1);
        assert!(
            decisions(&catalog, &choices, 1)
                .iter()
                .all(|decision| decision.key != skilled)
        );
    }

    #[test]
    fn validate_pick_enforces_count_and_pool() {
        let catalog = catalog();
        let decisions = decisions(&catalog, &with_class(CLERIC), 1);
        let skills = decision(&decisions, "Skill Proficiencies");
        let picked = ["skills:ins".to_owned(), "skills:rel".to_owned()];
        assert_eq!(validate_pick(skills, &picked), Ok(()));
        assert!(is_answered(skills, &picked));
        // One of two is storable while the player edits, but not answered.
        assert_eq!(validate_pick(skills, &picked[..1]), Ok(()));
        assert!(!is_answered(skills, &picked[..1]));
        // Three of two never is.
        assert_eq!(
            validate_pick(
                skills,
                &[
                    "skills:ins".to_owned(),
                    "skills:rel".to_owned(),
                    "skills:his".to_owned()
                ]
            ),
            Err("pick_count")
        );
        assert_eq!(
            validate_pick(skills, &["skills:ins".to_owned(), "armor:med".to_owned()]),
            Err("pick_not_offered")
        );
        assert_eq!(
            validate_pick(skills, &["skills:ins".to_owned(), "skills:ins".to_owned()]),
            Err("pick_duplicate")
        );

        let order = decision(&decisions, "Divine Order Choice");
        assert_eq!(validate_pick(order, &[]), Ok(()));
        assert!(!is_answered(order, &[]));
        assert_eq!(
            validate_pick(order, &[PROTECTOR.to_owned(), THAUMATURGE.to_owned()]),
            Err("pick_count")
        );
        assert_eq!(validate_pick(order, &[PROTECTOR.to_owned()]), Ok(()));
        assert!(is_answered(order, &[PROTECTOR.to_owned()]));
        assert_eq!(
            validate_pick(order, &[MAGIC_INITIATE.to_owned()]),
            Err("pick_not_offered")
        );

        // A grants-only Trait offers nothing to pick, so it is always answered.
        let armor = decision(&decisions, "Armor Training");
        assert_eq!(validate_pick(armor, &[]), Ok(()));
        assert!(is_answered(armor, &[]));
        assert_eq!(
            validate_pick(armor, &["armor:hvy".to_owned()]),
            Err("pick_not_offered")
        );
    }

    #[test]
    fn changing_class_clears_its_picks() {
        let catalog = catalog();
        let key = format!("{CLERIC}#2wUNkQzS4vL9nR6D#1");
        let mut choices = DraftChoices {
            class: Some(CLERIC.into()),
            picks: BTreeMap::from([(key.clone(), vec!["skills:ins".to_owned()])]),
            ..DraftChoices::default()
        };

        // Another class takes its decisions with it.
        let other = "Compendium.other.classes.Item.phbclcWizard000";
        choices.select(Slot::Class, Some(other.into()));
        prune_picks(&catalog, &mut choices, 1);
        assert_eq!(choices.class.as_deref(), Some(other));
        assert!(choices.picks.is_empty());

        // Re-selecting the class that is already chosen keeps the picks.
        choices.select(Slot::Class, Some(CLERIC.into()));
        choices.picks.insert(key, vec!["skills:ins".to_owned()]);
        choices.select(Slot::Class, Some(CLERIC.into()));
        prune_picks(&catalog, &mut choices, 1);
        assert_eq!(choices.picks.len(), 1);
    }

    #[test]
    fn changing_class_drops_nested_picks() {
        let catalog = catalog();
        let skills = format!("{CLERIC}#2wUNkQzS4vL9nR6D#1");
        let order = format!("{DIVINE_ORDER}#V8GfnoZYTe1PLXPI#1");
        let mut choices = DraftChoices {
            class: Some(CLERIC.into()),
            picks: BTreeMap::from([
                (
                    skills.clone(),
                    vec!["skills:ins".to_owned(), "skills:rel".to_owned()],
                ),
                (order.clone(), vec![PROTECTOR.to_owned()]),
            ]),
            ..DraftChoices::default()
        };
        // Divine Order's key carries the granted feature, not the class, so a
        // prefix rule on the slot's uuid would keep it.
        assert!(order.starts_with(DIVINE_ORDER) && !order.starts_with(CLERIC));

        choices.select(Slot::Class, Some(BARD.into()));
        prune_picks(&catalog, &mut choices, 1);
        assert!(choices.picks.is_empty(), "{:?}", choices.picks);

        // Coming back does not bring the old answer along.
        choices.select(Slot::Class, Some(CLERIC.into()));
        prune_picks(&catalog, &mut choices, 1);
        assert!(choices.picks.is_empty(), "{:?}", choices.picks);
        assert!(!is_answered(
            decision(&decisions(&catalog, &choices, 1), "Divine Order Choice"),
            &[]
        ));
    }

    #[test]
    fn secondary_class_advancements_are_skipped() {
        let mut catalog = catalog();
        let cleric = catalog
            .items
            .iter_mut()
            .find(|item| item.uuid == CLERIC)
            .expect("cleric");
        // The same advancement as a multiclass dip's: not this draft's.
        cleric.system["advancement"]["2wUNkQzS4vL9nR6D"]["classRestriction"] =
            serde_json::json!("secondary");
        let decisions = decisions(&catalog, &with_class(CLERIC), 1);
        assert!(
            decisions
                .iter()
                .all(|decision| decision.key != format!("{CLERIC}#2wUNkQzS4vL9nR6D#1")),
            "{decisions:?}"
        );
        // The primary-class advancements are untouched.
        assert!(
            decisions
                .iter()
                .any(|decision| decision.title == "Armor Training")
        );
    }

    #[test]
    fn expertise_offers_only_proficient_traits() {
        let catalog = catalog();
        // A level-2 Bard is proficient in the three skills it picked at level 1.
        let skills = format!("{BARD}#NGtmmsYBnIAGKUSn#1");
        let choices = DraftChoices {
            class: Some(BARD.into()),
            picks: BTreeMap::from([(
                skills,
                ["skills:ins", "skills:prf", "skills:per"]
                    .map(str::to_owned)
                    .to_vec(),
            )]),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &choices, 2);
        let expertise = decision(&decisions, "Expertise");
        let DecisionKind::Trait {
            mode,
            choices: pools,
            ..
        } = &expertise.kind
        else {
            panic!("{expertise:?}")
        };
        assert_eq!(*mode, TraitMode::Expertise);
        assert_eq!(pools[0].count, 2);
        assert_eq!(
            pools[0]
                .options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            ["skills:ins", "skills:per", "skills:prf"]
        );
        // An untrained skill is neither offered nor storable.
        assert_eq!(
            validate_pick(expertise, &["skills:arc".to_owned()]),
            Err("pick_not_offered")
        );
        assert_eq!(
            validate_pick(
                expertise,
                &["skills:ins".to_owned(), "skills:prf".to_owned()]
            ),
            Ok(())
        );

        // The other modes' offer rules, against the same two sets.
        let proficient: BTreeSet<String> = ["skills:ins", "weapon:sim"].map(str::to_owned).into();
        let expert: BTreeSet<String> = BTreeSet::from(["skills:ins".to_owned()]);
        let option = |value: &str| Opt {
            value: value.into(),
            label: String::new(),
        };
        let offers =
            |mode: TraitMode, value: &str| mode.offers(&option(value), &proficient, &expert);
        // Expertise builds on proficiency and stops at it.
        assert!(!offers(TraitMode::Expertise, "skills:arc"));
        assert!(!offers(TraitMode::Expertise, "skills:ins"));
        // Forced expertise and upgrade skip what is already expert.
        assert!(offers(TraitMode::ForcedExpertise, "skills:arc"));
        assert!(!offers(TraitMode::ForcedExpertise, "skills:ins"));
        assert!(offers(TraitMode::Upgrade, "skills:arc"));
        assert!(!offers(TraitMode::Upgrade, "skills:ins"));
        // Mastery reads the weapon catalogue key and its category.
        assert!(offers(TraitMode::Mastery, "weapon:sim:dagger"));
        assert!(!offers(TraitMode::Mastery, "weapon:mar:longsword"));
    }

    #[test]
    fn class_step_expertise_sees_background_skills() {
        // An Acolyte's Insight and Religion come from the background (level 0),
        // so the class step's Expertise must offer them, as `decisions` does.
        let catalog = catalog();
        let choices = DraftChoices {
            class: Some(ROGUE.into()),
            background: Some("Compendium.dnd5e.origins24.Item.phbbgAcolyte0000".into()),
            ..DraftChoices::default()
        };
        let details = class_details(&catalog, &choices, 1).expect("rogue");
        let expertise = decision(&details.decisions, "Expertise");
        let DecisionKind::Trait { choices: pools, .. } = &expertise.kind else {
            panic!("{expertise:?}")
        };
        let offered: Vec<&str> = pools[0].options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(offered, ["skills:ins", "skills:rel"]);
        assert!(details.decisions.iter().all(|d| d.step == Step::Class));
        assert_eq!(
            details.decisions,
            decisions(&catalog, &choices, 1)
                .into_iter()
                .filter(|d| d.step == Step::Class)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rogue_level_one_expertise_and_mastery_see_its_proficiencies() {
        let catalog = catalog();
        // A level-1 Rogue picks four of its ten skill proficiencies. Its item
        // grant carries Expertise and Weapon Mastery, whose offers depend on
        // what the class's own traits and those picks made it proficient with.
        let skills = format!("{ROGUE}#xE00qAIjMdw63Kja#1");
        let choices = DraftChoices {
            class: Some(ROGUE.into()),
            picks: BTreeMap::from([(
                skills,
                ["skills:ins", "skills:per", "skills:slt", "skills:ste"]
                    .map(str::to_owned)
                    .to_vec(),
            )]),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &choices, 1);

        // dnd5e applies the class's own steps before the features it grants:
        // saving throws, skills, armor, weapons and tools, then the grant.
        assert_eq!(
            decisions
                .iter()
                .map(|decision| decision.title.as_str())
                .collect::<Vec<_>>(),
            [
                "Saving Throw Proficiencies",
                "Skill Proficiencies",
                "Armor Training",
                "Weapon Proficiencies",
                "Tool Proficiencies",
                "Expertise",
                "Weapon Mastery",
            ]
        );

        // Expertise offers exactly the skills the draft is proficient with.
        let expertise = decision(&decisions, "Expertise");
        let DecisionKind::Trait {
            mode,
            choices: pools,
            ..
        } = &expertise.kind
        else {
            panic!("{expertise:?}")
        };
        assert_eq!(*mode, TraitMode::Expertise);
        assert_eq!(pools[0].count, 2);
        assert_eq!(
            pools[0]
                .options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            ["skills:ins", "skills:per", "skills:slt", "skills:ste"]
        );

        // Weapon Mastery offers the weapons the Rogue is proficient with: all
        // simple weapons by category, plus the martial ones it names.
        let mastery = decision(&decisions, "Weapon Mastery");
        let DecisionKind::Trait {
            mode,
            choices: pools,
            ..
        } = &mastery.kind
        else {
            panic!("{mastery:?}")
        };
        assert_eq!(*mode, TraitMode::Mastery);
        assert_eq!(pools[0].count, 2);
        let named = [
            "weapon:mar:handcrossbow",
            "weapon:mar:rapier",
            "weapon:mar:scimitar",
            "weapon:mar:shortsword",
            "weapon:mar:whip",
        ];
        let offered: Vec<&str> = pools[0]
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect();
        assert_eq!(offered.len(), 19, "{offered:?}");
        assert!(
            offered
                .iter()
                .all(|value| value.starts_with("weapon:sim:") || named.contains(value)),
            "{offered:?}"
        );
        assert!(!offered.contains(&"weapon:mar:longsword"));
        assert_eq!(
            validate_pick(mastery, &["weapon:mar:longsword".to_owned()]),
            Err("pick_not_offered")
        );
    }

    #[test]
    fn feat_prerequisites_gate_item_choice_offers() {
        let catalog = catalog();
        // Level 1 offers only the invocations it can meet; a level-5 one waits.
        let decisions = decisions(&catalog, &with_class(WARLOCK), 1);
        let invocation = decision(&decisions, "Choose Eldritch Invocation");
        let DecisionKind::Items { count, options } = &invocation.kind else {
            panic!("{invocation:?}")
        };
        assert_eq!(*count, 1);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            [PACT_OF_THE_BLADE]
        );
        assert_eq!(
            validate_pick(invocation, &[THIRSTING_BLADE.to_owned()]),
            Err("pick_not_offered")
        );

        // Thirsting Blade's own level and item prerequisites, both real.
        let thirsting = catalog.get(THIRSTING_BLADE).expect("thirsting blade");
        let mut held = BTreeSet::from(["feat:pact-of-the-blade".to_owned()]);
        assert!(!prerequisite_met(thirsting, 4, &held));
        assert!(prerequisite_met(thirsting, 5, &held));
        held.clear();
        held.insert("pact-of-the-blade".to_owned());
        assert!(!prerequisite_met(thirsting, 5, &held));
    }

    #[test]
    fn spell_item_choices_are_skipped() {
        let catalog = catalog();
        // Versatile's origin feat carries two spell choices of the real shape.
        let versatile = "Compendium.dnd5e.origins24.Item.phbsptVersatile0";
        let key = format!("{versatile}#KB8IQLwyuL6SOFnv#0");
        let choices = DraftChoices {
            species: Some(HUMAN.into()),
            picks: BTreeMap::from([(key, vec![MAGIC_INITIATE.to_owned()])]),
            ..DraftChoices::default()
        };
        let decisions = decisions(&catalog, &choices, 1);
        assert!(
            decisions
                .iter()
                .all(|decision| decision.owner_name != "Magic Initiate"),
            "{decisions:?}"
        );
        assert!(
            decisions
                .iter()
                .all(|decision| decision.title != "Two Cantrips"
                    && decision.title != "Level 1 Spell"),
            "{decisions:?}"
        );

        // The pending list no longer asks for an unanswerable spell pick.
        let to_do = pending(Some(&catalog), &choices, "Al", None, &decisions);
        assert!(
            to_do.iter().all(|item| !item.label.contains("Cantrip")),
            "{to_do:?}"
        );
    }
}
