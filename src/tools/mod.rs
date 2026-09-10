pub mod fs;
pub mod grounding;
pub mod search;
pub mod shell;

use crate::agents::{AgentManager, AgentTool, AgentToolKind};
use crate::client::types::{FunctionDeclaration, GeminiToolDeclaration};
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs as std_fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

pub type SharedWorkingDir = Arc<Mutex<PathBuf>>;

pub fn new_working_dir() -> SharedWorkingDir {
    Arc::new(Mutex::new(
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ))
}

pub fn working_dir_path(cwd: &SharedWorkingDir) -> PathBuf {
    cwd.lock()
        .map(|path| path.clone())
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn resolve_path(path_str: &str, cwd: &Path) -> PathBuf {
    let initial = PathBuf::from(path_str);
    let candidate = if initial.is_relative() {
        cwd.join(initial)
    } else {
        initial
    };

    let is_symlink = std_fs::symlink_metadata(&candidate)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    if !is_symlink {
        if let Ok(canonical) = candidate.canonicalize() {
            return canonical;
        }
    }
    lexical_normalize(&candidate)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DiffHunk {
    pub tag: String,
    pub line: String,
    pub old_line_no: Option<usize>,
    pub new_line_no: Option<usize>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ToolPreview {
    pub title: String,
    pub details: Vec<String>,
    pub reason: Option<String>,
    pub expected_effect: Option<String>,
    pub command: Option<String>,
    pub diff_hunks: Vec<DiffHunk>,
    pub is_mutation: bool,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview;
    async fn execute(&self, args: serde_json::Value) -> Result<String, String>;
}

#[derive(Debug, Clone)]
struct ToolSummary {
    name: String,
    description: String,
    category: ToolCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Filesystem,
    Shell,
    Web,
    Agents,
    Integrations,
    Other,
}

impl ToolCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Shell => "shell",
            Self::Web => "web",
            Self::Agents => "agents",
            Self::Integrations => "integrations",
            Self::Other => "other",
        }
    }

    fn matches_query(self, query: &str) -> bool {
        match self {
            Self::Filesystem => matches!(query, "file" | "files" | "filesystem" | "fs"),
            Self::Shell => matches!(query, "command" | "commands" | "shell" | "terminal"),
            Self::Web => matches!(query, "web" | "internet" | "browser" | "http"),
            Self::Agents => matches!(query, "agent" | "agents" | "subagent"),
            Self::Integrations => matches!(query, "mcp" | "integration" | "integrations"),
            Self::Other => query == "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolVisibility {
    Core,
    Lazy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRisk {
    ReadOnly,
    WorkspaceWrite,
    Destructive,
    HostExecution,
    ExternalSideEffect,
    AgentControl,
    SessionState,
}

impl ToolRisk {
    pub const fn permission_group(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::Destructive => "destructive",
            Self::HostExecution => "host_execution",
            Self::ExternalSideEffect => "external_side_effect",
            Self::AgentControl => "agent_control",
            Self::SessionState => "session_state",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolDescriptor {
    pub category: ToolCategory,
    pub visibility: ToolVisibility,
    pub risk: ToolRisk,
}

impl ToolDescriptor {
    pub const fn core(category: ToolCategory, risk: ToolRisk) -> Self {
        Self {
            category,
            visibility: ToolVisibility::Core,
            risk,
        }
    }

    pub const fn lazy(category: ToolCategory, risk: ToolRisk) -> Self {
        Self {
            category,
            visibility: ToolVisibility::Lazy,
            risk,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolContextMetrics {
    pub active_tools: usize,
    pub registered_tools: usize,
    pub discovered_tools: usize,
    pub estimated_schema_chars: usize,
    pub estimated_schema_tokens: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

pub type TodoStore = Arc<Mutex<Vec<TodoItem>>>;

struct SearchToolsTool {
    catalog: Arc<Mutex<Vec<ToolSummary>>>,
}

impl SearchToolsTool {
    fn new(catalog: Arc<Mutex<Vec<ToolSummary>>>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl Tool for SearchToolsTool {
    fn name(&self) -> &'static str {
        "search_tools"
    }

    fn description(&self) -> &'static str {
        "Finds optional tools by name, category, or purpose. Returns short descriptions; matching tools become available on the next model turn. Use a category query such as filesystem, web, agents, or integrations to activate a bundle."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A short purpose or name to search for, such as filesystem, database, or deploy"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 32,
                    "description": "Maximum optional tools to return and activate (default: 8)"
                }
            },
            "required": ["query"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        ToolPreview {
            title: "Search Tools".to_string(),
            details: vec![format!(
                "Query: {}",
                args.get("query")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            )],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let max_results = args
            .get("max_results")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(8)
            .clamp(1, 32) as usize;
        let terms = query.split_whitespace().collect::<Vec<_>>();
        let catalog = self
            .catalog
            .lock()
            .map_err(|_| "Tool catalog lock is poisoned".to_string())?;
        let mut matches = catalog
            .iter()
            .filter(|tool| {
                terms.is_empty()
                    || terms.iter().any(|term| {
                        tool.name.to_ascii_lowercase().contains(term)
                            || tool.description.to_ascii_lowercase().contains(term)
                            || tool.category.matches_query(term)
                    })
            })
            .map(|tool| {
                format!(
                    "- [{}] {}: {}",
                    tool.category.label(),
                    tool.name,
                    tool.description
                )
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches.truncate(max_results);
        if matches.is_empty() {
            return Ok("No optional tools matched that query.".to_string());
        }
        let heading = if terms.is_empty() {
            "Optional tool catalog (use a non-empty purpose or category query to activate matching schemas):"
        } else {
            "Matching optional tools (their full schemas are available on the next turn):"
        };
        Ok(format!("{}\n{}", heading, matches.join("\n")))
    }
}

#[derive(Clone)]
pub struct ToolRegistry {
    pub tasks: crate::tasks::TaskManager,
    tools: HashMap<String, Arc<dyn Tool>>,
    descriptors: HashMap<String, ToolDescriptor>,
    working_dir: SharedWorkingDir,
    catalog: Arc<Mutex<Vec<ToolSummary>>>,
    discovered: Arc<Mutex<HashSet<String>>>,
    todos: TodoStore,
    agent_manager: AgentManager,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut reg = Self {
            tasks: crate::tasks::TaskManager::default(),
            tools: HashMap::new(),
            descriptors: HashMap::new(),
            working_dir: new_working_dir(),
            catalog: Arc::new(Mutex::new(Vec::new())),
            discovered: Arc::new(Mutex::new(HashSet::new())),
            todos: Arc::new(Mutex::new(Vec::new())),
            agent_manager: AgentManager::new(),
        };

        reg.register_with_descriptor(
            Arc::new(SearchToolsTool::new(reg.catalog.clone())),
            ToolDescriptor::core(ToolCategory::Other, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(grounding::WebSearchTool),
            ToolDescriptor::lazy(ToolCategory::Web, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(grounding::WebFetchTool),
            ToolDescriptor::lazy(ToolCategory::Web, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(grounding::WeatherTool),
            ToolDescriptor::lazy(ToolCategory::Web, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(fs::ReadFileTool::new(reg.working_dir.clone())),
            ToolDescriptor::core(ToolCategory::Filesystem, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(fs::ListDirectoryTool::new(reg.working_dir.clone())),
            ToolDescriptor::core(ToolCategory::Filesystem, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(fs::StatPathTool::new(reg.working_dir.clone())),
            ToolDescriptor::core(ToolCategory::Filesystem, ToolRisk::ReadOnly),
        );
        reg.register_with_descriptor(
            Arc::new(fs::WriteFileTool::new(reg.working_dir.clone())),
            ToolDescriptor::lazy(ToolCategory::Filesystem, ToolRisk::WorkspaceWrite),
        );
        reg.register_with_descriptor(
            Arc::new(fs::EditFileTool::new(reg.working_dir.clone())),
            ToolDescriptor::core(ToolCategory::Filesystem, ToolRisk::WorkspaceWrite),
        );
        reg.register_with_descriptor(
            Arc::new(fs::CreateDirectoryTool::new(reg.working_dir.clone())),
            ToolDescriptor::lazy(ToolCategory::Filesystem, ToolRisk::WorkspaceWrite),
        );
        reg.register_with_descriptor(
            Arc::new(fs::MovePathTool::new(reg.working_dir.clone())),
            ToolDescriptor::lazy(ToolCategory::Filesystem, ToolRisk::WorkspaceWrite),
        );
        reg.register_with_descriptor(
            Arc::new(fs::DeletePathTool::new(reg.working_dir.clone())),
            ToolDescriptor::lazy(ToolCategory::Filesystem, ToolRisk::Destructive),
        );
        reg.register_with_descriptor(
            Arc::new(TodoTool::new(reg.todos.clone())),
            ToolDescriptor::core(ToolCategory::Other, ToolRisk::SessionState),
        );
        reg.register_with_descriptor(
            Arc::new(shell::RunCommandTool::with_tasks(
                reg.working_dir.clone(),
                reg.tasks.clone(),
            )),
            ToolDescriptor::core(ToolCategory::Shell, ToolRisk::HostExecution),
        );
        reg.register_with_descriptor(
            Arc::new(search::SearchFilesTool::new(reg.working_dir.clone())),
            ToolDescriptor::core(ToolCategory::Filesystem, ToolRisk::ReadOnly),
        );

        reg
    }

    pub fn register_with_descriptor(&mut self, tool: Arc<dyn Tool>, descriptor: ToolDescriptor) {
        let name = tool.name().to_string();
        if let Ok(mut catalog) = self.catalog.lock() {
            catalog.retain(|entry| entry.name != name);
            if descriptor.visibility == ToolVisibility::Lazy {
                catalog.push(ToolSummary {
                    name: name.clone(),
                    description: tool.description().to_string(),
                    category: descriptor.category,
                });
            }
        }
        self.descriptors.insert(name.clone(), descriptor);
        self.tools.insert(name, tool);
    }

    pub fn install_agent_tools(
        &mut self,
        client: crate::client::AiClient,
        model: String,
        fallbacks: Vec<String>,
        max_retries: u32,
    ) {
        for kind in [
            AgentToolKind::Spawn,
            AgentToolKind::Status,
            AgentToolKind::Message,
            AgentToolKind::Inspect,
            AgentToolKind::Kill,
        ] {
            self.register_with_descriptor(
                Arc::new(AgentTool::new(
                    kind,
                    self.agent_manager.clone(),
                    client.clone(),
                    model.clone(),
                    fallbacks.clone(),
                    max_retries,
                )),
                ToolDescriptor::lazy(ToolCategory::Agents, ToolRisk::AgentControl),
            );
        }
    }

    pub fn worker_runtime(
        &self,
        config: &crate::config::AppConfig,
        review: crate::review::ReviewStore,
    ) -> crate::worker::WorkerRuntime {
        let tools = self
            .tools
            .iter()
            .filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "read_file"
                        | "list_directory"
                        | "stat_path"
                        | "search_files"
                        | "write_file"
                        | "edit_file"
                )
            })
            .filter(|(name, _)| {
                self.descriptors.get(*name).is_some_and(|d| {
                    config.permission_mode_for(d.risk.permission_group())
                        == crate::config::PermissionMode::Allow
                })
            })
            .map(|(name, tool)| (name.clone(), tool.clone()))
            .collect();
        crate::worker::WorkerRuntime {
            root: self.working_dir(),
            tools,
            review,
            tasks: self.tasks.clone(),
        }
    }

    pub fn agent_manager(&self) -> AgentManager {
        self.agent_manager.clone()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn permission_group(&self, name: &str) -> &'static str {
        self.descriptors
            .get(name)
            .map(|descriptor| descriptor.risk.permission_group())
            .unwrap_or("external_side_effect")
    }

    pub fn tools_for_permission_group(&self, group: &str) -> Vec<(String, String)> {
        let mut tools = self
            .descriptors
            .iter()
            .filter(|(_, descriptor)| descriptor.risk.permission_group() == group)
            .filter_map(|(name, _)| {
                self.tools
                    .get(name)
                    .map(|tool| (name.clone(), tool.description().to_string()))
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.0.cmp(&right.0));
        tools
    }

    pub fn set_working_dir(&self, path: PathBuf) -> Result<(), String> {
        if !path.is_dir() {
            return Err(format!(
                "Working directory does not exist: {}",
                path.display()
            ));
        }
        let path = path.canonicalize().map_err(|error| {
            format!(
                "Could not resolve working directory '{}': {}",
                path.display(),
                error
            )
        })?;
        let mut working_dir = self
            .working_dir
            .lock()
            .map_err(|_| "Working directory lock is poisoned".to_string())?;
        *working_dir = path;
        Ok(())
    }

    pub fn working_dir(&self) -> PathBuf {
        working_dir_path(&self.working_dir)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names = self.tools.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn to_gemini_declarations(&self) -> Vec<GeminiToolDeclaration> {
        let discovered = self
            .discovered
            .lock()
            .map(|set| set.clone())
            .unwrap_or_default();
        let mut names = self
            .tools
            .keys()
            .filter(|name| {
                self.descriptors
                    .get(*name)
                    .map(|descriptor| {
                        descriptor.visibility == ToolVisibility::Core || discovered.contains(*name)
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        let decls = names
            .into_iter()
            .filter_map(|name| self.tools.get(&name))
            .map(|tool| FunctionDeclaration {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters_schema(),
            })
            .collect::<Vec<_>>();
        vec![GeminiToolDeclaration {
            function_declarations: decls,
        }]
    }

    pub fn context_metrics(&self) -> ToolContextMetrics {
        let declarations = self.to_gemini_declarations();
        let active = declarations
            .first()
            .map(|declaration| declaration.function_declarations.as_slice())
            .unwrap_or(&[]);
        let estimated_schema_chars = active
            .iter()
            .map(|declaration| {
                declaration.name.len()
                    + declaration.description.len()
                    + declaration.parameters.to_string().len()
            })
            .sum::<usize>();
        let discovered_tools = self
            .discovered
            .lock()
            .map(|set| set.len())
            .unwrap_or_default();

        ToolContextMetrics {
            active_tools: active.len(),
            registered_tools: self.tools.len(),
            discovered_tools,
            estimated_schema_chars,
            estimated_schema_tokens: estimated_schema_chars.saturating_add(3) / 4,
        }
    }

    pub fn discover_from_query(&self, args: &serde_json::Value) {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let max_results = args
            .get("max_results")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(8)
            .clamp(1, 32) as usize;
        let terms = query.split_whitespace().collect::<Vec<_>>();
        let Ok(catalog) = self.catalog.lock() else {
            return;
        };
        let Ok(mut discovered) = self.discovered.lock() else {
            return;
        };
        let mut matches = catalog
            .iter()
            .filter(|tool| {
                terms.iter().any(|term| {
                    tool.name.to_ascii_lowercase().contains(term)
                        || tool.description.to_ascii_lowercase().contains(term)
                        || tool.category.matches_query(term)
                })
            })
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        matches.sort();
        matches.truncate(max_results);
        for name in matches {
            discovered.insert(name);
        }
    }

    /// Activates explicitly selected lazy tools for a frontend session.
    /// This is intentionally separate from the global registry and from the
    /// natural-language search_tools flow, so trusted frontends can avoid an
    /// unnecessary discovery round trip without changing normal agent usage.
    pub fn preload_tools<I, S>(&self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let Ok(mut discovered) = self.discovered.lock() else {
            return;
        };
        for name in names {
            let name = name.as_ref();
            if self.tools.contains_key(name)
                && self
                    .descriptors
                    .get(name)
                    .is_some_and(|descriptor| descriptor.visibility == ToolVisibility::Lazy)
            {
                discovered.insert(name.to_string());
            }
        }
    }

    pub fn clear_discovered_tools(&self) {
        if let Ok(mut discovered) = self.discovered.lock() {
            discovered.clear();
        }
    }

    pub fn discovered_tools(&self) -> Vec<String> {
        let mut names = self
            .discovered
            .lock()
            .map(|set| set.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn todo_items(&self) -> Vec<TodoItem> {
        self.todos
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    pub fn set_todo_items(&self, items: Vec<TodoItem>) {
        if let Ok(mut todos) = self.todos.lock() {
            *todos = items;
        }
    }

    pub fn todo_add(&self, text: &str) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Todo text cannot be empty".to_string());
        }
        let mut todos = self
            .todos
            .lock()
            .map_err(|_| "Todo list lock is poisoned".to_string())?;
        todos.push(TodoItem {
            text: text.to_string(),
            done: false,
        });
        Ok(format_todos(&todos))
    }

    pub fn todo_done(&self, id: usize) -> Result<String, String> {
        let mut todos = self
            .todos
            .lock()
            .map_err(|_| "Todo list lock is poisoned".to_string())?;
        let item = todos
            .get_mut(id.saturating_sub(1))
            .ok_or_else(|| format!("No todo with id {}", id))?;
        item.done = true;
        Ok(format_todos(&todos))
    }

    pub fn todo_remove(&self, id: usize) -> Result<String, String> {
        let mut todos = self
            .todos
            .lock()
            .map_err(|_| "Todo list lock is poisoned".to_string())?;
        if id == 0 || id > todos.len() {
            return Err(format!("No todo with id {}", id));
        }
        todos.remove(id - 1);
        Ok(format_todos(&todos))
    }

    pub fn todo_clear(&self) -> Result<String, String> {
        let mut todos = self
            .todos
            .lock()
            .map_err(|_| "Todo list lock is poisoned".to_string())?;
        todos.clear();
        Ok("Todo list cleared.".to_string())
    }
}

pub struct TodoTool {
    todos: TodoStore,
}

impl TodoTool {
    fn new(todos: TodoStore) -> Self {
        Self { todos }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &'static str {
        "todo"
    }

    fn description(&self) -> &'static str {
        "Maintains a small task list for the current work session. Use action list, add, done, remove, or clear."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type":"string", "enum":["list","add","done","remove","clear"]},
                "task": {"type":"string", "description":"Task text for add"},
                "id": {"type":"integer", "minimum":1, "description":"1-based task number for done/remove"}
            },
            "required":["action"]
        })
    }

    fn generate_preview(&self, _args: &serde_json::Value) -> ToolPreview {
        ToolPreview {
            title: "Update Todo List".to_string(),
            details: vec!["Updates the in-session task list".to_string()],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let action = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("list");
        let task = args
            .get("task")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        match action {
            "list" => {
                let todos = self
                    .todos
                    .lock()
                    .map_err(|_| "Todo list lock is poisoned".to_string())?;
                Ok(format_todos(&todos))
            }
            "add" if !task.is_empty() => {
                let mut todos = self
                    .todos
                    .lock()
                    .map_err(|_| "Todo list lock is poisoned".to_string())?;
                todos.push(TodoItem {
                    text: task.to_string(),
                    done: false,
                });
                Ok(format_todos(&todos))
            }
            "add" => Err("'task' is required for todo add".to_string()),
            "done" | "remove" => {
                let id = args
                    .get("id")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;
                let mut todos = self
                    .todos
                    .lock()
                    .map_err(|_| "Todo list lock is poisoned".to_string())?;
                if id == 0 || id > todos.len() {
                    return Err(format!("No todo with id {}", id));
                }
                if action == "done" {
                    todos[id - 1].done = true;
                } else {
                    todos.remove(id - 1);
                }
                Ok(format_todos(&todos))
            }
            "clear" => {
                let mut todos = self
                    .todos
                    .lock()
                    .map_err(|_| "Todo list lock is poisoned".to_string())?;
                todos.clear();
                Ok("Todo list cleared.".to_string())
            }
            _ => Err("Unknown todo action. Use list, add, done, remove, or clear.".to_string()),
        }
    }
}

fn format_todos(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "Todo list is empty.".to_string();
    }
    todos
        .iter()
        .enumerate()
        .map(|(index, item)| {
            format!(
                "{} [{}] {}",
                index + 1,
                if item.done { "x" } else { " " },
                item.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::ToolRegistry;

    fn declaration_names(registry: &ToolRegistry) -> Vec<String> {
        registry.to_gemini_declarations()[0]
            .function_declarations
            .iter()
            .map(|declaration| declaration.name.clone())
            .collect()
    }

    #[test]
    fn specialized_tools_are_hidden_until_discovered() {
        let registry = ToolRegistry::new();
        let names = declaration_names(&registry);
        assert!(names.contains(&"search_tools".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"list_directory".to_string()));
        assert!(names.contains(&"stat_path".to_string()));
        assert!(names.contains(&"search_files".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"web_search".to_string()));
        assert!(!names.contains(&"weather".to_string()));

        registry.discover_from_query(&serde_json::json!({"query":"patch editing"}));
        let names = declaration_names(&registry);
        assert!(names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
    }

    #[test]
    fn category_queries_activate_tool_bundles() {
        let registry = ToolRegistry::new();
        registry.discover_from_query(&serde_json::json!({"query":"filesystem"}));
        let names = declaration_names(&registry);
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"web_search".to_string()));

        registry.discover_from_query(&serde_json::json!({"query":"web"}));
        let names = declaration_names(&registry);
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"web_fetch".to_string()));
    }

    #[test]
    fn discovery_limits_the_number_of_activated_tools() {
        let registry = ToolRegistry::new();
        registry.discover_from_query(&serde_json::json!({"query":"filesystem", "max_results":1}));
        let names = declaration_names(&registry);
        assert!(names.contains(&"create_directory".to_string()));
        assert!(names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
    }

    #[test]
    fn empty_discovery_query_does_not_activate_every_optional_tool() {
        let registry = ToolRegistry::new();
        registry.discover_from_query(&serde_json::json!({"query":""}));
        let names = declaration_names(&registry);
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"web_search".to_string()));
    }

    #[test]
    fn reports_active_tool_schema_context() {
        let registry = ToolRegistry::new();
        let initial = registry.context_metrics();
        assert!(initial.active_tools < initial.registered_tools);
        assert_eq!(initial.discovered_tools, 0);
        assert!(initial.estimated_schema_chars > 0);
        assert!(initial.estimated_schema_tokens > 0);

        registry.discover_from_query(&serde_json::json!({"query":"web"}));
        let discovered = registry.context_metrics();
        assert_eq!(discovered.discovered_tools, 3);
        assert!(discovered.active_tools > initial.active_tools);
    }

    #[test]
    fn explicit_preload_activates_only_requested_lazy_tools() {
        let registry = ToolRegistry::new();
        registry.preload_tools(["web_search"]);
        let declarations = registry.to_gemini_declarations();
        let names = declarations[0]
            .function_declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"web_search"));
        assert!(!names.contains(&"web_fetch"));
    }

    #[test]
    fn explicit_preload_activates_weather_without_discovery() {
        let registry = ToolRegistry::new();
        registry.preload_tools(["weather"]);
        let names = declaration_names(&registry);
        assert!(names.contains(&"weather".to_string()));
        assert!(!names.contains(&"web_search".to_string()));
    }

    #[test]
    fn permission_catalog_matches_registered_tools() {
        let registry = ToolRegistry::new();
        let read_only = registry.tools_for_permission_group("read_only");
        let host_execution = registry.tools_for_permission_group("host_execution");
        let destructive = registry.tools_for_permission_group("destructive");

        assert!(read_only.iter().any(|(name, _)| name == "read_file"));
        assert!(read_only.iter().any(|(name, _)| name == "search_files"));
        assert_eq!(
            host_execution
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["run_command"]
        );
        assert_eq!(
            destructive
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["delete_path"]
        );
    }

    #[test]
    fn todo_state_is_shared_and_bounded() {
        let registry = ToolRegistry::new();
        registry.todo_add("inspect the diff").unwrap();
        assert_eq!(registry.todo_items().len(), 1);
        registry.todo_done(1).unwrap();
        assert!(registry.todo_items()[0].done);
        registry.todo_clear().unwrap();
        assert!(registry.todo_items().is_empty());
    }
}
