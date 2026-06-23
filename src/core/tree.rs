//! Multi-file ("tree") aggregation over many `todo.md` files.
//!
//! [`TreeStore`] owns one [`Store`] per discovered `todo.md` and presents their
//! tasks as a single flat list. Every mutation is routed back to the owning
//! file's `Store`, so per-file persistence, reconciliation, recurrence and undo
//! all reuse the proven single-file logic unchanged — this layer only maps a
//! global task index to `(store, local index)` and rebuilds the flat view.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::outcome::{
    AddOutcome, ArchiveDeleteOutcome, ArchiveOutcome, BulkCompleteOutcome, BulkDeleteOutcome,
    CompleteOutcome, DeleteOutcome, DrainReport, EditOutcome, PriorityOutcome, Reconcile,
    TagOutcome, UnarchiveOutcome, UndoOutcome,
};
use super::{Archive, Store};
use crate::todo::Task;

/// Directory names skipped while discovering `todo.md` files (besides any
/// dot-directory).
const SKIP_DIRS: &[&str] = &["node_modules", "target", "vendor"];
const TODO_FILENAME: &str = "todo.md";

pub struct TreeStore {
    root: PathBuf,
    stores: Vec<Store>,
    /// Flat task view; `agg[i]` is a clone of the task at `map[i]`. Rebuilt
    /// after every mutation so callers can treat it like `Store::tasks`.
    agg: Vec<Task>,
    /// `map[i] = (store index, local index)` for `agg[i]`.
    map: Vec<(usize, usize)>,
    /// Stack of store indices in mutation order, for global undo.
    undo_order: Vec<usize>,
    today: String,
}

impl TreeStore {
    /// Discover every `todo.md` under `root`, load each into its own `Store`,
    /// and ensure a `root/todo.md` store exists as the target for new tasks.
    pub fn open(root: &Path, today: String) -> std::io::Result<Self> {
        let mut files = Vec::new();
        collect_todo_files(root, &mut files)?;
        files.sort();

        let root_todo = root.join(TODO_FILENAME);
        if !files.contains(&root_todo) {
            files.insert(0, root_todo);
        }

        let mut stores = Vec::with_capacity(files.len());
        for file in files {
            let body = std::fs::read_to_string(&file).unwrap_or_default();
            stores.push(Store::open_sync(file, body, today.clone()));
        }

        let mut ts = TreeStore {
            root: root.to_path_buf(),
            stores,
            agg: Vec::new(),
            map: Vec::new(),
            undo_order: Vec::new(),
            today,
        };
        ts.rebuild();
        Ok(ts)
    }

    /// Single-file mode: wrap exactly one `todo.md` (the degenerate N=1 tree).
    /// Archive/inbox behave exactly as the single-file `Store` did.
    pub fn open_file(file: PathBuf, body: String, today: String) -> Self {
        let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let store = Store::new(file, body, today.clone());
        let mut ts = TreeStore {
            root,
            stores: vec![store],
            agg: Vec::new(),
            map: Vec::new(),
            undo_order: Vec::new(),
            today,
        };
        ts.rebuild();
        ts
    }

    /// Single-file mode with an explicit `done.md` path (e.g. a `DONE_FILE`
    /// env var that isn't a sibling of the todo file).
    pub fn open_file_with_done(
        file: PathBuf,
        done_path: PathBuf,
        body: String,
        today: String,
    ) -> Self {
        let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let store = Store::new_with_done(file, done_path, body, today.clone());
        let mut ts = TreeStore {
            root,
            stores: vec![store],
            agg: Vec::new(),
            map: Vec::new(),
            undo_order: Vec::new(),
            today,
        };
        ts.rebuild();
        ts
    }

    /// Whether this is a single-file tree (N=1) — used by the UI to keep
    /// archive/inbox behaviour identical to the old single-file mode.
    pub fn is_single_file(&self) -> bool {
        self.stores.len() == 1
    }

    /// Test-only: seed the primary store's archive directly (the archive-view
    /// tests inject completed tasks without going through disk).
    #[cfg(test)]
    pub(crate) fn set_primary_archive(&mut self, archive: Archive) {
        let si = self.root_store();
        self.stores[si].archive = archive;
    }

