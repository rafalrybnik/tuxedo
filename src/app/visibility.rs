use std::path::PathBuf;

use super::App;
use super::types::{Sort, View};
use crate::core::filter::{self, ListDueBucket};

/// A row in the tree-mode list: a directory header, or a task referenced by its
/// index into `visible_cache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    Header {
        /// Directory depth (0 = directly under the scan root).
        depth: usize,
        name: String,
        collapsed: bool,
    },
    Task {
        /// Index into `visible_cache`.
        vis: usize,
        /// Area depth of the task (for indentation).
        depth: usize,
    },
}

/// One entry per visible row, parallel to `visible_cache`. Renderers detect
/// group transitions by comparing successive entries; under `Sort::File` every
/// row is `None` so the renderer skips headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKey {
    None,
    ArchiveDate(String),
    /// `Some('A'..='Z')` for a graded priority, `None` for unprioritized.
    ListPriority(Option<char>),
    ListDue(ListDueBucket),
}

impl App {
    /// Indices into the active view's task source after filter + sort, in
    /// display order. The source is `archive().tasks()` in Archive view,
    /// `tasks()` otherwise. Reads the cache populated by `recompute_visible`.
    pub fn visible_indices(&self) -> &[usize] {
        &self.visible_cache
    }

    /// Group key per row, parallel to `visible_indices()`.
    pub fn visible_groups(&self) -> &[GroupKey] {
        &self.visible_groups
    }

    /// Recompute the cached visible-index list and parallel group keys. Call
    /// after any mutation that affects filter/sort/view/tasks/archive.
    pub fn recompute_visible(&mut self) {
        match self.view {
            View::List => self.rebuild_list_cache(),
            View::Archive => self.rebuild_archive_cache(),
        }
    }

    fn rebuild_list_cache(&mut self) {
        let needle = (!self.filter.search.is_empty()).then_some(self.filter.search.as_str());
        let tasks = self.store.tasks();
        let today = self.store.today();

        let mut idxs: Vec<usize> = (0..tasks.len())
            .filter(|&i| {
                filter::list_predicate(
                    &tasks[i],
                    self.prefs.show_done,
                    self.prefs.show_future,
                    today,
                    &self.filter,
                    needle,
                )
            })
            .collect();

        filter::sort_by_prefs(&mut idxs, tasks, self.prefs.sort);

        if self.is_tree_mode() {
            // Tree mode: directory headers (with collapse) drive the layout, so
            // group keys are unused; tasks under a collapsed area are dropped.
            let (visible, rows) = self.build_tree_rows(&idxs);
            self.visible_groups = vec![GroupKey::None; visible.len()];
            self.visible_cache = visible;
            self.tree_rows = rows;
            return;
        }

        let groups: Vec<GroupKey> = match self.prefs.sort {
            Sort::File => vec![GroupKey::None; idxs.len()],
            Sort::Priority => idxs
                .iter()
                .map(|&i| GroupKey::ListPriority(tasks[i].priority))
                .collect(),
            Sort::Due => idxs
                .iter()
                .map(|&i| GroupKey::ListDue(filter::due_bucket(&tasks[i], today)))
                .collect(),
        };
        self.visible_groups = groups;
        self.visible_cache = idxs;
        self.tree_rows.clear();
    }

    /// Walk filtered tasks (file order) into `(visible_cache, tree_rows)`:
    /// emit a header on each new directory level and a task row for each task,
    /// but when a directory is collapsed emit only its `▸` header and skip its
    /// whole subtree (those tasks never enter `visible_cache`).
    fn build_tree_rows(&self, idxs: &[usize]) -> (Vec<usize>, Vec<TreeRow>) {
        let mut visible: Vec<usize> = Vec::new();
        let mut rows: Vec<TreeRow> = Vec::new();
        let mut prev: Vec<String> = Vec::new();
        let mut skip_under: Option<Vec<String>> = None;

        for &i in idxs {
            let comps = self.task_area_components(i);
            if let Some(s) = &skip_under {
                if comps.len() >= s.len() && comps[..s.len()] == s[..] {
                    continue; // hidden inside a collapsed subtree
                }
                skip_under = None;
            }
            let common = comps
                .iter()
                .zip(prev.iter())
                .take_while(|(a, b)| a == b)
                .count();
            let mut entered_collapsed = false;
            for d in common..comps.len() {
                let path: PathBuf = comps[..=d].iter().collect();
                let collapsed = self.collapsed.contains(&path);
                rows.push(TreeRow::Header {
                    depth: d,
                    name: comps[d].clone(),
                    collapsed,
                });
                if collapsed {
                    skip_under = Some(comps[..=d].to_vec());
                    entered_collapsed = true;
                    break;
                }
            }
            let depth = comps.len();
            prev = comps;
            if entered_collapsed {
                continue;
            }
            rows.push(TreeRow::Task {
                vis: visible.len(),
                depth,
            });
            visible.push(i);
        }
        (visible, rows)
    }

