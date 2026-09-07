pub mod fs;
pub mod grounding;
pub mod search;
pub mod shell;

use crate::agents::{AgentManager, AgentTool, AgentToolKind};
use crate::client::types::{FunctionDeclaration, GeminiToolDeclaration};
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
        "Finds hidden specialized and MCP tools by name or purpose. Returns short descriptions; matching tools become available on the next model turn. Use an empty query to list all hidden tools."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A short purpose or name to search for, such as filesystem, database, or deploy"
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
                    })
            })
            .map(|tool| format!("- {}: {}", tool.name, tool.description))
            .collect::<Vec<_>>();
        matches.sort();
        if matches.is_empty() {
            return Ok("No hidden tools matched that query.".to_string());
        }
        Ok(format!(
            "Matching hidden tools (their full schemas are now available on the next turn):\n{}",
            matches.join("\n")
        ))
    }
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    working_dir: SharedWorkingDir,
    catalog: Arc<Mutex<Vec<ToolSummary>>>,
    discovered: Arc<Mutex<HashSet<String>>>,
    todos: TodoStore,
    agent_manager: AgentManager,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut reg = Self {
            tools: HashMap::new(),
            working_dir: new_working_dir(),
            catalog: Arc::new(Mutex::new(Vec::new())),
            discovered: Arc::new(Mutex::new(HashSet::new())),
            todos: Arc::new(Mutex::new(Vec::new())),
            agent_manager: AgentManager::new(),
        };

        reg.register(Arc::new(SearchToolsTool::new(reg.catalog.clone())));
        reg.register(Arc::new(grounding::WebSearchTool));
        reg.register(Arc::new(grounding::WebFetchTool));
        reg.register(Arc::new(fs::ReadFileTool::new(reg.working_dir.clone())));
        reg.register(Arc::new(fs::WriteFileTool::new(reg.working_dir.clone())));
        reg.register(Arc::new(fs::EditFileTool::new(reg.working_dir.clone())));
        reg.register(Arc::new(TodoTool::new(reg.todos.clone())));
        reg.register(Arc::new(shell::RunCommandTool::new(
            reg.working_dir.clone(),
        )));
        reg.register(Arc::new(search::SearchFilesTool::new(
            reg.working_dir.clone(),
        )));

        reg
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        if let Ok(mut catalog) = self.catalog.lock() {
            catalog.retain(|entry| entry.name != name);
            if Self::is_hidden_name(&name) {
                catalog.push(ToolSummary {
                    name: name.clone(),
                    description: tool.description().to_string(),
                });
            }
        }
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
            self.register(Arc::new(AgentTool::new(
                kind,
                self.agent_manager.clone(),
                client.clone(),
                model.clone(),
                fallbacks.clone(),
                max_retries,
            )));
        }
    }

    pub fn agent_manager(&self) -> AgentManager {
        self.agent_manager.clone()
    }

    fn is_hidden_name(name: &str) -> bool {
        name == "write_file" || name == "edit_file" || name.starts_with("mcp__")
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn set_working_dir(&self, path: PathBuf) -> Result<(), String> {
        if !path.is_dir() {
            return Err(format!(
                "Working directory does not exist: {}",
                path.display()
            ));
        }
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
            .filter(|name| !Self::is_hidden_name(name) || discovered.contains(*name))
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

    pub fn discover_from_query(&self, args: &serde_json::Value) {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let terms = query.split_whitespace().collect::<Vec<_>>();
        let Ok(catalog) = self.catalog.lock() else {
            return;
        };
        let Ok(mut discovered) = self.discovered.lock() else {
            return;
        };
        for tool in catalog.iter() {
            if terms.is_empty()
                || terms.iter().any(|term| {
                    tool.name.to_ascii_lowercase().contains(term)
                        || tool.description.to_ascii_lowercase().contains(term)
                })
            {
                discovered.insert(tool.name.clone());
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
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"edit_file".to_string()));

        registry.discover_from_query(&serde_json::json!({"query":"patch editing"}));
        let names = declaration_names(&registry);
        assert!(names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
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
