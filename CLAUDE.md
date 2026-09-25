# ATrader

Paper-trading simulator and dashboard that an Attacca agent drives through zyris.
Spec: `docs/superpowers/specs/2026-09-24-atrader-design.md`. Plans: `docs/superpowers/plans/`.

## Commands

- Build and test (no DB): `cargo test --lib --test broker`
- Local Postgres: `scripts/dev-db.sh` prints a `DATABASE_URL`; export it, then `cargo test`
- Run without Attacca: `cargo run -- serve --no-zyris` (needs `DATABASE_URL`; the dev DB also has an `atrader` database: `createdb -h 127.0.0.1 -p 54329 -U atrader atrader`)
- Accounts: `cargo run -- account create <id> <name> --agent <agent-id>`, `cargo run -- account list`
- Live network smoke tests: `cargo test --test live -- --ignored`
- Dashboard: `cd web && npm ci && npm run build` (the binary embeds `web/dist`); `npm run dev` proxies `/api` to `127.0.0.1:8750`; `npm test` runs vitest
- Nix: `nix build` (package), `nix build .#checks.x86_64-linux.vm` (NixOS VM test of the module). After changing `web/package-lock.json`, update `npmDepsHash` in `nix/package.nix` (`nix run nixpkgs#prefetch-npm-deps -- web/package-lock.json`)
- Local-only feeds: `src/feed/private/` is git-ignored (it has its own local git repo) and is compiled with `--features private-feeds`; `feed::private::without_kis` supplies feeds when KIS has no keys. Deploys set `ATRADER_FEATURES=private-feeds` and copy the folder over. Never commit it to the public repo.
- Stop Postgres: `nix shell nixpkgs#postgresql_16 -c pg_ctl -D .dev/pg stop`

## Conventions

- Money is `rust_decimal::Decimal`; `f64` only for impact/decay/volatility maths.
- `sim` is pure: no I/O, no clock reads — time is passed in.
- Deliberate shortcuts carry a `ponytail:` comment naming the ceiling and the upgrade path.
- Add dependencies with `cargo add`.
- Dashboard UI follows shadcn/ui (base-nova, Base UI primitives): compose `web/src/components/ui/*` and theme tokens (`text-muted-foreground`, `--chart-*`); no ad-hoc colours except the up/down tones in `format.ts`. Add primitives with `npx shadcn@latest add <name>`.
