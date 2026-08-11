use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

impl TodoStatus {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "in_progress" | "inprogress" => Ok(Self::InProgress),
            "completed" | "done" => Ok(Self::Completed),
            "blocked" => Ok(Self::Blocked),
            other => Err(format!(
                "unknown todo status '{}': expected pending, in_progress, completed, or blocked",
                other
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

pub fn upsert(items: &[TodoItem], updates: &[TodoItem]) -> Vec<TodoItem> {
    let mut out: Vec<TodoItem> = items.to_vec();
    for upd in updates {
        if let Some(existing) = out.iter_mut().find(|i| i.id == upd.id) {
            existing.content = upd.content.clone();
            existing.status = upd.status.clone();
        } else {
            out.push(upd.clone());
        }
    }
    out
}

pub fn summary(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "no todos".to_string();
    }
    let counts = count_by_status(items);
    format!(
        "{} total: {} pending, {} in progress, {} completed, {} blocked",
        items.len(),
        counts.0,
        counts.1,
        counts.2,
        counts.3
    )
}

fn count_by_status(items: &[TodoItem]) -> (usize, usize, usize, usize) {
    let mut p = 0;
    let mut ip = 0;
    let mut c = 0;
    let mut b = 0;
    for i in items {
        match i.status {
            TodoStatus::Pending => p += 1,
            TodoStatus::InProgress => ip += 1,
            TodoStatus::Completed => c += 1,
            TodoStatus::Blocked => b += 1,
        }
    }
    (p, ip, c, b)
}
