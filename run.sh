#!/usr/bin/env bash
set -euo pipefail

dir="$(cd "$(dirname "$0")" && pwd)"
cd "$dir"

if [ ! -f target/release/agentic ]; then
  echo "building agentic (first run)..." >&2
  cargo build --release
fi

exec target/release/agentic "$@"
