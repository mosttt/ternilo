CREATE TABLE control_mfa_factors (
    user_id TEXT PRIMARY KEY REFERENCES control_users(user_id) ON DELETE CASCADE,
    generation TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','enabled')),
    expires_at_ms BIGINT NOT NULL,
    enabled_at_ms BIGINT NOT NULL,
    last_step BIGINT NOT NULL DEFAULT -1,
    failures BIGINT NOT NULL DEFAULT 0,
    blocked_until_ms BIGINT NOT NULL DEFAULT 0,
    nonce BYTEA NOT NULL, ciphertext BYTEA NOT NULL
);
CREATE TABLE control_mfa_oidc_challenges (
    token_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    generation TEXT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    nonce BYTEA NOT NULL, ciphertext BYTEA NOT NULL
);
CREATE INDEX control_mfa_oidc_challenges_expiry ON control_mfa_oidc_challenges(expires_at_ms);
CREATE TABLE control_oidc_mfa_assurances (
    session_id TEXT PRIMARY KEY REFERENCES control_oidc_sessions(session_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    generation TEXT NOT NULL
);
