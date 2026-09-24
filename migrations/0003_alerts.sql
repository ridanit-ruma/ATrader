ALTER TABLE accounts ADD COLUMN alert_session_id TEXT;

CREATE TABLE alerts (
    id             BIGSERIAL PRIMARY KEY,
    account_id     TEXT NOT NULL REFERENCES accounts(id),
    generation     INT  NOT NULL,
    kind           TEXT NOT NULL,
    instrument     TEXT,
    venue          TEXT,
    threshold      NUMERIC,
    window_minutes INT,
    note           TEXT NOT NULL,
    once           BOOLEAN NOT NULL,
    active         BOOLEAN NOT NULL DEFAULT true,
    created_at     TIMESTAMPTZ NOT NULL,
    last_fired_at  TIMESTAMPTZ
);
CREATE INDEX alerts_active ON alerts (account_id, generation) WHERE active;

CREATE TABLE alert_events (
    id         BIGSERIAL PRIMARY KEY,
    alert_id   BIGINT NOT NULL REFERENCES alerts(id),
    account_id TEXT NOT NULL,
    fired_at   TIMESTAMPTZ NOT NULL,
    message    TEXT NOT NULL,
    delivered  BOOLEAN NOT NULL,
    error      TEXT
);
