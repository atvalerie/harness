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

    // Now process links in spans: markdown links `[text](url)` and autolinks `http(s)://...`
    linkify_spans(spans)
}

/// Transform text spans containing markdown links `[label](url)` or plain URLs into styled spans.
/// If terminal hyperlink formatting or OSC 8 is desired, the text can be formatted as an OSC 8 escape sequence.
pub fn osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}

fn linkify_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for span in spans {
        let content = span.content.into_owned();
        let style = span.style;
        let mut sub_spans = parse_links_in_text(&content, style);
        out.append(&mut sub_spans);
    }
    out
}

fn parse_links_in_text(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut idx = 0;
    let bytes = text.as_bytes();
    let len = bytes.len();

    while idx < len {
        // Look for markdown link `[label](url)`
        if bytes[idx] == b'[' {
            if let Some(close_bracket) = text[idx..].find(']') {
                let label_end = idx + close_bracket;
                if label_end + 1 < len && bytes[label_end + 1] == b'(' {
                    if let Some(close_paren) = text[label_end + 1..].find(')') {
                        let url_end = label_end + 1 + close_paren;
                        let label = &text[idx + 1..label_end];
                        let url = &text[label_end + 2..url_end];

                        let link_style = Style::default()
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED);

                        let link_text = osc8_link(url, label);
                        spans.push(Span::styled(link_text, link_style));

                        idx = url_end + 1;
                        continue;
                    }
                }
            }
        }

        // Look for bare URL `http://` or `https://`
        if text[idx..].starts_with("https://") || text[idx..].starts_with("http://") {
            let rem = &text[idx..];
            let end_offset = rem
                .find(|c: char| c.is_whitespace() || c == ')' || c == ']' || c == '>' || c == '"' || c == '\'')
                .unwrap_or(rem.len());
            let url = &rem[..end_offset];

            let link_style = Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::UNDERLINED);
            let link_text = osc8_link(url, url);
            spans.push(Span::styled(link_text, link_style));

            idx += end_offset;
            continue;
        }

        // Normal character - find next candidate delimiter
        let next_delim = text[idx..]
            .find(|c: char| c == '[' || c == 'h')
            .map(|offset| if offset == 0 { 1 } else { offset })
            .unwrap_or(len - idx);

        let regular_text = &text[idx..idx + next_delim];
        spans.push(Span::styled(regular_text.to_string(), base_style));
        idx += next_delim;
    }

    spans
}

fn find_next_char(chars: &[char], start: usize, target: char) -> Option<usize> {
    (start..chars.len()).find(|&idx| chars[idx] == target)
}

fn find_next_pair(chars: &[char], start: usize, c1: char, c2: char) -> Option<usize> {
    if chars.len() < 2 {
        return None;
    }
    (start..chars.len().saturating_sub(1)).find(|&idx| chars[idx] == c1 && chars[idx + 1] == c2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_osc8_link_formatting() {
        let link = osc8_link("https://example.com", "Example");
        assert_eq!(link, "\x1b]8;;https://example.com\x1b\\Example\x1b]8;;\x1b\\");
    }

    #[test]
    fn test_markdown_link_parsing() {
        let lines = parse_markdown("Check out [Rust](https://rust-lang.org) now!", "");
        assert_eq!(lines.len(), 1);
        let text = lines[0].to_string();
        assert!(text.contains("Rust"));
        assert!(text.contains("https://rust-lang.org"));
    }

    #[test]
    fn test_bare_url_parsing() {
        let lines = parse_markdown("Visit https://github.com today.", "");
        assert_eq!(lines.len(), 1);
        let text = lines[0].to_string();
        assert!(text.contains("https://github.com"));
    }
}
