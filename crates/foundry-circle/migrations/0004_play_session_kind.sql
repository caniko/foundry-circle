-- The play host runs its own OIDC login against the same Rauthy client. Its
-- session cookie is host-only, and the row records which host created it so a
-- platform token never authenticates on the play host or the reverse: every
-- session lookup filters on `kind`.
ALTER TABLE sessions
    ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'platform'
    CHECK (kind IN ('platform', 'play'));
