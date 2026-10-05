//! PR stacks on the file tabs: the navigator's stack list, the `stack` scope's range, the
//! `All files` tab's stack PR tree, and the opt-in stack fetch's bookkeeping.
//!
//! A range or a tree is place state: only the reader's pick, refresh, or way back moves it.
//! Both are read-only, since neither is the checked-out work the comments belong to.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::Result;

use super::{App, Focus, Tab, step};
use crate::forge;
use crate::model::Scope;
use crate::stack::{EndSource, EndSpec, FetchJob, FetchOutcome, StackEnd, StackRange, TreeSource};

/// One stack list row: a stack PR (top of the stack first) or, last, the stack's base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackListRow {
    pub end: StackEnd,
    /// `#12`, or the base's branch.
    pub label: String,
    pub title: String,
    pub state: Option<(forge::PrState, bool)>,
    /// The checked-out branch's PR: always the worktree.
    pub checked_out: bool,
    /// The shown range's compared PR, or the tree the `All files` tab browses.
    pub head: bool,
    /// What the shown range compares against.
    pub against: bool,
    /// The same-named local branch where it differs from the PR head (never the checked-out
    /// row), with whether the reader chose it for this row.
    pub local: Option<(crate::stack::LocalBranch, bool)>,
    /// A PR merely based on a member of GitHub's own stack, not in it: that stack's number.
    pub outside: Option<u64>,
}

/// The stack fetch's bookkeeping: the one job in flight, the head each PR was last asked for
/// (so a PR is fetched again only once the forge reports a new head), the last failure per
/// PR, and the stack the last prune kept.
#[derive(Debug, Default)]
pub(crate) struct StackFetch {
    tag: u64,
    in_flight: Option<(u64, Vec<u64>)>,
    asked: HashMap<u64, String>,
    errors: HashMap<u64, String>,
    kept: Option<Vec<u64>>,
}

/// Why an unread row does nothing.
fn unread_status(row: &StackListRow) -> String {
    format!(
        "the stack goes on {} — reviewr read no further",
        if matches!(row.end, StackEnd::Unread { below: true }) { "below" } else { "above" }
    )
}

impl App {
    /// Whether a PR stack is known for the checked-out branch: GitHub reads one, the other
    /// forges list none, so the stack features are simply not offered there.
    #[must_use]
    pub fn stack_available(&self) -> bool {
        !self.pr_stack().is_empty()
    }

    /// The stack's base: the trunk the bottom PR targets — only when the read reached it. A
    /// cut stack's bottom PR targets another PR's branch, which is never the base.
    fn stack_base_branch(&self) -> Option<String> {
        self.pr_checked_out_snapshot()?.stack_base().map(str::to_string)
    }

    /// What the stack read proved about the stack's extent.
    fn stack_shape(&self) -> forge::StackShape {
        self.pr_checked_out_snapshot().map(|s| s.stack_shape.clone()).unwrap_or_default()
    }

    /// Why `vs parent` on PR `number` does nothing: its parent is below the part of the
    /// stack read, and its base is a PR's branch, not the stack's base.
    fn unread_parent_reason(&self, number: u64) -> String {
        let base = self
            .pr_stack()
            .iter()
            .find(|e| e.number == number)
            .map(|e| e.base_ref.clone())
            .unwrap_or_default();
        format!(
            "#{number}'s parent is below the part of the stack read — {base} is another PR's \
             branch, not the stack base"
        )
    }

    /// The forge's latest head commit for stack PR `number`: the checked-out PR's own read, or
    /// the stack cache's.
    fn stack_head_oid(&self, number: u64) -> Option<String> {
        if let forge::PrView::Pr(s) = self.pr_checked_out_view()
            && s.number == number
        {
            return Some(s.head_oid.clone()).filter(|o| !o.is_empty());
        }
        match self.stack_cache.entries.get(&number).map(|c| &c.view) {
            Some(forge::PrView::Pr(s)) => Some(s.head_oid.clone()).filter(|o| !o.is_empty()),
            _ => None,
        }
    }