    pub fn tree_rows(&self) -> &[TreeRow] {
        &self.tree_rows
    }

    /// Collapse (or expand) the directory of the task under the cursor. Root
    /// tasks have no directory to collapse.
    pub fn toggle_collapse_current(&mut self) {
        if !self.is_tree_mode() {
            return;
        }
        let Some(abs) = self.cur_abs() else { return };
        let area = self.store.area(abs);
        if area.as_os_str().is_empty() {
            self.flash("root tasks have no folder to collapse");
            return;
        }
        if !self.collapsed.remove(&area) {
            self.collapsed.insert(area);
        }
        self.recompute_visible();
        self.clamp_cursor();
    }

    /// Expand every collapsed directory.
    pub fn expand_all(&mut self) {
        if self.collapsed.is_empty() {
            return;
        }
        self.collapsed.clear();
        self.recompute_visible();
        self.clamp_cursor();
    }

    fn rebuild_archive_cache(&mut self) {
        let archive = self.store.archive().tasks();
        let mut idxs: Vec<usize> = (0..archive.len()).collect();
        idxs.sort_by(|&a, &b| {
            archive[b]
                .done_date
                .as_deref()
                .unwrap_or("")
                .cmp(archive[a].done_date.as_deref().unwrap_or(""))
        });
        let groups: Vec<GroupKey> = idxs
            .iter()
            .map(|&i| {
                let date = archive[i]
                    .done_date
                    .clone()
                    .unwrap_or_else(|| "unknown".into());
                GroupKey::ArchiveDate(date)
            })
            .collect();
        self.visible_cache = idxs;
        self.visible_groups = groups;
    }

    pub fn cur_abs(&self) -> Option<usize> {
        self.visible_cache.get(self.cursor).copied()
    }

    pub fn clamp_cursor(&mut self) {
        let len = self.visible_cache.len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    /// Move the cursor to wherever `abs` lives in the current visible list.
    /// Falls back to clamping if `abs` was filtered out.
    pub(super) fn follow_cursor(&mut self, abs: usize) {
        if let Some(pos) = self.visible_cache.iter().position(|&i| i == abs) {
            self.cursor = pos;
        } else {
            self.clamp_cursor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::build_app;
    use crate::core::filter::ListDueBucket;

    #[test]
    fn search_matches_subsequence() {
        let mut app = build_app("2026-05-01 Call dentist\n2026-05-01 buy milk\n");
        app.filter.search = "cade".into();
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 1);
    }

    #[test]
    fn search_matches_body_not_dates() {
        let mut app = build_app("2026-05-01 buy milk\n2026-04-01 something else\n");
        app.filter.search = "2026".into();
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 0);
    }

    #[test]
    fn visible_cache_updates_after_mutation() {
        let mut app = build_app("a\nb\nc\n");
        assert_eq!(app.visible_indices().len(), 3);
        app.draft_set("d".into());
        app.add_from_draft();
        assert_eq!(app.visible_indices().len(), 4);
    }

    #[test]
    fn list_cursor_survives_archive_roundtrip() {
        let mut app = build_app("a\nb\nc\nd\ne\n");
        app.cursor = 3;
        app.set_view(View::Archive);
        app.set_view(View::List);
        assert_eq!(app.cursor, 3, "cursor lost on List → Archive → List");
    }

    #[test]
    fn archive_indices_point_into_archive_tasks() {
        let mut app = build_app("a\n");
        let path = app.archive().path().to_path_buf();
        app.store.set_primary_archive(crate::app::Archive::for_test(
            crate::todo::parse_file(
                "- [x] 2026-05-01 2026-04-01 first\n- [x] 2026-05-02 2026-04-02 second\n",
            ),
            String::new(),
            path,
        ));
        app.set_view(View::Archive);
        let idxs = app.visible_indices();
        assert_eq!(idxs.len(), 2);
        for &i in idxs {
            assert!(app.archive().tasks().get(i).is_some());
        }
    }

    #[test]
    fn list_groups_are_none_under_sort_file() {
        let mut app = build_app("(A) a\n(B) b\nc\n");
        app.prefs.sort = Sort::File;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 3);
        for g in groups {
            assert!(matches!(g, GroupKey::None));
        }
    }

