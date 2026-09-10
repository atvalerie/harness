use crate::app::{App, EngineState};
use crate::client::get_model_pricing;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub fn render_usage(app: &App, frame: &mut Frame, area: Rect) {
    let streaming = app.state == EngineState::Streaming;
    let usage = if streaming {
        Some(&app.request_usage)
    } else {
        app.usage_records.last().map(|r| &r.details)
    };
    let input = usage
        .and_then(|u| u.input_tokens)
        .map(|n| n.to_string())
        .unwrap_or_else(|| format!("~{}", app.context_tokens));
    let output = usage
        .and_then(|u| u.output_tokens)
        .map(|n| n.to_string())
        .unwrap_or_else(|| {
            if streaming {
                format!(
                    "~{}",
                    (app.current_response_buffer.len() + app.current_thought_buffer.len())
                        .div_ceil(4)
                )
            } else {
                "unknown".into()
            }
        });
    let total = usage
        .and_then(|u| u.total())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unknown".into());
    let cached = usage
        .and_then(|u| u.cache_read_tokens)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    let rate = app.usage_summary.cache_rate();
    let known = app.usage_summary.cache_known;
    let cost = app
        .usage_records
        .last()
        .and_then(|r| r.cost_nano_usd)
        .map(|n| format!("${:.6}", n as f64 / 1_000_000_000.0))
        .unwrap_or_else(|| "unpriced".into());
    let text = format!(
        " {} | in {} out {} total {} | cached {} | cache {} ({}/{}) | {} | ~ estimated",
        if streaming { "Live" } else { "Last request" },
        input,
        output,
        total,
        cached,
        rate,
        known,
        app.usage_records.len(),
        if streaming { "cost pending" } else { &cost }
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Cyan)),
        area,
    );
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    #[test]
    fn footer_switches_from_estimated_stream_to_reported_totals() {
        let mut app = App::new(crate::config::AppConfig::default(), String::new());
        app.state = EngineState::Streaming;
        app.current_response_buffer = "12345678".into();
        let backend = ratatui::backend::TestBackend::new(180, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_usage(&app, frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Live"));
        assert!(text.contains("out ~2"));
        app.request_usage = crate::usage::TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            cache_read_tokens: Some(80),
            ..Default::default()
        };
        terminal
            .draw(|frame| render_usage(&app, frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("in 100 out 20 total 120"));
        assert!(text.contains("cached 80"));
        app.state = EngineState::Idle;
        app.usage_records.push(crate::session::UsageRecord {
            timestamp: String::new(),
            provider: "test".into(),
            model: "test".into(),
            prompt_tokens: 100,
            candidates_tokens: 20,
            total_tokens: 120,
            estimated: false,
            duration_ms: 1,
            status: "ok".into(),
            details: app.request_usage.clone(),
            cost_nano_usd: None,
            pricing: None,
        });
        terminal
            .draw(|frame| render_usage(&app, frame, frame.area()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Last request | in 100 out 20 total 120"));
    }
}

pub fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let (in_p, out_p) = get_model_pricing(&app.config.model);
    let pricing_str = match (in_p, out_p) {
        (Some(i), Some(o)) => format!(" (${:.2}/${:.2} M)", i, o),
        _ => String::new(),
    };

    let (state_badge, state_fg, state_bg) = match app.state {
        EngineState::Idle => ("IDLE", Color::Black, Color::DarkGray),
        EngineState::Streaming => ("STREAMING", Color::Black, Color::Green),
        EngineState::Compacting => ("COMPACTING", Color::Black, Color::Yellow),
        EngineState::AwaitingHitlApproval => ("GATE-HOLD", Color::Black, Color::Yellow),
        EngineState::ExecutingTool => ("TOOL-EXEC", Color::Black, Color::LightCyan),
    };

    let thinking_str = app.thinking_mode(&app.config.model);
    let temperature_text = if app
        .config
        .active_provider_config()
        .kind
        .eq_ignore_ascii_case("codex")
    {
        "n/a".to_string()
    } else {
        format!("{:.1}", app.config.temperature)
    };

    // Context tracking
    let context_limit = app.context_limit();

    let context_pct = context_limit
        .map(|limit| (app.context_tokens as f64 / limit.max(1) as f64) * 100.0)
        .unwrap_or(0.0);
    let context_limit_text = context_limit
        .map(format_compact_number)
        .unwrap_or_else(|| "?".to_string());

    let provider_limit_parts = app
        .provider_limits
        .as_deref()
        .map(compact_provider_limit_parts)
        .filter(|parts| !parts.is_empty());

    let tps_str = if app.state == EngineState::Streaming || app.current_tps > 0.0 {
        format!(" │ {:.1} tps", app.current_tps)
    } else {
        String::new()
    };

    let activity = match app.state {
        EngineState::Streaming | EngineState::Compacting | EngineState::ExecutingTool => {
            format!("{} ", spinner(app))
        }
        EngineState::AwaitingHitlApproval => "! ".to_string(),
        EngineState::Idle => String::new(),
    };

    let line = Line::from(vec![
        Span::styled(
            " HOLIDAY ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", state_badge),
            Style::default()
                .fg(state_fg)
                .bg(state_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if app.auto_mode { " AUTO REVIEW " } else { " " },
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(activity, Style::default().fg(Color::LightCyan)),
        Span::styled(
            &app.config.model,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(pricing_str, Style::default().fg(Color::DarkGray)),
        if provider_limit_parts.is_some() {
            Span::raw(" \u{2502} ")
        } else {
            Span::raw("")
        },
        if let Some(parts) = provider_limit_parts.as_ref() {
            Span::styled(
                parts.first().cloned().unwrap_or_default(),
                Style::default().fg(Color::LightGreen),
            )
        } else {
            Span::raw("")
        },
        if provider_limit_parts
            .as_ref()
            .is_some_and(|parts| parts.len() > 1)
        {
            Span::raw(" \u{2502} ")
        } else {
            Span::raw("")
        },
        if let Some(part) = provider_limit_parts.as_ref().and_then(|parts| parts.get(1)) {
            Span::styled(part.clone(), Style::default().fg(Color::LightGreen))
        } else {
            Span::raw("")
        },
        if provider_limit_parts
            .as_ref()
            .is_some_and(|parts| parts.len() > 2)
        {
            Span::raw(" \u{2502} ")
        } else {
            Span::raw("")
        },
        if let Some(part) = provider_limit_parts.as_ref().and_then(|parts| parts.get(2)) {
            Span::styled(part.clone(), Style::default().fg(Color::LightGreen))
        } else {
            Span::raw("")
        },
        Span::raw(" │ "),
        Span::styled("think: ", Style::default().fg(Color::DarkGray)),
        Span::styled(thinking_str, Style::default().fg(Color::Magenta)),
        Span::raw(" │ "),
        Span::styled("temp: ", Style::default().fg(Color::DarkGray)),
        Span::styled(temperature_text, Style::default().fg(Color::Yellow)),
        Span::raw(" │ "),
        Span::styled("ctx: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{}{}/{} ({:.1}%)",
                if app.context_tokens_estimated {
                    "~"
                } else {
                    ""
                },
                app.context_tokens,
                context_limit_text,
                context_pct
            ),
            Style::default().fg(if context_pct > 80.0 {
                Color::Red
            } else {
                Color::Green
            }),
        ),
        Span::styled(tps_str, Style::default().fg(Color::LightCyan)),
        Span::raw(" │ "),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

pub fn spinner(app: &App) -> char {
    const FRAMES: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
    let index = ((app.ui_started_at.elapsed().as_millis() / 100) as usize) % FRAMES.len();
    FRAMES[index]
}

fn format_compact_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn compact_provider_limit_parts(value: &str) -> Vec<String> {
    let summaries = value
        .lines()
        .filter_map(|line| {
            let percent = line.split_whitespace().find(|word| word.ends_with('%'))?;
            let reset = line
                .split_once("resets in ")
                .map(|(_, duration)| duration.trim().trim_end_matches(','))
                .unwrap_or("unknown");
            Some(format!("{} left ({} reset)", percent, reset))
        })
        .collect::<Vec<_>>();
    if !summaries.is_empty() {
        return summaries;
    }

    let fallback = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if fallback.is_empty() {
        Vec::new()
    } else {
        vec![fallback]
    }
}

#[cfg(test)]
mod tests {
    use super::compact_provider_limit_parts;

    #[test]
    fn status_limits_use_a_short_window_summary() {
        let value = "Codex usage (pro)\nPrimary: 85% left (5h), resets in 2h 10m\nSecondary: 40% left (7d), resets in 3d";
        let summary = compact_provider_limit_parts(value);
        assert_eq!(
            summary,
            vec!["85% left (2h 10m reset)", "40% left (3d reset)"]
        );
    }
}
