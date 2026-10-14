//! Which settled rows have already been written into the terminal's
//! scrollback. Scrollback is append-only from this side: a written row cannot
//! be repainted, reordered or removed, so identity — not position — decides
//! what is new (port of `planCommit`).

#[derive(Debug, Default, Clone)]
pub struct CommitState {
    committed: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommitPlan {
    /// Ids to write this frame, in order.
    pub fresh: Vec<String>,
    /// The settled list diverged from what went out (a row arrived late and
    /// sorted in ahead of committed ones). Written rows stay as they are and
    /// only never-written rows go out, at the end.
    pub rewritten: bool,
}

impl CommitState {
    pub fn committed_len(&self) -> usize {
        self.committed.len()
    }

    /// Plans the commit for `settled` ids and records them as written.
    pub fn plan(&mut self, settled: &[String]) -> CommitPlan {
        let committed = &self.committed;
        // Fast path, O(1): the settled list still extends what went out.
        let extends = committed.is_empty()
            || (settled.len() >= committed.len()
                && settled.get(committed.len() - 1) == committed.last());
        let plan = if extends {
            CommitPlan {
                fresh: settled[committed.len().min(settled.len())..].to_vec(),
                rewritten: false,
            }
        } else {
            let seen: std::collections::HashSet<&String> = committed.iter().collect();
            CommitPlan {
                fresh: settled
                    .iter()
                    .filter(|id| !seen.contains(id))
                    .cloned()
                    .collect(),
                rewritten: true,
            }
        };
        self.committed = settled.to_vec();
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extension_commits_only_the_new_tail() {
        let mut state = CommitState::default();
        assert_eq!(state.plan(&ids(&["a", "b"])).fresh, ids(&["a", "b"]));
        let plan = state.plan(&ids(&["a", "b", "c"]));
        assert_eq!(
            plan,
            CommitPlan {
                fresh: ids(&["c"]),
                rewritten: false
            }
        );
        assert!(state.plan(&ids(&["a", "b", "c"])).fresh.is_empty());
    }

    #[test]
    fn a_late_row_is_appended_not_reprinted() {
        let mut state = CommitState::default();
        state.plan(&ids(&["a", "c"]));
        // "b" sorts in between: it goes out once, at the end; a/c do not repeat.
        let plan = state.plan(&ids(&["a", "b", "c"]));
        assert!(plan.rewritten);
        assert_eq!(plan.fresh, ids(&["b"]));
        // And the next extension is back on the fast path.
        let plan = state.plan(&ids(&["a", "b", "c", "d"]));
        assert_eq!(
            plan,
            CommitPlan {
                fresh: ids(&["d"]),
                rewritten: false
            }
        );
    }
}
