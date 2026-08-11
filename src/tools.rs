use crate::security;
use crate::todo::{TodoItem, TodoStatus};
use serde_json::{json, Value};
use std::fs;
use std::io::{self};
use std::path::PathBuf;
use std::process::Command;

const MAX_READ_BYTES: usize = 1_048_576;
const MAX_EXEC_OUTPUT: usize = 10_240;
const MAX_GREP_LINES: usize = 100;

pub struct ToolContext {
    pub auto_approve: bool,
    pub workdir: PathBuf,
}

pub fn run_tool(name: &str, args: &Value, ctx: &ToolContext, todos: &mut Vec<TodoItem>) -> Value {
    match name {
        "read" => tool_read(args),
        "write" => tool_write(args),
        "grep" => tool_grep(args),
        "search" => tool_search(args),
        "exec" => tool_exec(args, ctx),
        "todo_update" => tool_todo_update(args, todos),
        other => json!({
            "error": format!("unknown tool: {}", other),
            "available": ["read", "write", "grep", "search", "exec", "todo_update", "finish"]
        }),
    }
}

fn want_str(args: &Value, key: &str) -> Result<String, Value> {
    match args.get(key) {
        Some(v) if v.is_string() => Ok(v.as_str().unwrap().to_string()),
        Some(_) => Err(json!({
            "error": format!("'{}' must be a string, got {}", key, type_name(args.get(key).unwrap())),
            "fix": format!("pass \"{}\": \"value\" in the arguments object", key)
        })),
        None => Err(json!({
            "error": format!("missing required argument: '{}'", key),
            "fix": format!("add \"{}\" to the arguments", key)
        })),
    }
}

fn want_arr(args: &Value, key: &str) -> Result<Vec<Value>, Value> {
    match args.get(key) {
        Some(v) if v.is_array() => Ok(v.as_array().unwrap().clone()),
        Some(_) => Err(json!({
            "error": format!("'{}' must be an array", key),
            "fix": format!("pass \"{}\": [...]", key)
        })),
        None => Err(json!({
            "error": format!("missing required argument: '{}'", key),
            "fix": format!("add \"{}\" to the arguments", key)
        })),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn tool_read(args: &Value) -> Value {
    let path = match want_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let p = PathBuf::from(&path);
    if p.is_dir() {
        match fs::read_dir(&p) {
            Ok(entries) => {
                let names: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                return json!({
                    "path": path,
                    "is_directory": true,
                    "entries": names,
                    "hint": "this is a directory. pass a specific file path to read its contents"
                });
            }
            Err(e) => return json!({ "error": format!("cannot list directory {}: {}", path, e) }),
        }
    }
    match fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > MAX_READ_BYTES {
                let truncated = String::from_utf8_lossy(&bytes[..MAX_READ_BYTES]);
                return json!({
                    "path": path,
                    "content": truncated.into_owned(),
                    "truncated": true,
                    "note": format!("file is {} bytes, showed first {}. read a specific section by reading a slice", bytes.len(), MAX_READ_BYTES)
                });
            }
            match String::from_utf8(bytes) {
                Ok(text) => json!({ "path": path, "content": text, "bytes": text.len() }),
                Err(_) => json!({
                    "error": "file is not valid utf-8 text",
                    "path": path,
                    "fix": "this looks like a binary file. if you need to inspect it, use exec with `file` or `xxd`"
                }),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => json!({
            "error": format!("no such file: {}", path),
            "fix": "check the path is right. if it should exist, create it first with write"
        }),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => json!({
            "error": format!("permission denied reading {}: {}", path, e),
            "fix": "the file exists but this process cannot read it. check the file permissions"
        }),
        Err(e) => json!({ "error": format!("could not read {}: {}", path, e) }),
    }
}

fn tool_write(args: &Value) -> Value {
    let path = match want_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let content = match want_str(args, "content") {
        Ok(c) => c,
        Err(e) => return e,
    };
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return json!({
                "error": format!("parent directory does not exist: {}", parent.display()),
                "fix": format!("create the directory first, or write to a path that exists. try: exec mkdir -p {}", parent.display())
            });
        }
    }
    let tmp = p.with_extension("tmp.agentic");
    if let Err(e) = fs::write(&tmp, &content) {
        return match e.kind() {
            io::ErrorKind::PermissionDenied => json!({
                "error": format!("permission denied writing to {}", path),
                "fix": "check the directory permissions. you may need to write to a different location"
            }),
            _ => json!({ "error": format!("could not write to {}: {}", path, e) }),
        };
    }
    if let Err(e) = fs::rename(&tmp, &p) {
        let _ = fs::remove_file(&tmp);
        return json!({ "error": format!("could not finalize file {}: {}", path, e) });
    }
    json!({ "path": path, "bytes": content.len(), "ok": true })
}

