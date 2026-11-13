-- A world the broker has taken over (G5): one row per adopted Foundry world,
-- recording the API user the steady-state driver logs in as. Foundry remains
-- the authority for the users themselves.
CREATE TABLE IF NOT EXISTS adopted_worlds (
    world_id text PRIMARY KEY,
    api_user_id text NOT NULL,
    adopted_at timestamptz NOT NULL DEFAULT now()
);

-- The broker-mapped Foundry user for one principal in one world. The id is
-- Foundry's, learned when the user is created on first entry.
CREATE TABLE IF NOT EXISTS foundry_users (
    subject text REFERENCES principals(subject),
    world_id text,
    user_id text NOT NULL,
    PRIMARY KEY (subject, world_id)
);