    fn rebuild(&mut self) {
        self.agg.clear();
        self.map.clear();
        for (si, store) in self.stores.iter().enumerate() {
            for (li, task) in store.tasks().iter().enumerate() {
                self.agg.push(task.clone());
                self.map.push((si, li));
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn tasks(&self) -> &[Task] {
        &self.agg
    }

    /// Source file of the aggregated task at `abs`.
    pub fn source(&self, abs: usize) -> Option<&Path> {
        self.map
            .get(abs)
            .map(|&(si, _)| self.stores[si].file_path())
    }

    /// Directory of the source file relative to the scan root (the task's
    /// "area"); empty for a `todo.md` at the root.
    pub fn area(&self, abs: usize) -> PathBuf {
        match self.source(abs).and_then(|f| f.parent()) {
            Some(dir) => dir.strip_prefix(&self.root).unwrap_or(dir).to_path_buf(),
            None => PathBuf::new(),
        }
    }

    pub fn today(&self) -> &str {
        &self.today
    }

    pub fn set_today(&mut self, today: String) -> bool {
        let mut changed = false;
        for store in &mut self.stores {
            changed |= store.set_today(today.clone());
        }
        if changed {
            self.today = today;
        }
        changed
    }

    pub fn has_completed(&self) -> bool {
        self.agg.iter().any(|t| t.done)
    }

    pub fn task_raw(&self, abs: usize) -> Option<String> {
        self.agg.get(abs).map(|t| t.raw.clone())
    }

    // -- single-index mutations: route to the owning store, then rebuild. --

    pub fn toggle_complete(&mut self, abs: usize) -> CompleteOutcome {
        let Some((si, li)) = self.locate(abs) else {
            return CompleteOutcome::OutOfRange;
        };
        let out = self.stores[si].toggle_complete(li);
        self.note_mutation(si, matches!(out, CompleteOutcome::Aborted(_)));
        self.rebuild();
        out
    }

    pub fn cycle_priority(&mut self, abs: usize) -> PriorityOutcome {
        let Some((si, li)) = self.locate(abs) else {
            return PriorityOutcome::OutOfRange;
        };
        let out = self.stores[si].cycle_priority(li);
        self.note_mutation(si, matches!(out, PriorityOutcome::Aborted(_)));
        self.rebuild();
        out
    }

    pub fn delete(&mut self, abs: usize) -> DeleteOutcome {
        let Some((si, li)) = self.locate(abs) else {
            return DeleteOutcome::OutOfRange;
        };
        let out = self.stores[si].delete(li);
        self.note_mutation(si, matches!(out, DeleteOutcome::Aborted(_)));
        self.rebuild();
        out
    }

    pub fn edit_line(&mut self, abs: usize, text: &str) -> EditOutcome {
        let Some((si, li)) = self.locate(abs) else {
            return EditOutcome::OutOfRange;
        };
        let out = self.stores[si].edit_line(li, text);
        self.note_mutation(si, matches!(out, EditOutcome::Aborted(_)));
        self.rebuild();
        out
    }

    pub fn add_project(&mut self, abs: usize, name: &str) -> TagOutcome {
        let Some((si, li)) = self.locate(abs) else {
            return TagOutcome::OutOfRange;
        };
        let out = self.stores[si].add_project(li, name);
        self.note_mutation(si, matches!(out, TagOutcome::Aborted(_)));
        self.rebuild();
        out
    }

    pub fn toggle_context(&mut self, abs: usize, name: &str) -> TagOutcome {
        let Some((si, li)) = self.locate(abs) else {
            return TagOutcome::OutOfRange;
        };
        let out = self.stores[si].toggle_context(li, name);
        self.note_mutation(si, matches!(out, TagOutcome::Aborted(_)));
        self.rebuild();
        out
    }

    /// Add a new task to the root `todo.md`.
    pub fn add_finalized(&mut self, text: &str) -> AddOutcome {
        let si = self.root_store();
        let out = self.stores[si].add_finalized(text);
        self.undo_order.push(si);
        self.rebuild();
        out
    }

    // -- archive / inbox --
    //
    // The archive *view* and inbox capture are single-file concerns. For N=1
    // they delegate to the one store (identical to the old behaviour). For a
    // multi-file tree, `archive_completed` fans out across every file; the
    // archive-browsing view and inbox are gated to single-file mode by the UI.

    /// The archive of the primary (root) file, for the archive-browsing view.
    pub fn archive(&self) -> &Archive {
        self.stores[self.root_store()].archive()
    }

    /// Pump the primary file's archive loader / external-change poll.
    pub fn poll_archive(&mut self) -> bool {
        let si = self.root_store();
        self.stores[si].poll_archive()
    }

    /// Move completed tasks to `done.md`. For a tree, every file archives into
    /// its own sibling `done.md`; counts are summed.
    pub fn archive_completed(&mut self) -> ArchiveOutcome {
        let mut total = 0;
        for store in &mut self.stores {
            match store.archive_completed() {
                ArchiveOutcome::Archived { count } => total += count,
                ArchiveOutcome::Nothing => {}
                ArchiveOutcome::Aborted(r) => {
                    self.rebuild();
                    return ArchiveOutcome::Aborted(r);
                }
                ArchiveOutcome::Error(e) => {
                    self.rebuild();
                    return ArchiveOutcome::Error(e);
                }
            }
        }
        self.rebuild();
        if total == 0 {
            ArchiveOutcome::Nothing
        } else {
            ArchiveOutcome::Archived { count: total }
        }
    }

    /// Restore an archived task (primary file's archive only).
    pub fn unarchive(&mut self, archive_idx: usize) -> UnarchiveOutcome {
        let si = self.root_store();
        let out = self.stores[si].unarchive(archive_idx);
        self.rebuild();
        out
    }

    /// Permanently delete an archived task (primary file's archive only).
    pub fn archive_delete(&mut self, archive_idx: usize) -> ArchiveDeleteOutcome {
        let si = self.root_store();
        self.stores[si].archive_delete(archive_idx)
    }

    /// Drain the primary file's sibling `inbox.md`.
    pub fn drain_inbox(&mut self) -> DrainReport {
        let si = self.root_store();
        let report = self.stores[si].drain_inbox();
        self.rebuild();
        report
    }

    // -- bulk mutations: group globals by store, fan out, merge. --

    pub fn complete_many(&mut self, indices: &[usize]) -> BulkCompleteOutcome {
        let grouped = self.group_by_store(indices);
        let mut completed = 0;
        let mut spawned = 0;
        let mut aborted = None;
        for (si, locals) in grouped {
            match self.stores[si].complete_many(&locals) {
                BulkCompleteOutcome::Done {
                    completed: c,
                    spawned: s,
                } => {
                    completed += c;
                    spawned += s;
                    self.undo_order.push(si);
                }
                BulkCompleteOutcome::NothingToComplete => {}
                BulkCompleteOutcome::Aborted(r) => aborted = Some(r),
                BulkCompleteOutcome::Error(e) => {
                    self.rebuild();
                    return BulkCompleteOutcome::Error(e);
                }
            }
        }
        self.rebuild();
        if let Some(r) = aborted {
            return BulkCompleteOutcome::Aborted(r);
        }
        if completed == 0 {
            BulkCompleteOutcome::NothingToComplete
        } else {
            BulkCompleteOutcome::Done { completed, spawned }
        }
    }

    pub fn delete_many(&mut self, indices: &[usize]) -> BulkDeleteOutcome {
        let grouped = self.group_by_store(indices);
        let mut deleted = 0;
        let mut aborted = None;
        for (si, locals) in grouped {
            match self.stores[si].delete_many(&locals) {
                BulkDeleteOutcome::Done { deleted: d } => {
                    deleted += d;
                    self.undo_order.push(si);
                }
                BulkDeleteOutcome::Nothing => {}
                BulkDeleteOutcome::Aborted(r) => aborted = Some(r),
                BulkDeleteOutcome::Error(e) => {
                    self.rebuild();
                    return BulkDeleteOutcome::Error(e);
                }
            }
        }
        self.rebuild();
        if let Some(r) = aborted {
            return BulkDeleteOutcome::Aborted(r);
        }
        if deleted == 0 {
            BulkDeleteOutcome::Nothing
        } else {
            BulkDeleteOutcome::Done { deleted }
        }
    }

    /// Undo the most recent mutation, on whichever file it touched.
    pub fn undo(&mut self) -> UndoOutcome {
        let Some(si) = self.undo_order.pop() else {
            return UndoOutcome::Nothing;
        };
        let out = self.stores[si].undo();
        self.rebuild();
        out
    }

    /// Reconcile every file against disk. Returns `Reloaded` if any file
    /// changed externally (and the flat view was rebuilt).
    pub fn reconcile(&mut self) -> Reconcile {
        let mut result = Reconcile::Unchanged;
        for store in &mut self.stores {
            match store.reconcile() {
                Reconcile::Unchanged => {}
                Reconcile::Reloaded => result = Reconcile::Reloaded,
                Reconcile::ReadError => {
                    if matches!(result, Reconcile::Unchanged) {
                        result = Reconcile::ReadError;
                    }
                }
            }
        }
        if !matches!(result, Reconcile::Unchanged) {
            self.undo_order.clear();
            self.rebuild();
        }
        result
    }

    fn locate(&mut self, abs: usize) -> Option<(usize, usize)> {
        self.map.get(abs).copied()
    }

    /// Record a mutation against store `si` for undo, unless it was aborted.
    fn note_mutation(&mut self, si: usize, aborted: bool) {
        if !aborted {
            self.undo_order.push(si);
        }
    }

    fn group_by_store(&self, indices: &[usize]) -> BTreeMap<usize, Vec<usize>> {
        let mut grouped: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for &g in indices {
            if let Some(&(si, li)) = self.map.get(g) {
                grouped.entry(si).or_default().push(li);
            }
        }
        grouped
    }

    /// Index of the store backing the root `todo.md` (guaranteed to exist).
    fn root_store(&self) -> usize {
        let root_todo = self.root.join(TODO_FILENAME);
        self.stores
            .iter()
            .position(|s| s.file_path() == root_todo)
            .unwrap_or(0)
    }
}

fn collect_todo_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_todo_files(&path, out)?;
        } else if file_type.is_file() && name == TODO_FILENAME {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tuxemdo-tree-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("todo.md"), "- [ ] (A) root task +root\n");
        write(
            &root.join("projects/alpha/todo.md"),
            "- [ ] ship alpha @work +alpha\n- [ ] write spec +alpha\n",
        );
        write(&root.join("node_modules/dep/todo.md"), "- [ ] skip me\n");
        root
    }

