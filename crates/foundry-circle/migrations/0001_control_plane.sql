-- Control-plane schema only. Foundry world documents remain in Foundry.
CREATE TABLE IF NOT EXISTS schema_guard (
    id boolean PRIMARY KEY DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

