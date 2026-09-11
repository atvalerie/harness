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

    // Current / Last Turn metrics
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
                "0".into()
            }
        });
    let total = usage
        .and_then(|u| u.total())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "0".into());
    let cached = usage
        .and_then(|u| u.cache_read_tokens)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "0".into());

    // Session-wide accumulating totals
    let session_input = app.usage_summary.input;
    let session_output = app.usage_summary.output;
    let session_cached = app.usage_summary.cache_read;
    let session_cache_rate = app.usage_summary.cache_rate();

    // Session total cost
    let total_cost_nano: u64 = app
        .usage_records
        .iter()
        .filter_map(|r| r.cost_nano_usd)
        .sum();
    let cost_str = if total_cost_nano > 0 {
        format!("${:.4}", total_cost_nano as f64 / 1_000_000_000.0)
    } else if let Some(last_cost) = app.usage_records.last().and_then(|r| r.cost_nano_usd) {
        format!("${:.4}", last_cost as f64 / 1_000_000_000.0)
    } else {
        "$0.00".into()
    };

    // Speed / TPS
    let tps = if streaming || app.state == EngineState::ExecutingTool {
        if app.current_tps > 0.0 {
            format!("{:.1} tps", app.current_tps)
        } else {
            "streaming".to_string()
        }
    } else if app.last_turn_tps > 0.0 {
        format!("{:.1} tps ({:.1}s)", app.last_turn_tps, app.last_turn_duration_secs)
    } else {
        String::new()
    };

    let mut spans = vec![
        Span::styled(
            if streaming { " ⠋ Live " } else { " ● " },
            Style::default().fg(if streaming { Color::Cyan } else { Color::Green }),
        ),
        Span::styled("session: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "in {} (cached {}) / out {}",
                format_compact_number(session_input),
                if session_cached > 0 {
                    format!("{} · {}", format_compact_number(session_cached), session_cache_rate)
                } else {
                    format_compact_number(session_cached)
                },
                format_compact_number(session_output)
            ),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ];

    spans.push(Span::raw(" │ "));
    spans.push(Span::styled("turn: ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("in {} out {} total {}", input, output, total),
        Style::default().fg(Color::LightCyan),
    ));

    if cached != "0" && cached != "?" {
        spans.push(Span::styled(
            format!(" (cached {})", cached),
            Style::default().fg(Color::LightGreen),
        ));
    }

    if cost_str != "$0.00" {
        spans.push(Span::raw(" │ "));
        spans.push(Span::styled(cost_str, Style::default().fg(Color::Yellow)));
    }

    if !tps.is_empty() {
        spans.push(Span::raw(" │ "));
        spans.push(Span::styled(tps, Style::default().fg(Color::Cyan)));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)),
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
        assert!(text.contains("in 100 out 20 total 120"));
    }
}

pub fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let (in_p, out_p) = get_model_pricing(&app.config.model);
    let pricing_str = match (in_p, out_p) {
        (Some(i), Some(o)) => format!(" (${:.2}/${:.2} M)", i, o),
        _ => String::new(),
    };

    let (state_icon, state_text, state_color) = match app.state {
        EngineState::Idle => ("●", "ready", Color::Green),
        EngineState::Streaming => ("◐", "generating", Color::Cyan),
        EngineState::Compacting => ("⟳", "compacting", Color::Yellow),
        EngineState::AwaitingHitlApproval => ("▲", "approval needed", Color::Yellow),
        EngineState::ExecutingTool => ("⚡", "tool execution", Color::LightCyan),
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

    let is_narrow = area.width < 90;
    let is_compact = area.width < 60;

    let mut spans = vec![
        Span::styled(
            if is_compact { " " } else { " holiday " },
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("│ "),
        Span::styled(
            format!("{} {} ", state_icon, state_text),
            Style::default()
                .fg(state_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if app.auto_mode {
        spans.push(Span::styled(
            "[auto] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::styled(
        &app.config.model,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    if !is_narrow {
        spans.push(Span::styled(pricing_str, Style::default().fg(Color::DarkGray)));
    }

    if let Some(parts) = provider_limit_parts.as_ref() {
        if !is_compact {
            spans.push(Span::raw(" │ "));
            spans.push(Span::styled(
                parts.first().cloned().unwrap_or_default(),
                Style::default().fg(Color::LightGreen),
            ));
            if !is_narrow {
                for part in parts.iter().skip(1) {
                    spans.push(Span::raw(" │ "));
                    spans.push(Span::styled(part.clone(), Style::default().fg(Color::LightGreen)));
                }
            }
        }
    }

    if !is_narrow {
        spans.push(Span::raw(" │ "));
        spans.push(Span::styled("think: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(thinking_str, Style::default().fg(Color::Magenta)));
        spans.push(Span::raw(" │ "));
        spans.push(Span::styled("temp: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(temperature_text, Style::default().fg(Color::Yellow)));
    }

    spans.push(Span::raw(" │ "));
    spans.push(Span::styled("ctx: ", Style::default().fg(Color::DarkGray)));
    let compact_indicator = if app.config.auto_compact {
        let thresh = format_compact_number(app.config.auto_compact_threshold_tokens);
        format!(" [compact@{}]", thresh)
    } else {
        String::new()
    };
    spans.push(Span::styled(
        format!(
            "{}{}/{} ({:.1}%){}",
            if app.context_tokens_estimated {
                "~"
            } else {
                ""
            },
            app.context_tokens,
            context_limit_text,
            context_pct,
            compact_indicator
        ),
        Style::default().fg(if context_pct > 80.0 {
            Color::Red
        } else {
            Color::Green
        }),
    ));

    let tps_str = if app.state == EngineState::Streaming || app.current_tps > 0.0 {
        format!(" │ {:.1} tps", app.current_tps)
    } else if app.last_turn_tps > 0.0 {
        format!(" │ {:.1} tps ({:.1}s)", app.last_turn_tps, app.last_turn_duration_secs)
    } else {
        String::new()
    };

    if !is_narrow && !tps_str.is_empty() {
        spans.push(Span::styled(tps_str, Style::default().fg(Color::LightCyan)));
    }

    let line = Line::from(spans);

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
