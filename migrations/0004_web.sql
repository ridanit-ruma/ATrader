CREATE TABLE users (
    id            BIGSERIAL PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    totp_secret   TEXT,
    totp_enabled  BOOLEAN NOT NULL DEFAULT false,
    -- Last accepted TOTP time step; a code is never accepted twice.
    totp_last_step BIGINT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_sessions (
    id_hash     TEXT PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    mfa_pending BOOLEAN NOT NULL,
    ip          TEXT NOT NULL,
    user_agent  TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    last_seen   TIMESTAMPTZ NOT NULL
);

CREATE TABLE recovery_codes (
    user_id   BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL,
    used_at   TIMESTAMPTZ
);

CREATE TABLE audit_log (
    id      BIGSERIAL PRIMARY KEY,
    at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_id BIGINT,
    action  TEXT NOT NULL,
    detail  TEXT NOT NULL,
    ip      TEXT NOT NULL
);