fn tool_grep(args: &Value) -> Value {
    let pattern = match want_str(args, "pattern") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".").to_string();

    let mut cmd = Command::new("rg");
    cmd.arg("--line-number").arg("--no-heading").arg("--color=never").arg("-N");
    cmd.arg(&pattern).arg(&path);
    cmd.env("RG_CLI_LEVEL", "0");
    match cmd.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = stdout.lines().collect();
            if lines.is_empty() {
                return json!({
                    "pattern": pattern,
                    "path": path,
                    "matches": 0,
                    "note": "no matches found"
                });
            }
            let total = lines.len();
            let shown: Vec<&str> = lines.into_iter().take(MAX_GREP_LINES).collect();
            json!({
                "pattern": pattern,
                "path": path,
                "matches": total,
                "shown": shown.len(),
                "truncated": total > MAX_GREP_LINES,
                "results": shown
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            json!({
                "error": "ripgrep (rg) is not installed. grep needs it",
                "fix": "install ripgrep: on debian/ubuntu `apt install ripgrep`, on mac `brew install ripgrep`, or from https://github.com/BurntSushi/ripgrep"
            })
        }
        Err(e) => json!({ "error": format!("could not run grep: {}", e) }),
    }
}

fn tool_search(args: &Value) -> Value {
    let query = match want_str(args, "query") {
        Ok(q) => q,
        Err(e) => return e,
    };
    let mut cmd = Command::new("ddgr");
    cmd.arg("--json").arg("-n").arg("5").arg(&query);
    cmd.env("DDGR_HARDCONFIRM", "1");
    match cmd.output() {
        Ok(out) => {
            if out.stdout.is_empty() {
                return json!({ "query": query, "results": [], "note": "no results or ddgr returned nothing" });
            }
            match serde_json::from_slice::<Value>(&out.stdout) {
                Ok(v) if v.is_array() => {
                    let results: Vec<Value> = v.as_array().unwrap().iter().take(5).cloned().collect();
                    json!({ "query": query, "results": results, "count": results.len() })
                }
                Ok(_) => json!({ "query": query, "results": [], "note": "ddgr returned unexpected format" }),
                Err(_) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    json!({ "query": query, "raw": text.to_string(), "note": "ddgr output was not json" })
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            json!({
                "error": "ddgr is not installed. web search needs it",
                "fix": "install ddgr: `pip install ddgr` or from https://github.com/jarun/ddgr"
            })
        }
        Err(e) => json!({ "error": format!("could not run ddgr: {}", e) }),
    }
}

fn tool_exec(args: &Value, ctx: &ToolContext) -> Value {
    let command = match want_str(args, "command") {
        Ok(c) => c,
        Err(e) => return e,
    };
    let raw_args = match args.get("args") {
        Some(v) if v.is_array() => v.as_array().unwrap().clone(),
        Some(_) => return json!({ "error": "'args' must be an array of strings", "fix": "pass \"args\": [\"-la\"] or \"args\": []" }),
        None => Vec::new(),
    };
    let mut cmd_args: Vec<String> = Vec::new();
    for a in raw_args {
        match a {
            Value::String(s) => cmd_args.push(s),
            Value::Number(n) => cmd_args.push(n.to_string()),
            Value::Bool(b) => cmd_args.push(b.to_string()),
            _ => return json!({ "error": format!("each arg must be a string, number, or bool. got: {}", a), "fix": "pass args as an array of strings" }),
        }
    }

    if !ctx.auto_approve && security::is_destructive(&command, &cmd_args) {
        return json!({
            "error": security::explain(&command, &cmd_args),
            "command": command,
            "args": cmd_args,
            "blocked": true,
            "fix": "the user must allow destructive commands. tell the user to rerun with --yes, or do the operation yourself outside the agent"
        });
    }

    let mut cmd = Command::new(&command);
    cmd.args(&cmd_args).current_dir(&ctx.workdir).stdin(std::process::Stdio::null());
    match cmd.output() {
        Ok(out) => {
            let stdout = truncate_string(&String::from_utf8_lossy(&out.stdout), MAX_EXEC_OUTPUT);
            let stderr = truncate_string(&String::from_utf8_lossy(&out.stderr), MAX_EXEC_OUTPUT);
            json!({
                "command": command,
                "args": cmd_args,
                "exit": out.status.code().unwrap_or(-1),
                "stdout": stdout.0,
                "stderr": stderr.0,
                "stdout_truncated": stdout.1,
                "stderr_truncated": stderr.1
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => json!({
            "error": format!("command not found: {}", command),
            "fix": format!("check that '{}' is installed and on your PATH", command)
        }),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => json!({
            "error": format!("cannot execute '{}': not executable or permission denied", command),
            "fix": format!("check that '{}' is an executable file", command)
        }),
        Err(e) => json!({ "error": format!("could not run '{}': {}", command, e) }),
    }
}

fn truncate_string(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        (s.to_string(), false)
    } else {
        (s[..max].to_string(), true)
    }
}

fn tool_todo_update(args: &Value, todos: &mut Vec<TodoItem>) -> Value {
    let items_raw = match want_arr(args, "items") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut updates = Vec::new();
    for (i, raw) in items_raw.iter().enumerate() {
        let obj = match raw.as_object() {
            Some(o) => o,
            None => return json!({ "error": format!("items[{}] is not an object", i), "fix": "each item must have id, content, and status" }),
        };
        let id = match obj.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return json!({ "error": format!("items[{}] missing 'id'", i), "fix": "each item needs a string id" }),
        };
        let content = match obj.get("content").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return json!({ "error": format!("items[{}] missing 'content'", i), "fix": "each item needs a content string" }),
        };
        let status_str = match obj.get("status").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return json!({ "error": format!("items[{}] missing 'status'", i), "fix": "status must be pending, in_progress, completed, or blocked" }),
        };
        let status = match TodoStatus::from_str(&status_str) {
            Ok(s) => s,
            Err(e) => return json!({ "error": format!("items[{}]: {}", i, e) }),
        };
        updates.push(TodoItem { id, content, status });
    }
    let before = todos.len();
    *todos = crate::todo::upsert(todos, &updates);
    json!({ "ok": true, "before_count": before, "after_count": todos.len(), "todos": todos })
}

