#!/usr/bin/env bash
set -euo pipefail

compose=(docker compose run --rm -T mellow-dev)

"${compose[@]}" bash scripts/native-ci.sh verify

echo "Mellow verification passed."
