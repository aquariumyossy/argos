#!/bin/sh
# Rebuild the bundled statute database from the e-Gov law API (no key).
# See docs/statute-db.md.
set -eu
cd "$(dirname "$0")/.."
python3 scripts/fetch_statutes.py "$@"
