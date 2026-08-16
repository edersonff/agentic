#!/usr/bin/env bash
set -euo pipefail
dir="$(cd "$(dirname "$0")" && pwd)"
cd "$dir"

if [ ! -f target/release/agentic ]; then
  echo "building agentic (first run)..." >&2
  cargo build --release
fi

export LLM_ADAPTER_BINARY="${LLM_ADAPTER_BINARY:-/tmp/sheol/edersonff/llm-adapter/target/release/llm-adapter}"
export LLM_CONFIG="${LLM_CONFIG:-$HOME/.config/llm-adapter/config.yaml}"

TASK_FILE="${1:?usage: run.sh task.txt — the task is the file content}"
exec target/release/agentic run \
  --task "$(cat "$TASK_FILE")" \
  --max-turns "${AGENTIC_MAX_TURNS:-1}" \
  --yes --json \
  --llm-config "$LLM_CONFIG" \
  --model "${AGENTIC_MODEL:-glm-5.2}"
