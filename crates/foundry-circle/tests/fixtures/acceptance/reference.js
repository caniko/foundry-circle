// The C4 reference character, built the way a player builds one: a new actor,
// scores typed on the sheet, each root dropped on the sheet, every advancement
// answered through dnd5e's own flow forms and buttons, and the starting
// equipment dragged in. It never calls the companion module, an advancement's
// `apply` or `automaticApplicationValue`. Evaluated only in the acceptance
// harness's own browser page (`src/acceptance.rs`), never by the broker.
window.__c4c5 = (() => {
  const POLL = 50;
  const LIMIT = 20000;

  const until = async (what, test, detail = () => "") => {
    const deadline = Date.now() + LIMIT;
    for (;;) {
      const value = await test();
      if (value) return value;
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${what} ${detail()}`);
      await new Promise(resolve => setTimeout(resolve, POLL));
    }
  };

  // The manager registers under a generated id (`app-N`), not its `id` getter.
  const manager = actorId => [...foundry.applications.instances.values()].find(app =>
    (app instanceof dnd5e.applications.advancement.AdvancementManager) && (app.actor?.id === actorId));
  // A V2 flow's element is the form; a V1 `FormApplication` flow's is jQuery.
  const flowElement = flow => flow?.element instanceof HTMLElement ? flow.element : flow?.element?.[0] ?? null;
  // An open manager showing a rendered step; a completed one lingers through
  // its close animation with no step, which is not open.
  const showing = actorId => {
    const m = manager(actorId);
    return m?.rendered && flowElement(m.step?.flow)?.isConnected ? m : null;
  };
  // What the manager is doing, for a timeout's message.
  const seen = actorId => {
    const m = manager(actorId);
    return JSON.stringify({
      manager: !!m, rendered: m?.rendered ?? null, steps: m?.steps?.length ?? null,
      flow: m?.step?.flow?.constructor.name ?? null, element: !!flowElement(m?.step?.flow),
      apps: [...foundry.applications.instances.keys()],
      notes: [...document.querySelectorAll("#notifications li")].map(li => li.textContent.trim()).slice(-3)
    });
  };

  const KINDS = { Trait: "trait", Size: "size", AbilityScoreImprovement: "asi", ItemChoice: "items" };

  const state = actorId => {
    const m = showing(actorId);
    if (!m) return { open: false };
    const flow = m.step.flow;
    const advancement = flow.advancement;
    const el = flowElement(flow);
    const type = advancement?.constructor.typeName ?? null;
    const choosable = {
      Trait: () => !!el.querySelector('select[name="added"]'),
      Size: () => !!el.querySelector('select[name="size"]'),
      AbilityScoreImprovement: () => !!el.querySelector('[data-action="increase"]'),
      ItemChoice: () => !!el.querySelector("dnd5e-checkbox")
    }[type]?.() ?? false;
    return {
      open: true,
      owner: advancement?.item?.name ?? null,
      kind: KINDS[type] ?? type,
      level: Number(flow.level ?? 0),
      id: advancement?.id ?? null,
      title: advancement?.title ?? null,
      choosable,
      spellChoice: type === "ItemChoice" && advancement.configuration?.type === "spell",
      error: m.step.error?.message ?? null
    };
  };

  // Set a form control the way a click or a selection does: the flow form
  // submits on change and re-renders.
  const choose = (control, value) => {
    control.value = value;
    control.dispatchEvent(new Event("change", { bubbles: true }));
  };

  const answer = async (actorId, kind, answer) => {
    const flow = () => showing(actorId).step.flow;
    switch (kind) {
      case "trait":
        for (const key of answer.values) {
          const select = flowElement(flow()).querySelector('select[name="added"]');
          if (![...(select?.options ?? [])].some(option => option.value === key)) {
            return { error: "not_offered", key, offered: [...(select?.options ?? [])].map(o => o.value) };
          }
          choose(select, key);
          await until(`trait ${key}`, () => flowElement(flow()).querySelector(`li[data-key="${CSS.escape(key)}"]`));
        }
        break;
      case "size": {
        const [size] = answer.values;
        choose(flowElement(flow()).querySelector('select[name="size"]'), size);
        await until(`size ${size}`, () => flow().advancement.value.size === size);
        break;
      }
      case "asi":
        for (const [key, points] of Object.entries(answer.asi)) {
          for (let point = 0; point < points; point++) {
            const input = () => flowElement(flow()).querySelector(`li[data-score="${key}"] input[type=number]`);
            const before = input().valueAsNumber;
            const plus = flowElement(flow()).querySelector(`li[data-score="${key}"] [data-action="increase"]`);
            if (!plus) return { error: "not_offered", key };
            plus.click();
            await until(`${key} +1`, () => input()?.valueAsNumber === before + 1);
          }
        }
        break;
      case "items":
        for (const name of answer.items) {
          const row = [...flowElement(flow()).querySelectorAll("li[data-uuid]")]
            .find(li => li.querySelector(".title")?.textContent.trim() === name);
          const box = row?.querySelector("dnd5e-checkbox");
          if (!box) return { error: "not_offered", name };
          const uuid = row.dataset.uuid;
          box.click();
          await until(`item ${name}`, () =>
            flowElement(flow()).querySelector(`li[data-uuid="${CSS.escape(uuid)}"] dnd5e-checkbox`)?.checked);
        }
        break;
      default:
        return { error: "unsupported", kind };
    }
    return state(actorId);
  };

  // Next or Complete. Waits for the next rendered step, a step error, or the
  // manager to be gone from the registry (completion applied and closed). A
  // re-render between steps leaves it briefly unrendered, and a completed one
  // lingers through its close animation; neither is the end.
  const next = async actorId => {
    const m = showing(actorId);
    const step = m.step;
    m.element.querySelector('[data-action="next"],[data-action="complete"]').click();
    await until("the next step", () => {
      const current = manager(actorId);
      if (!current) return true;
      // An error re-renders the same step; wait for it so `state()` carries it.
      if (current.step === step && step.error) return showing(actorId);
      return current.step && (current.step !== step) && showing(actorId);
    }, () => seen(actorId));
    return state(actorId);
  };

  // The starting equipment a player reads off the class or background and
  // drags in: the chosen option of a top-level choice, each linked item with
  // its count; anything that names a category ("any holy symbol") is a
  // player's own pick and is reported, not bought.
  const kit = async (actorId, rootUuid, option) => {
    const actor = game.actors.get(actorId);
    const root = await fromUuid(rootUuid);
    const plan = { items: [], gp: 0, manual: [] };
    const walk = entry => {
      switch (entry.type) {
        case "AND": entry.children.forEach(walk); break;
        case "linked": plan.items.push({ uuid: entry.key, count: entry.count ?? 1 }); break;
        case "currency":
          if (entry.key === "gp") plan.gp += entry.count ?? 0;
          else plan.manual.push(entry.label);
          break;
        default: plan.manual.push(entry.label);
      }
    };
    const top = root.system.startingEquipment.filter(entry => !entry.group).sort((a, b) => a.sort - b.sort);
    for (const entry of top) {
      if (entry.type === "OR") {
        const chosen = entry.children[option];
        if (!chosen) return { error: "no_option", root: root.name, option };
        walk(chosen);
      } else walk(entry);
    }
    for (const { uuid, count } of plan.items) {
      const item = await fromUuid(uuid);
      if (!item) return { error: "missing_item", uuid };
      const created = await actor.sheet._onDropCreateItems(new DragEvent("drop"), [item]);
      const own = created?.find(doc => doc.name === item.name);
      if (own && count > 1) await own.update({ "system.quantity": count });
    }
    if (plan.gp) await actor.update({ "system.currency.gp": (actor.system.currency.gp ?? 0) + plan.gp });
    return plan;
  };

  return {
    // A player creating an actor and typing the six scores on the sheet.
    async create({ name, base }) {
      const actor = await Actor.implementation.create({ name, type: "character" });
      const scores = Object.fromEntries(Object.entries(base).map(([key, value]) =>
        [`system.abilities.${key}.value`, value]));
      await actor.update(scores);
      await actor.sheet.render({ force: true });
      await until("the sheet", () => actor.sheet.rendered);
      return actor.id;
    },
    // A player dragging one compendium document onto the sheet.
    async drop({ actorId, uuid }) {
      const actor = game.actors.get(actorId);
      await actor.sheet._onDropCreateItems(new DragEvent("drop"), [await fromUuid(uuid)]);
      await until("the advancement prompt", () => showing(actorId), () => seen(actorId));
      return state(actorId);
    },
    state: ({ actorId }) => state(actorId),
    answer: ({ actorId, kind, answer: given }) => answer(actorId, kind, given),
    next: ({ actorId }) => next(actorId),
    kit: ({ actorId, root, option }) => kit(actorId, root, option),
    snapshot({ actorId }) {
      const actor = game.actors.get(actorId);
      const system = actor.toObject().system;
      const flags = item => item.flags?.dnd5e ?? {};
      return {
        abilities: system.abilities,
        skills: system.skills,
        tools: system.tools,
        traits: system.traits,
        hp: { value: actor.system.attributes.hp.value, max: actor.system.attributes.hp.max },
        currency: system.currency,
        items: actor.items.map(item => ({
          name: item.name,
          type: item.type,
          quantity: item.system.quantity ?? null,
          sourceId: flags(item).sourceId ?? null,
          advancementOrigin: flags(item).advancementOrigin ?? null
        }))
      };
    },
    // C5, evaluated in the player's own page.
    async owner({ actorId, ownerId }) {
      const actor = game.actors.get(actorId);
      const result = {
        isPlayer: game.user.id === ownerId && !game.user.isGM,
        character: game.users.get(ownerId)?.character?.id ?? null,
        owner: !!actor && actor.testUserPermission(game.user, "OWNER"),
        sheet: false
      };
      if (actor) {
        await actor.sheet.render({ force: true });
        result.sheet = await until("the player's sheet", () => actor.sheet.rendered) && actor.sheet.isEditable;
      }
      return result;
    }
  };
})();
