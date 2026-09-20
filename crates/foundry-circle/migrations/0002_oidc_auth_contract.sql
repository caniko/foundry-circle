ALTER TABLE oidc_transactions ADD COLUMN IF NOT EXISTS nonce text;
ALTER TABLE oidc_transactions ADD COLUMN IF NOT EXISTS return_to text;

UPDATE oidc_transactions SET nonce = state WHERE nonce IS NULL;
ALTER TABLE oidc_transactions ALTER COLUMN nonce SET NOT NULL;

ALTER TABLE sessions ADD COLUMN IF NOT EXISTS token_hash text;
-- Existing sessions were refresh-token sessions and cannot be converted into
-- opaque-cookie sessions. Revoking them during the one-time migration is the
-- safe boundary; every browser must authenticate again.
DELETE FROM sessions WHERE token_hash IS NULL;
ALTER TABLE sessions DROP COLUMN IF EXISTS refresh_token;
ALTER TABLE sessions ALTER COLUMN token_hash SET NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS sessions_token_hash_idx ON sessions (token_hash);
