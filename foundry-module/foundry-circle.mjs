/**
 * Foundry Circle's companion module: the compendium reads the broker's catalog
 * sync needs, and the one character write the broker's approval performs. The
 * broker calls this API from its own GM page only; nothing is cached between
 * calls and no caller ever supplies JavaScript.
 */

const MODULE_ID = "foundry-circle";
const API_VERSION = 1;
const MAX_TEXT = 4000;

/** Enricher syntax (`@UUID[...]{Label}`) reduced to its label. */
const ENRICHER = /@\w+\[[^\]]*\](?:\{([^}]*)\})?/g;

Hooks.once("init", () => {
  const module = game.modules.get(MODULE_ID);
  if ( module ) module.api = {
    apiVersion: API_VERSION, catalogPacks, catalogPage, uuidRedirects, createCharacter
  };
});

/** Every Item pack this world offers. */
function catalogPacks() {
  return game.packs
    .filter(pack => pack.documentName === "Item")
    .map(pack => ({
      id: pack.collection,
      label: pack.metadata.label,
      packageName: pack.metadata.packageName ?? null,
      sourceBook: pack.metadata.flags?.dnd5e?.sourceBook ?? null,
      rulesVersion: pack.metadata.flags?.dnd5e?.rulesVersion ?? null,
      count: pack.index.size
    }))
    .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/** One id-ordered page of a pack; the broker pages with `after`. */
async function catalogPage({pack: id, after = null, limit = 50} = {}) {
  const pack = game.packs.get(id);
  if ( !pack ) return { error: "not_found", message: "unknown pack" };
  if ( pack.documentName !== "Item" ) return { error: "unsupported", message: "not an Item pack" };
  // The index is already loaded at login, so paging costs no request of its own.
  const ids = [...pack.index.keys()]
    .sort()
    .filter(_id => after === null || _id > after)
    .slice(0, limit);
  if ( !ids.length ) return { items: [], nextCursor: null };
  const documents = new Map((await pack.getDocuments({_id__in: ids})).map(doc => [doc.id, doc]));
  const items = [];
  for ( const _id of ids ) {
    const document = documents.get(_id);
    if ( document ) items.push(await catalogItem(document));
  }
  return { items, nextCursor: ids.length === limit ? ids[ids.length - 1] : null };
}

/** One document in the shape the broker stores. */
async function catalogItem(document) {
  const source = document.toObject();
  const system = source.system ?? {};
  const description = system.description?.value ?? "";
  const traits = await traitOptions(system.advancement);
  delete system.description;
  const item = {
    uuid: document.uuid,
    name: document.name,
    type: document.type,
    img: document.img ?? null,
    text: plainText(description),
    system,
    traits
  };
  // Additive: a broker older than this field ignores it, and an item without
  // starting equipment sends none.
  const equipment = equipmentLabels(document);
  if ( equipment ) item.equipment = equipment;
  return item;
}

/**
 * dnd5e's own label for each starting-equipment entry, by entry id, so the
 * broker never keeps a label table of its own. A linked entry's label is an
 * HTML link to the document, which the broker replaces with the item's name.
 * Other systems have no such entries.
 */
function equipmentLabels(document) {
  if ( game.system.id !== "dnd5e" ) return null;
  const entries = document.system?.startingEquipment;
  if ( !Array.isArray(entries) || !entries.length ) return null;
  const labels = {};
  for ( const entry of entries ) {
    if ( entry?._id && typeof entry.label === "string" ) labels[entry._id] = entry.label;
  }
  return { labels };
}

/**
 * The world's compendium uuid redirects, as `{from, to}` pack prefixes: dnd5e
 * points a premium module's packs at the SRD packs while that module is not
 * active, and the broker applies them before any lookup.
 */
function uuidRedirects() {
  const redirects = CONFIG.compendium?.uuidRedirects ?? {};
  return Object.entries(redirects)
    .filter(([from, to]) => from.startsWith("Compendium.") && typeof to === "string")
    .map(([from, to]) => ({ from, to }))
    .sort((a, b) => (a.from < b.from ? -1 : a.from > b.from ? 1 : 0));
}

/**
 * Create one character from the broker's approved draft: the one world write.
 *
 * The broker sends the decisions its own walker resolved, keyed
 * `<compendium uuid>#<advancement id>#<level>`; this builds the actor in memory,
 * lets dnd5e's own advancement code apply them in the order `AdvancementManager`
 * uses for a new class item, and writes once. A failure throws before the
 * `Actor.create`, so no half-built character is left in the world.
 *
 * @param {object} args  The validated `CharacterCreate` argument.
 * @returns {Promise<object>} `{id, created, manual}`, or `{error, message}`.
 */
async function createCharacter(args) {
  if ( game.system.id !== "dnd5e" ) return { error: "unsupported", message: "not_dnd5e" };
  if ( !args?.draftId || !args.name || !args.ownerUserId || !Array.isArray(args.roots) ) {
    return { error: "invalid", message: "draftId, name, ownerUserId and roots are required" };
  }
  const manual = Array.isArray(args.manual) ? args.manual : [];
  // Retry-safe: an approval that timed out after the write returns its actor
  // rather than creating a second one.
  try {
    // A replay still completes the owner's assignment, which may be what failed.
    const existing = game.actors.find(actor => actor.flags?.[MODULE_ID]?.draftId === args.draftId);
    if ( existing ) {
      await assignCharacter(args.ownerUserId, existing.id);
      return { id: existing.id, created: false, manual };
    }
    const actor = await buildCharacter(args);
    // The document class the world configured (`Actor.implementation`,
    // common/abstract/document.mjs:182; `Document.create`, :721).
    const created = await Actor.create(actor.toObject());
    if ( !created ) throw new Error("Foundry refused the character");
    await assignCharacter(args.ownerUserId, created.id);
    return { id: created.id, created: true, manual };
  } catch ( error ) {
    console.error(`${MODULE_ID} | character creation failed`, error);
    return { error: "invalid", message: String(error?.message ?? error) };
  }
}

/**
 * The in-memory actor the recipe builds, or a thrown error naming what the
 * world is missing. Nothing here writes.
 * @param {object} args  The validated `CharacterCreate` argument.
 * @returns {Promise<Actor5e>}
 */
async function assignCharacter(userId, actorId) {
  // The player owns the sheet and their user opens it (`User#character`,
  // common/documents/user.mjs:57; `Document#update`, common/abstract/document.mjs:740).
  // A cancelled update resolves to undefined, so the write is checked.
  const user = game.users.get(userId);
  if ( !user ) throw new Error("the owner user does not exist");
  if ( user.character?.id === actorId ) return;
  const updated = await user.update({ character: actorId });
  if ( !updated ) throw new Error("Foundry refused the character assignment");
}

async function buildCharacter(args) {
  const choices = args.choices ?? {};
  const sources = new Map(); // in-page item id → the compendium uuid it came from
  const applied = new Set(); // `<uuid>#<advancement id>#<level>`, once each
  const level = args.level ?? 1;
  // `Actor.implementation` is the system's actor class
  // (common/abstract/document.mjs:182); ownership uses the named levels, never
  // literals (common/constants.mjs:470-497).
  const actor = new Actor.implementation({
    name: args.name,
    type: "character",
    system: {
      abilities: Object.fromEntries(
        Object.entries(args.abilities ?? {}).map(([ability, value]) => [ability, { value }])
      ),
      details: { ...(args.details ?? {}), biography: { value: args.biography ?? "" } }
    },
    ownership: {
      default: CONST.DOCUMENT_OWNERSHIP_LEVELS.NONE,
      [args.ownerUserId]: CONST.DOCUMENT_OWNERSHIP_LEVELS.OWNER
    },
    flags: { [MODULE_ID]: { draftId: args.draftId } }
  });

  /**
   * Embed one compendium document, with the fresh id and `_stats` a granted item
   * gets (module/documents/advancement/advancement.mjs:381-397).
   */
  const add = async uuid => {
    const document = await fromUuid(uuid); // client/utils/helpers.mjs:161
    if ( !document ) throw new Error(`the world has no ${uuid}`);
    // The system's own import: a fresh id and `_stats.compendiumSource`, no
    // world ownership (common/abstract/world-collection.mjs:109).
    const { _stats } = game.items.fromCompendium(document);
    const data = document.clone({ _stats, _id: foundry.utils.randomID() }, { keepId: true }).toObject();
    actor.updateSource({ items: [data] }); // common/abstract/data.mjs:669
    const item = actor.items.get(data._id);
    if ( !item ) throw new Error(`could not embed ${uuid}`);
    sources.set(item.id, uuid);
    return item;
  };

  /** The compendium uuid an embedded item came from: a root's, or the
   *  `flags.dnd5e.sourceId` its advancement stamped on it. */
  const sourceOf = item => sources.get(item.id) ?? item.flags?.dnd5e?.sourceId ?? null;

  /**
   * Apply one item's advancements at one level: the broker's answer for the
   * choice, else the system's own automatic value, and `false` there means a
   * choice nobody made (module/documents/advancement/advancement.mjs:330-350).
   */
  const apply = async (item, at) => {
    // `advancement.byLevel`, built per level by `Item#_prepareAdvancement`
    // (module/documents/item.mjs:625-637).
    for ( const advancement of item.advancement?.byLevel?.[at] ?? [] ) {
      if ( !advancement.appliesToClass ) continue; // advancement.mjs:110
      // ponytail: spell choices wait for slice 8, exactly as the broker's walker
      // skips them; the player picks those spells in Foundry for now.
      const configuration = advancement.configuration ?? {};
      if ( (advancement.type === "ItemChoice")
        && ((configuration.type === "spell") || (configuration.restriction?.type === "spell")) ) continue;
      // ponytail: one key per source document, as the broker's decision keys;
      // add an occurrence index if the content ever grants a feature twice.
      const key = `${sourceOf(item)}#${advancement.id}#${at}`;
      if ( applied.has(key) ) continue;
      applied.add(key);
      const data = choices[key] ?? await advancement.automaticApplicationValue(at);
      if ( data === false ) throw new Error(`the draft has no answer for ${key}`);
      await advancement.apply(at, data);
    }
  };

  /**
   * One item at one level, then every item the applies created, lowest level
   * first: the synthetic steps a level's new items get
   * (module/applications/advancement/advancement-manager.mjs:729-760).
   */
  const run = async (item, at) => {
    const before = new Set(actor.items.map(embedded => embedded.id));
    await apply(item, at);
    for ( const added of actor.items ) {
      if ( before.has(added.id) ) continue;
      for ( let inner = 0; inner <= at; inner++ ) await run(added, inner);
    }
  };

  // Species and background first, as level 0 is applied before the class
  // (module/applications/advancement/advancement-manager.mjs:401-425).
  for ( const root of args.roots.filter(entry => entry.kind !== "class") ) {
    await run(await add(root.uuid), 0);
  }
  const classRoot = args.roots.find(entry => entry.kind === "class");
  if ( !classRoot ) throw new Error("the draft has no class");
  const classItem = await add(classRoot.uuid);
  // A new class starts at level 0 and is the character's original class, which
  // is what its level-one automatic values read
  // (module/applications/advancement/advancement-manager.mjs:241-261).
  classItem.updateSource({ "system.levels": 0 });
  actor.updateSource({ "system.details.originalClass": classItem.id });
  for ( let at = 1; at <= level; at++ ) {
    const species = actor.items.find(item => item.type === "race");
    if ( species ) await run(species, at);
    await run(classItem, at);
    for ( const item of [...actor.items] ) {
      if ( (item.id === classItem.id) || (item.id === species?.id) ) continue;
      await run(item, at);
    }
    classItem.updateSource({ "system.levels": at });
    // A level's derived data is what the next level reads
    // (common/abstract/data.mjs:524).
    actor.reset();
  }

  // The kit's documents, each with the quantity the rules list. A pack arrives
  // with its contents, as a sheet drop creates it: the system's own
  // `createWithContents` gives every item a fresh id, `_stats` from
  // `fromCompendium`, and the pack's id as `system.container`
  // (module/documents/item.mjs, `Item5e.createWithContents`).
  for ( const entry of args.items ?? [] ) {
    const document = await fromUuid(entry.uuid);
    if ( !document ) throw new Error(`the world has no ${entry.uuid}`);
    const [data, ...contents] = await CONFIG.Item.documentClass.createWithContents([document]);
    data.system.quantity = entry.quantity;
    actor.updateSource({ items: [data, ...contents] });
  }

  // Starting gold: the kits' own coin plus any wealth formula the draft chose
  // instead of a kit (client/dice/roll.mjs:29,274).
  let gp = args.gp ?? 0;
  for ( const formula of args.wealth ?? [] ) {
    const roll = await new Roll(formula, actor.getRollData()).evaluate();
    gp += Math.floor(roll.total);
  }
  actor.updateSource({ "system.currency.gp": gp });

  // A level-one character starts at full health: the derived maximum is the
  // class's hit points plus Constitution
  // (module/data/actor/character.mjs:248-258).
  actor.reset();
  const hp = actor.system.attributes.hp;
  if ( hp.max !== null ) actor.updateSource({ "system.attributes.hp.value": hp.max });
  return actor;
}

/**
 * Each Trait advancement's grants and pools, labelled by the system itself, so
 * the builder never resolves a trait key or expands a wildcard.
 */
async function traitOptions(advancement = {}) {
  if ( game.system.id !== "dnd5e" ) return {};
  const traits = {};
  for ( const entry of Object.values(advancement ?? {}) ) {
    if ( entry.type !== "Trait" ) continue;
    const choices = [];
    for ( const choice of entry.configuration?.choices ?? [] ) {
      choices.push({ count: choice.count, options: await labelled(choice.pool ?? []) });
    }
    traits[entry._id ?? entry.id] = {
      grants: await labelled(entry.configuration?.grants ?? []),
      choices
    };
  }
  return traits;
}

/** Keys expanded to the config's own entries, then labelled. */
async function labelled(keys) {
  if ( !keys.length ) return [];
  const known = (await dnd5e.documents.Trait.mixedChoices(new Set(keys))).asSet();
  return [...known]
    .sort()
    .map(key => ({ key, label: dnd5e.documents.Trait.keyLabel(key) }));
}

/**
 * The description as plain text. `DOMParser` builds an inert document, so no
 * script, image or enricher in the source runs or is fetched.
 */
function plainText(html) {
  const reduced = html.replace(ENRICHER, (match, label) => label ?? "");
  const body = new DOMParser().parseFromString(reduced, "text/html").body;
  return (body.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, MAX_TEXT);
}
