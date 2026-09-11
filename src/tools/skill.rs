use std::path::PathBuf;
use async_trait::async_trait;
use serde_json::json;

use crate::config::AppConfig;
use crate::tools::{Tool, ToolPreview};

pub const SKILL_UI: &str = include_str!("../prompts/skills/ui.md");
pub const SKILL_COPYWRITING: &str = include_str!("../prompts/skills/copywriting.md");
pub const SKILL_HUMAN: &str = include_str!("../prompts/skills/human.md");
pub const SKILL_LAYOUTMOBILE: &str = include_str!("../prompts/skills/layoutmobile.md");
pub const SKILL_CODE: &str = include_str!("../prompts/skills/code.md");
pub const SKILL_CLI: &str = include_str!("../prompts/skills/cli.md");

const BUILTIN_SKILLS: &[(&str, &str, &str)] = &[
    (
        "ui",
        "Visual design, color restraint, shadows, border radii, and template structure.",
        SKILL_UI,
    ),
    (
        "copywriting",
        "Product copy, anti-hype phrasing, clear CTA buttons, and human error messages.",
        SKILL_COPYWRITING,
    ),
    (
        "human",
        "Accessibility, WCAG AA color contrast, focus outlines, and keyboard navigation.",
        SKILL_HUMAN,
    ),
    (
        "layoutmobile",
        "Responsive reflow, dynamic viewports, scalable typography, and tap target ergonomics.",
        SKILL_LAYOUTMOBILE,
    ),
    (
        "code",
        "Code hygiene, removing decorative separator banners, and clean comment practices.",
        SKILL_CODE,
    ),
    (
        "cli",
        "Terminal application UX, stdout vs stderr, exit codes, and responsive TUI layout.",
        SKILL_CLI,
    ),
];

pub struct SkillTool {
    working_dir: std::sync::Arc<std::sync::Mutex<PathBuf>>,
}

impl SkillTool {
    pub fn new(working_dir: std::sync::Arc<std::sync::Mutex<PathBuf>>) -> Self {
        Self { working_dir }
    }

    fn find_custom_skill(&self, name: &str) -> Option<String> {
        let work_dir = self.working_dir.lock().ok()?.clone();

        // 1. Check <working_dir>/.holiday/skills/<name>.md
        let p1 = work_dir.join(".holiday").join("skills").join(format!("{name}.md"));
        if let Ok(c) = std::fs::read_to_string(&p1) {
            return Some(c);
        }

        // 2. Check <working_dir>/skills/<name>/SKILL.md (anti-slop standard structure)
        let p2 = work_dir.join("skills").join(name).join("SKILL.md");
        if let Ok(c) = std::fs::read_to_string(&p2) {
            return Some(c);
        }

        // 3. Check <working_dir>/skills/<name>.md
        let p3 = work_dir.join("skills").join(format!("{name}.md"));
        if let Ok(c) = std::fs::read_to_string(&p3) {
            return Some(c);
        }

        // 4. Check user config directory: holiday/skills/<name>.md
        if let Some(config_dir) = AppConfig::config_dir() {
            let p4 = config_dir.join("skills").join(format!("{name}.md"));
            if let Ok(c) = std::fs::read_to_string(&p4) {
                return Some(c);
            }
        }

        None
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> &'static str {
        "Loads specialized domain guidelines and anti-slop principles into the active conversation. Use when working on UI, copywriting, accessibility, mobile layout, code hygiene, or CLI design. Use name='list' to see available skills."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name to load (e.g. 'ui', 'copywriting', 'human', 'layoutmobile', 'code', 'cli') or 'list' to view available skills"
                }
            },
            "required": ["name"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        ToolPreview {
            title: format!("Load Skill ({name})"),
            details: vec![format!("Skill: {name}")],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        if name.is_empty() || name == "list" {
            let mut list = String::from("Available built-in skills:\n");
            for (s_name, s_desc, _) in BUILTIN_SKILLS {
                list.push_str(&format!("- **{}**: {}\n", s_name, s_desc));
            }
            list.push_str("\nCustom skills can also be placed in `.holiday/skills/<name>.md` or `skills/<name>/SKILL.md`.");
            return Ok(list);
        }

        // Check custom skills first (allowing project overrides)
        if let Some(content) = self.find_custom_skill(&name) {
            return Ok(format!("# Custom Skill: {}\n\n{}", name, content.trim()));
        }

        // Check built-in skills
        for (s_name, _, content) in BUILTIN_SKILLS {
            if *s_name == name || format!("antislop-{}", s_name) == name {
                return Ok(format!("# Skill: {}\n\n{}", s_name, content.trim()));
            }
        }

        Err(format!(
            "Skill '{}' not found. Call skill with name='list' to see available skills.",
            name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skill_tool_lists_available_skills() {
        let cwd = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::from(".")));
        let tool = SkillTool::new(cwd);
        let res = tool.execute(json!({ "name": "list" })).await.unwrap();
        assert!(res.contains("ui"));
        assert!(res.contains("copywriting"));
        assert!(res.contains("human"));
        assert!(res.contains("layoutmobile"));
        assert!(res.contains("code"));
        assert!(res.contains("cli"));
    }

    #[tokio::test]
    async fn skill_tool_loads_builtin_skill() {
        let cwd = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::from(".")));
        let tool = SkillTool::new(cwd);
        let res = tool.execute(json!({ "name": "ui" })).await.unwrap();
        assert!(res.contains("antislop-ui"));
        assert!(res.contains("Eliminate AI Cliché Gradients"));
    }
}
