/**
 * Foundry Circle's companion module: the read-only compendium reads the broker's
 * catalog sync needs. The broker calls this API from its own GM page only;
 * nothing here writes a document and nothing is cached between calls.
 */

const MODULE_ID = "foundry-circle";
const API_VERSION = 1;
const MAX_TEXT = 4000;

/** Enricher syntax (`@UUID[...]{Label}`) reduced to its label. */
const ENRICHER = /@\w+\[[^\]]*\](?:\{([^}]*)\})?/g;

Hooks.once("init", () => {
  const module = game.modules.get(MODULE_ID);
  if ( module ) module.api = { apiVersion: API_VERSION, catalogPacks, catalogPage, uuidRedirects };
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
