use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, GroupKey, ListDueBucket, Mode, View};
use crate::theme::Theme;
use crate::ui::{header, keep_cursor_visible, task_row};

/// Columns of indentation per directory level in tree mode.
const INDENT: usize = 2;

/// A directory header line for tree mode: `<indent><name>/` in the accent
/// colour. Lifetime-free so it can be pushed into the borrowed `lines` vec.
fn area_header<'a>(theme: &Theme, name: &str, indent: usize) -> Line<'a> {
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(
            format!("{name}/"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    super::fill_bg(frame, area, Style::default().bg(theme.bg));

    let [header_area, _spacer, body_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);

    let filter_label = header::filter_label(&app.filter);
    header::render(
        frame,
        header_area,
        theme,
        header::HeaderProps {
            title: Some(&display_path(&app.file_path)),
            // title: None,
            // file: &display_path(&app.file_path),
            count: app.visible_indices().len(),
            sort: app.sort_label(),
            filter: filter_label.as_deref(),
        },
    );

    if app.tasks().is_empty() {
        crate::ui::empty::render(frame, body_area, app);
        return;
    }

    let visible = app.visible_indices();
    let groups = app.visible_groups();
    let tree = app.is_tree_mode();
    let width = body_area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line: Option<usize> = None;

    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no tasks match".to_string(),
            Style::default().fg(theme.dim),
        )));
    } else {
        let blank = super::density_blank_lines(app.prefs.density);
        let counts = group_counts(groups);
        let last = visible.len().saturating_sub(1);
        let mut last_group: Option<&GroupKey> = None;

        // Tree mode groups tasks under a hierarchy of directory headers: the
        // scan-root folder is the top header and each subfolder nests under it,
        // one indent level deeper. (Always expanded.)
        if tree {
            lines.push(area_header(theme, &app.tree_root_label(), 0));
        }
        let mut prev_area: Vec<String> = Vec::new();

        for (i, (&abs, gk)) in visible.iter().zip(groups.iter()).enumerate() {
            let indent = if tree {
                let comps = app.task_area_components(abs);
                // Emit a header for each directory level new since the previous
                // task. Depth `d` sits under the root header, so it indents by
                // `(d + 1) * INDENT`.
                let common = comps
                    .iter()
                    .zip(prev_area.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                for (d, name) in comps.iter().enumerate().skip(common) {
                    lines.push(area_header(theme, name, (d + 1) * INDENT));
                }
                let depth = comps.len();
                prev_area = comps;
                (depth + 1) * INDENT
            } else {
                // Single-file: priority/due section headers on transitions.
                // `GroupKey::None` (Sort::File) renders no header, so the layout
                // is identical to the pre-grouping version.
                if !matches!(gk, GroupKey::None) && last_group != Some(gk) {
                    if !lines.is_empty() {
                        push_blanks(&mut lines, blank);
                    }
                    lines.push(group_header(theme, gk, counts.lookup(gk)));
                    last_group = Some(gk);
                }
                0
            };

            let task = &app.tasks()[abs];
            let opts = task_row::RowOpts {
                idx_label: i,
                cursor: i == app.cursor && app.mode != Mode::Help && app.mode != Mode::Settings,
                multi_mode: app.effective_mode() == Mode::Visual,
                multi_checked: app.selection.is_selected(abs),
                selected: app.selection.is_selected(abs),
                show_line_num: app.prefs.layout.line_num,
                match_term: if app.filter.search.is_empty() {
                    None
                } else {
                    Some(&app.filter.search)
                },
                today: app.today(),
                hidden_keys: &app.prefs.hidden_keys,
                area: None,
            };
            if i == app.cursor {
                cursor_line = Some(lines.len());
            }
            let avail = width.saturating_sub(indent).max(8);
            let mut rows = task_row::build_wrapped_lines(task, opts, theme, avail);
            if indent > 0 {
                let pad = " ".repeat(indent);
                for row in &mut rows {
                    row.spans.insert(0, Span::raw(pad.clone()));
                }
            }
            lines.extend(rows);
            if !tree && matches!(gk, GroupKey::None) && i != last {
                for _ in 0..blank {
                    lines.push(Line::raw(""));
                }
            }
        }
    }

    let scroll_cell = &app.view_scroll[View::List.idx()];
    let scroll = keep_cursor_visible(
        scroll_cell.get(),
        cursor_line,
        body_area.height,
        lines.len(),
    );
    scroll_cell.set(scroll);

    let para = Paragraph::new(lines)
        .style(Style::default().bg(theme.bg).fg(theme.fg))
        .scroll((scroll, 0));
    frame.render_widget(para, body_area);
}

fn display_path(p: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rel) = p.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    p.display().to_string()
}

/// Tally rows per `GroupKey` so each header can show its count without an
/// extra rescan during the render loop. Keyed by a stable string form so
/// `GroupKey::ListPriority(None)` and `ListPriority(Some('A'))` don't collide.
struct GroupCounts {
    inner: std::collections::HashMap<String, usize>,
}

impl GroupCounts {
    fn lookup(&self, gk: &GroupKey) -> usize {
        self.inner.get(&group_count_key(gk)).copied().unwrap_or(0)
    }
}

fn group_counts(groups: &[GroupKey]) -> GroupCounts {
    let mut inner: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for g in groups {
        if matches!(g, GroupKey::None) {
            continue;
        }
        *inner.entry(group_count_key(g)).or_insert(0) += 1;
    }
    GroupCounts { inner }
}

fn group_count_key(gk: &GroupKey) -> String {
    match gk {
        GroupKey::ListPriority(Some(c)) => format!("p:{c}"),
        GroupKey::ListPriority(None) => "p:_".to_string(),
        GroupKey::ListDue(b) => format!("d:{}", b.label()),
        // Not produced for List view; encode defensively.
        GroupKey::ArchiveDate(d) => format!("a:{d}"),
        GroupKey::None => String::new(),
    }
}

fn group_header<'a>(theme: &Theme, gk: &GroupKey, count: usize) -> Line<'a> {
    let (label, color) = match gk {
        GroupKey::ListPriority(Some(c)) => (format!("PRIORITY {c}"), theme.priority_color(*c)),
        GroupKey::ListPriority(None) => ("NO PRIORITY".to_string(), theme.dim),
        GroupKey::ListDue(b) => (b.label().to_string(), due_bucket_color(theme, *b)),
        // Defensive fallthrough — not produced under List view.
        GroupKey::ArchiveDate(d) => (d.clone(), theme.accent),
        GroupKey::None => (String::new(), theme.fg),
    };

    let char_count = count.to_string().len() + label.len();
    let divider_length = 80 - char_count;

    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("({})", count), Style::default().fg(theme.dim)),
        Span::raw("  "),
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "─".repeat(divider_length),
            Style::default().fg(theme.border),
        ),
    ])
}

fn due_bucket_color(theme: &Theme, b: ListDueBucket) -> Color {
    match b {
        ListDueBucket::Overdue => theme.overdue,
        ListDueBucket::Today => theme.today,
        ListDueBucket::ThisWeek => theme.accent,
        ListDueBucket::NextWeek => theme.accent,
        ListDueBucket::Later => theme.accent,
        ListDueBucket::NoDue => theme.dim,
    }
}

fn push_blanks(lines: &mut Vec<Line>, n: usize) {
    for _ in 0..n {
        lines.push(Line::raw(" "));
    }
}
