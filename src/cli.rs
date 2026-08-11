use crate::chat::{install_interrupt_handler, outcome_to_json, run_task, was_interrupted, Config};
use crate::llm::RealLlmClient;
use crate::state::{exit_code_for_status, load_state, list_saved_tasks};
use crate::tools::tool_schemas;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agentic",
    about = "runs a task to completion using an llm and tools, with security gates, a todo list, and interruptible state",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "run a task until it is complete or blocked")]
    Run {
        #[arg(long, help = "the task to complete")]
        task: String,
        #[arg(long, default_value_t = 20, help = "maximum turns before saving and stopping")]
        max_turns: u32,
        #[arg(long, help = "output a json envelope on stdout for agents")]
        json: bool,
        #[arg(long, help = "allow destructive commands (rm, sudo, git push) without prompting")]
        yes: bool,
        #[arg(long, help = "llm-adapter config file path", default_value = "config.yaml")]
        llm_config: String,
        #[arg(long, help = "model name to pass to llm-adapter")]
        model: Option<String>,
        #[arg(long, help = "path to the llm-adapter binary, or set LLM_ADAPTER_BINARY")]
        llm_binary: Option<String>,
    },
    #[command(about = "resume an interrupted or blocked task by id")]
    Resume {
        #[arg(help = "the task id shown when the task was saved")]
        task_id: String,
        #[arg(long, help = "output a json envelope on stdout for agents")]
        json: bool,
        #[arg(long, help = "allow destructive commands without prompting")]
        yes: bool,
        #[arg(long, help = "llm-adapter config file path", default_value = "config.yaml")]
        llm_config: String,
        #[arg(long, help = "model name to pass to llm-adapter")]
        model: Option<String>,
        #[arg(long, help = "path to the llm-adapter binary")]
        llm_binary: Option<String>,
        #[arg(long, help = "override the max turns from the original run")]
        max_turns: Option<u32>,
    },
    #[command(about = "list available tools and what they do")]
    Tools,
    #[command(about = "list saved task ids")]
    Tasks,
    #[command(about = "run a task with a live terminal interface showing turns, tool calls, and results")]
    Chat {
        #[arg(long, help = "the task to complete")]
        task: String,
        #[arg(long, default_value_t = 20, help = "maximum turns before saving and stopping")]
        max_turns: u32,
        #[arg(long, help = "allow destructive commands without prompting")]
        yes: bool,
        #[arg(long, help = "llm-adapter config file path", default_value = "config.yaml")]
        llm_config: String,
        #[arg(long, help = "model name to pass to llm-adapter")]
        model: Option<String>,
        #[arg(long, help = "path to the llm-adapter binary, or set LLM_ADAPTER_BINARY")]
        llm_binary: Option<String>,
    },
}

pub fn run() -> i32 {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            task,
            max_turns,
            json,
            yes,
            llm_config,
            model,
            llm_binary,
        } => cmd_run(&task, max_turns, json, yes, &llm_config, model, llm_binary),
        Command::Resume {
            task_id,
            json,
            yes,
            llm_config,
            model,
            llm_binary,
            max_turns,
        } => cmd_resume(&task_id, json, yes, &llm_config, model, llm_binary, max_turns),
        Command::Tools => {
            print_tools();
            0
        }
        Command::Tasks => {
            print_tasks();
            0
        }
        Command::Chat {
            task,
            max_turns,
            yes,
            llm_config,
            model,
            llm_binary,
        } => {
            let config = Config {
                task,
                max_turns,
                json_output: false,
                auto_approve: yes,
                workdir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                model,
                config_path: llm_config,
                binary_flag: llm_binary,
                state_dir: None,
            };
            crate::tui::run(config)
        }
    }
}