    /// The live spec of `end`, from the checked-out PR's stack. `None` once the PR left it.
    fn end_spec(&self, end: StackEnd) -> Option<EndSpec> {
        match end {
            StackEnd::Base => {
                let branch = self.stack_base_branch()?;
                Some(EndSpec {
                    end,
                    label: branch.clone(),
                    branch,
                    head_oid: None,
                    source: EndSource::Pr,
                })
            }
            StackEnd::Unread { .. } => None,
            StackEnd::Pr(n) => {
                let entry = self.pr_stack().iter().find(|e| e.number == n)?;
                // The checked-out PR is always the worktree; another PR is the PR as its
                // reviewers see it, unless the reader chose its local branch on its row.
                let source = if self.pr_checked_out_number() == Some(n) {
                    EndSource::Worktree
                } else if self.stack_local.contains(&n) {
                    EndSource::Local
                } else {
                    EndSource::Pr
                };
                let label = match source {
                    EndSource::Local => format!("#{n} (local)"),
                    _ => format!("#{n}"),
                };
                Some(EndSpec {
                    end,
                    label,
                    branch: entry.head_ref.clone(),
                    head_oid: self.stack_head_oid(n),
                    source,
                })
            }
        }
    }

    /// The PR `number` stacks on: the stack PR whose head is its base, else the stack's base
    /// — or, in a stack the read cut, the unread part below: a base no listed PR has is then
    /// another PR's branch, never the base.
    pub(crate) fn stack_parent(&self, number: u64) -> StackEnd {
        let stack = self.pr_stack();
        let Some(base) = stack.iter().find(|e| e.number == number).map(|e| &e.base_ref) else {
            return StackEnd::Base;
        };
        let beyond = if self.stack_shape().more_below {
            StackEnd::Unread { below: true }
        } else {
            StackEnd::Base
        };
        stack.iter().find(|e| &e.head_ref == base).map_or(beyond, |e| StackEnd::Pr(e.number))
    }

    /// Whether the read pane shows something that is not the checked-out work: a stack range
    /// on `Changes`, a stack PR tree on `All files`. Nothing is commented or edited there.
    #[must_use]
    pub fn read_only_view(&self) -> bool {
        match self.tab {
            Tab::Changes => self.scope == Scope::Stack,
            Tab::AllFiles => self.files_source.is_some(),
            Tab::Pr => false,
        }
    }

    /// Why the comment and edit keys do nothing here, for the status line.
    #[must_use]
    pub fn read_only_reason(&self) -> Option<String> {
        if !self.read_only_view() {
            return None;
        }
        let back = self.keymap().hint(crate::keymap::Action::CheckedOutPr).label();
        let what = match self.tab {
            Tab::AllFiles => self.files_source.as_ref().map(|t| format!("{}'s tree", t.spec.label)),
            _ => self.stack_range.as_ref().map(StackRange::label),
        }
        .unwrap_or_default();
        Some(format!("read-only: {what} isn't the checked-out work — {back} goes back to it"))
    }