pub fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a text file from disk. Returns the file contents. Fails on binary files, missing files, or directories.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "absolute or relative path to the file" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write",
                "description": "Write text content to a file. Overwrites if it exists. Atomic: writes to a temp file then renames.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "file path to write" },
                        "content": { "type": "string", "description": "the full text content to write" }
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search file contents using ripgrep. Returns matching lines with line numbers.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "regex pattern to search for" },
                        "path": { "type": "string", "description": "file or directory to search in (default: current directory)" }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search the web using ddgr (DuckDuckGo). Returns up to 5 results.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "search query" }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "exec",
                "description": "Run a command with arguments. No shell features (pipes, redirects). Destructive commands (rm, sudo, git push, etc) are blocked unless the user passed --yes.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "the program to run, like 'ls' or 'cargo'" },
                        "args": { "type": "array", "items": { "type": "string" }, "description": "arguments to pass" }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "todo_update",
                "description": "Set the task todo list. Each item is added or updated by id (upsert). Send only the items you want to add or change.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "description": "stable identifier like 'step1'" },
                                    "content": { "type": "string", "description": "what needs to be done" },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "blocked"] }
                                },
                                "required": ["id", "content", "status"]
                            }
                        }
                    },
                    "required": ["items"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "finish",
                "description": "Signal that the task is complete or blocked. Always call this when done; do not just stop responding.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "result": { "type": "string", "description": "summary of what was accomplished" },
                        "blocked": { "type": "boolean", "description": "true if the task cannot be completed. explain why in 'result'" }
                    },
                    "required": ["result"]
                }
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn ctx(workdir: PathBuf) -> ToolContext {
        ToolContext { auto_approve: false, workdir }
    }

    #[test]
    fn read_missing_file_returns_error_with_fix() {
        let result = tool_read(&json!({"path": "/nonexistent/path/xyz"}));
        assert!(result.get("error").unwrap().as_str().unwrap().contains("no such file"));
        assert!(result.get("fix").is_some());
    }

    #[test]
    fn read_returns_content_for_real_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let result = tool_read(&json!({"path": path.to_str().unwrap()}));
        assert_eq!(result["content"], "hello world");
        assert_eq!(result["bytes"], 11);
    }

    #[test]
    fn read_missing_path_arg_returns_error() {
        let result = tool_read(&json!({}));
        assert!(result["error"].as_str().unwrap().contains("missing required argument"));
    }

    #[test]
    fn read_non_string_path_returns_error() {
        let result = tool_read(&json!({"path": 42}));
        assert!(result["error"].as_str().unwrap().contains("must be a string"));
    }

    #[test]
    fn read_directory_returns_listing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        let result = tool_read(&json!({"path": dir.path().to_str().unwrap()}));
        assert_eq!(result["is_directory"], true);
        let entries = result["entries"].as_array().unwrap();
        assert!(entries.len() >= 2);
    }

    #[test]
    fn write_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let result = tool_write(&json!({"path": path.to_str().unwrap(), "content": "data"}));
        assert_eq!(result["ok"], true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "data");
    }

    #[test]
    fn write_missing_parent_returns_error_with_fix() {
        let result = tool_write(&json!({"path": "/tmp/no-such-dir-xyz/file.txt", "content": "x"}));
        assert!(result["error"].as_str().unwrap().contains("parent directory"));
    }

    #[test]
    fn write_missing_content_arg_returns_error() {
        let result = tool_write(&json!({"path": "/tmp/x"}));
        assert!(result["error"].as_str().unwrap().contains("missing required argument: 'content'"));
    }

    #[test]
    fn exec_runs_ls() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("visible.txt"), "x").unwrap();
        let result = tool_exec(
            &json!({"command": "ls", "args": [dir.path().to_str().unwrap()]}),
            &ctx(dir.path().to_path_buf()),
        );
        assert!(result["stdout"].as_str().unwrap().contains("visible.txt"));
        assert_eq!(result["exit"], 0);
    }

    #[test]
    fn exec_rm_blocked_without_auto_approve() {
        let dir = tempdir().unwrap();
        let result = tool_exec(
            &json!({"command": "rm", "args": [dir.path().join("x").to_str().unwrap()]}),
            &ctx(dir.path().to_path_buf()),
        );
        assert_eq!(result["blocked"], true);
        assert!(result["error"].as_str().unwrap().contains("--yes"));
    }

    #[test]
    fn exec_rm_runs_with_auto_approve() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("doomed.txt");
        fs::write(&file, "x").unwrap();
        let mut c = ctx(dir.path().to_path_buf());
        c.auto_approve = true;
        let result = tool_exec(&json!({"command": "rm", "args": [file.to_str().unwrap()]}), &c);
        assert_eq!(result["exit"], 0);
        assert!(!file.exists());
    }

    #[test]
    fn exec_command_not_found_returns_error() {
        let dir = tempdir().unwrap();
        let result = tool_exec(
            &json!({"command": "nonexistent-bin-xyz", "args": []}),
            &ctx(dir.path().to_path_buf()),
        );
        assert!(result["error"].as_str().unwrap().contains("command not found"));
    }

    #[test]
    fn exec_missing_command_arg_returns_error() {
        let dir = tempdir().unwrap();
        let result = tool_exec(&json!({}), &ctx(dir.path().to_path_buf()));
        assert!(result["error"].as_str().unwrap().contains("missing required argument"));
    }

    #[test]
    fn exec_non_string_arg_returns_error() {
        let dir = tempdir().unwrap();
        let result = tool_exec(
            &json!({"command": "echo", "args": [["nested"]]}),
            &ctx(dir.path().to_path_buf()),
        );
        assert!(result["error"].as_str().unwrap().contains("must be a string"));
    }

    #[test]
    fn unknown_tool_returns_available_list() {
        let dir = tempdir().unwrap();
        let mut todos = Vec::new();
        let result = run_tool("frobnicate", &json!({}), &ctx(dir.path().to_path_buf()), &mut todos);
        assert!(result["error"].as_str().unwrap().contains("unknown tool"));
        assert!(result["available"].is_array());
    }

    #[test]
    fn todo_update_adds_items() {
        let mut todos = Vec::new();
        let result = tool_todo_update(
            &json!({"items": [{"id": "s1", "content": "step one", "status": "pending"}]}),
            &mut todos,
        );
        assert_eq!(result["ok"], true);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].id, "s1");
    }

    #[test]
    fn todo_update_upserts_by_id() {
        let mut todos = vec![TodoItem {
            id: "s1".into(),
            content: "old".into(),
            status: TodoStatus::Pending,
        }];
        tool_todo_update(
            &json!({"items": [{"id": "s1", "content": "new", "status": "completed"}]}),
            &mut todos,
        );
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "new");
        assert_eq!(todos[0].status, TodoStatus::Completed);
    }

    #[test]
    fn todo_update_bad_status_returns_error() {
        let mut todos = Vec::new();
        let result = tool_todo_update(
            &json!({"items": [{"id": "s1", "content": "x", "status": "finished"}]}),
            &mut todos,
        );
        assert!(result["error"].as_str().unwrap().contains("unknown todo status"));
    }

    #[test]
    fn tool_schemas_cover_all_tools() {
        let schemas = tool_schemas();
        let names: Vec<&str> = schemas
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        for expected in ["read", "write", "grep", "search", "exec", "todo_update", "finish"] {
            assert!(names.contains(&expected), "missing schema for {}", expected);
        }
    }
}
