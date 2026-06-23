//! Fuzzy "jump to" overlay state: an fzf-style finder that moves the list
//! cursor to a task (matched by its text) or to a folder/area (matched by
//! name). A purely numeric query jumps to that 1-based visible row, so it also
//! covers vim's `<count>G`.

use super::types::Mode;
use crate::search::subseq_match_ci;

/// One jump target built from the current visible list.
#[derive(Debug, Clone)]
pub struct JumpCandidate {
    /// Index into the visible list to move the cursor to.
    pub target: usize,
    /// Shown text — a task's body, or `area/` for a folder row.
    pub label: String,
    /// Source area (directory), shown dimmed and folded into the match text;
    /// empty in single-file mode and for folder rows themselves.
    pub area: String,
    pub is_area: bool,
}

/// A candidate that passed the current filter, with the byte offsets in its
/// `label` that matched (for highlighting).
#[derive(Debug, Clone)]
pub struct JumpHit {
    pub cand: usize,
    pub positions: Vec<usize>,
}

#[derive(Debug)]
pub struct JumpState {
    prior: Mode,
    /// Highlighted row in the filtered list.
    pub cursor: usize,
    candidates: Vec<JumpCandidate>,
    hits: Vec<JumpHit>,
}

impl Default for JumpState {
    fn default() -> Self {
        Self {
            prior: Mode::Normal,
            cursor: 0,
            candidates: Vec::new(),
            hits: Vec::new(),
        }
    }
}

impl JumpState {
    pub fn open(&mut self, prior: Mode, candidates: Vec<JumpCandidate>) {
        self.prior = prior;
        self.cursor = 0;
        self.candidates = candidates;
        self.hits = self.compute("");
    }

    pub fn take_prior(&mut self) -> Mode {
        self.prior
    }

    pub fn prior_mode(&self) -> Mode {
        self.prior
    }

    pub fn refresh(&mut self, needle: &str) {
        self.hits = self.compute(needle);
        self.cursor = 0;
    }

    pub fn hits(&self) -> &[JumpHit] {
        &self.hits
    }

    pub fn candidate(&self, cand: usize) -> &JumpCandidate {
        &self.candidates[cand]
    }

    pub fn step(&mut self, dir: i32) {
        if self.hits.is_empty() {
            return;
        }
        let len = self.hits.len() as i32;
        self.cursor = (((self.cursor as i32 + dir) % len + len) % len) as usize;
    }

    /// The visible-list index the current selection points at.
    pub fn current_target(&self) -> Option<usize> {
        self.hits
            .get(self.cursor)
            .map(|h| self.candidates[h.cand].target)
    }

    fn compute(&self, needle: &str) -> Vec<JumpHit> {
        if needle.is_empty() {
            return self
                .candidates
                .iter()
                .enumerate()
                .map(|(cand, _)| JumpHit {
                    cand,
                    positions: Vec::new(),
                })
                .collect();
        }
        // Match against "area label" so folder names are searchable too; score
        // by how tightly the needle packs (smaller span = better), tie-broken by
        // shorter labels.
        let mut scored: Vec<(JumpHit, usize, usize)> = Vec::new();
        for (cand, c) in self.candidates.iter().enumerate() {
            let hay = if c.area.is_empty() {
                c.label.clone()
            } else {
                format!("{} {}", c.area, c.label)
            };
            let Some(pos) = subseq_match_ci(&hay, needle) else {
                continue;
            };
            let span = pos.last().copied().unwrap_or(0) - pos.first().copied().unwrap_or(0);
            // Highlight positions are taken against the label alone (best
            // effort; empty when the match landed in the area prefix).
            let positions = subseq_match_ci(&c.label, needle).unwrap_or_default();
            scored.push((JumpHit { cand, positions }, span, c.label.len()));
        }
        scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
        scored.into_iter().map(|(h, _, _)| h).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(target: usize, label: &str, area: &str) -> JumpCandidate {
        JumpCandidate {
            target,
            label: label.into(),
            area: area.into(),
            is_area: false,
        }
    }

    fn opened(candidates: Vec<JumpCandidate>) -> JumpState {
        let mut s = JumpState::default();
        s.open(Mode::Normal, candidates);
        s
    }

    #[test]
    fn empty_query_keeps_every_candidate_in_order() {
        let s = opened(vec![task(0, "a", ""), task(1, "b", "")]);
        assert_eq!(s.hits().len(), 2);
        assert_eq!(s.current_target(), Some(0));
    }

    #[test]
    fn fuzzy_query_filters_to_the_match() {
        let mut s = opened(vec![
            task(0, "Call dentist", "health"),
            task(2, "Buy milk", "shop"),
        ]);
        s.refresh("milk");
        assert_eq!(s.hits().len(), 1);
        assert_eq!(s.current_target(), Some(2));
    }

    #[test]
    fn query_matches_the_source_area_too() {
        let mut s = opened(vec![
            task(0, "Call dentist", "health"),
            task(2, "Buy milk", "shop"),
        ]);
        s.refresh("health");
        assert_eq!(s.current_target(), Some(0));
    }

    #[test]
    fn step_wraps_and_no_match_is_safe() {
        let mut s = opened(vec![task(0, "a", ""), task(1, "b", "")]);
        s.step(-1);
        assert_eq!(s.current_target(), Some(1)); // wrapped to last
        s.refresh("zzz");
        assert!(s.hits().is_empty());
        assert_eq!(s.current_target(), None);
        s.step(1); // must not panic on empty
    }
}