    /// The header's name and tail for a stack range (`#14 vs #12`, ` · read-only`) or a
    /// browsed tree (`#12 tree`), with a moved end's hint.
    #[must_use]
    pub fn stack_header(&self) -> Option<(String, String)> {
        let refresh = self.keymap().hint(crate::keymap::Action::Refresh).label();
        match self.tab {
            Tab::Changes if self.scope == Scope::Stack => {
                let range = self.stack_range.as_ref()?;
                let mut tail = " · read-only".to_string();
                let moved = self.stack_moved();
                if !moved.is_empty() {
                    let _ = write!(tail, " · {} moved — {refresh} follows", moved.join(", "));
                }
                for (spec, at) in [(&range.to, &range.to_at), (&range.from, &range.from_at)] {
                    if at.as_ref().is_some_and(|a| a.local_gone) {
                        let _ = write!(
                            tail,
                            " · {}'s local branch is gone — PR head",
                            spec.as_pr().label
                        );
                    }
                }
                Some((range.label(), tail))
            }
            Tab::AllFiles => {
                let tree = self.files_source.as_ref()?;
                let at =
                    tree.at.as_ref().map(|a| format!(" {}", crate::git::abbreviate_oid(&a.oid)));
                let mut tail = " · read-only".to_string();
                if self.tree_moved || self.end_moved_on_forge(&tree.spec, tree.at.as_ref()) {
                    let _ = write!(tail, " · {} moved — {refresh} follows", tree.spec.label);
                }
                if tree.at.as_ref().is_some_and(|a| a.local_gone) {
                    let _ = write!(
                        tail,
                        " · {}'s local branch is gone — PR head",
                        tree.spec.as_pr().label
                    );
                }
                Some((format!("{} tree{}", tree.spec.label, at.unwrap_or_default()), tail))
            }
            _ => None,
        }
    }

    /// Whether the forge reports a head for `spec` other than the one it had at the pick: the
    /// stack cache read a push. A range never follows it on its own (Continuity).
    fn end_moved_on_forge(&self, spec: &EndSpec, at: Option<&crate::stack::Resolved>) -> bool {
        let StackEnd::Pr(n) = spec.end else { return false };
        if spec.source != EndSource::Pr {
            return false;
        }
        let Some(live) = self.stack_head_oid(n) else { return false };
        at.is_some() && spec.head_oid.as_ref().is_some_and(|picked| *picked != live)
    }

    /// The labels of the open range's ends that moved since the pick: their refs (the latest
    /// build's finding), or their PR's head on the forge.
    #[must_use]
    pub fn stack_moved(&self) -> Vec<String> {
        let Some(range) = &self.stack_range else { return Vec::new() };
        let mut moved = self.stack_status.as_ref().map(|s| s.moved.clone()).unwrap_or_default();
        for (spec, at) in [(&range.to, &range.to_at), (&range.from, &range.from_at)] {
            if !moved.contains(&spec.label) && self.end_moved_on_forge(spec, at.as_ref()) {
                moved.push(spec.label.clone());
            }
        }
        moved
    }

    /// The not-fetched message for `spec`: fetching, the fetch's failure, or the command that
    /// brings it in by hand.
    fn not_fetched(&self, spec: &EndSpec) -> String {
        if let StackEnd::Pr(n) = spec.end
            && self.stack_fetch_enabled()
        {
            if let Some(error) = self.stack_fetch.errors.get(&n) {
                return format!("{}'s branch isn't fetched — fetch failed: {error}", spec.label);
            }
            if self.stack_fetch.in_flight.as_ref().is_some_and(|(_, ns)| ns.contains(&n)) {
                return format!("{}'s branch isn't fetched yet — fetching…", spec.label);
            }
        }
        let what = match spec.end {
            StackEnd::Pr(_) => format!("{}'s branch", spec.label),
            StackEnd::Base => format!("the stack base {}", spec.label),
            StackEnd::Unread { .. } => "the unread part of the stack".to_string(),
        };
        format!("{what} isn't fetched — `{}`", spec.fetch_hint())
    }

    /// The empty state both panes paint for a stack range or tree with nothing to show: an end
    /// that is not fetched, or a range that adds nothing over the other end.
    #[must_use]
    pub fn stack_message(&self) -> Option<String> {
        match self.tab {
            Tab::Changes if self.scope == Scope::Stack => {
                let range = self.stack_range.as_ref()?;
                if let Some(spec) = range.missing() {
                    return Some(self.not_fetched(spec));
                }
                Some(format!("{} adds nothing over {}", range.to.label, range.from.label))
            }
            Tab::AllFiles => {
                let tree = self.files_source.as_ref()?;
                if tree.at.is_none() {
                    return Some(self.not_fetched(&tree.spec));
                }
                None
            }
            _ => None,
        }
    }

