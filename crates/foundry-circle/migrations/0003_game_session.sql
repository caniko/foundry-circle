-- One GM session lease at a time. Control-plane state only: Foundry keeps the
-- world documents and the launched world itself.
CREATE TABLE IF NOT EXISTS game_session (
    id uuid PRIMARY KEY,
    holder text NOT NULL REFERENCES principals(subject),
    phase text NOT NULL CHECK (phase IN ('setup', 'running', 'ending')),
    world_id text,
    started_at timestamptz NOT NULL DEFAULT now(),
    last_seen timestamptz NOT NULL DEFAULT now(),
    ended_at timestamptz,
    end_reason text
);

-- At most one open lease, whichever GM asks.
CREATE UNIQUE INDEX IF NOT EXISTS one_open_session ON game_session ((true)) WHERE ended_at IS NULL;
