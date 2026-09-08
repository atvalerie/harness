//! Maintained instructions are composed per request, never copied into user settings.
pub const MAIN: &str = include_str!("prompts/main.md");
pub const SUBAGENT: &str = include_str!("prompts/subagent.md");
pub const COMPACTION: &str = include_str!("prompts/compaction.md");
pub const PLAN: &str = include_str!("prompts/plan.md");

pub fn compose(custom: &str, project: &str, plan_mode: bool) -> String {
    let mut result = MAIN.trim().to_string();
    for (label, content) in [
        ("User customization", custom),
        ("Project guidance", project),
    ] {
        if !content.trim().is_empty() {
            result.push_str(&format!(
                "\n\n--- BEGIN {label} ---\n{}\n--- END {label} ---",
                content.trim()
            ));
        }
    }
    if plan_mode {
        result.push_str("\n\n");
        result.push_str(PLAN.trim());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_separate_and_mode_is_request_specific() {
        let normal = compose("Use Polish.", "# /project/AGENTS.md\nUse tabs.", false);
        assert!(normal.starts_with(MAIN.trim()));
        assert!(normal.contains("--- BEGIN User customization ---\nUse Polish."));
        assert!(normal.contains("--- BEGIN Project guidance ---\n# /project/AGENTS.md"));
        assert!(!normal.contains(PLAN.trim()));
        let planned = compose("Use Polish.", "# /project/AGENTS.md\nUse tabs.", true);
        assert!(planned.starts_with(&normal));
        assert!(planned.ends_with(PLAN.trim()));
        assert_eq!(compose("", "", false), MAIN.trim());
    }
}
