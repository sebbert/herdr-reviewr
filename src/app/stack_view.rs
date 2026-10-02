//! PR stacks on the file tabs: the stack picker, the `stack` scope's range, the `All files`
//! tab's stack PR tree, and the opt-in stack fetch's bookkeeping.
//!
//! A range or a tree is place state: only the reader's pick, refresh, or way back moves it.
//! Both are read-only, since neither is the checked-out work the comments belong to.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::Result;

use super::{App, Mode, Tab, step};
use crate::forge;
use crate::model::Scope;
use crate::stack::{EndSpec, FetchJob, FetchOutcome, StackEnd, StackRange, TreeSource};

/// What the open stack picker chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackPickerPurpose {
    /// A range for the `stack` scope: the compared PR first, then what it is compared against.
    Range,
    /// The `All files` tab's source: the worktree or one stack PR's tree.
    Tree,
}

/// One stack picker row. `end` is `None` on the tree picker's worktree row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackRow {
    pub end: Option<StackEnd>,
    /// `#12`, the base's branch, or `worktree`.
    pub label: String,
    pub title: String,
    /// The checked-out branch's PR (or the worktree row).
    pub checked_out: bool,
    /// How the end resolves right now, for the row's trail: the spelling, or `None` when
    /// it is not fetched. Read once at open.
    pub via: Option<String>,
}

/// The stack picker's state while it is open. The rows freeze at open; the highlight and the
/// picked compared side are the reader's own place state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackPicker {
    pub purpose: StackPickerPurpose,
    pub rows: Vec<StackRow>,
    pub cursor: usize,
    /// The range picker's second step: the row picked as the compared PR, waiting for the
    /// other end.
    pub to: Option<usize>,
}

impl StackPicker {
    /// The picker's title: the step it waits on.
    #[must_use]
    pub fn title(&self) -> String {
        match (self.purpose, self.to) {
            (StackPickerPurpose::Tree, _) => "files · browse a stack PR".to_string(),
            (StackPickerPurpose::Range, None) => "stack · compare which PR?".to_string(),
            (StackPickerPurpose::Range, Some(i)) => format!("stack · {} vs …", self.rows[i].label),
        }
    }
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

impl App {
    /// Whether a PR stack is known for the checked-out branch: GitHub reads one, the other
    /// forges list none, so the stack features are simply not offered there.
    #[must_use]
    pub fn stack_available(&self) -> bool {
        !self.pr_stack().is_empty()
    }