    #[test]
    fn list_groups_track_priority_under_sort_priority() {
        let mut app = build_app("(A) a\n(B) b\nc\n(A) a2\n");
        app.prefs.sort = Sort::Priority;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0], GroupKey::ListPriority(Some('A')));
        assert_eq!(groups[1], GroupKey::ListPriority(Some('A')));
        assert_eq!(groups[2], GroupKey::ListPriority(Some('B')));
        assert_eq!(groups[3], GroupKey::ListPriority(None));
    }

    #[test]
    fn list_groups_bucket_due_dates_under_sort_due() {
        let raw = "a due:2026-05-04\n\
                   b due:2026-05-06\n\
                   c due:2026-05-08\n\
                   d due:2026-05-15\n\
                   e due:2026-05-25\n\
                   f\n";
        let mut app = build_app(raw);
        app.prefs.sort = Sort::Due;
        app.recompute_visible();
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 6);
        assert_eq!(groups[0], GroupKey::ListDue(ListDueBucket::Overdue));
        assert_eq!(groups[1], GroupKey::ListDue(ListDueBucket::Today));
        assert_eq!(groups[2], GroupKey::ListDue(ListDueBucket::ThisWeek));
        assert_eq!(groups[3], GroupKey::ListDue(ListDueBucket::NextWeek));
        assert_eq!(groups[4], GroupKey::ListDue(ListDueBucket::Later));
        assert_eq!(groups[5], GroupKey::ListDue(ListDueBucket::NoDue));
    }

    #[test]
    fn future_absolute_threshold_hidden_by_default() {
        let mut app = build_app("future task t:2030-01-01\nvisible task\n");
        assert_eq!(app.visible_indices().len(), 1);
        assert_eq!(
            app.tasks()[app.visible_indices()[0]].raw,
            "- [ ] visible task"
        );
        app.prefs.show_future = true;
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 2);
    }

    #[test]
    fn relative_threshold_anchors_on_due() {
        let mut app = build_app("Pay rent due:2026-05-15 t:-3d\n");
        assert_eq!(app.visible_indices().len(), 0);
        app.prefs.show_future = true;
        app.recompute_visible();
        assert_eq!(app.visible_indices().len(), 1);
    }

    #[test]
    fn refresh_today_unhides_tasks_when_date_advances() {
        let mut app = build_app("future task t:2026-05-07\nvisible task\n");
        assert_eq!(app.visible_indices().len(), 1);
        let changed = app.refresh_today("2026-05-07".into());
        assert!(changed);
        assert_eq!(app.today(), "2026-05-07");
        assert_eq!(app.visible_indices().len(), 2);
    }

    #[test]
    fn refresh_today_is_noop_when_date_unchanged() {
        let mut app = build_app("a\n");
        let changed = app.refresh_today("2026-05-06".into());
        assert!(!changed);
        assert_eq!(app.today(), "2026-05-06");
    }

    #[test]
    fn archive_visible_groups_are_done_date_desc() {
        let mut app = build_app("a\n");
        let path = app.archive().path().to_path_buf();
        app.store.set_primary_archive(crate::app::Archive::for_test(
            crate::todo::parse_file(
                "- [x] 2026-04-01 2026-03-01 older\n- [x] 2026-05-02 2026-04-02 newer\n",
            ),
            String::new(),
            path,
        ));
        app.set_view(View::Archive);
        let groups = app.visible_groups();
        assert_eq!(groups.len(), 2);
        let first = match &groups[0] {
            GroupKey::ArchiveDate(d) => d.as_str(),
            _ => panic!("expected ArchiveDate"),
        };
        let second = match &groups[1] {
            GroupKey::ArchiveDate(d) => d.as_str(),
            _ => panic!("expected ArchiveDate"),
        };
        assert_eq!(first, "2026-05-02");
        assert_eq!(second, "2026-04-01");
    }
}
