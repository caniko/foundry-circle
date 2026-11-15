# Changelog

All notable changes to Foundry Circle are documented here.

## Unreleased

- Add the equipment step to the character builder: a draft chooses its class's
  kit or gold and its background's, within the campaign's equipment mode. The
  builder reads dnd5e's own `system.startingEquipment` and supports both a
  single top-level `AND` and the 2024 classes' top-level `OR` (the fighter's two
  kits, each with its own gold), treats `count: null` as one, adds a kit's own
  currency to its gold, embeds each `linked` document with its count, and lists
  a category such as a holy symbol — or a choice nested inside a kit — as a line
  the DM hands out. Kit items resolve against every synced pack and through the
  world's compendium uuid redirects, which the companion module's new
  `uuidRedirects()` reports and the sync stores in `builder_redirects`: the
  2024 kits name the premium Player's Handbook's pack, and dnd5e redirects that
  prefix to the SRD pack while the module is inactive. Entry labels come from
  the module's reading of dnd5e's own `EquipmentEntryData#label`, so no label
  table lives in the broker; a module without either addition still syncs.
  Answers live in `DraftChoices::equipment`, `update_draft` re-validates each
  against the chosen owners' options, and a replaced class or background prunes
  its answer like its picks.
- Add the origin step to the character builder: a draft picks a species and a
  background from the campaign's enabled sources and the console walks their
  dnd5e advancements like it walks a class's, so a size with more than one
  option is a radio group, the Human's Versatile feat opens the origin feat it
  grants, and the Acolyte's background bonus is the 2024 +2/+1 or +1/+1/+1 over
  its Intelligence, Wisdom and Charisma, never above 20. `validate_asi` is the
  one rule the page and `update_draft` share; ability bonuses live in
  `DraftChoices::asi` and are pruned with the picks, the "to do" list gains
  Species, Background and every unresolved size or bonus, and each abilities
  row reads base + bonus = total with the modifier of the total.
- Add the class step to the character builder: a draft picks a class from the
  campaign's enabled sources and the console walks that class's dnd5e
  advancements to list every decision a level-one character owes, including
  choices that hang off a granted feature such as Divine Order. Trait pools are
  checkbox groups that stop at their count, item choices radio groups, options
  the draft already has are marked "already have", the "to do" list links each
  open decision to its step, and `update_draft` re-validates every pick against
  the decision it names. Choosing another class clears the old class's picks,
  including those of a feature that class granted. A trait mode offers only what
  the draft can still take, a feat whose prerequisites it does not meet is not
  offered, and a class the enabled sources no longer hold puts the Class step
  back on the to do list.
- Add the rules-content sync to the character builder: a GM snapshots the
  running dnd5e world's Item packs into `builder_catalog` and picks the packs
  players may build from. The reads come from a new companion Foundry module
  (`foundry-module/`, packaged as `packages.<system>.foundry-circle-module`)
  whose `game.modules.get("foundry-circle").api` the driver calls with fixed
  scripts, failing closed with `module_missing`/`module_outdated`; the source
  toggles, the last sync time and the SRD 5.2.1 attribution render in the
  console.
- Add session zero to the console: a GM opens character creation for an adopted
  world and chooses the ability methods, players create drafts and set ability
  scores by the standard array, point buy, a server roll (4d6 drop lowest, OS
  random, written once per draft) or a manual array, and the GM board lists
  every draft with its honest rolls. Drafts and rules live in PostgreSQL; no
  Foundry write happens until approval.
- Package the SQL migrations with the broker: the Nix source filter dropped
  `crates/foundry-circle/migrations`, so the packaged server ran an empty
  migrator and every login failed on a missing table. A `packaged-migrations`
  check now fails when the packaged binary lacks any migration.
- Let players and GMs join the running game from the desktop app. The session
  endpoint now reports a `joinUrl`, the play host's `/_circle/enter`, once the
  world is running and adopted; `foundry-circle-upload` exposes `join_url`
  using the stored session, and the window opens that URL in the system browser
  so the play host runs its own OIDC login and Foundry handoff.
- Add the static landing page for `vtt.tartanoglu.com` as
  `packages.<system>.site` (built with `plinth`), describing the table and
  linking sign-in at `/api/auth/login` and the lobby at `/api/console`.
- Foundry Circle is now licensed `EUPL-1.2` (see `LICENSE`), and `media-oxide`
  is consumed under its new `MIT OR Apache-2.0` crate license.
- Ship the game-master tools on every platform: `foundry-circle-upload` and
  `foundry-circle-desktop` join the portable Linux archive, a MinGW-crossed
  Windows zip carries both `.exe` files and the WebView2 loader, and a native
  macOS arm64 archive holds both binaries. The release workflow builds
  `packages.<system>.release-bundle` on `ubuntu-24.04` and `macos-15` and merges
  the archives into the single signed GitHub release; `[release.artifacts]
platforms` names the matrix, and the `binstall-metadata` checks carry the
  three platforms' target triples.
- Target Dioxus 0.8.0-alpha.3 (tartan-ui pinned to its 0.8 branch) and build
  `dioxus-cli` 0.8.0-alpha.3 plus the matching `wasm-bindgen-cli` 0.2.130 in this
  flake: nixpkgs' 0.7 `dx` cannot build the 0.8 crates and ships no wasm-bindgen
  for the ABI pair the lockfile needs.
- Add `foundry-circle-desktop`, the uploader as a Dioxus desktop window: browser
  sign-in, the converted kind and options, drag-and-drop or picker, and a
  one-at-a-time queue that reuses `foundry-circle-upload` without a second
  protocol.
- Add GM/admin asset upload at `PUT /api/v1/assets/{kind}/{name}`, re-validating the media-oxide Foundry asset policy server side (kind, normalised name, extension, magic bytes, safe SVG), writing without ever replacing an existing file, and serving the result from a Foundry-mounted directory.
- Add native client sign-in (RFC 8252 loopback + PKCE): `GET /api/auth/native` starts the Rauthy login, the callback hands a single-use code to the client's loopback listener, and `POST /api/auth/native/token` exchanges it for the client's own session.
- Add `foundry-circle-upload`: browser PKCE sign-in, stored broker session, and
  `push` to convert images and audio with ffmpeg and upload them as Foundry
  tokens, maps, tiles, artwork, and audio.
- Raise the minimum Rust version to 1.93, which `media-oxide` requires for its
  Foundry conversion policy.
- Implement typed world document reads with SHA-256 ETags and the chat, dice, scene, combat, and playlist commands; events and JSON-patch mutations stay unsupported.
- Add credential-safe Foundry VTT account, signed-URL, archive, and offline-cache acquisition.
- Validate Foundry v13/v14 package metadata and emit SHA-256 plus Nix SRI provenance.
- Add fail-closed declarative module/system add, version, rollback, and tombstone reconciliation.
- Add the Nix package and NixOS module interfaces consumed by Canix.
- Fix the local PostgreSQL URL (empty-host userinfo form rejected by sqlx), report ready once the database is up even with no world launched, and add GM/player lobby roles (`oidc.gmGroup`, `foundryWorld.playBaseUrl`) with a live-game card from Foundry `/api/status`.

## 0.1.0 - 2026-07-16

- Bootstrap the PostgreSQL-backed Rust control plane and Dioxus/tartan-ui operator console.
