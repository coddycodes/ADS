#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CODDY_STUDIO_GITLAB_HARNESS_RTT_MS="${1:-200}"
export CODDY_STUDIO_GITLAB_HARNESS_RUNS="${2:-5}"
exec cargo test -j 2 -p coddy-studio-server --test pull_requests_gitlab_load_harness -- --include-ignored --nocapture --test-threads=1
