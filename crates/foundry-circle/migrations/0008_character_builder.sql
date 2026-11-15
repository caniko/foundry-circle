-- Session zero: one rules row per adopted world and the players' drafts. Both
-- are control-plane state only; Foundry owns the Actor once a draft is approved.
CREATE TABLE IF NOT EXISTS campaign_rules (
    world_id text PRIMARY KEY REFERENCES adopted_worlds(world_id),
    open boolean NOT NULL DEFAULT false,
    rules jsonb NOT NULL,
    updated_by text NOT NULL REFERENCES principals(subject),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS character_drafts (
    id uuid PRIMARY KEY,
    world_id text NOT NULL REFERENCES campaign_rules(world_id),
    subject text NOT NULL REFERENCES principals(subject),
    name text NOT NULL DEFAULT '',
    status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'approved')),
    choices jsonb NOT NULL DEFAULT '{}'::jsonb,
    rolls jsonb,      -- server-written only
    dm_note text,
    actor_id text,    -- Foundry's id, set on approval
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS character_drafts_world_idx ON character_drafts (world_id, subject);
