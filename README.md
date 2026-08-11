# agentic

Runs a task to completion using an LLM and a set of tools. Calls `llm-adapter` as a subprocess for the model, dispatches tools (read, write, grep, search, exec, todo, finish), gates destructive commands, tracks a todo list, saves state on interrupt, and resumes by task id. No server, no config file — one binary, three commands.

## Input

A task string via `--task`, plus flags:

```
agentic run --task "find all TODO comments in src/ and list them" [--max-turns 20] [--json] [--yes]
```

## Output

- **stdout**: the final answer text (prose mode), or a JSON envelope with `--json`
- **stderr**: a per-turn log — tool calls, tool results, todo changes

JSON envelope shape:

```json
{
  "task_id": "20260810-153022-ab3f",
  "status": "completed",
  "turns": 4,
  "todos": [{"id":"s1","content":"grep TODO","status":"completed"}],
  "final_answer": "found 12 TODO comments across 3 files"
}
```

## Run it

```
agentic run --task "read main.rs and summarize it" --max-turns 10
agentic run --task "grep for panic! in src/" --json
agentic resume 20260810-153022-ab3f
agentic tools
agentic tasks
```

`--json` on any command returns a machine-readable envelope for agents.

## How it works

Each turn:

1. agentic sends the conversation + tool schemas to `llm-adapter` (stdin/stdout subprocess)
2. the model responds with text and/or tool calls
3. tools execute: read a file, write a file, grep, search the web, run a command, update the todo list, or signal completion
4. results go back to the model as tool messages
5. the loop ends when the model calls `finish`, hits `--max-turns`, gets stuck (3 identical calls), or you press Ctrl+C

State is saved to `~/.local/share/agentic/<task-id>.json` after every turn. Ctrl+C saves and exits 130. `resume <task-id>` picks up from the last saved turn.

## Tools

| tool | args | what it does |
|------|------|-------------|
| `read` | `path` | reads a file (capped at 1MB), lists a directory if given one |
| `write` | `path`, `content` | writes text to a file (atomic: temp + rename) |
| `grep` | `pattern`, `path?` | searches file contents with ripgrep (default: current directory) |
| `search` | `query` | web search via `ddgr` (DuckDuckGo), returns 5 results |
| `exec` | `command`, `args[]` | runs a command with arguments — no shell features (pipes, redirects) |
| `todo_update` | `items[]` | sets the todo list — each item has `id`, `content`, `status` |
| `finish` | `result`, `blocked?` | signals the task is done or blocked |

Tool results are JSON. Errors carry an `error` field with a message and a `fix` field with the next step.

## Security

`exec` checks every command before running. Destructive commands — `rm`, `sudo`, `git push`, `kill`, anything with `--force` or `-rf` — are blocked unless you pass `--yes`. When blocked, the agent sees the error and the fix, and tells you to rerun with `--yes` or do the operation yourself.

`exec` does not use a shell. Pipes (`|`), redirects (`>`), and shell operators (`&&`, `;`) are passed as literal arguments, not interpreted. This is deliberate — it prevents shell injection. If the model needs to chain commands, it makes multiple `exec` calls.

## Settings, and what they default to

| flag | default | what it controls |
|------|---------|-----------------|
| `--max-turns` | 20 | hard cap on turns before saving and stopping |
| `--json` | off | json envelope on stdout instead of prose |
| `--yes` | off | allow destructive exec commands |
| `--llm-config` | `config.yaml` | path to the llm-adapter config file |
| `--model` | none | model name override passed to llm-adapter |
| `--llm-binary` | auto | path to llm-adapter binary (or set `LLM_ADAPTER_BINARY`) |

State directory: `~/.local/share/agentic/`, or `$XDG_DATA_HOME/agentic/`, or `$AGENT_RUNTIME_HOME` if set.

## What it needs

- `llm-adapter` on PATH (or via `--llm-binary` / `LLM_ADAPTER_BINARY`) — pull it with `sheol pull edersonff/llm-adapter`
- `ripgrep` (`rg`) on PATH for the grep tool
- `ddgr` on PATH for the search tool (optional — without it, `search` returns a clear install hint)
- A `config.yaml` for llm-adapter with at least one provider configured

## What breaks

Measured on rust 1.95, linux:

- **llm-adapter not on PATH** → exit 1, `llm-adapter not found. tried: ... install with: sheol pull edersonff/llm-adapter, then build and put the binary on your PATH, or set --llm-binary <path>`
- **config.yaml missing** → exit 1, message names the path and says copy it from llm-adapter's template
- **empty --task** → exit 1, `task cannot be empty. pass --task "what you want done"`
- **--max-turns 0** → exit 1, `max-turns must be at least 1`
- **Ctrl+C mid-run** → state saved, exit 130, message says `resume with: agentic resume <id>`
- **max turns reached** → status `blocked`, exit 2, state saved, message says how to resume
- **stuck (3 identical tool calls)** → status `blocked`, exit 2, state saved
- **destructive command without --yes** → tool returns error, agent sees it and can tell you to rerun with `--yes`
- **read a binary file** → error, `file is not valid utf-8 text`, suggests `exec` with `file` or `xxd`
- **read a directory** → returns the listing with `is_directory: true`, not an error
- **write to missing parent dir** → error, names the missing directory, suggests `exec mkdir -p`
- **grep with no matches** → `matches: 0`, not an error
- **resume a bad task id** → exit 1, `no saved task with id "X"`, names where it looked
- **provider returns non-JSON** → exit 1, `llm-adapter returned something that is not valid openai json`, usually means the config is wrong or the provider returned an error page
- **read output over 1MB** → truncated, `truncated: true`, tells the model to read a specific section

## Exit codes

`0` task completed. `1` error — bad input, missing dependency, provider failure. `2` blocked — max turns, stuck, or model called `finish(blocked=true)`. `130` interrupted — Ctrl+C, state saved, resume to continue.

## Why rust

Sheol is rust. The operations are process spawning, file I/O, and JSON — all stable in std and serde. The binary is one file, no runtime dependency, and composes with the rest of the shelf. `llm-adapter` is already rust; calling it as a subprocess keeps the boundary clean and avoids HTTP server overhead.
