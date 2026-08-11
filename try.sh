#!/usr/bin/env bash
set -euo pipefail
task=$(cat "$1")
dir="$(cd "$(dirname "$0")" && pwd)"
cd "$dir"
if [ ! -f target/release/agentic ]; then
  echo "building agentic..." >&2
  cargo build --release 2>&1 | tail -1
fi
target/release/agentic run --task "$task" --max-turns 1 --yes --json 2>/dev/null
