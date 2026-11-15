-- A cache of the world's compendium items; Foundry stays the authority and is
-- re-read by uuid when a character is created.
CREATE TABLE IF NOT EXISTS builder_catalog (
    world_id text NOT NULL REFERENCES adopted_worlds(world_id),
    pack_id text NOT NULL,
    label text NOT NULL,
    source_book text,
    rules_version text,
    system_version text NOT NULL,
    items jsonb NOT NULL,
    synced_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (world_id, pack_id)
);
