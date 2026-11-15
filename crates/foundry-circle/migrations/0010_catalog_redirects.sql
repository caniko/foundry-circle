-- The world's own compendium uuid redirects, as dnd5e registers them: while a
-- premium module is not active it points that module's pack prefixes at the SRD
-- packs. They are replaced by each catalog sync, in the same transaction as the
-- documents they rewrite.
CREATE TABLE IF NOT EXISTS builder_redirects (
    world_id text NOT NULL REFERENCES adopted_worlds(world_id),
    from_prefix text NOT NULL,
    to_prefix text NOT NULL,
    PRIMARY KEY (world_id, from_prefix)
);
