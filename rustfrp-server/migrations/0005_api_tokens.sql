CREATE TABLE api_tokens (
 id TEXT PRIMARY KEY,
 token_hash TEXT NOT NULL UNIQUE,
 admin_id TEXT NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 scope TEXT NOT NULL CHECK(scope IN ('read','tunnels','admin')),
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 revoked_at INTEGER
);
CREATE INDEX api_tokens_admin ON api_tokens(admin_id,expires_at);
CREATE INDEX api_tokens_expiry ON api_tokens(expires_at,id);
CREATE INDEX api_tokens_revoked ON api_tokens(revoked_at,id) WHERE revoked_at IS NOT NULL;
