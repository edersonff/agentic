use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: &str) -> Self {
        Self { role: "system".into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn user(text: &str) -> Self {
        Self { role: "user".into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn assistant(text: Option<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self { role: "assistant".into(), content: text, tool_calls, tool_call_id: None }
    }
    pub fn tool_result(tool_call_id: &str, text: &str) -> Self {
        Self { role: "tool".into(), content: Some(text.into()), tool_calls: None, tool_call_id: Some(tool_call_id.into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

impl ToolCall {
    pub fn arguments_value(&self) -> Value {
        if self.function.arguments.is_empty() {
            return json!({});
        }
        serde_json::from_str(&self.function.arguments).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

pub struct LlmResponse {
    pub message: Message,
    pub finish_reason: String,
}

#[derive(Debug)]
pub enum LlmError {
    BinaryNotFound { searched: Vec<String> },
    Spawn(io::Error),
    StdinWrite(io::Error),
    StdoutRead(io::Error),
    Timeout { secs: u64 },
    NonZeroExit { code: i32, stderr: String },
    BadJson(String),
    NoChoices,
}

impl LlmError {
    pub fn human_message(&self) -> String {
        match self {
            LlmError::BinaryNotFound { searched } => format!(
                "llm-adapter not found. tried: {}. install with: sheol pull edersonff/llm-adapter, then build and put the binary on your PATH, or set --llm-binary <path> or LLM_ADAPTER_BINARY=<path>",
                searched.join(", ")
            ),
            LlmError::Spawn(e) => format!("could not start llm-adapter: {}", e),
            LlmError::StdinWrite(e) => format!("could not send request to llm-adapter: {}", e),
            LlmError::StdoutRead(e) => format!("could not read answer from llm-adapter: {}", e),
            LlmError::Timeout { secs } => format!(
                "llm-adapter did not answer within {}s, so the task stops here instead of inventing an answer. next steps: check the gateway the adapter config points at is up, then resume with: agentic resume <task-id>. raise the limit with AGENTIC_LLM_TIMEOUT_SECS if the model is just slow",
                secs
            ),
            LlmError::NonZeroExit { code, stderr } => format!(
                "llm-adapter exited with code {}. stderr: {}",
                code,
                stderr.trim()
            ),
            LlmError::BadJson(reason) => format!(
                "llm-adapter returned something that is not valid openai json: {}. this usually means the provider returned an error page or the config is wrong",
                reason
            ),
            LlmError::NoChoices => "llm-adapter returned no choices. the model may have refused or the request was rejected".into(),
        }
    }
}

pub trait LlmClient {
    fn complete(&self, messages: &[Message], tools: &[Value]) -> Result<LlmResponse, LlmError>;
}

pub struct RealLlmClient {
    pub binary: String,
    pub config_path: String,
    pub model: Option<String>,
    pub timeout_secs: u64,
}

impl RealLlmClient {
    pub fn resolve(binary_flag: Option<&str>, config_path: &str, model: Option<&str>) -> Result<Self, LlmError> {
        let binary = resolve_binary(binary_flag)?;
        let timeout_secs = std::env::var("AGENTIC_LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(300);
        Ok(Self {
            binary,
            config_path: config_path.to_string(),
            model: model.map(|s| s.to_string()),
            timeout_secs,
        })
    }
}

fn resolve_binary(flag: Option<&str>) -> Result<String, LlmError> {
    let mut searched = Vec::new();
    if let Some(path) = flag {
        if std::path::Path::new(path).exists() || which(path) {
            return Ok(path.to_string());
        }
        searched.push(format!("flag --llm-binary={}", path));
    }
    if let Ok(env_path) = std::env::var("LLM_ADAPTER_BINARY") {
        if std::path::Path::new(&env_path).exists() || which(&env_path) {
            return Ok(env_path);
        }
        searched.push(format!("env LLM_ADAPTER_BINARY={}", env_path));
    }
    if which("llm-adapter") {
        return Ok("llm-adapter".to_string());
    }
    searched.push("PATH:llm-adapter".to_string());
    Err(LlmError::BinaryNotFound { searched })
}

fn which(name: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if std::path::Path::new(dir).join(name).exists() {
                return true;
            }
        }
    }
    false
}

impl LlmClient for RealLlmClient {
    fn complete(&self, messages: &[Message], tools: &[Value]) -> Result<LlmResponse, LlmError> {
        let mut body = json!({
            "messages": messages,
            "temperature": 0.7,
        });
        if let Some(m) = &self.model {
            body["model"] = json!(m);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        let mut child = Command::new(&self.binary)
            .arg("--config")
            .arg(&self.config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(LlmError::Spawn)?;

        let stdin = child.stdin.as_mut().ok_or(LlmError::StdinWrite(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "no stdin",
        )))?;
        let payload = serde_json::to_string(&body).unwrap_or_default();
        stdin
            .write_all(payload.as_bytes())
            .map_err(LlmError::StdinWrite)?;
        drop(child.stdin.take());

        let stdout_reader = child.stdout.take();
        let stderr_reader = child.stderr.take();
        let stdout_thread = std::thread::spawn(move || {
            let mut out = String::new();
            if let Some(mut r) = stdout_reader {
                let _ = r.read_to_string(&mut out);
            }
            out
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut err = String::new();
            if let Some(mut r) = stderr_reader {
                let _ = r.read_to_string(&mut err);
            }
            err
        });

        let deadline = Instant::now() + Duration::from_secs(self.timeout_secs);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(LlmError::Timeout { secs: self.timeout_secs });
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(LlmError::Spawn(e)),
            }
        };

        let out = stdout_thread.join().unwrap_or_default();
        let err = stderr_thread.join().unwrap_or_default();

        if !status.success() {
            return Err(LlmError::NonZeroExit {
                code: status.code().unwrap_or(-1),
                stderr: err,
            });
        }

        if !err.trim().is_empty() {
            eprintln!("{}", err.trim());
        }

        let resp: Value = serde_json::from_str(&out).map_err(|e| LlmError::BadJson(e.to_string()))?;
        let choices = resp.get("choices").and_then(|c| c.as_array());
        let first = choices
            .and_then(|c| c.first())
            .ok_or(LlmError::NoChoices)?;
        let msg = first.get("message").ok_or(LlmError::NoChoices)?;
        let finish_reason = first
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .unwrap_or("stop")
            .to_string();
        let message: Message = serde_json::from_value(msg.clone())
            .map_err(|e| LlmError::BadJson(e.to_string()))?;

        Ok(LlmResponse { message, finish_reason })
    }
}

#[cfg(test)]
pub struct MockLlmClient {
    pub responses: Vec<Result<LlmResponse, LlmError>>,
    pub call_count: std::cell::Cell<usize>,
    pub calls: std::cell::RefCell<Vec<Vec<Message>>>,
}

#[cfg(test)]
impl MockLlmClient {
    pub fn new(responses: Vec<Result<LlmResponse, LlmError>>) -> Self {
        Self {
            responses,
            call_count: std::cell::Cell::new(0),
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl LlmClient for MockLlmClient {
    fn complete(&self, messages: &[Message], _tools: &[Value]) -> Result<LlmResponse, LlmError> {
        self.calls.borrow_mut().push(messages.to_vec());
        let i = self.call_count.get();
        self.call_count.set(i + 1);
        if i < self.responses.len() {
            match &self.responses[i] {
                Ok(r) => Ok(LlmResponse {
                    message: r.message.clone(),
                    finish_reason: r.finish_reason.clone(),
                }),
                Err(e) => Err(clone_error(e)),
            }
        } else {
            Err(LlmError::NoChoices)
        }
    }
}

#[cfg(test)]
fn clone_error(e: &LlmError) -> LlmError {
    match e {
        LlmError::BinaryNotFound { searched } => LlmError::BinaryNotFound { searched: searched.clone() },
        LlmError::NoChoices => LlmError::NoChoices,
        LlmError::BadJson(s) => LlmError::BadJson(s.clone()),
        _ => LlmError::NoChoices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_system_serializes_openai() {
        let m = Message::system("you are helpful");
        let v: Value = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(v["role"], "system");
        assert_eq!(v["content"], "you are helpful");
    }

    #[test]
    fn tool_result_has_tool_call_id() {
        let m = Message::tool_result("call_1", "42");
        let v: Value = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "call_1");
        assert_eq!(v["content"], "42");
    }

    #[test]
    fn tool_call_arguments_parsed_as_json() {
        let tc = ToolCall {
            id: "x".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read".into(),
                arguments: r#"{"path":"/tmp/x"}"#.into(),
            },
        };
        let args = tc.arguments_value();
        assert_eq!(args["path"], "/tmp/x");
    }

    #[test]
    fn tool_call_bad_arguments_returns_empty_object() {
        let tc = ToolCall {
            id: "x".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read".into(),
                arguments: "not json".into(),
            },
        };
        let args = tc.arguments_value();
        assert!(args.is_object());
    }

    #[test]
    fn binary_not_found_message_names_fix() {
        let err = LlmError::BinaryNotFound { searched: vec!["PATH:llm-adapter".into()] };
        let msg = err.human_message();
        assert!(msg.contains("llm-adapter not found"));
        assert!(msg.contains("sheol pull"));
    }

    #[test]
    fn timeout_message_names_next_step() {
        let msg = LlmError::Timeout { secs: 30 }.human_message();
        assert!(msg.contains("did not answer within 30s"));
        assert!(msg.contains("AGENTIC_LLM_TIMEOUT_SECS"));
        assert!(msg.contains("agentic resume"));
    }

    fn write_fake_adapter(dir: &std::path::Path, body: &str) -> String {
        let path = dir.join("fake-adapter.sh");
        std::fs::write(&path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn real_client_sends_tools_and_tool_messages_to_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("captured.json");
        let capture_str = capture.to_str().unwrap().to_string();
        let script = write_fake_adapter(
            dir.path(),
            &format!(
                "#!/bin/bash\ncat > \"{capture_str}\"\nprintf '{{\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":null,\"tool_calls\":[{{\"id\":\"call_1\",\"type\":\"function\",\"function\":{{\"name\":\"read\",\"arguments\":\"{{\\\\\"path\\\\\":\\\\\"/tmp/x\\\\\"}}\"}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}'\n"
            ),
        );
        let client = RealLlmClient {
            binary: script,
            config_path: "unused.yaml".into(),
            model: Some("glm-5.2".into()),
            timeout_secs: 30,
        };
        let messages = vec![
            Message::user("read /tmp/x"),
            Message::assistant(None, Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall { name: "read".into(), arguments: "{\"path\":\"/tmp/x\"}".into() },
            }])),
            Message::tool_result("call_1", "{\"content\": \"hi\"}"),
        ];
        let resp = client.complete(&messages, &[json!({"type": "function", "function": {"name": "read"}})]).unwrap();
        assert_eq!(resp.finish_reason, "tool_calls");
        assert_eq!(resp.message.tool_calls.as_ref().unwrap()[0].id, "call_1");

        let sent: Value = serde_json::from_str(&std::fs::read_to_string(&capture).unwrap()).unwrap();
        assert_eq!(sent["model"], "glm-5.2");
        assert_eq!(sent["tools"][0]["function"]["name"], "read", "tools must be forwarded: {sent}");
        assert_eq!(sent["tool_choice"], "auto", "tool_choice must be forwarded: {sent}");
        assert_eq!(sent["messages"][1]["tool_calls"][0]["id"], "call_1", "assistant tool_calls must be forwarded: {sent}");
        assert_eq!(sent["messages"][2]["role"], "tool", "tool result must be forwarded: {sent}");
        assert_eq!(sent["messages"][2]["tool_call_id"], "call_1");
    }

    #[test]
    fn real_client_times_out_hanging_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_fake_adapter(dir.path(), "#!/bin/bash\nsleep 30\n");
        let client = RealLlmClient {
            binary: script,
            config_path: "unused.yaml".into(),
            model: None,
            timeout_secs: 1,
        };
        let started = std::time::Instant::now();
        let result = client.complete(&[Message::user("hi")], &[]);
        match result {
            Err(LlmError::Timeout { secs }) => assert_eq!(secs, 1),
            other => panic!("expected Timeout, got: {:?}", other.map(|r| r.finish_reason)),
        }
        assert!(started.elapsed().as_secs() < 10, "timeout must fire near the deadline, took {:?}", started.elapsed());
    }

    #[test]
    fn real_client_adapter_nonzero_exit_is_honest_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_fake_adapter(dir.path(), "#!/bin/bash\necho 'error: gateway down' >&2\nexit 2\n");
        let client = RealLlmClient {
            binary: script,
            config_path: "unused.yaml".into(),
            model: None,
            timeout_secs: 30,
        };
        match client.complete(&[Message::user("hi")], &[]) {
            Err(LlmError::NonZeroExit { code, stderr }) => {
                assert_eq!(code, 2);
                assert!(stderr.contains("gateway down"), "stderr must surface the real cause, got: {stderr}");
            }
            other => panic!("expected NonZeroExit, got: {:?}", other.map(|r| r.finish_reason)),
        }
    }
}
