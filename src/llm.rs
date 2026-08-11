use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

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
}

impl RealLlmClient {
    pub fn resolve(binary_flag: Option<&str>, config_path: &str, model: Option<&str>) -> Result<Self, LlmError> {
        let binary = resolve_binary(binary_flag)?;
        Ok(Self {
            binary,
            config_path: config_path.to_string(),
            model: model.map(|s| s.to_string()),
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

        let mut out = String::new();
        let mut err = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            stdout.read_to_string(&mut out).map_err(LlmError::StdoutRead)?;
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
        }
        let status = child.wait().map_err(LlmError::Spawn)?;

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
}

#[cfg(test)]
impl MockLlmClient {
    pub fn new(responses: Vec<Result<LlmResponse, LlmError>>) -> Self {
        Self { responses, call_count: std::cell::Cell::new(0) }
    }
}

#[cfg(test)]
impl LlmClient for MockLlmClient {
    fn complete(&self, _messages: &[Message], _tools: &[Value]) -> Result<LlmResponse, LlmError> {
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
}