    // --- the stack list -------------------------------------------------------------------

    /// Whether the navigator shows the stack list: a file tab with a known stack.
    #[must_use]
    pub fn stack_list_shown(&self) -> bool {
        self.tab.is_file_tab() && self.stack_available()
    }

    /// The stack PRs whose local branches the next build badges: every one but the
    /// checked-out PR, as the PR, while the list shows.
    pub(super) fn stack_badge_specs(&self) -> Vec<EndSpec> {
        if !self.stack_list_shown() {
            return Vec::new();
        }
        let checked_out = self.pr_checked_out_number();
        self.pr_stack()
            .iter()
            .filter(|e| Some(e.number) != checked_out)
            .filter_map(|e| self.end_spec(StackEnd::Pr(e.number)))
            .map(|s| s.as_pr())
            .collect()
    }

    /// The stack list's rows: each stack PR top first, then the stack's base, with the roles
    /// the shown range or tree gives them.
    #[must_use]
    pub fn stack_list_rows(&self) -> Vec<StackListRow> {
        let checked_out = self.pr_checked_out_number();
        let (head, against) = match self.tab {
            Tab::Changes if self.scope == Scope::Stack => {
                let r = self.stack_range.as_ref();
                (r.map(|r| r.to.end), r.map(|r| r.from.end))
            }
            Tab::AllFiles => (self.files_source.as_ref().map(|t| t.spec.end), None),
            _ => (None, None),
        };
        let shape = self.stack_shape();
        let unread = |below: bool| StackListRow {
            end: StackEnd::Unread { below },
            label: "…".to_string(),
            title: if below { "more below — not read" } else { "more above — not read" }
                .to_string(),
            state: None,
            checked_out: false,
            head: false,
            against: false,
            local: None,
            outside: None,
        };
        let mut rows: Vec<StackListRow> =
            shape.more_above.then(|| unread(false)).into_iter().collect();
        rows.extend(self.pr_stack().iter().rev().map(|e| {
            let end = StackEnd::Pr(e.number);
            let is_checked_out = checked_out == Some(e.number);
            StackListRow {
                end,
                label: format!("#{}", e.number),
                title: e.title.clone(),
                state: Some((e.state, e.is_draft)),
                checked_out: is_checked_out,
                head: head == Some(end),
                against: against == Some(end),
                local: (!is_checked_out)
                    .then(|| self.stack_locals.get(&e.number))
                    .flatten()
                    .map(|l| (l.clone(), self.stack_local.contains(&e.number))),
                outside: shape
                    .native
                    .as_ref()
                    .filter(|s| !s.members.contains(&e.number))
                    .map(|s| s.number),
            }
        }));
        // A cut stack ends on what was not read, never on a PR's branch posing as the base.
        if shape.more_below {
            rows.push(unread(true));
        } else if let Some(base) = self.stack_base_branch().filter(|b| !b.is_empty()) {
            rows.push(StackListRow {
                end: StackEnd::Base,
                label: base,
                title: "stack base".to_string(),
                state: None,
                checked_out: false,
                head: head == Some(StackEnd::Base),
                against: against == Some(StackEnd::Base),
                local: None,
                outside: None,
            });
        }
        rows
    }

    /// The stack list's highlighted row index: its end by identity, else the nearest
    /// surviving row, clamped — so a stack refresh never moves it by index.
    #[must_use]
    pub fn stack_list_cursor(&self) -> usize {
        let rows = self.stack_list_rows();
        self.stack_cursor
            .0
            .and_then(|end| rows.iter().position(|r| r.end == end))
            .unwrap_or(self.stack_cursor.1)
            .min(rows.len().saturating_sub(1))
    }

    fn set_stack_cursor(&mut self, i: usize) {
        let rows = self.stack_list_rows();
        let i = i.min(rows.len().saturating_sub(1));
        self.stack_cursor = (rows.get(i).map(|r| r.end), i);
        self.reveal_stack.set(true);
    }