    /// The stack's base: the trunk the bottom PR targets.
    fn stack_base_branch(&self) -> Option<String> {
        self.pr_stack().first().map(|e| e.base_ref.clone())
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
                Some(EndSpec { end, label: branch.clone(), branch, head_oid: None })
            }
            StackEnd::Pr(n) => {
                let entry = self.pr_stack().iter().find(|e| e.number == n)?;
                Some(EndSpec {
                    end,
                    label: format!("#{n}"),
                    branch: entry.head_ref.clone(),
                    head_oid: self.stack_head_oid(n),
                })
            }
        }
    }

    /// The PR `number` stacks on: the stack PR whose head is its base, else the stack's base.
    fn stack_parent(&self, number: u64) -> StackEnd {
        let stack = self.pr_stack();
        let Some(base) = stack.iter().find(|e| e.number == number).map(|e| &e.base_ref) else {
            return StackEnd::Base;
        };
        stack
            .iter()
            .find(|e| &e.head_ref == base)
            .map_or(StackEnd::Base, |e| StackEnd::Pr(e.number))
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
                Some((format!("{} tree{}", tree.spec.label, at.unwrap_or_default()), tail))
            }
            _ => None,
        }
    }

    /// Whether the forge reports a head for `spec` other than the one it had at the pick: the
    /// stack cache read a push. A range never follows it on its own (Continuity).
    fn end_moved_on_forge(&self, spec: &EndSpec, at: Option<&crate::stack::Resolved>) -> bool {
        let StackEnd::Pr(n) = spec.end else { return false };
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

    // --- the picker -----------------------------------------------------------------------

    /// Open the stack picker: a range on `Changes`, a tree on `All files`. Inert under any
    /// other overlay; without a known stack it says why.
    pub fn open_stack_picker(&mut self) {
        if !self.tab.is_file_tab() || self.mode != Mode::Normal {
            return;
        }
        if !self.stack_available() {
            self.status = "no PR stack here — stacks are read from GitHub".to_string();
            return;
        }
        let checked_out = self.pr_checked_out_number();
        let pr_row = |app: &App, e: &forge::StackEntry| StackRow {
            end: Some(StackEnd::Pr(e.number)),
            label: format!("#{}", e.number),
            title: e.title.clone(),
            checked_out: checked_out == Some(e.number),
            via: app
                .end_spec(StackEnd::Pr(e.number))
                .and_then(|s| crate::stack::resolve(&app.repo, &s))
                .map(|r| r.via),
        };
        let prs: Vec<StackRow> = self.pr_stack().iter().rev().map(|e| pr_row(self, e)).collect();
        let (purpose, rows, cursor) = if self.tab == Tab::Changes {
            let mut rows = prs;
            if let Some(spec) = self.end_spec(StackEnd::Base) {
                let via = crate::stack::resolve(&self.repo, &spec).map(|r| r.via);
                rows.push(StackRow {
                    end: Some(StackEnd::Base),
                    label: spec.label,
                    title: "stack base".to_string(),
                    checked_out: false,
                    via,
                });
            }
            // On the shown range's compared PR, else the checked-out one.
            let want = match (&self.stack_range, self.scope) {
                (Some(r), Scope::Stack) => Some(r.to.end),
                _ => checked_out.map(StackEnd::Pr),
            };
            let cursor = rows.iter().position(|r| r.end == want).unwrap_or(0);
            (StackPickerPurpose::Range, rows, cursor)
        } else {
            let mut rows = vec![StackRow {
                end: None,
                label: "worktree".to_string(),
                title: "the checked-out work".to_string(),
                checked_out: true,
                via: None,
            }];
            rows.extend(prs);
            let want = self.files_source.as_ref().map(|t| t.spec.end);
            let cursor = rows.iter().position(|r| r.end == want).unwrap_or(0);
            (StackPickerPurpose::Tree, rows, cursor)
        };
        self.stack_picker = Some(StackPicker { purpose, rows, cursor, to: None });
        self.mode = Mode::StackPick;
    }

    pub fn close_stack_picker(&mut self) {
        if self.mode == Mode::StackPick {
            self.mode = Mode::Normal;
        }
        self.stack_picker = None;
    }

    /// `esc`: back from the second step to the first, else close.
    pub fn stack_picker_escape(&mut self) {
        match self.stack_picker.as_mut() {
            Some(sp) if sp.to.is_some() => {
                sp.cursor = sp.to.take().unwrap_or(0);
            }
            _ => self.close_stack_picker(),
        }
    }

    pub fn stack_picker_move(&mut self, delta: isize) {
        if let Some(sp) = self.stack_picker.as_mut() {
            sp.cursor = step(sp.cursor, delta, sp.rows.len());
        }
    }

    /// Move the highlight to `row`, for a click. A row past the end is inert.
    pub fn stack_picker_goto(&mut self, row: usize) {
        if let Some(sp) = self.stack_picker.as_mut()
            && row < sp.rows.len()
        {
            sp.cursor = row;
        }
    }

    /// `enter`: on the tree picker, browse the highlight; on the range picker, take the
    /// highlight as the compared PR, then as the end it is compared against.
    pub fn stack_picker_pick(&mut self) -> Result<()> {
        let Some(sp) = self.stack_picker.as_mut() else { return Ok(()) };
        let Some(row) = sp.rows.get(sp.cursor).cloned() else { return Ok(()) };
        match (sp.purpose, sp.to) {
            (StackPickerPurpose::Tree, _) => {
                self.close_stack_picker();
                self.set_files_source(row.end)
            }
            (StackPickerPurpose::Range, None) => {
                let Some(StackEnd::Pr(n)) = row.end else {
                    self.status =
                        "pick a PR to compare first; the base is only compared against".into();
                    return Ok(());
                };
                sp.to = Some(sp.cursor);
                // The second step opens on the PR's parent: `enter enter` is this PR vs its parent.
                let parent = self.stack_parent(n);
                if let Some(sp) = self.stack_picker.as_mut() {
                    sp.cursor =
                        sp.rows.iter().position(|r| r.end == Some(parent)).unwrap_or(sp.cursor);
                }
                Ok(())
            }
            (StackPickerPurpose::Range, Some(to)) => {
                let to = sp.rows[to].end;
                if row.end == to {
                    self.status = "pick another end to compare against".into();
                    return Ok(());
                }
                let (Some(to), Some(from)) = (to, row.end) else { return Ok(()) };
                self.close_stack_picker();
                self.pick_stack_range(to, from)
            }
        }
    }

    /// The highlighted (or already picked) PR against its parent, or against the stack's base.
    pub fn stack_picker_shortcut(&mut self, against_base: bool) -> Result<()> {
        let Some(sp) = self.stack_picker.as_ref() else { return Ok(()) };
        if sp.purpose != StackPickerPurpose::Range {
            return Ok(());
        }
        let Some(StackEnd::Pr(n)) = sp.rows.get(sp.to.unwrap_or(sp.cursor)).and_then(|r| r.end)
        else {
            return Ok(());
        };
        let from = if against_base { StackEnd::Base } else { self.stack_parent(n) };
        self.close_stack_picker();
        self.pick_stack_range(StackEnd::Pr(n), from)
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
