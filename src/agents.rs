use crate::client::types::{Content, GenerateContentRequest, GenerationConfig, Part};
use crate::client::AiClient;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Semaphore;

const MAX_AGENT_OUTPUT_CHARS: usize = 16_000;

#[derive(Clone)]
pub struct AgentManager {
    next_id: Arc<AtomicU64>,
    agents: Arc<Mutex<BTreeMap<String, AgentRecord>>>,
    slots: Arc<Semaphore>,
}

struct AgentRecord {
    task: String,
    status: String,
    output: String,
    tx: UnboundedSender<AgentCommand>,
}

enum AgentCommand {
    Message(String),
    Stop,
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            agents: Arc::new(Mutex::new(BTreeMap::new())),
            slots: Arc::new(Semaphore::new(8)),
        }
    }

    pub fn spawn(
        &self,
        client: AiClient,
        model: String,
        fallbacks: Vec<String>,
        max_retries: u32,
        task: String,
    ) -> Result<String, String> {
        let task = task.trim().to_string();
        if task.is_empty() {
            return Err("Agent task cannot be empty".to_string());
        }
        let id = format!("agent-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::unbounded_channel();
        self.agents
            .lock()
            .map_err(|_| "Agent manager lock is poisoned".to_string())?
            .insert(
                id.clone(),
                AgentRecord {
                    task: task.clone(),
                    status: "queued".to_string(),
                    output: String::new(),
                    tx,
                },
            );

        let agents = self.agents.clone();
        let slots = self.slots.clone();
        let worker_id = id.clone();
        tokio::spawn(async move {
            agent_loop(
                agents,
                worker_id,
                client,
                model,
                fallbacks,
                max_retries,
                task,
                rx,
                slots,
            )
            .await;
        });
        Ok(format!(
            "Spawned {}. Use agent_status or agent_inspect to check it.",
            id
        ))
    }

    pub fn message(&self, id: &str, message: &str) -> Result<String, String> {
        let agents = self
            .agents
            .lock()
            .map_err(|_| "Agent manager lock is poisoned".to_string())?;
        let agent = agents
            .get(id)
            .ok_or_else(|| format!("Unknown agent '{}'", id))?;
        agent
            .tx
            .send(AgentCommand::Message(message.trim().to_string()))
            .map_err(|_| format!("Agent '{}' is no longer running", id))?;
        Ok(format!("Message queued for {}.", id))
    }

    pub fn stop(&self, id: &str) -> Result<String, String> {
        let agents = self
            .agents
            .lock()
            .map_err(|_| "Agent manager lock is poisoned".to_string())?;
        let agent = agents
            .get(id)
            .ok_or_else(|| format!("Unknown agent '{}'", id))?;
        agent
            .tx
            .send(AgentCommand::Stop)
            .map_err(|_| format!("Agent '{}' is no longer running", id))?;
        Ok(format!("Stop requested for {}.", id))
    }

    pub fn status(&self) -> String {
        let Ok(agents) = self.agents.lock() else {
            return "Agent manager lock is poisoned.".to_string();
        };
        if agents.is_empty() {
            return "No agents are running.".to_string();
        }
        agents
            .iter()
            .map(|(id, agent)| {
                let preview = one_line(&agent.output, 120);
                format!(
                    "{} [{}] task={}{}",
                    id,
                    agent.status,
                    one_line(&agent.task, 100),
                    if preview.is_empty() {
                        String::new()
                    } else {
                        format!(" | {}", preview)
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn inspect(&self, id: &str, max_chars: usize) -> Result<String, String> {
        let agents = self
            .agents
            .lock()
            .map_err(|_| "Agent manager lock is poisoned".to_string())?;
        let agent = agents
            .get(id)
            .ok_or_else(|| format!("Unknown agent '{}'", id))?;
        let output = agent
            .output
            .chars()
            .take(max_chars.clamp(200, 16_000))
            .collect::<String>();
        Ok(format!(
            "{} [{}]\nTask: {}\n\n{}",
            id, agent.status, agent.task, output
        ))
    }
}

async fn agent_loop(
    agents: Arc<Mutex<BTreeMap<String, AgentRecord>>>,
    id: String,
    client: AiClient,
    model: String,
    fallbacks: Vec<String>,
    max_retries: u32,
    task: String,
    mut commands: UnboundedReceiver<AgentCommand>,
    slots: Arc<Semaphore>,
) {
    let mut conversation = vec![Content {
        role: Some("user".to_string()),
        parts: vec![Part::Text {
            text: task.clone(),
            thought: None,
        }],
    }];

    loop {
        let Ok(_slot) = slots.clone().acquire_owned().await else {
            set_agent_status(
                &agents,
                &id,
                "failed",
                Some("Agent scheduler closed.".to_string()),
            );
            return;
        };
        set_agent_status(&agents, &id, "working", None);
        let request = GenerateContentRequest {
            contents: conversation.clone(),
            system_instruction: Some(Content {
                role: Some("system".to_string()),
                parts: vec![Part::Text {
                    text: "You are a focused subagent inside a developer harness. Work only on the assigned task, be concise, state assumptions, and return a useful report for the parent agent. You do not have permission to mutate files or execute host commands.".to_string(),
                    thought: None,
                }],
            }),
            generation_config: Some(GenerationConfig {
                temperature: Some(0.2),
                max_output_tokens: Some(4096),
                thinking_config: None,
                reasoning_effort: None,
                extra: None,
            }),
            safety_settings: None,
            tools: None,
        };

        match client
            .generate_content_with_fallback(&model, &fallbacks, max_retries, &request)
            .await
        {
            Ok(output) => {
                let bounded = output
                    .chars()
                    .take(MAX_AGENT_OUTPUT_CHARS)
                    .collect::<String>();
                set_agent_status(&agents, &id, "idle", Some(bounded.clone()));
                conversation.push(Content {
                    role: Some("model".to_string()),
                    parts: vec![Part::Text {
                        text: bounded,
                        thought: None,
                    }],
                });
            }
            Err(error) => {
                set_agent_status(&agents, &id, "failed", Some(format!("Error: {}", error)));
                return;
            }
        }

        drop(_slot);
        match commands.recv().await {
            Some(AgentCommand::Message(message)) if !message.trim().is_empty() => {
                conversation.push(Content {
                    role: Some("user".to_string()),
                    parts: vec![Part::Text {
                        text: message,
                        thought: None,
                    }],
                });
            }
            Some(AgentCommand::Message(_)) => {}
            Some(AgentCommand::Stop) | None => {
                set_agent_status(&agents, &id, "stopped", None);
                return;
            }
        }
    }
}

fn set_agent_status(
    agents: &Arc<Mutex<BTreeMap<String, AgentRecord>>>,
    id: &str,
    status: &str,
    output: Option<String>,
) {
    if let Ok(mut all) = agents.lock() {
        if let Some(agent) = all.get_mut(id) {
            agent.status = status.to_string();
            if let Some(output) = output {
                agent.output = output;
            }
        }
    }
}

fn one_line(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = compact.chars().take(max_chars).collect::<String>();
    if compact.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

pub enum AgentToolKind {
    Spawn,
    Status,
    Message,
    Inspect,
    Kill,
}

pub struct AgentTool {
    kind: AgentToolKind,
    manager: AgentManager,
    client: AiClient,
    model: String,
    fallbacks: Vec<String>,
    max_retries: u32,
}

impl AgentTool {
    pub fn new(
        kind: AgentToolKind,
        manager: AgentManager,
        client: AiClient,
        model: String,
        fallbacks: Vec<String>,
        max_retries: u32,
    ) -> Self {
        Self {
            kind,
            manager,
            client,
            model,
            fallbacks,
            max_retries,
        }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for AgentTool {
    fn name(&self) -> &'static str {
        match self.kind {
            AgentToolKind::Spawn => "spawn_agent",
            AgentToolKind::Status => "agent_status",
            AgentToolKind::Message => "message_agent",
            AgentToolKind::Inspect => "agent_inspect",
            AgentToolKind::Kill => "kill_agent",
        }
    }

    fn description(&self) -> &'static str {
        match self.kind {
            AgentToolKind::Spawn => "Starts a focused in-process research or review subagent. It returns immediately with an agent id.",
            AgentToolKind::Status => "Lists in-process subagents and their compact latest result.",
            AgentToolKind::Message => "Sends a follow-up instruction to an existing subagent.",
            AgentToolKind::Inspect => "Reads the bounded latest report from an existing subagent.",
            AgentToolKind::Kill => "Stops an existing subagent.",
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        match self.kind {
            AgentToolKind::Spawn => json!({
                "type":"object",
                "properties":{"task":{"type":"string","description":"Focused task for the subagent"}},
                "required":["task"]
            }),
            AgentToolKind::Status => json!({"type":"object","properties":{}}),
            AgentToolKind::Message => json!({
                "type":"object",
                "properties":{"id":{"type":"string"},"message":{"type":"string"}},
                "required":["id","message"]
            }),
            AgentToolKind::Inspect => json!({
                "type":"object",
                "properties":{"id":{"type":"string"},"max_chars":{"type":"integer","minimum":200,"maximum":16000}},
                "required":["id"]
            }),
            AgentToolKind::Kill => json!({
                "type":"object",
                "properties":{"id":{"type":"string"}},
                "required":["id"]
            }),
        }
    }

    fn generate_preview(&self, args: &serde_json::Value) -> crate::tools::ToolPreview {
        crate::tools::ToolPreview {
            title: self.description().to_string(),
            details: vec![args.to_string()],
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        match self.kind {
            AgentToolKind::Spawn => self.manager.spawn(
                self.client.clone(),
                self.model.clone(),
                self.fallbacks.clone(),
                self.max_retries,
                args.get("task")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            AgentToolKind::Status => Ok(self.manager.status()),
            AgentToolKind::Message => self.manager.message(
                args.get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                args.get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ),
            AgentToolKind::Inspect => self.manager.inspect(
                args.get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                args.get("max_chars")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(8_000) as usize,
            ),
            AgentToolKind::Kill => self.manager.stop(
                args.get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ),
        }
    }
}
