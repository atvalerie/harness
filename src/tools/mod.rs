pub mod fs;
pub mod grounding;
pub mod shell;

use async_trait::async_trait;
use crate::client::types::{FunctionDeclaration, GeminiToolDeclaration};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

pub type SharedWorkingDir = Arc<Mutex<PathBuf>>;

pub fn new_working_dir() -> SharedWorkingDir {
    Arc::new(Mutex::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))))
}

pub fn working_dir_path(cwd: &SharedWorkingDir) -> PathBuf {
    cwd.lock().map(|path| path.clone()).unwrap_or_else(|_| PathBuf::from("."))
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

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    working_dir: SharedWorkingDir,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut reg = Self {
            tools: HashMap::new(),
            working_dir: new_working_dir(),
        };

        reg.register(Arc::new(grounding::WebSearchTool));
        reg.register(Arc::new(grounding::WebFetchTool));
        reg.register(Arc::new(fs::ReadFileTool::new(reg.working_dir.clone())));
        reg.register(Arc::new(fs::WriteFileTool::new(reg.working_dir.clone())));
        reg.register(Arc::new(shell::RunCommandTool::new(reg.working_dir.clone())));

        reg
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn to_gemini_declarations(&self) -> Vec<GeminiToolDeclaration> {
        let mut decls = Vec::new();
        for tool in self.tools.values() {
            decls.push(FunctionDeclaration {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters_schema(),
            });
        }
        vec![GeminiToolDeclaration {
            function_declarations: decls,
        }]
    }
}
