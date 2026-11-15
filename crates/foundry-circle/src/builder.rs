//! Session-zero rules and the pure half of the character builder.
//!
//! Everything here is I/O-free so the wasm bundle and the server agree on the
//! rules; `builder_store` owns the SQL and `builder_ui` the pages.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
    /// The class or background uuid → the equipment option it picked: `kit`,
    /// `kit2`, … or `gold`.
    pub equipment: BTreeMap<String, String>,
    /// The sheet's free text, by allowlisted key; all optional.
    pub details: BTreeMap<String, String>,
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
    Equipment {
        /// The class or background the equipment belongs to.
        owner: String,
        /// One of the options that owner offers under the campaign's mode.
        option: String,
    },
    Detail {
        /// One of `DETAIL_KEYS`; anything else is refused.
        key: String,
        value: String,
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
    /// The starting-equipment entry labels dnd5e's own
    /// `EquipmentEntryData#label` produced; a module older than this field
    /// sends none, which leaves the builder on the item's name or entry type.
    #[serde(default)]
    pub equipment: EquipmentLabels,
}

/// The entry labels the module resolved for one item's starting equipment, by
/// entry id.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct EquipmentLabels {
    pub labels: BTreeMap<String, String>,
}

/// One of the world's compendium uuid redirects: the pack prefix dnd5e rewrites
/// (a premium module's) and the pack it names instead (the SRD one), from
/// `CONFIG.compendium.uuidRedirects`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Redirect {
    pub from: String,
    pub to: String,
}

impl Redirect {
    /// Whether the pair is a compendium pack prefix the broker will store; a
    /// module is not trusted to send anything else.
    pub fn valid(&self) -> bool {
        [&self.from, &self.to].into_iter().all(|prefix| {
            prefix.starts_with("Compendium.")
                && prefix.len() <= 200
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    }
}

/// One world's cached documents and its uuid redirects: the three root slots
/// may only pick from the enabled `packs`, while a kit's items and a granted
/// feature resolve anything the world synced. Foundry stays the authority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// The enabled packs, by label; the item's uuid names which one it is.
    pub packs: Vec<PackInfo>,
    /// Every synced document of the world: the enabled packs' plus those the
    /// builder never picks from but a kit or a granted feature names.
    pub items: Vec<CatalogItem>,
    /// The world's own uuid redirects, as dnd5e registers them.
    pub redirects: Vec<Redirect>,
}

impl Catalog {
    /// The document one uuid names, after the world's redirects.
    pub fn get(&self, uuid: &str) -> Option<&CatalogItem> {
        let uuid = self.redirect(uuid);
        self.items.iter().find(|item| item.uuid == uuid.as_ref())
    }

    /// The uuid a lookup uses: dnd5e points a premium module's pack prefix at
    /// the SRD pack when that module is not active, so every lookup — the
    /// walker's, a pool's, a kit's — resolves the SRD document. This is the one
    /// place the redirects apply.
    pub fn redirect<'a>(&'a self, uuid: &'a str) -> Cow<'a, str> {
        for redirect in &self.redirects {
            let rest = uuid.strip_prefix(redirect.from.as_str()).filter(|rest| {
                // A pack prefix ends where the document id begins.
                rest.is_empty() || rest.starts_with('.')
            });
            if let Some(rest) = rest {
                // The world's map holds one entry per pack and no prefix inside
                // another, so the first match is the rewrite.
                return Cow::Owned(format!("{}{rest}", redirect.to));
            }
        }
        Cow::Borrowed(uuid)
    }

    /// Whether a document belongs to one of the enabled sources. Only the three
    /// root slots are gated this way: a kit's items and a granted feature are
    /// mandated by a document the player already chose.
    pub fn enabled(&self, uuid: &str) -> bool {
        let uuid = self.redirect(uuid);
        pack_id(&uuid).is_some_and(|id| self.packs.iter().any(|pack| pack.id == id))
    }

    /// Whether an enabled source offers a document of this type: the one rule
    /// the three root slots obey.
    pub fn offers(&self, uuid: &str, item_type: &str) -> bool {
        self.enabled(uuid)
            && self
                .get(uuid)
                .is_some_and(|item| item.item_type == item_type)
    }

