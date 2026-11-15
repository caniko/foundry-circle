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

| File             | Documents                                                | Source paths                                                                                   |
| ---------------- | -------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `classes24.json` | Cleric (`phbclcCleric0000`)                              | `packs/_source/classes24/cleric/cleric.yml`                                                    |
| `origins24.json` | Acolyte (`phbbgAcolyte0000`), Human (`phbspHuman000000`) | `packs/_source/origins24/backgrounds/acolyte.yml`, `packs/_source/origins24/species/human.yml` |

## Transcription rules

- `text` is the description with `@X[...]`/`@X[...]{Label}` enrichers reduced to
  `Label` (or dropped) and the HTML flattened to text, exactly as the module's
  inert `DOMParser` pass produces it.
- `system` is the item's stored data minus `description`, restricted to the
  fields this slice and the next ones read: `source`, `identifier`, `levels`,
  `hd`, `primaryAbility`, `spellcasting`, `wealth`, `movement`, `senses`, `type`
  and `advancement`. Class advancements above level 1, and
  `system.startingEquipment` (slice 5's data), are omitted.
- `advancement` is in the keyed form dnd5e stores after migration; the Acolyte
  source still writes the legacy array form, which dnd5e converts on load.
- `traits` is what `dnd5e.documents.Trait` labels in the page: each pool key is
  passed through `mixedChoices(new Set(pool)).asSet()` and then `keyLabel`, keys
  sorted. The Acolyte's `languages:standard:*` pool is therefore the ten
  standard languages of `module/config.mjs#L4077-L4093` in key order, not a
  sample. Labels come from `lang/en.json` at 6.1.0.