    /// `stack-list`: move the keyboard into the stack list, opening on the shown range's PR
    /// or tree, else the checked-out PR — or back to the file list from it.
    pub fn focus_stack_list(&mut self) {
        if self.focus == Focus::Stack {
            self.focus = Focus::Files;
            return;
        }
        if !self.stack_list_shown() {
            self.status = "no PR stack here — stacks are read from GitHub".to_string();
            return;
        }
        if self.navigator_hidden_here() {
            self.navigator_hidden = false;
        }
        if self.stack_cursor.0.is_none() {
            let rows = self.stack_list_rows();
            let at = rows
                .iter()
                .position(|r| r.head)
                .or_else(|| rows.iter().position(|r| r.checked_out))
                .unwrap_or(0);
            self.set_stack_cursor(at);
        }
        self.focus = Focus::Stack;
        self.reveal_stack.set(true);
    }

    pub fn stack_list_move(&mut self, delta: isize) {
        let len = self.stack_list_rows().len();
        if len > 0 {
            self.set_stack_cursor(step(self.stack_list_cursor(), delta, len));
        }
    }

    /// A click on row `i`: highlight and focus it, then activate it — or, with `against`
    /// (a modifier-click), compare the shown PR against it.
    pub fn stack_list_click(&mut self, i: usize, against: bool) -> Result<()> {
        if i >= self.stack_list_rows().len() {
            return Ok(());
        }
        self.focus = Focus::Stack;
        self.set_stack_cursor(i);
        if against { self.stack_list_against() } else { self.stack_list_activate() }
    }

    /// A click on row `i`'s local badge: highlight it and toggle its local branch.
    pub fn stack_list_click_local(&mut self, i: usize) -> Result<()> {
        self.focus = Focus::Stack;
        self.set_stack_cursor(i);
        self.stack_list_toggle_local()
    }

    /// `enter` on a row. `Changes`: a PR shows against its parent; the checked-out PR's row
    /// goes back to the scope the range was entered from. `All files`: a row browses that
    /// end's tree; the checked-out row goes back to the worktree.
    pub fn stack_list_activate(&mut self) -> Result<()> {
        let rows = self.stack_list_rows();
        let Some(row) = rows.get(self.stack_list_cursor()).cloned() else { return Ok(()) };
        match self.tab {
            Tab::AllFiles => self.set_files_source((!row.checked_out).then_some(row.end)),
            _ if row.checked_out => self.back_to_checked_out(),
            _ => match row.end {
                StackEnd::Pr(n) => match self.stack_parent(n) {
                    StackEnd::Unread { .. } => {
                        self.status = self.unread_parent_reason(n);
                        Ok(())
                    }
                    parent => self.pick_stack_range(row.end, parent),
                },
                StackEnd::Unread { .. } => {
                    self.status = unread_status(&row);
                    Ok(())
                }
                StackEnd::Base => {
                    let key = self.keymap().hint(crate::keymap::Action::StackAgainst).label();
                    self.status = format!("the base is only compared against — {key} sets it");
                    Ok(())
                }
            },
        }
    }

    /// `stack-against` on a row: compare the shown range's PR — or, with none shown, the
    /// checked-out PR — against this row.
    pub fn stack_list_against(&mut self) -> Result<()> {
        if self.tab != Tab::Changes {
            self.status = "against sets a stack range's other end, on the Changes tab".into();
            return Ok(());
        }
        let rows = self.stack_list_rows();
        let Some(row) = rows.get(self.stack_list_cursor()).cloned() else { return Ok(()) };
        if matches!(row.end, StackEnd::Unread { .. }) {
            self.status = unread_status(&row);
            return Ok(());
        }
        let head = match (&self.stack_range, self.scope) {
            (Some(r), Scope::Stack) => Some(r.to.end),
            _ => self.pr_checked_out_number().map(StackEnd::Pr),
        };
        let Some(head) = head else { return Ok(()) };
        if head == row.end {
            self.status =
                format!("{} is the PR shown — pick another row to compare against", row.label);
            return Ok(());
        }
        self.pick_stack_range(head, row.end)
    }