    #[test]
    fn aggregates_across_files_skipping_noise() {
        let root = fixture("agg");
        let ts = TreeStore::open(&root, "2026-06-23".into()).unwrap();
        assert_eq!(ts.tasks().len(), 3);
        assert!(ts.tasks().iter().all(|t| !t.raw.contains("skip me")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn complete_routes_to_owning_file_and_persists() {
        let root = fixture("complete");
        let mut ts = TreeStore::open(&root, "2026-06-23".into()).unwrap();
        // Find the "ship alpha" task and complete it.
        let abs = ts
            .tasks()
            .iter()
            .position(|t| t.raw.contains("ship alpha"))
            .unwrap();
        let source = ts.source(abs).unwrap().to_path_buf();
        assert!(matches!(
            ts.toggle_complete(abs),
            CompleteOutcome::Completed { .. }
        ));
        // The change landed in the alpha file, not the root file.
        let on_disk = std::fs::read_to_string(&source).unwrap();
        assert!(on_disk.contains("- [x]") && on_disk.contains("ship alpha"));
        let root_body = std::fs::read_to_string(root.join("todo.md")).unwrap();
        assert!(!root_body.contains("- [x]"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn add_goes_to_root_and_undo_reverts_it() {
        let root = fixture("add");
        let mut ts = TreeStore::open(&root, "2026-06-23".into()).unwrap();
        let before = ts.tasks().len();
        ts.add_finalized("brand new task +misc");
        assert_eq!(ts.tasks().len(), before + 1);
        let root_body = std::fs::read_to_string(root.join("todo.md")).unwrap();
        assert!(root_body.contains("brand new task"));
        // Undo removes it again.
        ts.undo();
        assert_eq!(ts.tasks().len(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn area_is_relative_to_root() {
        let root = fixture("area");
        let ts = TreeStore::open(&root, "2026-06-23".into()).unwrap();
        let abs = ts
            .tasks()
            .iter()
            .position(|t| t.raw.contains("ship alpha"))
            .unwrap();
        assert_eq!(ts.area(abs), PathBuf::from("projects/alpha"));
        let root_abs = ts
            .tasks()
            .iter()
            .position(|t| t.raw.contains("root task"))
            .unwrap();
        assert_eq!(ts.area(root_abs), PathBuf::new());
        let _ = std::fs::remove_dir_all(&root);
    }
}
