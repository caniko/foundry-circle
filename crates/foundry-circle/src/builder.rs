//! Session-zero rules and the pure half of the character builder.
//!
//! Everything here is I/O-free so the wasm bundle and the server agree on the
//! rules; `builder_store` owns the SQL and `builder_ui` the pages.

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
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

/// Everything a player has decided so far. Only the slice-one fields exist yet.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DraftChoices {
    pub method: Option<AbilityMethod>,
    pub base: Option<Scores>,
}

/// One change the builder sends; the server re-validates all of it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DraftPatch {
    Name(String),
    Abilities { method: AbilityMethod, base: Scores },
}

/// Where an unfinished draft needs the player.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    Details,
    Abilities,
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
pub fn pending(choices: &DraftChoices, name: &str, rolls: Option<&Rolls>) -> Vec<Pending> {
    let mut pending = Vec::new();
    if name.trim().is_empty() {
        pending.push(Pending {
            step: Step::Details,
            key: "name".into(),
            label: "Name your character".into(),
        });
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
    pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

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
            ["Acolyte", "Human"]
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
    }

    #[test]
    fn pending_lists_name_and_abilities() {
        let empty = pending(&DraftChoices::default(), "  ", None);
        assert_eq!(empty.len(), 2);
        assert_eq!(empty[0].step, Step::Details);
        assert_eq!(empty[0].key, "name");
        assert_eq!(empty[1].step, Step::Abilities);
        assert_eq!(empty[1].key, "method");

        let rolling = pending(
            &DraftChoices {
                method: Some(AbilityMethod::Roll),
                base: None,
            },
            "Al",
            None,
        );
        assert_eq!(rolling.len(), 1);
        assert_eq!(rolling[0].key, "roll");

        let done = pending(
            &DraftChoices {
                method: Some(AbilityMethod::StandardArray),
                base: Some(scores(15, 14, 13, 12, 10, 8)),
            },
            "Al",
            None,
        );
        assert!(done.is_empty(), "{done:?}");
        assert_eq!(modifier(8), -1);
        assert_eq!(modifier(15), 2);
    }
}