    /// `stack-local` on a row: use the PR's same-named local branch instead of the PR head,
    /// or back. Place state by PR number. A shown range or tree with that PR re-resolves at
    /// once — it is the reader's own choice.
    pub fn stack_list_toggle_local(&mut self) -> Result<()> {
        let rows = self.stack_list_rows();
        let Some(row) = rows.get(self.stack_list_cursor()).cloned() else { return Ok(()) };
        let StackEnd::Pr(n) = row.end else { return Ok(()) };
        if row.checked_out {
            self.status = format!("{} is the worktree — it has no other local branch", row.label);
            return Ok(());
        }
        if !self.stack_local.remove(&n) {
            if row.local.is_none() {
                self.status = format!("{} has no local branch that differs from the PR", row.label);
                return Ok(());
            }
            self.stack_local.insert(n);
        }
        self.reresolve_end(row.end)
    }

    /// Re-resolve the shown range's or tree's `end` from its live spec, after the reader
    /// changed which commit it stands for.
    fn reresolve_end(&mut self, end: StackEnd) -> Result<()> {
        if let Some(range) = self.stack_range.clone()
            && (range.to.end == end || range.from.end == end)
        {
            let to = self.end_spec(range.to.end).unwrap_or(range.to);
            let from = self.end_spec(range.from.end).unwrap_or(range.from);
            self.stack_range = Some(StackRange::resolve(&self.repo, to, from));
            self.stack_status = None;
            self.cache = crate::diff::DiffCache::new();
            if self.scope == Scope::Stack {
                self.reload()?;
            }
        }
        if let Some(tree) = self.files_source.clone()
            && tree.spec.end == end
        {
            let spec = self.end_spec(end).unwrap_or(tree.spec);
            self.files_source = Some(TreeSource::resolve(&self.repo, spec));
            self.tree_moved = false;
            if self.tab == Tab::AllFiles {
                self.reload()?;
            }
        }
        Ok(())
    }

    /// The stack list's top row as last painted, and the renderer's write-back.
    #[must_use]
    pub fn stack_list_scroll(&self) -> usize {
        self.stack_scroll.get()
    }

    pub fn set_stack_list_scroll(&self, scroll: usize) {
        self.stack_scroll.set(scroll);
    }

    /// Consume a pending reveal of the highlighted row.
    pub fn take_stack_reveal(&self) -> bool {
        self.reveal_stack.replace(false)
    }

    /// The wheel over the stack list: move the viewport alone.
    pub fn scroll_stack_list(&mut self, delta: isize) {
        let next = self.stack_scroll.get().saturating_add_signed(delta);
        self.stack_scroll.set(next);
    }

    /// Show `to` against `from` in the `stack` scope: both ends resolve now and freeze, the
    /// scope switches, and the Changes tab rebuilds before the frame at its top.
    pub fn pick_stack_range(&mut self, to: StackEnd, from: StackEnd) -> Result<()> {
        let (Some(to), Some(from)) = (self.end_spec(to), self.end_spec(from)) else {
            return Ok(());
        };
        self.stack_range = Some(StackRange::resolve(&self.repo, to, from));
        self.stack_status = None;
        if self.scope != Scope::Stack {
            self.stack_return = Some(self.scope);
        }
        self.scope = Scope::Stack;
        self.rebase_changes()?;
        self.reveal_files = true;
        Ok(())
    }