fn cmd_run(
    task: &str,
    max_turns: u32,
    json: bool,
    yes: bool,
    llm_config: &str,
    model: Option<String>,
    llm_binary: Option<String>,
) -> i32 {
    if task.trim().is_empty() {
        return fail("task cannot be empty. pass --task \"what you want done\"", json);
    }
    if max_turns == 0 {
        return fail("max-turns must be at least 1", json);
    }
    if !std::path::Path::new(llm_config).exists() {
        return fail(
            &format!(
                "llm config not found at {}. copy it from llm-adapter's config.yaml.template and fill in your keys",
                llm_config
            ),
            json,
        );
    }

    let client = match RealLlmClient::resolve(llm_binary.as_deref(), llm_config, model.as_deref()) {
        Ok(c) => c,
        Err(e) => return fail(&e.human_message(), json),
    };

    install_interrupt_handler();

    let workdir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let config = Config {
        task: task.to_string(),
        max_turns,
        json_output: json,
        auto_approve: yes,
        workdir,
        model,
        config_path: llm_config.to_string(),
        binary_flag: llm_binary,
        state_dir: None,
    };

    match run_task(&client, &config, None) {
        Ok(outcome) => finish_outcome(&outcome, json),
        Err(e) => fail(&e, json),
    }
}

fn cmd_resume(
    task_id: &str,
    json: bool,
    yes: bool,
    llm_config: &str,
    model: Option<String>,
    llm_binary: Option<String>,
    max_turns_override: Option<u32>,
) -> i32 {
    let state = match load_state(task_id) {
        Ok(s) => s,
        Err(e) => return fail(&e.human_message(), json),
    };
    if !std::path::Path::new(llm_config).exists() {
        return fail(
            &format!("llm config not found at {}. the original run used: {}", llm_config, state.config_path),
            json,
        );
    }
    let client = match RealLlmClient::resolve(llm_binary.as_deref(), llm_config, model.as_deref()) {
        Ok(c) => c,
        Err(e) => return fail(&e.human_message(), json),
    };
    install_interrupt_handler();

    let workdir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let config = Config {
        task: state.task.clone(),
        max_turns: max_turns_override.unwrap_or(state.max_turns),
        json_output: json,
        auto_approve: yes || state.auto_approve,
        workdir,
        model: model.or(state.model.clone()),
        config_path: llm_config.to_string(),
        binary_flag: llm_binary,
        state_dir: None,
    };

    eprintln!("[resume] task {} at turn {}/{}", task_id, state.turn_count, config.max_turns);
    match run_task(&client, &config, Some(state)) {
        Ok(outcome) => finish_outcome(&outcome, json),
        Err(e) => fail(&e, json),
    }
}

fn finish_outcome(outcome: &crate::chat::Outcome, json: bool) -> i32 {
    if json {
        let envelope = outcome_to_json(outcome);
        println!("{}", serde_json::to_string_pretty(&envelope).unwrap_or_default());
    } else {
        println!("{}", outcome.final_answer);
        if !outcome.todos.is_empty() {
            eprintln!();
            eprintln!("todos: {}", crate::todo::summary(&outcome.todos));
            for t in &outcome.todos {
                eprintln!("  [{}] {} — {}", t.status.as_str(), t.id, t.content);
            }
        }
        if outcome.status == "interrupted" {
            eprintln!();
            eprintln!("interrupted. {}", outcome.final_answer);
        }
    }
    let code = exit_code_for_status(&outcome.status);
    if was_interrupted() {
        return 130;
    }
    code
}

fn fail(msg: &str, json: bool) -> i32 {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"status": "error", "error": msg}))
                .unwrap_or_default()
        );
    } else {
        eprintln!("{}", msg);
    }
    1
}

fn print_tools() {
    let schemas = tool_schemas();
    for s in &schemas {
        let f = &s["function"];
        println!("{}", f["name"].as_str().unwrap_or("?"));
        println!("  {}", f["description"].as_str().unwrap_or(""));
        if let Some(props) = f["parameters"]["properties"].as_object() {
            let required: Vec<&str> = f["parameters"]["required"]
                .as_array()
                .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            for (k, v) in props {
                let marker = if required.contains(&k.as_str()) { "*" } else { " " };
                let desc = v.get("description").and_then(|d| d.as_str()).unwrap_or("");
                println!("    {} {}: {}", marker, k, desc);
            }
        }
        println!();
    }
}

fn print_tasks() {
    let ids = list_saved_tasks();
    if ids.is_empty() {
        println!("no saved tasks");
        return;
    }
    for id in &ids {
        match load_state(id) {
            Ok(s) => println!("{}  turn {}/{}  {}", id, s.turn_count, s.max_turns, s.task),
            Err(_) => println!("{}  (unreadable)", id),
        }
    }
}
