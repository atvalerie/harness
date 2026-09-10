//! One registry for dispatch, discovery, aliases and completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    Help,
    Compact,
    Plan,
    Todos,
    Agents,
    Todo,
    Models,
    Providers,
    Provider,
    Baseurl,
    Config,
    Model,
    Thinking,
    Reasoning,
    Autocompact,
    Auto,
    Session,
    Sessions,
    Resume,
    Temp,
    Sys,
    Key,
    Clear,
    Retry,
    Fork,
    New,
    Usage,
    Limits,
    Context,
    Status,
    Pwd,
    Tools,
    Permissions,
    Attachments,
    Attach,
    Remove,
    Edit,
    Save,
    Copy,
    Quit,
    Tasks,
    Review,
    Rollback,
    Palette,
    Search,
    Inspect,
    Queue,
    Interrupt,
    Editor,
    History,
    Jump,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arguments {
    Optional,
    Text,
    Path,
}
pub struct CommandSpec {
    pub id: CommandId,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub arguments: Arguments,
    pub idle_only: bool,
}
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::Auto,
        name: "/auto",
        aliases: &[],
        description:
            "Session-wide model approval review: on, off, status (saved permissions unchanged)",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Jump,
        name: "/jump",
        aliases: &[],
        description: "Jump to message number, prev/next turn, or latest error",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Help,
        name: "/help",
        aliases: &[],
        description: "List commands",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Compact,
        name: "/compact",
        aliases: &[],
        description: "Summarize history while preserving task handoff",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Plan,
        name: "/plan",
        aliases: &[],
        description: "Enter planning mode or review the plan",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Todos,
        name: "/todos",
        aliases: &[],
        description: "Show current todo items",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Agents,
        name: "/agents",
        aliases: &[],
        description: "Show worker status and output",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Todo,
        name: "/todo",
        aliases: &[],
        description: "Edit todos: add, done, remove, clear, mode",
        arguments: Arguments::Text,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Models,
        name: "/models",
        aliases: &[],
        description: "Choose a model from the live catalog",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Providers,
        name: "/providers",
        aliases: &[],
        description: "List configured provider connections",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Provider,
        name: "/provider",
        aliases: &[],
        description: "Select a provider",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Baseurl,
        name: "/baseurl",
        aliases: &[],
        description: "Inspect or change provider endpoint",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Config,
        name: "/config",
        aliases: &[],
        description: "Inspect or open persisted configuration",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Model,
        name: "/model",
        aliases: &[],
        description: "Select a model",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Thinking,
        name: "/thinking",
        aliases: &[],
        description: "Choose supported reasoning effort",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Reasoning,
        name: "/reasoning",
        aliases: &[],
        description: "Toggle reasoning for current model",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Autocompact,
        name: "/autocompact",
        aliases: &[],
        description: "Configure automatic compaction",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Session,
        name: "/session",
        aliases: &[],
        description: "Save or inspect the current session",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "/sessions",
        aliases: &[],
        description: "Browse saved sessions",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Resume,
        name: "/resume",
        aliases: &[],
        description: "Resume a session by name or quoted path",
        arguments: Arguments::Path,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Temp,
        name: "/temp",
        aliases: &[],
        description: "Set model temperature",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Sys,
        name: "/sys",
        aliases: &[],
        description: "Set system customization; /save persists it",
        arguments: Arguments::Text,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Key,
        name: "/key",
        aliases: &[],
        description: "Save a provider-scoped key securely; excluded from history",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Clear,
        name: "/clear",
        aliases: &[],
        description: "Clear conversation; does not revert files",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Retry,
        name: "/retry",
        aliases: &[],
        description: "Retry continuation, preserving history and completed tools",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Fork,
        name: "/fork",
        aliases: &[],
        description: "Fork conversation; does not revert files",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::New,
        name: "/new",
        aliases: &[],
        description: "Start a new conversation",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Usage,
        name: "/usage",
        aliases: &[],
        description: "Inspect usage records",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Limits,
        name: "/limits",
        aliases: &[],
        description: "Fetch provider account limits",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Context,
        name: "/context",
        aliases: &[],
        description: "Inspect context budget and metadata",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Status,
        name: "/status",
        aliases: &[],
        description: "Show provider, model and session status",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Pwd,
        name: "/pwd",
        aliases: &[],
        description: "Show working directory",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Tools,
        name: "/tools",
        aliases: &[],
        description: "Inspect available tools and discovery",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Permissions,
        name: "/permissions",
        aliases: &[],
        description: "Review persistent tool permission policy",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Attachments,
        name: "/attachments",
        aliases: &[],
        description: "Inspect draft attachments",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Attach,
        name: "/attach",
        aliases: &[],
        description: "Attach a file; quoted paths supported",
        arguments: Arguments::Path,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Remove,
        name: "/remove",
        aliases: &[],
        description: "Remove draft attachment by number",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Edit,
        name: "/edit",
        aliases: &[],
        description: "Edit draft attachment by number",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Save,
        name: "/save",
        aliases: &[],
        description: "Persist configuration",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Copy,
        name: "/copy",
        aliases: &[],
        description: "Copy response or /copy MESSAGE code BLOCK",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Quit,
        name: "/quit",
        aliases: &["/exit"],
        description: "Exit Holiday",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Tasks,
        name: "/tasks",
        aliases: &[],
        description: "Inspect or cancel managed tasks",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Review,
        name: "/review",
        aliases: &[],
        description: "Review checkpointed file changes",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Rollback,
        name: "/rollback",
        aliases: &[],
        description: "Restore a checkpoint if files have not changed since",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::Palette,
        name: "/palette",
        aliases: &[],
        description: "Open command palette",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Search,
        name: "/search",
        aliases: &[],
        description: "Search conversation",
        arguments: Arguments::Text,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Inspect,
        name: "/inspect",
        aliases: &["/expand"],
        description: "Expand a transcript item by number",
        arguments: Arguments::Optional,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Queue,
        name: "/queue",
        aliases: &[],
        description: "Queue a prompt for the next turn",
        arguments: Arguments::Text,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Interrupt,
        name: "/interrupt",
        aliases: &[],
        description: "Cancel the current turn and send a new prompt",
        arguments: Arguments::Text,
        idle_only: false,
    },
    CommandSpec {
        id: CommandId::Editor,
        name: "/editor",
        aliases: &[],
        description: "Edit draft in an external editor",
        arguments: Arguments::Optional,
        idle_only: true,
    },
    CommandSpec {
        id: CommandId::History,
        name: "/history",
        aliases: &[],
        description: "Search prompt history",
        arguments: Arguments::Text,
        idle_only: false,
    },
];
pub fn parse(line: &str) -> Result<(&'static CommandSpec, String), String> {
    let mut parts = line.trim().splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_ascii_lowercase();
    let spec = COMMANDS
        .iter()
        .find(|s| s.name == name || s.aliases.contains(&name.as_str()))
        .ok_or_else(|| format!("Unknown command '{name}'. Use /help or Ctrl+K."))?;
    let arg = parts.next().unwrap_or("").trim();
    let arg =
        if spec.arguments == Arguments::Path && (arg.starts_with('"') || arg.starts_with('\'')) {
            let quote = arg.chars().next().unwrap();
            if arg.len() < 2 || !arg.ends_with(quote) {
                return Err("Unclosed quoted path".into());
            }
            &arg[1..arg.len() - 1]
        } else {
            arg
        };
    Ok((spec, arg.to_string()))
}
pub fn matching(query: &str) -> Vec<&'static CommandSpec> {
    let query = query.trim_start_matches('/').to_ascii_lowercase();
    let mut matches = COMMANDS
        .iter()
        .filter(|s| s.name.contains(&query) || s.description.to_ascii_lowercase().contains(&query))
        .collect::<Vec<_>>();
    matches.sort_by_key(|s| (!s.name.trim_start_matches('/').starts_with(&query), s.name));
    matches
}
pub fn help() -> String {
    COMMANDS
        .iter()
        .map(|s| {
            format!(
                "{} - {}{}",
                s.name,
                s.description,
                if s.idle_only { " (idle only)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_free_text() {
        assert_eq!(parse("/exit").unwrap().0.id, CommandId::Quit);
        assert_eq!(parse("/sys keep  spaces").unwrap().1, "keep  spaces");
    }
    #[test]
    fn quoted_paths() {
        assert_eq!(parse("/attach \"a b.txt\"").unwrap().1, "a b.txt");
        assert!(parse("/attach \"broken").is_err());
    }
    #[test]
    fn unique_names() {
        let mut names = std::collections::HashSet::new();
        for s in COMMANDS {
            assert!(names.insert(s.name));
            for alias in s.aliases {
                assert!(names.insert(alias));
            }
        }
    }
}
