#!/usr/bin/env bash
# Start a throwaway local Postgres (from nixpkgs) for development and tests.
set -euo pipefail
DIR="$(cd "$(dirname "$0")/.." && pwd)/.dev/pg"
PORT="${ATRADER_DEV_DB_PORT:-54329}"
run() { nix shell nixpkgs#postgresql_16 -c "$@"; }
if [ ! -d "$DIR" ]; then
  run initdb -D "$DIR" -U atrader --auth=trust >/dev/null
fi
run pg_ctl -D "$DIR" -o "-p $PORT -k $DIR -c listen_addresses=127.0.0.1" -l "$DIR/log" start >/dev/null
echo "export DATABASE_URL=postgres://atrader@127.0.0.1:$PORT/postgres"
