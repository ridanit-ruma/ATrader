-- 1-minute bars built from KRX and US trade prints (crypto venues serve their own candles).
CREATE TABLE bars (
    instrument TEXT NOT NULL,
    start      TIMESTAMPTZ NOT NULL,
    open       NUMERIC NOT NULL,
    high       NUMERIC NOT NULL,
    low        NUMERIC NOT NULL,
    close      NUMERIC NOT NULL,
    volume     NUMERIC NOT NULL,
    value      NUMERIC NOT NULL,
    PRIMARY KEY (instrument, start)
);

CREATE TABLE equity_snapshots (
    account_id    TEXT NOT NULL REFERENCES accounts(id),
    generation    INT  NOT NULL,
    at            TIMESTAMPTZ NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('minute', 'daily')),
    equity_krw    NUMERIC NOT NULL,
    cash_krw      NUMERIC NOT NULL,
    positions_krw NUMERIC NOT NULL,
    PRIMARY KEY (account_id, generation, kind, at)
);
