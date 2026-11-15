# Catalog fixtures

Hand-transcribed from the installable `dnd5e` 6.1.0 SRD 5.2 compendium sources
(`/data/scratch/tmp/dnd5e-research`, commit `0f69e6d5`), not exported from a
world; a live export replaces them when capture step C3 runs. Each file is a
`Vec<CatalogItem>` in the shape the `foundry-circle` module's `catalogPage`
returns.

Licence: the transcribed material is SRD 5.2 content, © Wizards of the Coast
LLC, licensed CC-BY-4.0. Attribution:

> This work includes material from the System Reference Document 5.2.1
> ("SRD 5.2.1") by Wizards of the Coast LLC, available at
> https://www.dndbeyond.com/srd. The SRD 5.2.1 is licensed under the Creative
> Commons Attribution 4.0 International License, available at
> https://creativecommons.org/licenses/by/4.0/legalcode.

| File               | Documents                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Source paths                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `classes24.json`   | Cleric (`phbclcCleric0000`), Divine Order (`phbclcDivineOrde`), Divine Order: Protector (`phbDivineOrderPr`), Divine Order: Thaumaturge (`phbDivineOrderTh`), Bard (`phbbrdBard000000`), Expertise (`phbbrdExpertise0`), Warlock (`phbwlkWarlock000`), Eldritch Invocations (`phbwlkEldritchIn`), Pact of the Blade (`phbinvPactBlade0`), Agonizing Blast (`phbinvAgonizingB`), Thirsting Blade (`phbinvThirstingB`), Rogue (`phbrgeRogue00000`), Expertise (`phbrgeExpertise0`), Weapon Mastery (`phbrgeWeaponMast`), Fighter (`phbftrFighter000`)                                                                                      | `packs/_source/classes24/cleric/cleric.yml`, `.../class-features/divine-order.yml`, `.../divine-order-protector.yml`, `.../divine-order-thaumaturge.yml`, `packs/_source/classes24/bard/bard.yml`, `packs/_source/classes24/bard/class-features/expertise.yml`, `packs/_source/classes24/warlock/warlock.yml`, `.../class-features/eldritch-invocations.yml`, `.../eldritch-invocation-options/{pact-of-the-blade,agonizing-blast,thirsting-blade}.yml`, `packs/_source/classes24/rogue/rogue.yml`, `packs/_source/classes24/rogue/class-features/{expertise,weapon-mastery}.yml`, `packs/_source/classes24/fighter/fighter.yml` |
| `feats24.json`     | Magic Initiate (`phbftMagicInitia`), Skilled (`phbftSkilled0000`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | `packs/_source/feats24/origin-feats/magic-initiate.yml`, `packs/_source/feats24/origin-feats/skilled.yml`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `origins24.json`   | Acolyte (`phbbgAcolyte0000`), Human (`phbspHuman000000`), Versatile (`phbsptVersatile0`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | `packs/_source/origins24/backgrounds/acolyte.yml`, `packs/_source/origins24/species/human.yml`, `.../human/versatile.yml`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `equipment24.json` | Chain Shirt (`phbarmChainShirt`), Shield (`phbarmShield0000`), Mace (`phbwepMace000000`), Priest's Pack (`phbagPriestsPack`), Chain Mail (`phbarmChainMail0`), Greatsword (`phbwepGreatsword`), Flail (`phbwepFlail00000`), Javelin (`phbwepJavelin000`), Dungeoneer's Pack (`phbagDungeoneers`), Studded Leather Armor (`phbarmStuddedLea`), Scimitar (`phbwepScimitar00`), Shortsword (`phbwepShortsword`), Longbow (`phbwepLongbow000`), Arrows (`phbamoArrows0000`), Quiver (`phbagQuiver00000`), Calligrapher's Supplies (`phbtulCalligraph`), Book (`phbagBook0000000`), Robe (`phbagRobe0000000`), Parchment (`phbagParchment00`) | `packs/_source/equipment24/armor/{medium/chain-shirt,shield,heavy/chain-mail,light/studded-leather-armor}.yml`, `packs/_source/equipment24/weapons/{simple-melee/mace,simple-melee/javelin,martial-melee/greatsword,martial-melee/flail,martial-melee/scimitar,martial-melee/shortsword,martial-ranged/longbow}.yml`, `packs/_source/equipment24/adventuring-gear/{priests-pack,dungeoneers-pack,quiver}/_container.yml`, `.../ammunition/arrows.yml`, `.../book.yml`, `.../robe.yml`, `.../parchment.yml`, `packs/_source/equipment24/tools/artisan/calligraphers-supplies.yml`                                                 |

## Transcription rules

- `text` is the description with `@X[...]`/`@X[...]{Label}` enrichers reduced to
  `Label` (or dropped) and the HTML flattened to text, exactly as the module's
  inert `DOMParser` pass produces it.
- `system` is the item's stored data minus `description`, restricted to the
  fields this slice and the next ones read: `source`, `identifier`, `levels`,
  `hd`, `primaryAbility`, `spellcasting`, `wealth`, `movement`, `senses`, `type`,
  `prerequisites` and `advancement`. Class advancements above level 1, and
  `system.startingEquipment` (slice 5's data), are omitted. Bard keeps its
  level-2 `ItemGrant` because Expertise (mode `expertise`) arrives through it;
  an ItemChoice keeps its whole `choices` map, since dnd5e keys one count per
  class level.
- Rogue keeps its level-1 advancements only: its `ItemGrant` (which carries
  Expertise and Weapon Mastery), its Hit Points advancement, and the four
  primary Trait advancements plus Armor Training. Its multiclass dip's skill
  Trait (`classRestriction: secondary`) is omitted, and so are the features its
  grant names that no slice reads yet (Sneak Attack, Thieves' Cant). Its Weapon
  Mastery pool is the full `weapon:*` expansion: all 38 weapons of
  `CONFIG.DND5E.weaponIds` as `weapon:<category>:<id>`, labelled from
  `equipment24`.
- `advancement` is in the keyed form dnd5e stores after migration; the Acolyte
  source still writes the legacy array form, which dnd5e converts on load, and
  Magic Initiate's is converted here the same way.
- `traits` is what `dnd5e.documents.Trait` labels in the page: each pool key is
  passed through `mixedChoices(new Set(pool)).asSet()` and then `keyLabel`, keys
  sorted. The Acolyte's `languages:standard:*` pool is therefore the ten
  standard languages of `module/config.mjs#L4077-L4093` in key order, not a
  sample; Bard's `skills:*` is all eighteen skills and its `tool:music:*` the
  ten instruments of `CONFIG.DND5E.tools` whose `system.type.value` is `music`,
  named from the `equipment24` pack. Labels come from `lang/en.json` at 6.1.0.
- Skilled's `skills:*`/`tool:*` pools are a synthetic four-option sample
  (Insight, Perception, Stealth, Thieves' Tools), enough to answer its three
  proficiencies; the full expansion is all eighteen skills and every tool of
  `CONFIG.DND5E.tools`.
- Only the documents a slice reads are transcribed. Cleric's Spellcasting
  (`phbclcSpellcasti`) is omitted because no slice reads it before spells ship
  (slice 8); Bardic Inspiration and Jack of All Trades for the same reason, and
  Warlock's Pact Magic. Human also grants Resourceful and Skillful, which no
  slice reads before the origin step. The three Warlock invocations are the ones
  the prerequisite test reads; Magic Initiate's two spell ItemChoices are kept,
  since the walker must skip their real shape until slice 8.
- Slice five adds the classes' and the Acolyte's real `system.startingEquipment`
  and `wealth`, and the Fighter (whose top-level `OR` is the multi-kit case).
  The Fighter carries no description and no advancements: no slice renders a
  class's own text, and its level-one features would need documents the class
  step does not read. The `equipment24` documents carry only an `identifier`
  and their source block — no slice reads an equipment item's price, weight or
  description — so their `text` is empty rather than invented.
- `equipment.labels` holds what dnd5e's `EquipmentEntryData#label` getter words
  itself: the counted currencies (`7 GP`) and the choosable categories
  (`any holy focus`, from `CONFIG.DND5E.focusTypes` and
  `DND5E.TRAIT.Choose.AnyUncounted`). A `linked` entry's own label is HTML
  anchoring the document, which the builder replaces with the item's name, so
  those labels are not transcribed, and neither are the `AND`/`OR` groups' —
  the builder words a kit from its children. A module that sends no labels
  leaves the builder on the entry's type.