    /// The document one of the draft's root slots names, while an enabled
    /// source still offers it.
    pub fn root<'a>(&'a self, choices: &DraftChoices, slot: Slot) -> Option<&'a CatalogItem> {
        let uuid = slot.chosen(choices)?;
        if !self.offers(uuid, slot.item_type()) {
            return None;
        }
        self.get(uuid)
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
    /// The class casts spells (`system.spellcasting.progression` is set and not
    /// `none`); until slice 8, its spells are picked in Foundry.
    pub caster: bool,
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
    // The player chooses these, so only packs the GM enabled may offer them.
    for entry in configuration["pool"].as_array().into_iter().flatten() {
        if let Some(item) = entry["uuid"].as_str().and_then(|uuid| catalog.get(uuid))
            && catalog.enabled(&item.uuid)
        {
            push_option(&mut options, item, level, held);
        }
    }
    let restriction = &configuration["restriction"];
    let subtype = restriction["subtype"].as_str().unwrap_or_default();
    if configuration["allowDrops"].as_bool().unwrap_or(false) && !subtype.is_empty() {
        let value = restriction["type"].as_str().unwrap_or_default();
        for item in &catalog.items {
            let kind = &item.system["type"];
            if kind["value"].as_str() == Some(value)
                && kind["subtype"].as_str() == Some(subtype)
                && catalog.enabled(&item.uuid)
            {
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
                    // Only a saved pick still on offer opens its own decisions: a
                    // pack the GM disabled since then no longer counts.
                    chosen = choices
                        .picks
                        .get(&key)
                        .into_iter()
                        .flatten()
                        .filter(|uuid| options.iter().any(|option| &option.value == *uuid))
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
    for (slot, step) in [
        (Slot::Species, Step::Origin),
        (Slot::Background, Step::Origin),
        (Slot::Class, Step::Class),
    ] {
        if let Some(item) = catalog.root(choices, slot) {
            walk(catalog, item, choices, level, step, &mut found);
        }
    }
    found.decisions
}

/// Drop every answer the draft's roots no longer open: a replaced root's own and
/// nested decisions, the sub-decisions of an item it no longer picks, and the
/// equipment of a class or background it no longer has. The walker produces keys
/// only for the current roots, so this is the one rule, and it covers the
/// ability bonuses and the equipment beside the picks.
pub fn prune_picks(catalog: &Catalog, choices: &mut DraftChoices, level: u8) {
    let keys: BTreeSet<String> = decisions(catalog, choices, level)
        .into_iter()
        .map(|decision| decision.key)
        .collect();
    choices.picks.retain(|key, _| keys.contains(key));
    choices.asi.retain(|key, _| keys.contains(key));
    let owners: Vec<String> = [&choices.class, &choices.background]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    choices.equipment.retain(|owner, _| owners.contains(owner));
}

/// The documents of one item type the enabled sources offer, by source then name.
fn offered(catalog: &Catalog, item_type: &str) -> Vec<ClassOption> {
    let mut options: Vec<ClassOption> = catalog
        .items
        .iter()
        .filter(|item| item.item_type == item_type && catalog.enabled(&item.uuid))
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
    let class = catalog.root(choices, Slot::Class)?;
    // Walk the origin first, as `decisions` does, so class offers (Expertise)
    // see background proficiencies; then keep only what the class opened.
    let mut found = Walk::default();
    for slot in [Slot::Species, Slot::Background] {
        if let Some(item) = catalog.root(choices, slot) {
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
        caster: class.system["spellcasting"]["progression"]
            .as_str()
            .is_some_and(|progression| !progression.is_empty() && progression != "none"),
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
        let Some(item) = catalog.root(choices, root) else {
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

/// One document a kit embeds, with how many of it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentItem {
    pub uuid: String,
    pub count: u32,
}

/// One starting-equipment choice: a kit the draft receives, or the gold that
/// replaces it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentOption {
    /// `kit`, `kit2`, … for an owner with several kits, or `gold`.
    pub id: String,
    /// The option's one line, already worded for the page.
    pub label: String,
    /// The documents the kit embeds, in the order the rules list them.
    pub items: Vec<EquipmentItem>,
    /// Plain gold the kit's own currency entries add.
    pub gp: u32,
    /// The owner's wealth formula, on the gold option only.
    pub wealth: Option<String>,
    /// What the builder cannot embed, as dnd5e words it: a category such as a
    /// focus, a choice inside a kit, or a document the synced packs do not
    /// hold. The DM hands these out.
    pub manual: Vec<String>,
}

/// One chosen root's starting equipment and the options it offers.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EquipmentPlan {
    /// The class or background uuid the options belong to.
    pub owner: String,
    pub owner_name: String,
    pub options: Vec<EquipmentOption>,
}

impl EquipmentPlan {
    /// The option the draft picked, when it is still one this plan offers.
    pub fn chosen<'a>(&'a self, choices: &'a DraftChoices) -> Option<&'a EquipmentOption> {
        let id = choices.equipment.get(&self.owner)?;
        self.options.iter().find(|option| &option.id == id)
    }
}

/// Every starting-equipment choice the draft's chosen class and background owe
/// under the campaign's equipment mode: what the equipment step shows and what
/// `update_draft` validates against. An owner that offers nothing — no kit and
/// no wealth — asks nothing.
pub fn equipment_plans(
    catalog: &Catalog,
    choices: &DraftChoices,
    rules: &CampaignRules,
) -> Vec<EquipmentPlan> {
    let mut plans = Vec::new();
    for slot in [Slot::Class, Slot::Background] {
        let Some(item) = catalog.root(choices, slot) else {
            continue;
        };
        let plan = equipment_plan(catalog, item, rules);
        if !plan.options.is_empty() {
            plans.push(plan);
        }
    }
    plans
}

/// What a draft may store for one owner's equipment: an option the owner offers
/// under the campaign's mode. The page and `update_draft` share this one rule.
pub fn validate_equipment(
    plans: &[EquipmentPlan],
    owner: &str,
    option: &str,
) -> Result<(), &'static str> {
    let plan = plans
        .iter()
        .find(|plan| plan.owner == owner)
        .ok_or("not_offered")?;
    plan.options
        .iter()
        .any(|offered| offered.id == option)
        .then_some(())
        .ok_or("not_offered")
}

/// One owner's options: each kit the rules offer, then the wealth that replaces
/// all of them, as the campaign's mode allows.
fn equipment_plan(catalog: &Catalog, item: &CatalogItem, rules: &CampaignRules) -> EquipmentPlan {
    let mut options = Vec::new();
    if rules.equipment != EquipmentMode::Gold {
        options = kit_options(catalog, item);
    }
    if rules.equipment != EquipmentMode::Kit
        && let Some(wealth) = item.system["wealth"]
            .as_str()
            .map(str::trim)
            .filter(|wealth| !wealth.is_empty())
    {
        options.push(EquipmentOption {
            id: "gold".into(),
            label: format!("{wealth} GP"),
            items: Vec::new(),
            gp: 0,
            wealth: Some(wealth.to_owned()),
            manual: Vec::new(),
        });
    }
    EquipmentPlan {
        owner: item.uuid.clone(),
        owner_name: item.name.clone(),
        options,
    }
}

/// The kits one document offers: each child of a top-level `OR` (the 2024
/// classes' "choose A, B or C"), the document's one top-level `AND`, or every
/// top-level entry when the content lists several.
fn kit_options(catalog: &Catalog, item: &CatalogItem) -> Vec<EquipmentOption> {
    let entries: Vec<&Value> = item.system["startingEquipment"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let top: Vec<&Value> = entries
        .iter()
        .copied()
        .filter(|entry| group(entry).is_empty())
        .collect();
    let roots: Vec<&Value> = match top.as_slice() {
        [entry] if entry_type(entry) == "OR" => children(&entries, entry),
        [entry] if entry_type(entry) == "AND" => vec![*entry],
        _ => top,
    };
    let mut options = Vec::new();
    for (index, root) in roots.into_iter().enumerate() {
        let mut kit = Kit::default();
        kit.add(catalog, &item.equipment.labels, &entries, root, 0);
        options.push(EquipmentOption {
            // The first kit is the plan's plain `kit`, so a class with one kit
            // stores the same option a background does.
            id: if index == 0 {
                "kit".into()
            } else {
                format!("kit{}", index + 1)
            },
            label: kit.label(),
            items: kit.items,
            gp: kit.gp,
            wealth: None,
            manual: kit.manual,
        });
    }
    options
}

/// One kit while it is read: its documents, the gold its entries add, what the
/// DM hands out, and the page's own words for the option.
#[derive(Default)]
struct Kit {
    items: Vec<EquipmentItem>,
    gp: u32,
    manual: Vec<String>,
    parts: Vec<String>,
}

/// How deep starting-equipment entries may nest, as `EquipmentEntryData` caps
/// it; broken content stops here instead of looping.
const KIT_DEPTH: u8 = 4;

impl Kit {
    /// Fold one entry, and everything below it, into the kit.
    fn add(
        &mut self,
        catalog: &Catalog,
        labels: &BTreeMap<String, String>,
        entries: &[&Value],
        entry: &Value,
        depth: u8,
    ) {
        if depth >= KIT_DEPTH {
            return;
        }
        match entry_type(entry) {
            "AND" => {
                for child in children(entries, entry) {
                    self.add(catalog, labels, entries, child, depth + 1);
                }
            }
            "OR" => {
                // ponytail: a choice inside a kit is handed to the DM as one
                // line; add a picker when a kit needs one.
                let choices: Vec<String> = children(entries, entry)
                    .iter()
                    .map(|child| entry_label(child, labels, catalog))
                    .collect();
                if !choices.is_empty() {
                    let line = format!("choose one: {}", choices.join(", "));
                    self.manual.push(line.clone());
                    self.parts.push(line);
                }
            }
            "linked" => {
                let count = entry_count(entry);
                match entry["key"].as_str().and_then(|uuid| catalog.get(uuid)) {
                    Some(item) => {
                        self.parts.push(counted(&item.name, count));
                        self.items.push(EquipmentItem {
                            uuid: item.uuid.clone(),
                            count,
                        });
                    }
                    None => self.line(entry_label(entry, labels, catalog)),
                }
            }
            "currency" if entry["key"].as_str() == Some("gp") => self.gp += entry_count(entry),
            _ => self.line(entry_label(entry, labels, catalog)),
        }
    }

    /// Note a line the kit cannot embed, in the manual list and the summary.
    fn line(&mut self, label: String) {
        self.manual.push(label.clone());
        self.parts.push(label);
    }

    /// The option's one line: everything the kit contains, gold included.
    fn label(&self) -> String {
        let mut parts = self.parts.clone();
        if self.gp > 0 {
            parts.push(format!("{} GP", self.gp));
        }
        if parts.is_empty() {
            "Kit".to_owned()
        } else {
            format!("Kit: {}", parts.join(", "))
        }
    }
}

/// An entry's type, as `EquipmentEntryData#TYPES` spells it.
fn entry_type(entry: &Value) -> &str {
    entry["type"].as_str().unwrap_or_default()
}

/// The group an entry sits in; the top level has none, as `''` or null.
fn group(entry: &Value) -> &str {
    entry["group"].as_str().unwrap_or_default()
}

/// How many of an entry a kit grants; dnd5e leaves `count` empty for one.
fn entry_count(entry: &Value) -> u32 {
    entry["count"]
        .as_u64()
        .filter(|count| *count > 0)
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or(1)
}

/// The entries of one group, in the order their `sort` gives them.
fn children<'a>(entries: &[&'a Value], parent: &Value) -> Vec<&'a Value> {
    let id = parent["_id"].as_str().unwrap_or_default();
    let mut children: Vec<&Value> = entries
        .iter()
        .copied()
        .filter(|entry| !id.is_empty() && group(entry) == id)
        .collect();
    children.sort_by_key(|entry| entry["sort"].as_i64().unwrap_or(0));
    children
}

/// An item line: the count only when there is more than one.
fn counted(name: &str, count: u32) -> String {
    if count > 1 {
        format!("{count}× {name}")
    } else {
        name.to_owned()
    }
}

/// An entry as dnd5e's own `label` getter words it. A linked entry's label is
/// HTML anchoring the document, so the builder shows the item's name instead; a
/// module that sends no labels leaves the entry's type.
fn entry_label(entry: &Value, labels: &BTreeMap<String, String>, catalog: &Catalog) -> String {
    let linked = entry_type(entry) == "linked";
    // A linked entry the catalog resolves reads as its item's name.
    if linked && let Some(item) = entry["key"].as_str().and_then(|uuid| catalog.get(uuid)) {
        return counted(&item.name, entry_count(entry));
    }
    // Otherwise dnd5e's own label. The getter punctuates a count with an HTML
    // entity, and a linked entry's label is an HTML anchor, so it is reduced to
    // plain text.
    if let Some(label) = entry["_id"]
        .as_str()
        .and_then(|id| labels.get(id))
        .filter(|label| !label.is_empty())
    {
        let label = label.replace("&times;", "×");
        return if linked { plain(&label) } else { label };
    }
    counted(entry_type(entry), entry_count(entry))
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

/// The trait options a decision holds, with the labels the catalog resolved:
/// what it grants and what its pools offer.
pub fn trait_options(decision: &Decision) -> Vec<&Opt> {
    match &decision.kind {
        DecisionKind::Trait {
            grants, choices, ..
        } => grants
            .iter()
            .chain(choices.iter().flat_map(|pool| pool.options.iter()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The party's skill coverage: every skill a draft holds, read from the same
/// `granted_traits` the pages use and labelled by the trait options the catalog
/// resolved, plus the skills of those options nobody holds. The DM board's
/// table and the pickers therefore name a skill the same way.
/// Every skill the world's rules content names (key → label): the full set a
/// party's coverage is measured against, read from the synced trait options.
pub fn catalog_skills(catalog: &Catalog) -> BTreeMap<String, String> {
    let mut skills = BTreeMap::new();
    for item in &catalog.items {
        for advancement in item
            .traits
            .as_object()
            .into_iter()
            .flat_map(|map| map.values())
        {
            let grants = advancement["grants"].as_array().into_iter().flatten();
            let options = advancement["choices"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|pool| pool["options"].as_array().into_iter().flatten());
            for option in grants.chain(options) {
                if let (Some(key), Some(label)) = (option["key"].as_str(), option["label"].as_str())
                    && key.starts_with("skills:")
                    && !key.ends_with('*')
                {
                    skills
                        .entry(key.to_owned())
                        .or_insert_with(|| label.to_owned());
                }
            }
        }
    }
    skills
}

pub fn party_summary(
    drafts: &[(DraftSummary, Vec<Decision>)],
    world_skills: &BTreeMap<String, String>,
) -> PartySummary {
    let mut labels: BTreeMap<String, String> = world_skills.clone();
    let mut held: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (summary, decisions) in drafts {
        for option in decisions.iter().flat_map(trait_options) {
            if option.value.starts_with("skills:") && !option.label.is_empty() {
                labels
                    .entry(option.value.clone())
                    .or_insert_with(|| option.label.clone());
            }
        }
        let name = if summary.name.trim().is_empty() {
            summary.player.clone()
        } else {
            summary.name.clone()
        };
        for key in granted_traits(decisions, &summary.choices) {
            if key.starts_with("skills:") {
                held.entry(key).or_default().insert(name.clone());
            }
        }
    }
    let label = |key: &str| labels.get(key).cloned().unwrap_or_else(|| key.to_owned());
    let mut skills: Vec<(String, Vec<String>)> = held
        .iter()
        .map(|(key, names)| (label(key), names.iter().cloned().collect()))
        .collect();
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    let mut uncovered: Vec<String> = labels
        .keys()
        .filter(|key| !held.contains_key(*key))
        .map(|key| label(key))
        .collect();
    uncovered.sort();
    uncovered.dedup();
    PartySummary { skills, uncovered }
}

/// Whether a draft may be submitted: nothing may be left on the to-do list.
/// `submit_draft` recomputes that list from the world's real content first.
pub fn submit_refusal(pending: &[Pending]) -> Result<(), &'static str> {
    if pending.is_empty() {
        Ok(())
    } else {
        Err("draft_incomplete")
    }
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
    /// What the player still owes, from the same rule the builder page uses.
    pub pending: u32,
    /// The chosen roots' names, so the board reads names instead of uuids.
    pub class_name: Option<String>,
    pub species_name: Option<String>,
    pub background_name: Option<String>,
    /// Base scores plus every ability bonus; the board shows these.
    pub totals: Scores,
}

/// The party's skill coverage, as the DM board's table reads it.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PartySummary {
    /// Skill label → the characters who have it, by character name.
    pub skills: Vec<(String, Vec<String>)>,
    /// The skills of the party's content that nobody holds.
    pub uncovered: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CharactersData {
    pub role: Role,
    pub campaigns: Vec<CampaignView>,
    pub mine: Vec<DraftSummary>,
    /// Empty for players.
    pub party: Vec<DraftSummary>,
    /// The party's skill coverage; empty for players.
    pub coverage: PartySummary,
    /// The world the driver reports Ready; only it can sync its catalog.
    pub ready_world: Option<String>,
    /// The viewer may approve: an admin, or the GM holding the running
    /// session, while a world is ready. Only a hint; the approval re-checks.
    pub can_approve: bool,
    /// Where an approved character is played: `<playBaseUrl>/_circle/enter`.
    pub play_url: Option<String>,
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
    /// The starting equipment the chosen class and background owe.
    pub equipment: Vec<EquipmentPlan>,
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

/// A level-one character's hit points: the class's hit die maximum plus the
/// Constitution modifier of the totals, never below 0. Levels above one, and
/// the average-or-roll method, wait for slice 9.
pub fn hit_points(hit_die: &str, con: u8) -> u16 {
    let die = hit_die
        .trim()
        .strip_prefix('d')
        .and_then(|sides| sides.parse::<u8>().ok())
        .unwrap_or(0);
    u16::try_from(i16::from(die) + i16::from(modifier(con))).unwrap_or(0)
}

/// The sheet fields the details step may store, with their page labels.
pub const DETAIL_LABELS: [(&str, &str); 15] = [
    ("alignment", "Alignment"),
    ("faith", "Faith"),
    ("gender", "Gender"),
    ("age", "Age"),
    ("height", "Height"),
    ("weight", "Weight"),
    ("eyes", "Eyes"),
    ("hair", "Hair"),
    ("skin", "Skin"),
    ("appearance", "Appearance"),
    ("trait", "Personality trait"),
    ("ideal", "Ideal"),
    ("bond", "Bond"),
    ("flaw", "Flaw"),
    ("backstory", "Backstory"),
];

/// How long one detail may be, as the actor fields it becomes.
pub const DETAIL_MAX: usize = 2000;

/// What a draft may store for one sheet field: an allowlisted key, and at most
/// `DETAIL_MAX` characters once trimmed. The page and `update_draft` share this
/// one rule; an empty value clears the field.
pub fn validate_detail(key: &str, value: &str) -> Result<String, &'static str> {
    if !DETAIL_LABELS.iter().any(|(field, _)| *field == key) {
        return Err("invalid_detail");
    }
    let value = value.trim();
    if value.chars().count() > DETAIL_MAX {
        return Err("invalid_detail");
    }
    Ok(value.to_owned())
}

/// How long a DM's note may be, as the board's textarea allows.
pub const NOTE_MAX: usize = 1000;

/// The note a DM's "request changes" stores, trimmed; one to `NOTE_MAX`
/// characters, since a note that says nothing is no note.
pub fn validate_note(note: &str) -> Result<String, &'static str> {
    let note = note.trim();
    if note.is_empty() || note.chars().count() > NOTE_MAX {
        return Err("invalid_note");
    }
    Ok(note.to_owned())
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
/// caller without one cannot see a root fall out of the sources, nor the
/// equipment its roots owe.
pub fn pending(
    catalog: Option<&Catalog>,
    rules: &CampaignRules,
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
        (Some(uuid), Some(catalog)) => catalog.offers(uuid, Slot::Class.item_type()),
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
            (Some(uuid), Some(catalog)) => catalog.offers(uuid, slot.item_type()),
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
        // The GM may have changed the rules since: the scores must still pass.
        (Some(method), Some(base)) => validate_scores(method, &base, rolls, rules)
            .err()
            .map(|_| ("abilities", "Fix your ability scores for the current rules")),
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
    if let Some(catalog) = catalog {
        for plan in equipment_plans(catalog, choices, rules) {
            if plan.chosen(choices).is_none() {
                pending.push(Pending {
                    step: Step::Equipment,
                    key: plan.owner.clone(),
                    label: format!("Choose {} starting equipment", plan.owner_name),
                });
            }
        }
    }
    pending
}

/// The one `CharacterCreate` argument an approved draft becomes.
///
/// The scores are the draft's base: every bonus arrives as the advancement that
/// grants it, so the module applies each exactly once. A decision's answer is
/// the apply data its advancement takes, and every advancement without one is
/// absent, which is what makes the module ask for the system's automatic value
/// (`6.1.0:module/documents/advancement/advancement.mjs#L336-L350`).
pub fn create_args(
    catalog: &Catalog,
    rules: &CampaignRules,
    choices: &DraftChoices,
    name: &str,
    rolls: Option<&Rolls>,
    draft_id: &str,
    owner_user_id: &str,
) -> Result<Value, &'static str> {
    let level = rules.starting_level;
    let decisions = decisions(catalog, choices, level);
    // The same rule as submit: a draft with anything still open never reaches a
    // world. `approve_draft` recomputes this from the world's real content.
    submit_refusal(&pending(
        Some(catalog),
        rules,
        choices,
        name,
        rolls,
        &decisions,
    ))?;
    // dnd5e applies level 0 (species, then background) before the class, so the
    // module adds its roots in that order.
    let mut roots = Vec::with_capacity(3);
    for (slot, kind) in [
        (Slot::Species, "species"),
        (Slot::Background, "background"),
        (Slot::Class, "class"),
    ] {
        let item = catalog.root(choices, slot).ok_or("invalid_draft")?;
        roots.push(json!({"uuid": item.uuid, "kind": kind}));
    }
    let mut answers = serde_json::Map::new();
    for decision in &decisions {
        let answer = match &decision.kind {
            // A Trait's data is everything it grants plus what the player chose;
            // its own `apply` writes both into the actor's traits.
            DecisionKind::Trait { grants, .. } => {
                let chosen: BTreeSet<&str> = grants
                    .iter()
                    .map(|option| option.value.as_str())
                    .chain(
                        choices
                            .picks
                            .get(&decision.key)
                            .into_iter()
                            .flatten()
                            .map(String::as_str),
                    )
                    .collect();
                json!({"chosen": chosen})
            }
            // ItemChoice and ItemGrant take the documents to embed.
            DecisionKind::Items { .. } => json!({
                "selected": choices.picks.get(&decision.key).cloned().unwrap_or_default()
            }),
            // The background's bonus: the configuration's own fixed points and
            // the player's assignment, which `apply` adds to the base scores.
            DecisionKind::Asi { fixed, .. } => {
                let mut assignments = fixed.clone();
                for (key, delta) in choices.asi.get(&decision.key).into_iter().flatten() {
                    let total = assignments
                        .get(key)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(*delta);
                    assignments.insert(key.clone(), total);
                }
                assignments.retain(|_, delta| *delta > 0);
                json!({"type": "asi", "assignments": assignments})
            }
            DecisionKind::Size { .. } => {
                let size = choices
                    .picks
                    .get(&decision.key)
                    .and_then(|values| values.first())
                    .ok_or("invalid_draft")?;
                json!({"size": size})
            }
            // `pending` refused an unresolved unsupported step above.
            DecisionKind::Unsupported { .. } => continue,
        };
        answers.insert(decision.key.clone(), answer);
    }

    // The chosen kits' documents and coin, or the gold that replaces a kit; the
    // manual lines are what the DM still hands out.
    let mut items: Vec<(String, u32)> = Vec::new();
    let mut gp = 0_u32;
    let mut wealth: Vec<String> = Vec::new();
    let mut manual: Vec<String> = Vec::new();
    for plan in equipment_plans(catalog, choices, rules) {
        let option = plan.chosen(choices).ok_or("draft_incomplete")?;
        gp = gp.saturating_add(option.gp);
        wealth.extend(option.wealth.clone());
        manual.extend(option.manual.clone());
        for item in &option.items {
            match items.iter_mut().find(|(uuid, _)| *uuid == item.uuid) {
                Some((_, count)) => *count = count.saturating_add(item.count),
                None => items.push((item.uuid.clone(), item.count)),
            }
        }
    }

    let base = choices.base.ok_or("invalid_draft")?;
    let mut details = serde_json::Map::new();
    let mut biography = String::new();
    for (key, _) in DETAIL_LABELS {
        let Some(value) = choices.details.get(key) else {
            continue;
        };
        // The biography is the one details field with a path of its own.
        if key == "backstory" {
            biography = value.clone();
        } else {
            details.insert(key.to_owned(), json!(value));
        }
    }
    let items: Vec<Value> = items
        .iter()
        .map(|(uuid, quantity)| json!({"uuid": uuid, "quantity": quantity}))
        .collect();
    Ok(json!({
        "draftId": draft_id,
        "name": name.trim(),
        "ownerUserId": owner_user_id,
        "level": level,
        "abilities": {
            "str": base.str, "dex": base.dex, "con": base.con,
            "int": base.int, "wis": base.wis, "cha": base.cha
        },
        "details": details,
        "biography": biography,
        "roots": roots,
        "choices": answers,
        "items": items,
        "gp": gp,
        "wealth": wealth,
        "manual": manual
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    const CLERIC: &str = "Compendium.dnd5e.classes24.Item.phbclcCleric0000";
    const DIVINE_ORDER: &str = "Compendium.dnd5e.classes24.Item.phbclcDivineOrde";
    const FIGHTER: &str = "Compendium.dnd5e.classes24.Item.phbftrFighter000";
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
    const CHAIN_SHIRT: &str = "Compendium.dnd5e.equipment24.Item.phbarmChainShirt";
    const SHIELD: &str = "Compendium.dnd5e.equipment24.Item.phbarmShield0000";
    const MACE: &str = "Compendium.dnd5e.equipment24.Item.phbwepMace000000";
    const PRIESTS_PACK: &str = "Compendium.dnd5e.equipment24.Item.phbagPriestsPack";
    const JAVELIN: &str = "Compendium.dnd5e.equipment24.Item.phbwepJavelin000";
    const PLAYERS_HANDBOOK_EQUIPMENT: &str = "Compendium.dnd-players-handbook.equipment";
    const SRD_EQUIPMENT: &str = "Compendium.dnd5e.equipment24";

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
            (
                "dnd5e.equipment24",
                "Equipment",
                include_str!("../tests/fixtures/catalog/equipment24.json"),
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
        // What dnd5e registers while the premium Player's Handbook is not
        // active: the 2024 kits and classes still name its packs.
        catalog.redirects.push(Redirect {
            from: PLAYERS_HANDBOOK_EQUIPMENT.into(),
            to: SRD_EQUIPMENT.into(),
        });
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

    fn plan<'a>(plans: &'a [EquipmentPlan], owner: &str) -> &'a EquipmentPlan {
        plans
            .iter()
            .find(|plan| plan.owner == owner)
            .unwrap_or_else(|| panic!("{owner} in {plans:?}"))
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

    /// The fixture draft the approval tests use: Human, Acolyte, Cleric 1, every
    /// choice answered, kit for both owners.
    fn fixture_cleric() -> DraftChoices {
        let key =
            |uuid: &str, advancement: &str, level: u8| format!("{uuid}#{advancement}#{level}");
        DraftChoices {
            method: Some(AbilityMethod::StandardArray),
            base: Some(scores(15, 14, 13, 12, 10, 8)),
            class: Some(CLERIC.into()),
            species: Some(HUMAN.into()),
            background: Some(ACOLYTE.into()),
            picks: BTreeMap::from([
                (key(HUMAN, "dLxv96vt2B2KOEe2", 0), vec!["med".to_owned()]),
                (
                    key(VERSATILE, "KB8IQLwyuL6SOFnv", 0),
                    vec![MAGIC_INITIATE.to_owned()],
                ),
                (
                    key(ACOLYTE, "McRHkSL3WnG2Po7g", 0),
                    vec![
                        "languages:standard:dwarvish".to_owned(),
                        "languages:standard:elvish".to_owned(),
                    ],
                ),
                (
                    key(CLERIC, "2wUNkQzS4vL9nR6D", 1),
                    vec!["skills:his".to_owned(), "skills:ins".to_owned()],
                ),
                (
                    key(DIVINE_ORDER, "V8GfnoZYTe1PLXPI", 1),
                    vec![PROTECTOR.to_owned()],
                ),
            ]),
            asi: BTreeMap::from([(
                key(ACOLYTE, "kDI6zoUamHzFTvHG", 0),
                BTreeMap::from([("int".to_owned(), 2), ("wis".to_owned(), 1)]),
            )]),
            equipment: BTreeMap::from([
                (CLERIC.to_owned(), "kit".to_owned()),
                (ACOLYTE.to_owned(), "kit".to_owned()),
            ]),
            details: BTreeMap::from([
                ("alignment".to_owned(), "Neutral".to_owned()),
                ("backstory".to_owned(), "Raised in the temple.".to_owned()),
            ]),
        }
    }

    #[test]
    fn create_args_for_the_fixture_cleric() {
        const NEW_CALLIGRAPHER: &str = "Compendium.dnd5e.equipment24.Item.phbtulCalligraph";
        const BOOK: &str = "Compendium.dnd5e.equipment24.Item.phbagBook0000000";
        const PARCHMENT: &str = "Compendium.dnd5e.equipment24.Item.phbagParchment00";
        const ROBE: &str = "Compendium.dnd5e.equipment24.Item.phbagRobe0000000";
        const DRAFT: &str = "0b6f4c2e-0000-4000-8000-000000000000";
        let catalog = catalog();
        let args = create_args(
            &catalog,
            &CampaignRules::default(),
            &fixture_cleric(),
            "Al",
            None,
            DRAFT,
            "playercase000001",
        )
        .expect("a finished draft");

        // Every answer in dnd5e's own apply-data shape, and nothing else: an
        // advancement without one is asked for its automatic value.
        let key =
            |uuid: &str, advancement: &str, level: u8| format!("{uuid}#{advancement}#{level}");
        let choices = json!({
            key(ACOLYTE, "kDI6zoUamHzFTvHG", 0): {"type": "asi", "assignments": {"int": 2, "wis": 1}},
            key(ACOLYTE, "Lk1ScjOCqRDtX2Tg", 0): {
                "chosen": ["skills:ins", "skills:rel", "tool:art:calligrapher"]
            },
            key(ACOLYTE, "McRHkSL3WnG2Po7g", 0): {
                "chosen": [
                    "languages:standard:common",
                    "languages:standard:dwarvish",
                    "languages:standard:elvish"
                ]
            },
            key(CLERIC, "U6GGwY4ZKiXqXBQN", 1): {"chosen": ["weapon:sim"]},
            key(CLERIC, "Z9K5kHYMLvroIMpT", 1): {
                "chosen": ["armor:med", "armor:shl", "saves:cha", "saves:wis"]
            },
            key(CLERIC, "2wUNkQzS4vL9nR6D", 1): {"chosen": ["skills:his", "skills:ins"]},
            key(DIVINE_ORDER, "V8GfnoZYTe1PLXPI", 1): {"selected": [PROTECTOR]},
            key(HUMAN, "dLxv96vt2B2KOEe2", 0): {"size": "med"},
            key(PROTECTOR, "Pw2NhGslXxcl0btG", 1): {"chosen": ["armor:hvy", "weapon:mar"]},
            key(VERSATILE, "KB8IQLwyuL6SOFnv", 0): {"selected": [MAGIC_INITIATE]}
        });
        let expected = json!({
            "draftId": DRAFT,
            "name": "Al",
            "ownerUserId": "playercase000001",
            "level": 1,
            // The base scores: the background's bonus is applied by its own
            // advancement, not added twice here.
            "abilities": {"str": 15, "dex": 14, "con": 13, "int": 12, "wis": 10, "cha": 8},
            "details": {"alignment": "Neutral"},
            "biography": "Raised in the temple.",
            "roots": [
                {"uuid": HUMAN, "kind": "species"},
                {"uuid": ACOLYTE, "kind": "background"},
                {"uuid": CLERIC, "kind": "class"}
            ],
            "choices": choices,
            "items": [
                {"uuid": CHAIN_SHIRT, "quantity": 1},
                {"uuid": SHIELD, "quantity": 1},
                {"uuid": MACE, "quantity": 1},
                {"uuid": PRIESTS_PACK, "quantity": 1},
                {"uuid": NEW_CALLIGRAPHER, "quantity": 1},
                {"uuid": BOOK, "quantity": 1},
                {"uuid": PARCHMENT, "quantity": 10},
                {"uuid": ROBE, "quantity": 1}
            ],
            "gp": 15,
            "wealth": [],
            "manual": ["any holy focus", "any holy focus"]
        });
        assert_eq!(args, expected);
    }

    #[test]
    fn create_args_refuse_a_draft_with_pending_items() {
        let catalog = catalog();
        let rules = CampaignRules::default();
        let unfinished = |choices: DraftChoices| {
            create_args(
                &catalog,
                &rules,
                &choices,
                "Al",
                None,
                "0b6f4c2e-0000-4000-8000-000000000000",
                "playercase000001",
            )
        };
        // The class's two skills are unanswered.
        let mut choices = fixture_cleric();
        choices
            .picks
            .remove(&format!("{CLERIC}#2wUNkQzS4vL9nR6D#1"));
        assert_eq!(unfinished(choices), Err("draft_incomplete"));
        // No equipment answer is the same refusal, from the same list submit uses.
        let mut choices = fixture_cleric();
        choices.equipment.remove(CLERIC);
        assert_eq!(unfinished(choices), Err("draft_incomplete"));
        // A draft with no scores is not a character.
        let mut choices = fixture_cleric();
        choices.base = None;
        assert_eq!(unfinished(choices), Err("draft_incomplete"));
        // An unanswered size, and the roots it belongs to, refuse too.
        let mut choices = fixture_cleric();
        choices.species = None;
        assert_eq!(unfinished(choices), Err("draft_incomplete"));
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

        // Slice five: the class and background kits, and the entry labels
        // dnd5e's own label getter produced.
        assert_eq!(cleric.system["startingEquipment"][0]["type"], "AND");
        assert_eq!(
            cleric
                .equipment
                .labels
                .get("DJWicB3geFrbTgwZ")
                .map(String::as_str),
            Some("any holy focus")
        );
        let fighter = classes.last().expect("fighter");
        assert_eq!(fighter.name, "Fighter");
        assert_eq!(fighter.system["startingEquipment"][0]["type"], "OR");
        assert_eq!(fighter.system["wealth"], "155");
        // An item without starting equipment carries empty labels.
        let equipment: Vec<CatalogItem> =
            serde_json::from_str(include_str!("../tests/fixtures/catalog/equipment24.json"))
                .expect("equipment24 fixture parses");
        assert_eq!(equipment[0].uuid, CHAIN_SHIRT);
        assert!(equipment[0].equipment.labels.is_empty());
    }

    #[test]
    fn pending_lists_name_abilities_and_class() {
        let catalog = catalog();
        let empty = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &DraftChoices::default(),
            "  ",
            None,
            &[],
        );
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
            &CampaignRules::default(),
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
            &CampaignRules::default(),
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
        // The class and the background each owe their starting equipment.
        assert_eq!(
            done.iter()
                .map(|item| (item.step, item.label.as_str()))
                .collect::<Vec<_>>(),
            [
                (Step::Equipment, "Choose Cleric starting equipment"),
                (Step::Equipment, "Choose Acolyte starting equipment"),
            ],
            "{done:?}"
        );
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
        let lost = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &stale,
            "Al",
            None,
            &[],
        );
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
            &CampaignRules::default(),
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
            ["History", "Insight", "Medicine", "Persuasion", "Religion"]
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
    fn disabled_packs_offer_no_item_choices() {
        let enabled = catalog();
        let mut catalog = catalog();
        catalog.packs.retain(|pack| pack.id != "dnd5e.feats24");
        // A pick saved while the feats were enabled.
        let versatile_key = {
            let open = super::decisions(
                &enabled,
                &DraftChoices {
                    species: Some(HUMAN.into()),
                    ..DraftChoices::default()
                },
                1,
            );
            decision(&open, "Versatile").key.clone()
        };
        let choices = DraftChoices {
            species: Some(HUMAN.into()),
            picks: BTreeMap::from([(versatile_key, vec![SKILLED.to_owned()])]),
            ..DraftChoices::default()
        };
        let found = decisions(&catalog, &choices, 1);
        // The player chooses Versatile's feat, so a disabled pack offers none,
        // and the saved Skilled pick no longer opens Skilled's own decisions.
        let versatile = decision(&found, "Versatile");
        assert!(
            matches!(&versatile.kind, DecisionKind::Unsupported { .. }),
            "{versatile:?}"
        );
        assert!(found.iter().all(|d| d.owner_name != "Skilled"), "{found:?}");
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
        let before = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &answered,
            "Al",
            None,
            &found,
        );
        assert!(before.iter().any(|item| item.key == asi.key), "{before:?}");
        answered.asi.insert(asi.key.clone(), two_one);
        let found = super::decisions(&catalog, &answered, 1);
        let after = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &answered,
            "Al",
            None,
            &found,
        );
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
        let to_do = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &human,
            "Al",
            None,
            &decisions,
        );
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
        let to_do = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &answered,
            "Al",
            None,
            &found,
        );
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
        let to_do = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &choices,
            "Al",
            None,
            &decisions,
        );
        assert!(
            to_do.iter().all(|item| !item.label.contains("Cantrip")),
            "{to_do:?}"
        );
    }

    #[test]
    fn cleric_kit_lists_linked_items_gold_and_manual_focus() {
        let catalog = catalog();
        let choices = DraftChoices {
            class: Some(CLERIC.into()),
            background: Some(ACOLYTE.into()),
            ..DraftChoices::default()
        };
        let plans = equipment_plans(&catalog, &choices, &CampaignRules::default());
        assert_eq!(
            plans
                .iter()
                .map(|plan| plan.owner.as_str())
                .collect::<Vec<_>>(),
            [CLERIC, ACOLYTE]
        );

        // The class has one top-level AND, so one kit and the wealth.
        let cleric = plan(&plans, CLERIC);
        assert_eq!(cleric.owner_name, "Cleric");
        assert_eq!(
            cleric
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["kit", "gold"]
        );
        let kit = &cleric.options[0];
        assert_eq!(
            kit.label,
            "Kit: Chain Shirt, Shield, Mace, any holy focus, Priest's Pack, 7 GP"
        );
        // The PHB-prefixed links resolve to the SRD documents through the
        // world's redirect, and `count: null` is one.
        assert_eq!(
            kit.items
                .iter()
                .map(|item| (item.uuid.as_str(), item.count))
                .collect::<Vec<_>>(),
            [(CHAIN_SHIRT, 1), (SHIELD, 1), (MACE, 1), (PRIESTS_PACK, 1)]
        );
        assert_eq!(kit.gp, 7);
        assert_eq!(kit.wealth, None);
        // A focus category is not a document; the DM hands it out.
        assert_eq!(kit.manual, ["any holy focus"]);

        // The background's kit is a second plan of the same shape.
        let acolyte = plan(&plans, ACOLYTE);
        assert_eq!(acolyte.options[0].items.len(), 4);
        assert_eq!(acolyte.options[0].gp, 8);
        assert_eq!(acolyte.options[0].manual, ["any holy focus"]);

        // Every owner without a choice is on the to do list; answering one
        // leaves the other.
        let rules = CampaignRules::default();
        let mut answered = choices.clone();
        let found = decisions(&catalog, &answered, 1);
        let to_do = pending(Some(&catalog), &rules, &answered, "Al", None, &found);
        assert_eq!(
            to_do
                .iter()
                .filter(|item| item.step == Step::Equipment)
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Choose Cleric starting equipment",
                "Choose Acolyte starting equipment"
            ]
        );
        answered.equipment.insert(CLERIC.into(), "kit".into());
        let to_do = pending(Some(&catalog), &rules, &answered, "Al", None, &found);
        assert_eq!(
            to_do
                .iter()
                .filter(|item| item.step == Step::Equipment)
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["Choose Acolyte starting equipment"]
        );

        // A replaced class takes its equipment answer with it; the background's
        // stays.
        answered.equipment.insert(ACOLYTE.into(), "gold".into());
        answered.select(Slot::Class, Some(BARD.into()));
        prune_picks(&catalog, &mut answered, 1);
        assert_eq!(answered.equipment.get(CLERIC), None);
        assert_eq!(
            answered.equipment.get(ACOLYTE).map(String::as_str),
            Some("gold")
        );
    }

