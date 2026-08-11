use crate::llm::Message;
use crate::todo::TodoItem;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    pub task: String,
    pub messages: Vec<Message>,
    pub todos: Vec<TodoItem>,
    pub turn_count: u32,
    pub max_turns: u32,
    pub model: Option<String>,
    pub config_path: String,
    pub auto_approve: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub fn state_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("AGENT_RUNTIME_HOME") {
        return PathBuf::from(custom);
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("agentic");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local").join("share").join("agentic")
}

pub fn task_path(task_id: &str) -> PathBuf {
    state_dir().join(format!("{}.json", task_id))
}

pub fn save_state(state: &TaskState) -> Result<(), io::Error> {
    save_state_in(&state_dir(), state)
}

pub fn save_state_in(dir: &PathBuf, state: &TaskState) -> Result<(), io::Error> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", state.task_id));
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load_state(task_id: &str) -> Result<TaskState, LoadError> {
    let path = task_path(task_id);
    if !path.exists() {
        let dir = state_dir();
        return Err(LoadError::NotFound {
            id: task_id.to_string(),
            looked_at: path,
            state_dir: dir,
        });
    }
    let body = fs::read_to_string(&path)?;
    let state: TaskState = serde_json::from_str(&body).map_err(|e| LoadError::Corrupt {
        id: task_id.to_string(),
        path,
        reason: e.to_string(),
    })?;
    Ok(state)
}

#[derive(Debug)]
pub enum LoadError {
    NotFound { id: String, looked_at: PathBuf, state_dir: PathBuf },
    Corrupt { id: String, path: PathBuf, reason: String },
    Io(io::Error),
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        LoadError::Io(e)
    }
}

impl LoadError {
    pub fn human_message(&self) -> String {
        match self {
            LoadError::NotFound { id, looked_at, state_dir } => format!(
                "no saved task with id \"{}\". looked at {}. saved tasks live in {}",
                id,
                looked_at.display(),
                state_dir.display()
            ),
            LoadError::Corrupt { id, path, reason } => format!(
                "task \"{}\" at {} is unreadable: {}. delete it and start a new task",
                id,
                path.display(),
                reason
            ),
            LoadError::Io(e) => format!("could not read task state: {}", e),
        }
    }
}

pub fn exit_code_for_status(status: &str) -> i32 {
    match status {
        "completed" => 0,
        "blocked" => 2,
        "interrupted" => 130,
        _ => 1,
    }
}

pub fn list_saved_tasks() -> Vec<String> {
    let dir = state_dir();
    let mut ids = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str()) {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_returns_not_found_with_fix() {
        let err = load_state("nonexistent-task-12345").unwrap_err();
        let msg = err.human_message();
        assert!(msg.contains("no saved task"));
        assert!(msg.contains("nonexistent-task-12345"));
    }

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(exit_code_for_status("completed"), 0);
        assert_eq!(exit_code_for_status("blocked"), 2);
        assert_eq!(exit_code_for_status("interrupted"), 130);
        assert_eq!(exit_code_for_status("error"), 1);
    }
}
