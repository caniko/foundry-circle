-- Native client sign-in (RFC 8252 loopback + PKCE). The client cannot share the
-- browser's session: a session row carries a Rauthy refresh token that rotates,
-- so copying it would make the browser's next refresh fail and revoke the
-- subject. The native flow therefore runs its own OIDC login and receives its
-- own session.
--
-- The login transaction remembers where to send the result: the client's
-- loopback port, its CSRF state, and the S256 challenge its verifier must meet.
ALTER TABLE oidc_transactions ADD COLUMN IF NOT EXISTS native_port integer;
ALTER TABLE oidc_transactions ADD COLUMN IF NOT EXISTS native_state text;
ALTER TABLE oidc_transactions ADD COLUMN IF NOT EXISTS native_challenge text;

-- One redeemable code per completed native login, kept for two minutes. Only
-- the code's hash is stored; the session token is here because the client has
-- not received it yet, and the row is deleted on first use either way.
CREATE TABLE IF NOT EXISTS native_handoffs (
    code_hash text PRIMARY KEY,
    challenge text NOT NULL,
    session_token text NOT NULL,
    expires_at timestamptz NOT NULL
);
