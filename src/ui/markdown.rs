use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub fn parse_markdown(text: &str, indent_prefix: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;

    for raw_line in text.lines() {
        let trimmed = raw_line.trim_end();

        if trimmed.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                lines.push(Line::from(vec![
                    Span::raw(indent_prefix.to_string()),
                    Span::styled("\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}", Style::default().fg(Color::DarkGray)),
                ]));
                continue;
            } else {
                in_code_block = true;
                let code_block_lang = trimmed.trim_start_matches('`').trim();
                let title = if code_block_lang.is_empty() {
                    "code"
                } else {
                    code_block_lang
                };
                let header = format!("\u{250c}\u{2500}\u{2500} {} \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}", title);
                lines.push(Line::from(vec![
                    Span::raw(indent_prefix.to_string()),
                    Span::styled(header, Style::default().fg(Color::DarkGray)),
                ]));
                continue;
            }
        }

        if in_code_block {
            lines.push(Line::from(vec![
                Span::raw(indent_prefix.to_string()),
                Span::styled("\u{2502} ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    raw_line.to_string(),
                    Style::default().fg(Color::LightYellow),
                ),
            ]));
            continue;
        }

        if let Some(h3) = trimmed.strip_prefix("### ") {
            lines.push(Line::from(vec![
                Span::raw(indent_prefix.to_string()),
                Span::styled(
                    "### ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    h3.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            continue;
        } else if let Some(h2) = trimmed.strip_prefix("## ") {
            lines.push(Line::from(vec![
                Span::raw(indent_prefix.to_string()),
                Span::styled(
                    "## ",
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    h2.to_string(),
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            continue;
        } else if let Some(h1) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(vec![
                Span::raw(indent_prefix.to_string()),
                Span::styled(
                    "# ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    h1.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ]));
            continue;
        }

        if let Some(quote) = trimmed.strip_prefix("> ") {
            let mut spans = vec![
                Span::raw(indent_prefix.to_string()),
                Span::styled("\u{2502} ", Style::default().fg(Color::DarkGray)),
            ];
            spans.extend(parse_inline_spans(
                quote,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
            lines.push(Line::from(spans));
            continue;
        }

        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![
                Span::raw(indent_prefix.to_string()),
                Span::styled("\u{2022} ", Style::default().fg(Color::Cyan)),
            ];
            spans.extend(parse_inline_spans(item, Style::default().fg(Color::White)));
            lines.push(Line::from(spans));
            continue;
        }

        if trimmed.is_empty() {
            lines.push(Line::from(""));
        } else {
            let mut spans = vec![Span::raw(indent_prefix.to_string())];
            spans.extend(parse_inline_spans(
                trimmed,
                Style::default().fg(Color::White),
            ));
            lines.push(Line::from(spans));
        }
    }

    if in_code_block {
        lines.push(Line::from(vec![
            Span::raw(indent_prefix.to_string()),
            Span::styled("\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}", Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines
}

fn parse_inline_spans(input: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut current = String::new();

    while i < len {
        if chars[i] == '`' {
            if let Some(end) = find_next_char(&chars, i + 1, '`') {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let code_text: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    format!("`{}`", code_text),
                    Style::default().fg(Color::LightYellow),
                ));
                i = end + 1;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_next_pair(&chars, i + 2, '*', '*') {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let bold_text: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(
                    bold_text,
                    base_style.add_modifier(Modifier::BOLD),
                ));
                i = end + 2;
                continue;
            }
        }

        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if let Some(end) = find_next_char(&chars, i + 1, '*') {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let italic_text: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    italic_text,
                    base_style.add_modifier(Modifier::ITALIC),
                ));
                i = end + 1;
                continue;
            }
        }

        current.push(chars[i]);
        i += 1;
    }

    if !current.is_empty() {
        spans.push(Span::styled(current, base_style));
    }

    spans
}

fn find_next_char(chars: &[char], start: usize, target: char) -> Option<usize> {
    for idx in start..chars.len() {
        if chars[idx] == target {
            return Some(idx);
        }
    }
    None
}

fn find_next_pair(chars: &[char], start: usize, c1: char, c2: char) -> Option<usize> {
    if chars.len() < 2 {
        return None;
    }
    for idx in start..chars.len().saturating_sub(1) {
        if chars[idx] == c1 && chars[idx + 1] == c2 {
            return Some(idx);
        }
    }
    None
}