    #[test]
    fn gold_option_uses_wealth() {
        let catalog = catalog();
        let plans = equipment_plans(&catalog, &with_class(CLERIC), &CampaignRules::default());
        let gold = plans[0]
            .options
            .iter()
            .find(|option| option.id == "gold")
            .expect("gold");
        // The class table's 110 GP, not the kit's own 7.
        assert_eq!(gold.label, "110 GP");
        assert_eq!(gold.wealth.as_deref(), Some("110"));
        assert_eq!(gold.gp, 0);
        assert!(gold.items.is_empty() && gold.manual.is_empty());

        // A stored choice is the plan's, and only while it is offered.
        let mut choices = with_class(CLERIC);
        choices.equipment.insert(CLERIC.into(), "gold".into());
        assert_eq!(
            plans[0]
                .chosen(&choices)
                .map(|option| option.label.as_str()),
            Some("110 GP")
        );
        choices.equipment.insert(CLERIC.into(), "kit2".into());
        assert!(plans[0].chosen(&choices).is_none());
    }

    #[test]
    fn equipment_mode_restricts_options() {
        let catalog = catalog();
        let choices = with_class(CLERIC);
        let offered = |mode: EquipmentMode| {
            equipment_plans(
                &catalog,
                &choices,
                &CampaignRules {
                    equipment: mode,
                    ..CampaignRules::default()
                },
            )
            .into_iter()
            .next()
            .map(|plan| {
                plan.options
                    .into_iter()
                    .map(|option| option.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        };
        assert_eq!(offered(EquipmentMode::Kit), ["kit"]);
        assert_eq!(offered(EquipmentMode::Gold), ["gold"]);
        assert_eq!(offered(EquipmentMode::Either), ["kit", "gold"]);

        // The server stores only what the mode offers, for its own owners only.
        let gold = equipment_plans(
            &catalog,
            &choices,
            &CampaignRules {
                equipment: EquipmentMode::Gold,
                ..CampaignRules::default()
            },
        );
        assert_eq!(validate_equipment(&gold, CLERIC, "gold"), Ok(()));
        assert_eq!(validate_equipment(&gold, CLERIC, "kit"), Err("not_offered"));
        assert_eq!(
            validate_equipment(&gold, ACOLYTE, "gold"),
            Err("not_offered")
        );
        let either = equipment_plans(&catalog, &choices, &CampaignRules::default());
        assert_eq!(validate_equipment(&either, CLERIC, "kit"), Ok(()));
        assert_eq!(
            validate_equipment(&either, CLERIC, "kit2"),
            Err("not_offered")
        );
    }

    #[test]
    fn equipment_needs_an_enabled_source() {
        let mut catalog = catalog();
        // A pack the GM turned off keeps its documents out of the root slots,
        // though a kit may still resolve them.
        catalog.packs.retain(|pack| pack.id != "dnd5e.classes24");
        let choices = DraftChoices {
            class: Some(CLERIC.into()),
            background: Some(ACOLYTE.into()),
            ..DraftChoices::default()
        };
        assert!(catalog.get(CLERIC).is_some());
        assert!(!catalog.offers(CLERIC, Slot::Class.item_type()));
        // Only the background's equipment is owed now, and only its decisions
        // are walked.
        let plans = equipment_plans(&catalog, &choices, &CampaignRules::default());
        assert_eq!(
            plans
                .iter()
                .map(|plan| plan.owner.as_str())
                .collect::<Vec<_>>(),
            [ACOLYTE]
        );
        let found = decisions(&catalog, &choices, 1);
        assert!(found.iter().all(|decision| decision.step == Step::Origin));
        let to_do = pending(
            Some(&catalog),
            &CampaignRules::default(),
            &choices,
            "Al",
            None,
            &found,
        );
        assert!(to_do.iter().any(|item| item.key == "class"), "{to_do:?}");
    }

    #[test]
    fn fighter_or_offers_each_kit_and_gold() {
        let catalog = catalog();
        let plans = equipment_plans(&catalog, &with_class(FIGHTER), &CampaignRules::default());
        let fighter = plan(&plans, FIGHTER);
        // Each child of the top-level OR is an option, and the wealth a third.
        assert_eq!(
            fighter
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["kit", "kit2", "gold"]
        );

        let heavy = &fighter.options[0];
        assert_eq!(
            heavy.label,
            "Kit: Chain Mail, Greatsword, Flail, 8× Javelin, Dungeoneer's Pack, 4 GP"
        );
        assert_eq!(heavy.items[0].count, 1);
        assert_eq!(
            heavy.items[3],
            EquipmentItem {
                uuid: JAVELIN.into(),
                count: 8
            }
        );
        assert_eq!(heavy.gp, 4);
        assert_eq!(heavy.wealth, None);
        assert!(heavy.manual.is_empty(), "{heavy:?}");

        let light = &fighter.options[1];
        assert_eq!(light.items.len(), 7);
        assert_eq!(light.gp, 11);
        assert!(light.manual.is_empty(), "{light:?}");

        assert_eq!(fighter.options[2].wealth.as_deref(), Some("155"));
    }

    #[test]
    fn phb_uuid_redirects_to_srd_item() {
        let catalog = catalog();
        let handbook = "Compendium.dnd-players-handbook.equipment.Item.phbarmChainShirt";
        // The 2024 kits name the premium module's pack; the world points that
        // prefix at the SRD pack while the module is not active.
        assert_eq!(
            catalog.get(handbook).map(|item| item.name.as_str()),
            Some("Chain Shirt")
        );
        assert_eq!(catalog.redirect(handbook).as_ref(), CHAIN_SHIRT);
        // A pack id that merely starts with the redirected one is another pack.
        let neighbour = "Compendium.dnd-players-handbook.equipment24.Item.phbarmChainShirt";
        assert_eq!(catalog.redirect(neighbour).as_ref(), neighbour);
        // The redirect leaves other prefixes alone.
        assert_eq!(catalog.redirect(CLERIC).as_ref(), CLERIC);

        // Without the world's redirect the same uuid names nothing, and the kit
        // line degrades to the entry instead of guessing a document.
        let mut bare = catalog.clone();
        bare.redirects.clear();
        assert!(bare.get(handbook).is_none());
        assert_eq!(bare.redirect(handbook).as_ref(), handbook);
        let plans = equipment_plans(&bare, &with_class(CLERIC), &CampaignRules::default());
        assert!(plans[0].options[0].items.is_empty());
        assert_eq!(
            plans[0].options[0]
                .manual
                .iter()
                .filter(|line| *line == "linked")
                .count(),
            4
        );

        // With it, the same kit embeds the SRD chain shirt.
        let plans = equipment_plans(&catalog, &with_class(CLERIC), &CampaignRules::default());
        assert_eq!(plans[0].options[0].items[0].uuid, CHAIN_SHIRT);
    }

    /// One party row, as `summaries` builds it for the board.
    fn party_row(
        name: &str,
        choices: DraftChoices,
        decisions: Vec<Decision>,
    ) -> (DraftSummary, Vec<Decision>) {
        (
            DraftSummary {
                id: "0b6f4c2e-0000-4000-8000-000000000000".into(),
                name: name.into(),
                status: DraftStatus::Draft,
                player: "Piotr".into(),
                pending: 0,
                class_name: None,
                species_name: None,
                background_name: None,
                totals: Scores::default(),
                choices,
                rolls: None,
            },
            decisions,
        )
    }

    #[test]
    fn submit_requires_an_empty_todo_list() {
        let catalog = catalog();
        let rules = CampaignRules::default();
        let mut choices = DraftChoices {
            method: Some(AbilityMethod::StandardArray),
            base: Some(scores(15, 14, 13, 12, 10, 8)),
            class: Some(CLERIC.into()),
            species: Some(HUMAN.into()),
            background: Some(ACOLYTE.into()),
            ..DraftChoices::default()
        };
        let to_do = |choices: &DraftChoices| {
            let found = decisions(&catalog, choices, rules.starting_level);
            pending(Some(&catalog), &rules, choices, "Al", None, &found)
        };
        // Everything the class, the species, the background and the equipment
        // ask for is still open, so nothing is submitted.
        let open = to_do(&choices);
        assert!(open.len() >= 6, "{open:?}");
        assert_eq!(submit_refusal(&open), Err("draft_incomplete"));

        // Answer every decision the walker opens, then whatever answering opened
        // in turn: the Human's feat is only chosen here.
        for _ in 0..3 {
            for decision in decisions(&catalog, &choices, rules.starting_level) {
                let values: Vec<String> = match &decision.kind {
                    DecisionKind::Trait { choices: pools, .. } => pools
                        .iter()
                        .flat_map(|pool| {
                            pool.options
                                .iter()
                                .take(usize::from(pool.count))
                                .map(|option| option.value.clone())
                        })
                        .collect(),
                    DecisionKind::Items { count, options } => options
                        .iter()
                        .take(usize::from(*count))
                        .map(|option| option.value.clone())
                        .collect(),
                    DecisionKind::Size { options } => {
                        options.first().cloned().into_iter().collect()
                    }
                    DecisionKind::Asi { .. } => {
                        let mut deltas = BTreeMap::new();
                        if let DecisionKind::Asi {
                            points, cap, open, ..
                        } = &decision.kind
                        {
                            let mut left = *points;
                            for key in open {
                                if left == 0 {
                                    break;
                                }
                                let give = left.min(*cap);
                                deltas.insert(key.clone(), give);
                                left -= give;
                            }
                        }
                        choices.asi.insert(decision.key.clone(), deltas);
                        continue;
                    }
                    DecisionKind::Unsupported { .. } => Vec::new(),
                };
                choices.picks.insert(decision.key.clone(), values);
            }
        }
        choices.equipment.insert(CLERIC.into(), "kit".into());
        choices.equipment.insert(ACOLYTE.into(), "kit".into());

        let done = to_do(&choices);
        assert_eq!(done, Vec::new(), "{done:?}");
        assert_eq!(submit_refusal(&done), Ok(()));
        // No usable scores, no submit either.
        assert_eq!(
            submit_refusal(&[Pending {
                step: Step::Abilities,
                key: "abilities".into(),
                label: "Assign your ability scores".into(),
            }]),
            Err("draft_incomplete")
        );
    }

    #[test]
    fn pending_rechecks_scores_against_the_current_rules() {
        let choices = DraftChoices {
            method: Some(AbilityMethod::Manual),
            base: Some(scores(18, 10, 10, 10, 10, 10)),
            ..DraftChoices::default()
        };
        let open = |rules: &CampaignRules| {
            pending(None, rules, &choices, "Al", None, &[])
                .into_iter()
                .any(|item| item.key == "abilities")
        };
        let manual = CampaignRules {
            ability_methods: vec![AbilityMethod::Manual],
            ..CampaignRules::default()
        };
        assert!(!open(&manual));
        // The GM turned manual scores off after the draft was finished.
        assert!(open(&CampaignRules::default()));
    }

    #[test]
    fn party_summary_reports_uncovered_skills() {
        let catalog = catalog();
        let cleric = format!("{CLERIC}#2wUNkQzS4vL9nR6D#1");
        let al = DraftChoices {
            class: Some(CLERIC.into()),
            background: Some(ACOLYTE.into()),
            picks: BTreeMap::from([(
                cleric.clone(),
                ["skills:his", "skills:med"].map(str::to_owned).to_vec(),
            )]),
            ..DraftChoices::default()
        };
        // The background grants Insight and Religion; the class picked History
        // and Medicine, leaving Persuasion (`skills:per`) of the five it offers.
        let found = decisions(&catalog, &al, 1);
        // Coverage is measured against every skill the world's content names,
        // not only the pools these drafts happened to open.
        let world = catalog_skills(&catalog);
        assert!(world.contains_key("skills:ath"), "{world:?}");
        let party = party_summary(&[party_row("Al", al.clone(), found)], &world);
        assert_eq!(
            party
                .skills
                .iter()
                .map(|(skill, names)| (skill.as_str(), names.as_slice()))
                .collect::<Vec<_>>(),
            [
                ("History", &["Al".to_owned()][..]),
                ("Insight", &["Al".to_owned()][..]),
                ("Medicine", &["Al".to_owned()][..]),
                ("Religion", &["Al".to_owned()][..]),
            ]
        );
        assert!(party.uncovered.contains(&"Persuasion".to_owned()));
        assert!(party.uncovered.contains(&"Athletics".to_owned()));
        assert!(!party.uncovered.contains(&"Insight".to_owned()));

        // A second character who picks Persuasion covers it (Athletics stays a gap), and the
        // table lists both names for History.
        let bryn = DraftChoices {
            class: Some(CLERIC.into()),
            picks: BTreeMap::from([(
                cleric,
                ["skills:his", "skills:per"].map(str::to_owned).to_vec(),
            )]),
            ..DraftChoices::default()
        };
        let party = party_summary(
            &[
                party_row("Al", al.clone(), decisions(&catalog, &al, 1)),
                party_row("Bryn", bryn.clone(), decisions(&catalog, &bryn, 1)),
            ],
            &world,
        );
        assert!(!party.uncovered.contains(&"Persuasion".to_owned()));
        assert!(party.uncovered.contains(&"Athletics".to_owned()));
        assert_eq!(
            party
                .skills
                .iter()
                .find(|(skill, _)| skill == "History")
                .map(|(_, names)| names.clone()),
            Some(vec!["Al".to_owned(), "Bryn".to_owned()])
        );

        // An unnamed draft counts under its player's name.
        let empty = party_summary(
            &[party_row("", DraftChoices::default(), Vec::new())],
            &BTreeMap::new(),
        );
        assert!(empty.skills.is_empty());
        assert!(empty.uncovered.is_empty());
    }

    #[test]
    fn details_accept_only_allowlisted_keys() {
        // The plan's allowlist, and nothing else.
        assert_eq!(
            DETAIL_LABELS.map(|(key, _)| key),
            [
                "alignment",
                "faith",
                "gender",
                "age",
                "height",
                "weight",
                "eyes",
                "hair",
                "skin",
                "appearance",
                "trait",
                "ideal",
                "bond",
                "flaw",
                "backstory",
            ]
        );
        assert_eq!(
            validate_detail("backstory", "  Raised in the temple.  "),
            Ok("Raised in the temple.".into())
        );
        // Every field is optional; an emptied one stores nothing.
        assert_eq!(validate_detail("alignment", "   "), Ok(String::new()));
        assert_eq!(
            validate_detail("password", "hunter2"),
            Err("invalid_detail")
        );
        assert_eq!(validate_detail("Backstory", "x"), Err("invalid_detail"));
        assert_eq!(
            validate_detail("ideal", &"x".repeat(DETAIL_MAX + 1)),
            Err("invalid_detail")
        );
        assert_eq!(
            validate_detail("ideal", &"x".repeat(DETAIL_MAX)),
            Ok("x".repeat(DETAIL_MAX))
        );
    }

    #[test]
    fn note_bounds() {
        assert_eq!(
            validate_note("  Pick a different deity. \n"),
            Ok("Pick a different deity.".into())
        );
        assert_eq!(validate_note("   "), Err("invalid_note"));
        assert_eq!(
            validate_note(&"x".repeat(NOTE_MAX + 1)),
            Err("invalid_note")
        );
        assert_eq!(
            validate_note(&"x".repeat(NOTE_MAX)),
            Ok("x".repeat(NOTE_MAX))
        );
        // The hit points the review step shows are level one's.
        assert_eq!(hit_points("d8", 13), 9);
        assert_eq!(hit_points("d12", 8), 11);
        // A CON modifier can only take a level-one character so far.
        assert_eq!(hit_points("d4", 1), 0);
        assert_eq!(hit_points("", 10), 0);
    }
}
