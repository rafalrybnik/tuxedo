use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::app::jump::JumpCandidate;
use crate::theme::Theme;
use crate::ui::dialog::draft_cursor_spans;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border).bg(theme.panel))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "tuxemdo",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · jump ", Style::default().fg(theme.dim)),
        ]))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let bg = Style::default().bg(theme.panel).fg(theme.fg);
    let hits = app.jump.hits();

    let [input_area, divider_area, list_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let mut input_spans = vec![
        Span::raw(" "),
        Span::styled(
            ">",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    input_spans.extend(draft_cursor_spans(
        app.draft.text(),
        app.draft.cursor(),
        theme.fg,
        theme.panel,
    ));
    let summary = if app.draft.text().is_empty() {
        String::new()
    } else if app.draft.text().chars().all(|c| c.is_ascii_digit()) {
        format!("  → row {}", app.draft.text())
    } else {
        format!("  {} matches", hits.len())
    };
    input_spans.push(Span::styled(summary, Style::default().fg(theme.dim)));
    frame.render_widget(
        Paragraph::new(Line::from(input_spans).style(bg)).style(bg),
        input_area,
    );

    frame.render_widget(
        Paragraph::new(
            Line::from(Span::styled(
                "─".repeat(usize::from(divider_area.width)),
                Style::default().fg(theme.border),
            ))
            .style(bg),
        )
        .style(bg),
        divider_area,
    );

    let list_h = usize::from(list_area.height);
    if hits.is_empty() {
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled("no matches", Style::default().fg(theme.dim)),
        ])
        .style(bg);
        frame.render_widget(Paragraph::new(line).style(bg), list_area);
    } else {
        let cursor = app.jump.cursor.min(hits.len() - 1);
        let start = if cursor < list_h {
            0
        } else {
            cursor + 1 - list_h
        };
        let end = (start + list_h).min(hits.len());
        let lines: Vec<Line> = hits[start..end]
            .iter()
            .enumerate()
            .map(|(i, hit)| {
                let abs = start + i;
                render_row(
                    app.jump.candidate(hit.cand),
                    &hit.positions,
                    abs == cursor,
                    theme,
                )
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).style(bg), list_area);
    }

    let footer = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "↑↓ / Ctrl-N/P",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " · type text/folder, or a number · ",
            Style::default().fg(theme.dim),
        ),
        Span::styled(
            "Enter",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" jump · ", Style::default().fg(theme.dim)),
        Span::styled(
            "Esc",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(theme.dim)),
    ])
    .style(bg);
    frame.render_widget(Paragraph::new(footer).style(bg), footer_area);
}

fn render_row<'a>(
    cand: &'a JumpCandidate,
    positions: &[usize],
    selected: bool,
    theme: &Theme,
) -> Line<'a> {
    let bg = if selected { theme.cursor } else { theme.panel };
    let base = if selected {
        Style::default()
            .fg(theme.fg)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg).bg(bg)
    };
    let hl = Style::default()
        .fg(theme.bg)
        .bg(theme.matched)
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'a>> = Vec::new();
    spans.push(Span::styled(
        if selected { " ▶ " } else { "   " },
        Style::default()
            .fg(if selected { theme.accent } else { bg })
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));

    if cand.is_area {
        spans.push(Span::styled(
            cand.label.clone(),
            Style::default()
                .fg(theme.accent)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        return Line::from(spans).style(Style::default().bg(bg));
    }

    if !cand.area.is_empty() {
        spans.push(Span::styled(
            format!("{}/ ", cand.area),
            Style::default().fg(theme.dim).bg(bg),
        ));
    }

    // Label with per-byte match highlighting.
    let label = &cand.label;
    let mut cur = 0usize;
    for &p in positions {
        if p < cur || p >= label.len() {
            continue;
        }
        if cur < p {
            spans.push(Span::styled(label[cur..p].to_string(), base));
        }
        let ch_len = label[p..].chars().next().map(char::len_utf8).unwrap_or(1);
        spans.push(Span::styled(label[p..p + ch_len].to_string(), hl));
        cur = p + ch_len;
    }
    if cur < label.len() {
        spans.push(Span::styled(label[cur..].to_string(), base));
    }

    Line::from(spans).style(Style::default().bg(bg))
}
