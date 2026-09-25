#!/usr/bin/env bash
# Create ./secrets for compose.yaml: a random database password, the matching DATABASE_URL, and
# empty files for the optional keys. Existing files are left alone.
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
mkdir -p secrets
[ -s secrets/db_password ] || head -c 24 /dev/urandom | base64 | tr -d '/+=' > secrets/db_password
[ -s secrets/database_url ] || echo "postgres://atrader:$(cat secrets/db_password)@127.0.0.1:${ATRADER_DB_PORT:-54330}/atrader" > secrets/database_url
for f in zyris_credential kis_app_key kis_app_secret dart_api_key; do
  [ -e "secrets/$f" ] || : > "secrets/$f"
done
# The postgres container reads its password as its own user; the 0700 directory still keeps
# other host users out.
chmod 644 secrets/db_password
echo "secrets ready in $(pwd)/secrets"
