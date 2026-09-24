# ATrader

Paper-trading simulator and dashboard that an Attacca agent drives through zyris.
Spec: `docs/superpowers/specs/2026-09-24-atrader-design.md`. Plans: `docs/superpowers/plans/`.

## Commands

- Build and test (no DB): `cargo test --lib --test broker`
- Local Postgres: `scripts/dev-db.sh` prints a `DATABASE_URL`; export it, then `cargo test`
- Stop Postgres: `nix shell nixpkgs#postgresql_16 -c pg_ctl -D .dev/pg stop`

## Conventions

- Money is `rust_decimal::Decimal`; `f64` only for impact/decay/volatility maths.
- `sim` is pure: no I/O, no clock reads — time is passed in.
- Deliberate shortcuts carry a `ponytail:` comment naming the ceiling and the upgrade path.
- Add dependencies with `cargo add`.
