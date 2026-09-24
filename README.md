# ATrader

Let an AI agent trade stocks and crypto, and watch how it does.

ATrader connects to [Attacca](https://attacca.cc) as a [zyris](https://github.com/attacca-cc/zyris-protocol)
node. It gives an Attacca agent the tools to research markets and trade on paper accounts driven by
real-time market data, and it gives you a private dashboard showing every trade, the agent's
reason for it, and how each account is doing.

> **Status:** paper trading, research tools, alerts and the dashboard work. Korean and US stocks
> (KIS) are implemented but not yet verified against live keys; the NixOS deployment is next. See the
> [design spec](docs/superpowers/specs/2026-09-24-atrader-design.md) and
> [plans](docs/superpowers/plans/).

## What it does

- **Paper trading with market impact.** A shadow order book sits on top of the real order book.
  Large orders walk the book and pay slippage. The liquidity they consume recovers over time, and
  their price impact decays gradually. An agent learns that splitting a big order pays off.
- **Three markets.** Korean stocks (KRX) and US stocks, both through the KIS Open API, and crypto
  on Upbit and Binance.
- **Tools built for an AI agent.**
  - Quotes, order books, candles, and server-side technical indicators.
  - Screeners.
  - Company filings and financial statements, from DART in Korea and SEC EDGAR in the US.
  - A dry-run order estimator.
  - Account performance stats.
  - Alerts that wake the agent up through Attacca.
- **Multiple accounts.** Run one account per agent or strategy, compare them, and reset them.
- **Dashboard.** Equity curves, daily and realised profit and loss, positions, fills with the
  agent's reasoning, and charts marked with the agent's trades. It requires a login with two-factor
  authentication and is served only on your Tailscale network.
- **Ready for real brokers.** Execution sits behind a `Broker` trait. Connecting a real brokerage
  account is planned but not built.

## Running

You need Rust and Postgres. For a throwaway local database, `scripts/dev-db.sh` starts one
through nix and prints a `DATABASE_URL`.

```bash
export DATABASE_URL=postgres://atrader@127.0.0.1:54329/atrader

# Create an account. --agent is the id of the Attacca agent that trades it; the agent cannot
# see accounts without one.
atrader account create bot "Momentum bot" --agent <attacca-agent-id> --cash KRW=10000000 --cash USDT=7000
atrader account list

# Serve the zyris `trader` capability. Issue the credential in Attacca under /settings/zyris.
ZYRIS_CREDENTIAL=zc_... atrader serve

# Or run the simulator and market data without connecting to Attacca.
atrader serve --no-zyris
```

Fundamentals are optional: set `DART_API_KEY` (free, issued at opendart.fss.or.kr) for Korean
companies and `EDGAR_USER_AGENT` (for example `"ATrader you@example.com"`, as SEC requires) for US
companies. KIS keys (`KIS_APP_KEY`, `KIS_APP_SECRET`) enable Korean and US stock quotes.

`atrader account reset <id>` starts an account over and keeps its history. Stop `atrader serve`
before a reset: a running server keeps trading the old state in memory.

## Dashboard

Build the dashboard before the binary, which embeds `web/dist`:

```bash
(cd web && npm ci && npm run build)
cargo build --release
```

`atrader serve` also serves the dashboard on `127.0.0.1:8750` (set `ATRADER_HTTP_ADDR` to change
it). Create the one login with `atrader user create <name>`; the first sign-in enrols an
authenticator app (TOTP) and shows ten single-use recovery codes. To reach it from your other
devices, publish it on your tailnet only:

```bash
tailscale serve --bg --https=443 http://127.0.0.1:8750
```

Lost the authenticator? `atrader user reset-2fa <name>` turns the second factor off and signs every
session out; the next sign-in enrols again.

## Alerts

The agent can set alerts (price levels, % moves, volume surges, its own fills, market open and
close). When one fires, ATrader messages the account's agent in a dedicated Attacca session with
the agent's own note, so it can act without polling. The zyris credential needs the
`agents:read`, `sessions:read` and `sessions:write` scopes for this.

## Stack

Rust (tokio, axum, sqlx/Postgres) in a single binary, with a React dashboard served by the same
binary.

## License

[AGPL-3.0](LICENSE)
