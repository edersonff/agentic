use crate::llm::{LlmClient, Message};
use crate::state::TaskState;
use crate::todo::{summary, TodoItem};
use crate::tools::{run_tool, tool_schemas, ToolContext};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub enum LoopEvent {
    TurnStart { turn: u32, max: u32 },
    AssistantResponse { text: String },
    ToolCalled { name: String, args: String },
    ToolFinished { name: String, result: String },
    TodoChanged { summary: String },
}

pub fn run_task_callback(
    client: &dyn LlmClient,
    config: &Config,
    existing: Option<TaskState>,
    callback: &mut dyn FnMut(&LoopEvent),
) -> Result<Outcome, String> {
    run_task_inner(client, config, existing, Some(callback))
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn install_interrupt_handler() {
    let _ = ctrlc::set_handler(|| {
        INTERRUPTED.store(true, Ordering::SeqCst);
    });
}

pub fn was_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn clear_interrupt() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

pub fn set_interrupted() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

#[derive(Clone)]
pub struct Config {
    pub task: String,
    pub max_turns: u32,
    pub json_output: bool,
    pub auto_approve: bool,
    pub workdir: PathBuf,
    pub model: Option<String>,
    pub config_path: String,
    pub binary_flag: Option<String>,
    pub state_dir: Option<PathBuf>,
}

pub struct Outcome {
    pub status: String,
    pub turns: u32,
    pub todos: Vec<TodoItem>,
    pub final_answer: String,
    pub task_id: String,
}

const STUCK_REPEAT_LIMIT: u32 = 3;

pub fn run_task(
    client: &dyn LlmClient,
    config: &Config,
    existing: Option<TaskState>,
) -> Result<Outcome, String> {
    run_task_inner(client, config, existing, None)
}

fn run_task_inner(
    client: &dyn LlmClient,
    config: &Config,
    existing: Option<TaskState>,
    mut callback: Option<&mut dyn FnMut(&LoopEvent)>
) -> Result<Outcome, String> {
    let (task_id, mut messages, mut todos, turn_count, original_created) = match &existing {
        Some(s) => (s.task_id.clone(), s.messages.clone(), s.todos.clone(), s.turn_count, s.created_at.clone()),
        None => {
            let id = new_task_id();
            let mut msgs = vec![
                Message::system(&system_prompt(&config.workdir)),
                Message::user(&format!("Task: {}", config.task)),
            ];
            if config.auto_approve {
                msgs.push(Message::system("destructive commands are allowed: the user passed --yes"));
            }
            let now = chrono::Local::now().to_rfc3339();
            (id, msgs, Vec::new(), 0u32, now)
        }
    };

    let tools = tool_schemas();
    let mut last_call_signature: Option<String> = None;
    let mut repeat_count: u32 = 0;

    let mut current_turn = turn_count;
    while current_turn < config.max_turns {
        if was_interrupted() {
            save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
            return Ok(Outcome {
                status: "interrupted".into(),
                turns: current_turn,
                todos: todos.clone(),
                final_answer: format!(
                    "interrupted at turn {}. task saved as {}. resume with: agentic resume {}",
                    current_turn, task_id, task_id
                ),
                task_id,
            });
        }

        current_turn += 1;
        log_turn(config, current_turn, "calling llm");
        if let Some(cb) = callback.as_mut() {
            cb(&LoopEvent::TurnStart { turn: current_turn, max: config.max_turns });
        }

        let response = match client.complete(&messages, &tools) {
            Ok(r) => r,
            Err(e) => {
                let msg = e.human_message();
                save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
                return Ok(Outcome {
                    status: "error".into(),
                    turns: current_turn,
                    todos: todos.clone(),
                    final_answer: msg,
                    task_id,
                });
            }
        };

        let assistant_content = response.message.content.clone();
        let tool_calls = response.message.tool_calls.clone().unwrap_or_default();
        messages.push(Message::assistant(assistant_content.clone(), response.message.tool_calls.clone()));

        if let Some(text) = &assistant_content {
            if !text.is_empty() {
                log_turn(config, current_turn, &format!("llm said: {}", truncate_log(text, 200)));
                if let Some(cb) = callback.as_mut() {
                    cb(&LoopEvent::AssistantResponse { text: text.clone() });
                }
            }
        }

        if tool_calls.is_empty() {
            if let Some(text) = &assistant_content {
                if let Some(cleaned) = extract_inline_finish(text) {
                    save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
                    return Ok(Outcome {
                        status: "completed".into(),
                        turns: current_turn,
                        todos: todos.clone(),
                        final_answer: cleaned,
                        task_id,
                    });
                }
            }
            save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
            return Ok(Outcome {
                status: "completed".into(),
                turns: current_turn,
                todos: todos.clone(),
                final_answer: assistant_content.unwrap_or_default(),
                task_id,
            });
        }

        for call in &tool_calls {
            log_turn(config, current_turn, &format!("tool: {}({})", call.function.name, call.function.arguments));
            if let Some(cb) = callback.as_mut() {
                cb(&LoopEvent::ToolCalled { name: call.function.name.clone(), args: call.function.arguments.clone() });
            }

            if call.function.name == "finish" {
                let args = call.arguments_value();
                let result = args.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let blocked = args.get("blocked").and_then(|v| v.as_bool()).unwrap_or(false);
                let status = if blocked { "blocked" } else { "completed" };
                messages.push(Message::tool_result(&call.id, &json!({"ok": true, "status": status}).to_string()));
                save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
                return Ok(Outcome {
                    status: status.into(),
                    turns: current_turn,
                    todos: todos.clone(),
                    final_answer: result,
                    task_id,
                });
            }

            let sig = format!("{}:{}", call.function.name, call.function.arguments);
            if Some(&sig) == last_call_signature.as_ref() {
                repeat_count += 1;
            } else {
                repeat_count = 1;
                last_call_signature = Some(sig);
            }

            if repeat_count >= STUCK_REPEAT_LIMIT {
                let msg = format!(
                    "stuck: the agent called {} with the same arguments {} times in a row. task saved as {}",
                    call.function.name, repeat_count, task_id
                );
                messages.push(Message::tool_result(&call.id, &json!({"error": msg, "stuck": true}).to_string()));
                save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
                return Ok(Outcome {
                    status: "blocked".into(),
                    turns: current_turn,
                    todos: todos.clone(),
                    final_answer: msg,
                    task_id,
                });
            }

            let tool_ctx = ToolContext {
                auto_approve: config.auto_approve,
                workdir: config.workdir.clone(),
            };
            let result = run_tool(&call.function.name, &call.arguments_value(), &tool_ctx, &mut todos);
            let result_str = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
            messages.push(Message::tool_result(&call.id, &result_str));

            if let Some(cb) = callback.as_mut() {
                cb(&LoopEvent::ToolFinished { name: call.function.name.clone(), result: result_str.clone() });
            }

            if !todos.is_empty() {
                log_turn(config, current_turn, &format!("todos: {}", summary(&todos)));
                if let Some(cb) = callback.as_mut() {
                    cb(&LoopEvent::TodoChanged { summary: summary(&todos) });
                }
            }
        }

        save_state_now(&task_id, config, &messages, &todos, current_turn, &original_created)?;
    }

    let msg = format!(
        "reached max turns ({}). task saved as {}. resume with: agentic resume {}",
        config.max_turns, task_id, task_id
    );
    Ok(Outcome {
        status: "blocked".into(),
        turns: current_turn,
        todos: todos.clone(),
        final_answer: msg,
        task_id,
    })
}

fn system_prompt(workdir: &PathBuf) -> String {
    format!(
        r#"you are a task agent. you complete tasks by calling tools.

tools (call these as function/tool calls, NEVER write them as text):
- read(path): read a file's text contents
- write(path, content): write text to a file (overwrites)
- grep(pattern, path?): search file contents with regex, path defaults to current directory
- search(query): search the web with ddgr
- exec(command, args): run a command (no shell features; destructive commands blocked without --yes)
- todo_update(items): set your todo list. items = [{{id, content, status}}]. status = pending|in_progress|completed|blocked
- finish(result, blocked?): signal done. MUST be called as a tool call, not written as text.

rules:
- read a file before writing it, so you know what you are changing
- use todo_update for tasks with more than one step
- call finish as a tool call when done or blocked. if blocked, set blocked=true and explain why in result
- if a tool returns an error, read the "fix" field and try a different approach
- do not repeat the same tool call that already failed
- be concise. call tools to act, do not narrate what you will do
- NEVER write tool names like finish() or read() as text. always use the tool call mechanism.

working directory: {}"#,
        workdir.display()
    )
}

fn new_task_id() -> String {
    let now = chrono::Local::now();
    let short = uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("x").to_string();
    format!("{}-{}", now.format("%Y%m%d-%H%M%S"), short)
}

fn save_state_now(
    task_id: &str,
    config: &Config,
    messages: &[Message],
    todos: &[TodoItem],
    turn_count: u32,
    created_at: &str,
) -> Result<(), String> {
    let base = config
        .state_dir
        .clone()
        .unwrap_or_else(crate::state::state_dir);
    let now = chrono::Local::now().to_rfc3339();
    let state = TaskState {
        task_id: task_id.to_string(),
        task: config.task.clone(),
        messages: messages.to_vec(),
        todos: todos.to_vec(),
        turn_count,
        max_turns: config.max_turns,
        model: config.model.clone(),
        config_path: config.config_path.clone(),
        auto_approve: config.auto_approve,
        created_at: created_at.to_string(),
        updated_at: now,
    };
    crate::state::save_state_in(&base, &state).map_err(|e| format!("could not save task state: {}", e))
}

fn log_turn(config: &Config, turn: u32, msg: &str) {
    if !config.json_output {
        eprintln!("[turn {}] {}", turn, msg);
    }
}

fn truncate_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn extract_inline_finish(text: &str) -> Option<String> {
    let patterns = [
        r#"finish\(result\s*[:=]\s*"([^"]*)""#,
        r#"finish\(result\s*[:=]\s*'([^']*)'"#,
        r#"finish\("([^"]*)"\)"#,
        r#"finish\('([^']*)'\)"#,
    ];
    for pat in &patterns {
        if let Some(captured) = simple_regex_capture(pat, text) {
            let before = text.find("finish(").unwrap_or(0);
            let cleaned = text[..before].trim_end().to_string();
            if cleaned.is_empty() {
                return Some(captured);
            }
            return Some(cleaned);
        }
    }
    None
}

fn simple_regex_capture(pattern: &str, text: &str) -> Option<String> {
    let pat_lower = pattern.to_lowercase();
    let text_lower = text.to_lowercase();
    if pat_lower.contains(r#"result"#) {
        let markers = [r#"result=""#, r#"result:""#, r#"result =""#, r#"result= ""#];
        for marker in markers {
            if let Some(pos) = text_lower.find(marker) {
                let after = pos + marker.len();
                if after >= text.len() {
                    continue;
                }
                let remaining = &text[after..];
                let end = remaining.find('"').or_else(|| remaining.find("'"));
                if let Some(e) = end {
                    return Some(remaining[..e].to_string());
                }
            }
        }
    }
    if pat_lower.starts_with(r#"finish(""#) || pat_lower.starts_with(r#"finish('"#) {
        if let Some(pos) = text_lower.find("finish(") {
            let after = pos + 7;
            if after >= text.len() {
                return None;
            }
            let remaining = &text[after..];
            let quote = remaining.chars().next()?;
            if quote == '"' || quote == '\'' {
                let inner = &remaining[1..];
                let end = inner.find(quote)?;
                return Some(inner[..end].to_string());
            }
        }
    }
    None
}

pub fn outcome_to_json(out: &Outcome) -> Value {
    json!({
        "task_id": out.task_id,
        "status": out.status,
        "turns": out.turns,
        "todos": out.todos,
        "final_answer": out.final_answer
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmResponse, MockLlmClient, LlmError, ToolCall, FunctionCall};
    use tempfile::tempdir;

    fn base_config(workdir: PathBuf, state_dir: PathBuf) -> Config {
        Config {
            task: "test task".into(),
            max_turns: 5,
            json_output: false,
            auto_approve: false,
            workdir,
            model: None,
            config_path: "config.yaml".into(),
            binary_flag: None,
            state_dir: Some(state_dir),
        }
    }

    #[test]
    fn finish_tool_completes_task() {
        clear_interrupt();
        let dir = tempdir().unwrap();
        let client = MockLlmClient::new(vec![Ok(LlmResponse {
            message: Message::assistant(
                Some("done".into()),
                Some(vec![ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "finish".into(),
                        arguments: r#"{"result":"task complete"}"#.into(),
                    },
                }]),
            ),
            finish_reason: "tool_calls".into(),
        })]);
        let out = run_task(&client, &base_config(dir.path().to_path_buf(), dir.path().to_path_buf()), None).unwrap();
        assert_eq!(out.status, "completed");
        assert_eq!(out.final_answer, "task complete");
        assert_eq!(out.turns, 1);
    }

    #[test]
    fn finish_blocked_returns_blocked_status() {
        clear_interrupt();
        let dir = tempdir().unwrap();
        let client = MockLlmClient::new(vec![Ok(LlmResponse {
            message: Message::assistant(
                Some("stuck".into()),
                Some(vec![ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "finish".into(),
                        arguments: r#"{"result":"cannot find file","blocked":true}"#.into(),
                    },
                }]),
            ),
            finish_reason: "tool_calls".into(),
        })]);
        let out = run_task(&client, &base_config(dir.path().to_path_buf(), dir.path().to_path_buf()), None).unwrap();
        assert_eq!(out.status, "blocked");
        assert_eq!(out.final_answer, "cannot find file");
    }

    #[test]
    fn no_tool_calls_completes_with_text() {
        clear_interrupt();
        let dir = tempdir().unwrap();
        let client = MockLlmClient::new(vec![Ok(LlmResponse {
            message: Message::assistant(Some("here is the answer".into()), None),
            finish_reason: "stop".into(),
        })]);
        let out = run_task(&client, &base_config(dir.path().to_path_buf(), dir.path().to_path_buf()), None).unwrap();
        assert_eq!(out.status, "completed");
        assert_eq!(out.final_answer, "here is the answer");
    }

    #[test]
    fn max_turns_reached_returns_blocked() {
        clear_interrupt();
        let dir = tempdir().unwrap();
        let paths = ["/tmp/a", "/tmp/b", "/tmp/c"];
        let responses: Vec<Result<LlmResponse, LlmError>> = paths
            .iter()
            .map(|p| {
                Ok(LlmResponse {
                    message: Message::assistant(
                        None,
                        Some(vec![ToolCall {
                            id: "c".into(),
                            call_type: "function".into(),
                            function: FunctionCall {
                                name: "read".into(),
                                arguments: format!(r#"{{"path":"{}"}}"#, p),
                            },
                        }]),
                    ),
                    finish_reason: "tool_calls".into(),
                })
            })
            .collect();
        let client = MockLlmClient::new(responses);
        let mut cfg = base_config(dir.path().to_path_buf(), dir.path().to_path_buf());
        cfg.max_turns = 3;
        let out = run_task(&client, &cfg, None).unwrap();
        assert_eq!(out.status, "blocked");
        assert!(out.final_answer.contains("max turns"), "got: {}", out.final_answer);
    }

    #[test]
    fn stuck_detection_blocks_on_repeat() {
        clear_interrupt();
        let dir = tempdir().unwrap();
        let same_call = || Ok(LlmResponse {
            message: Message::assistant(
                None,
                Some(vec![ToolCall {
                    id: "c".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "read".into(),
                        arguments: r#"{"path":"/nonexistent/same/path"}"#.into(),
                    },
                }]),
            ),
            finish_reason: "tool_calls".into(),
        });
        let client = MockLlmClient::new(vec![same_call(), same_call(), same_call()]);
        let out = run_task(&client, &base_config(dir.path().to_path_buf(), dir.path().to_path_buf()), None).unwrap();
        assert_eq!(out.status, "blocked");
        assert!(out.final_answer.contains("stuck"));
    }

    #[test]
    fn llm_error_returns_error_status() {
        clear_interrupt();
        let dir = tempdir().unwrap();
        let client = MockLlmClient::new(vec![Err(LlmError::NoChoices)]);
        let out = run_task(&client, &base_config(dir.path().to_path_buf(), dir.path().to_path_buf()), None).unwrap();
        assert_eq!(out.status, "error");
    }

    #[test]
    fn empty_task_is_rejected_at_cli_level() {
        let task = "";
        assert!(task.trim().is_empty());
    }
}
