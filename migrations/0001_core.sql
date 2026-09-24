CREATE TABLE accounts (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    agent_id    TEXT,
    generation  INT  NOT NULL DEFAULT 1,
    broker      TEXT NOT NULL DEFAULT 'sim',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE orders (
    id           BIGINT PRIMARY KEY,
    account_id   TEXT NOT NULL REFERENCES accounts(id),
    generation   INT  NOT NULL,
    instrument   TEXT NOT NULL,
    side         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    qty          NUMERIC,
    notional     NUMERIC,
    limit_price  NUMERIC,
    tif          TEXT NOT NULL,
    reason       TEXT NOT NULL,
    status       TEXT NOT NULL,
    filled_qty   NUMERIC NOT NULL,
    filled_notional NUMERIC NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX orders_account ON orders (account_id, generation, created_at);

CREATE TABLE fills (
    id           BIGSERIAL PRIMARY KEY,
    order_id     BIGINT NOT NULL REFERENCES orders(id),
    account_id   TEXT NOT NULL REFERENCES accounts(id),
    generation   INT  NOT NULL,
    instrument   TEXT NOT NULL,
    side         TEXT NOT NULL,
    qty          NUMERIC NOT NULL,
    notional     NUMERIC NOT NULL,
    price        NUMERIC NOT NULL,
    fee          NUMERIC NOT NULL,
    tax          NUMERIC NOT NULL,
    realized_pnl NUMERIC,
    liquidity    TEXT NOT NULL,
    at           TIMESTAMPTZ NOT NULL
);
CREATE INDEX fills_account ON fills (account_id, generation, id);

-- Append-only. Cash balance = SUM(amount) per (account, generation, currency).
CREATE TABLE ledger_entries (
    id          BIGSERIAL PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    generation  INT  NOT NULL,
    currency    TEXT NOT NULL,
    amount      NUMERIC NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('deposit', 'trade', 'fee', 'tax', 'fx')),
    fill_id     BIGINT REFERENCES fills(id),
    at          TIMESTAMPTZ NOT NULL
);
CREATE INDEX ledger_account ON ledger_entries (account_id, generation);