    /// Switch the `All files` source: a stack PR's tree, or back to the worktree (`None`).
    /// The tree reconciles by path like any refresh, so the cursor stays on the same file.
    pub fn set_files_source(&mut self, end: Option<StackEnd>) -> Result<()> {
        let source = end.and_then(|e| self.end_spec(e)).map(|s| TreeSource::resolve(&self.repo, s));
        if source == self.files_source && end.is_some() == self.files_source.is_some() {
            return Ok(());
        }
        self.files_source = source;
        self.tree_moved = false;
        self.reload()?;
        self.reveal_files = true;
        Ok(())
    }

    /// `checked-out-pr` on a file tab: back from a stack range to the scope it was entered
    /// from, or from a stack PR tree to the worktree.
    pub fn back_to_checked_out(&mut self) -> Result<()> {
        if self.composing() {
            return Ok(());
        }
        match self.tab {
            Tab::Changes if self.scope == Scope::Stack => {
                let scope = self.stack_return.take().unwrap_or(Scope::Uncommitted);
                self.set_scope(scope)
            }
            Tab::AllFiles if self.files_source.is_some() => self.set_files_source(None),
            _ => Ok(()),
        }
    }

    /// The reader's refresh: a stack range or tree follows its ends by identity — each PR by
    /// number, re-resolved from the live stack — and the view reconciles by path. A failed
    /// fetch is tried again. Nothing else ever re-resolves a shown end.
    pub fn follow_stack(&mut self) -> Result<()> {
        // A failed fetch is asked again; the worker skips every head already in the store.
        self.stack_fetch.errors.clear();
        self.stack_fetch.asked.clear();
        match self.tab {
            Tab::Changes if self.scope == Scope::Stack => {
                let Some(range) = self.stack_range.clone() else { return Ok(()) };
                let to = self.end_spec(range.to.end).unwrap_or(range.to);
                let from = self.end_spec(range.from.end).unwrap_or(range.from);
                let fresh = StackRange::resolve(&self.repo, to, from);
                if Some(&fresh) != self.stack_range.as_ref() {
                    self.stack_range = Some(fresh);
                    self.stack_status = None;
                    self.cache = crate::diff::DiffCache::new();
                    self.reload()?;
                }
            }
            Tab::AllFiles => {
                let Some(tree) = self.files_source.clone() else { return Ok(()) };
                let spec = self.end_spec(tree.spec.end).unwrap_or(tree.spec);
                let fresh = TreeSource::resolve(&self.repo, spec);
                if Some(&fresh) != self.files_source.as_ref() {
                    self.files_source = Some(fresh);
                    self.tree_moved = false;
                    self.reload()?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve the ends that were not fetched when picked, now that a fetch or a stack read
    /// landed. An end with nothing shown has nothing to yank, so filling it in is newer
    /// content for the same range, never a move (Continuity). Returns whether one filled.
    fn fill_missing_stack_ends(&mut self) -> bool {
        let mut filled = false;
        if let Some(mut range) = self.stack_range.take() {
            for (spec, at) in
                [(&mut range.to, &mut range.to_at), (&mut range.from, &mut range.from_at)]
            {
                if at.is_none() {
                    if let Some(live) = self.end_spec(spec.end) {
                        *spec = live;
                    }
                    *at = crate::stack::resolve(&self.repo, spec);
                    filled |= at.is_some();
                }
            }
            self.stack_range = Some(range);
        }
        let mut tree_filled = false;
        if let Some(mut tree) = self.files_source.take() {
            if tree.at.is_none() {
                if let Some(live) = self.end_spec(tree.spec.end) {
                    tree.spec = live;
                }
                tree.at = crate::stack::resolve(&self.repo, &tree.spec);
                tree_filled = tree.at.is_some();
            }
            self.files_source = Some(tree);
        }
        if filled && self.scope == Scope::Stack {
            self.stack_status = None;
            if self.tab == Tab::Changes {
                let _ = self.reload();
            } else {
                self.request_world_refresh(false, false);
            }
        }
        if tree_filled {
            if self.tab == Tab::AllFiles {
                let _ = self.reload();
            } else {
                self.request_world_refresh(false, false);
            }
        }
        filled || tree_filled
    }

    /// After the stack cache landed: a not-fetched end may resolve now.
    pub(crate) fn after_stack_read(&mut self) {
        if self.stack_range.as_ref().is_some_and(|r| r.missing().is_some())
            || self.files_source.as_ref().is_some_and(|t| t.at.is_none())
        {
            self.fill_missing_stack_ends();
        }
    }

    // --- the opt-in fetch (`stack_fetch`) -------------------------------------------------

    fn stack_fetch_enabled(&self) -> bool {
        self.plugin_config().is_some_and(crate::config::PluginConfig::stack_fetch)
    }

    /// The next stack fetch to run, if one is due: with `stack_fetch` on and a stack known,
    /// every stack PR whose forge head was not asked for yet, and a prune whenever the stack's
    /// membership changed. One job in flight at a time; whether a head is already in the
    /// store is the worker's check, so nothing here runs git.
    pub fn take_stack_fetch(&mut self) -> Option<FetchJob> {
        if !self.stack_fetch_enabled()
            || !self.stack_available()
            || self.stack_fetch.in_flight.is_some()
        {
            return None;
        }
        let checked_out = self.pr_checked_out_number();
        let keep: Vec<u64> = self.pr_stack().iter().map(|e| e.number).collect();
        let wanted: Vec<(u64, String)> = keep
            .iter()
            .filter(|&&n| Some(n) != checked_out)
            .filter_map(|&n| self.stack_head_oid(n).map(|oid| (n, oid)))
            .filter(|(n, oid)| self.stack_fetch.asked.get(n) != Some(oid))
            .collect();
        if wanted.is_empty() && self.stack_fetch.kept.as_ref() == Some(&keep) {
            return None;
        }
        for (n, oid) in &wanted {
            self.stack_fetch.asked.insert(*n, oid.clone());
        }
        self.stack_fetch.kept = Some(keep.clone());
        self.stack_fetch.tag = self.stack_fetch.tag.wrapping_add(1);
        let tag = self.stack_fetch.tag;
        self.stack_fetch.in_flight = Some((tag, wanted.iter().map(|(n, _)| *n).collect()));
        Some(FetchJob { tag, keep, wanted })
    }

    /// Land a stack fetch under its tag: record each PR's failure (or clear it), then fill
    /// any end that was waiting on it. A superseded job is dropped. Returns whether the view
    /// changed.
    pub fn land_stack_fetch(&mut self, outcome: &FetchOutcome) -> bool {
        let Some((tag, asked)) = self.stack_fetch.in_flight.take() else { return false };
        if tag != outcome.tag {
            self.stack_fetch.in_flight = Some((tag, asked));
            return false;
        }
        for n in &asked {
            match &outcome.error {
                Some(e) if outcome.fetched.contains(n) => {
                    self.stack_fetch.errors.insert(*n, e.clone());
                }
                _ => {
                    self.stack_fetch.errors.remove(n);
                }
            }
        }
        if let Some(e) = &outcome.error {
            crate::logln!("stack fetch failed: {e}");
        }
        // A failure paints its hint; a success may fill an end.
        self.fill_missing_stack_ends() || outcome.error.is_some()
    }

    /// The `All files` read pane for `path` in commit `oid`'s tree: the too-large notice for
    /// an over-budget blob, read by size alone, else the blob through the shared
    /// content-hash cache — the worktree File view's twin, from the object store.
    pub(super) fn tree_file_view(
        &mut self,
        oid: &str,
        path: &str,
    ) -> (crate::diff::FileDiff, String) {
        let oversize =
            crate::git::blob_size(&self.repo, oid, path).is_some_and(crate::diff::over_byte_budget);
        if oversize {
            return (crate::diff::FileDiff::too_large_notice(path.to_string()), String::new());
        }
        let content = crate::git::file_content(&self.repo, oid, path);
        let diff = self.cache.get_file(path.to_string(), &content, &self.highlighter);
        (diff, content)
    }
}
