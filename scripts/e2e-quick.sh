#!/usr/bin/env bash
# e2e-quick.sh - Run the nucbox E2E smoke test from the mac and print the JSON result.
#
# Usage:
#   scripts/e2e-quick.sh                # default: CS2, 90s
#   scripts/e2e-quick.sh 730 60         # appId 730, 60s
#   NUCBOX_SSH=nucbox scripts/e2e-quick.sh
#
# Prereqs: `ssh <host> pwsh` must work, and scripts/e2e-smoke.ps1 must be on
# the nucbox under the path given by NUCBOX_REPO (default /root/gamedata-recorder).

set -euo pipefail

HOST="${NUCBOX_SSH:-nucbox-wsl}"
REPO="${NUCBOX_REPO:-/root/gamedata-recorder}"
APP_ID="${1:-730}"
DURATION="${2:-90}"
REMOTE_OUT='$env:TEMP\gdr-e2e-result.json'
LOCAL_OUT="${LOCAL_OUT:-/tmp/gdr-e2e-result.json}"

echo "[e2e-quick] host=$HOST repo=$REPO app=$APP_ID dur=${DURATION}s"

# Run the smoke test on nucbox. We capture exit code separately so we can still
# pull the JSON on failure.
set +e
ssh -o BatchMode=yes "$HOST" "pwsh -NoProfile -NonInteractive -File $REPO/scripts/e2e-smoke.ps1 -GameAppId $APP_ID -DurationSec $DURATION -OutputJson $REMOTE_OUT"
REMOTE_EXIT=$?
set -e
echo "[e2e-quick] remote exit=$REMOTE_EXIT"

# Pull the JSON result back. Nested quoting: outer single quotes keep $env:TEMP
# literal so the remote shell expands it.
ssh -o BatchMode=yes "$HOST" 'pwsh -NoProfile -NonInteractive -Command "Get-Content -Raw $env:TEMP\gdr-e2e-result.json"' > "$LOCAL_OUT" || true

if [[ -s "$LOCAL_OUT" ]]; then
    if command -v jq >/dev/null 2>&1; then jq . "$LOCAL_OUT"; else cat "$LOCAL_OUT"; fi
    echo "[e2e-quick] wrote $LOCAL_OUT"
else
    echo "[e2e-quick] no result JSON retrieved"
fi

exit "$REMOTE_EXIT"
