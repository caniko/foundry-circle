-- Session tokens for gate revalidation (D5, G6). The access token is asked of
-- Rauthy's userinfo on every gate check, the refresh token keeps a live session
-- alive past the access token's expiry, and the ID token is the RP-initiated
-- logout hint. Rows created before this migration hold no tokens: they are a
-- session nothing can revalidate, so the gate revokes that row alone (never the
-- subject's other sessions) and the browser signs in again.
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS access_token text;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS refresh_token text;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS id_token text;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS access_expires_at timestamptz;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS validated_at timestamptz;
