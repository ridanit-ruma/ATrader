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
- **Multiple accounts.** Run one account per strategy, compare them, and reset them. The connected
  agent sees every account; with just one, it never has to name it.
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

# Create an account (the dashboard does this too, from just a name).
atrader account create bot "Momentum bot" --cash KRW=10000000 --cash USDT=7000
atrader account list

# Serve the zyris `trader` capability. `zyris enroll` prints a code to approve in Attacca, then
# the credential.
atrader zyris enroll > zyris.credential
ZYRIS_CREDENTIAL_FILE=zyris.credential atrader serve

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

## Running with Docker

`compose.yaml` runs ATrader with its own Postgres. Secrets are files under `./secrets`, never
environment variables:

```bash
scripts/docker-init.sh                        # database password + empty optional key files
docker compose up -d --build
docker compose run --rm atrader user create <name>
tailscale serve --bg --https=443 http://127.0.0.1:8750   # dashboard on your tailnet only
```

Then connect Attacca and add data keys from the dashboard's settings page:

- **Attacca:** "연결 코드 받기" shows a code; approve it in Attacca and the server restarts
  connected. (`atrader zyris enroll` does the same from a terminal and prints the credential.)
- **Data keys:** KIS (Korean and US stocks), OpenDART and the SEC EDGAR User-Agent. They are
  stored in the state directory (mode 0600), never shown again, and applied by a restart.

Keys set in the environment or in `./secrets` files take precedence and cannot be changed from the
dashboard. The server exits with code 75 to restart; compose (`restart: unless-stopped`) and the
NixOS module (`Restart=on-failure`) start it again.

## Deploying on NixOS

The flake provides a package and a module. The module runs `atrader` as a hardened systemd service
with a local Postgres database. Secrets are passed as systemd credentials, so they never enter the
Nix store:

```nix
{
  inputs.atrader.url = "github:ridanit-ruma/ATrader";
  # in your nixosSystem modules:
  #   atrader.nixosModules.default
  #   {
  #     services.atrader = {
  #       enable = true;
  #       tailscaleServe = true; # https://<machine>.<tailnet>.ts.net
  #       credentials = {
  #         ZYRIS_CREDENTIAL = "/var/lib/secrets/atrader-zyris";
  #         KIS_APP_KEY = "/var/lib/secrets/kis-app-key";
  #         KIS_APP_SECRET = "/var/lib/secrets/kis-app-secret";
  #         DART_API_KEY = "/var/lib/secrets/dart-api-key";
  #       };
  #       environment.EDGAR_USER_AGENT = "ATrader you@example.com";
  #     };
  #   }
}
```

Then create the login and accounts with `atrader-manage`, which runs the CLI as the service user:
`sudo atrader-manage user create <name>`, `sudo atrader-manage account create ...`.
`nix build .#checks.x86_64-linux.vm` boots the module in a VM and checks it end to end.

## Alerts

The agent can set alerts (price levels, % moves, volume surges, its own fills, market open and
close). When one fires, ATrader posts the agent's own note into the Attacca conversation chosen for
that account on the dashboard's alerts page, from the conversations in the Attacca project
"ATrader" (created on first use). Until one is chosen, alerts go back to the conversation that set
them. The zyris credential needs the `projects:read`, `projects:write`, `sessions:read` and
`sessions:write` scopes, which `zyris enroll` and the dashboard request.

## Stack

Rust (tokio, axum, sqlx/Postgres) in a single binary, with a React dashboard served by the same
binary.

## License

[AGPL-3.0](LICENSE)
