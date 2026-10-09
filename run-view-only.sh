#!/usr/bin/env bash
set -euo pipefail
cd /var/www/espo-xcp
exec ./target/release/espo --config-path ./config.json --view-only "$@"
