-- Control-plane schema only. Foundry world documents remain in Foundry.
CREATE TABLE IF NOT EXISTS schema_guard (
    id boolean PRIMARY KEY DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS principals (
    subject text PRIMARY KEY,
    issuer text NOT NULL,
    display_name text,
    email text,
    claims jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS oidc_transactions (
    state text PRIMARY KEY,
    code_verifier bytea NOT NULL,
    redirect_uri text NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS sessions (
    id uuid PRIMARY KEY,
    subject text NOT NULL REFERENCES principals(subject),
    refresh_token bytea,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);

CREATE TABLE IF NOT EXISTS idempotency_keys (
    subject text NOT NULL REFERENCES principals(subject),
    key text NOT NULL,
    request_hash bytea NOT NULL,
    response jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (subject, key)
);

CREATE TABLE IF NOT EXISTS jobs (
    id uuid PRIMARY KEY,
    subject text NOT NULL REFERENCES principals(subject),
    kind text NOT NULL,
    status text NOT NULL,
    input jsonb NOT NULL,
    output jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS world_events (
    sequence bigserial PRIMARY KEY,
    world_epoch bigint NOT NULL,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS audit_log (
    id bigserial PRIMARY KEY,
    subject text,
    action text NOT NULL,
    resource text NOT NULL,
    request_id text,
    outcome text NOT NULL,
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS world_events_epoch_sequence_idx
    ON world_events (world_epoch, sequence);
CREATE INDEX IF NOT EXISTS audit_log_created_at_idx
    ON audit_log (created_at DESC);
