#!/usr/bin/env bash
set -euo pipefail
dir="$(cd "$(dirname "$0")" && pwd)"
cd "$dir"
if [ ! -f target/release/agentic ]; then
  cargo build --release 2>&1 | tail -1
fi
target/release/agentic run --task "say hello" --max-turns 1 --yes --json 2>/dev/null
