//! PR stack ranges and trees: what a stack PR (or the stack's base) resolves to locally, and
//! the opt-in fetch that brings a stack PR's head into reviewr's private refs.
//!
//! An end is a stack PR by number, or the trunk the bottom PR targets. Its commit is read
//! from refs that already exist — the PR's local branch, then its `origin/` tracking branch,
//! then reviewr's own fetched ref, then the forge's head oid if that object is present.
//! Nothing here checks out, stages, or moves a branch. The one write is
//! [`fetch_stack_heads`], and only with `stack_fetch = true`: refs under
//! `refs/worktree/reviewr/stack/` and the objects they bring into the store.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::git;

/// How long one batched stack fetch may run before it is killed and reported failed.
pub const FETCH_TIMEOUT: Duration = Duration::from_mins(1);

/// One end of a stack range: a stack PR by number, or the stack's base (the trunk the
/// bottom PR targets).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StackEnd {
    Base,
    Pr(u64),
}

/// What one end is and how to find it: the identity, the label the header paints, the
/// branch it names on the forge, and the forge's head commit for a PR when the read named one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndSpec {
    pub end: StackEnd,
    /// `#12` for a PR, the branch name for the base.
    pub label: String,
    /// The PR's head branch, or the base's branch name.
    pub branch: String,
    /// The forge's head commit for a PR, from the latest read of it. `None` for the base, or
    /// a PR not read yet.
    pub head_oid: Option<String>,
}

impl EndSpec {
    /// The ref spellings tried, in order. A PR prefers the local branch — a stack is
    /// authored locally, and an unpushed commit is part of it — then its `origin/` tracking
    /// branch, then reviewr's fetched ref. The base prefers `origin/`: the trunk moves on the
    /// forge, and a stale local trunk would push the merge-base back and pull trunk commits
    /// into the diff.
    #[must_use]
    pub fn candidates(&self) -> Vec<String> {
        let b = &self.branch;
        match self.end {
            StackEnd::Pr(n) => {
                vec![
                    format!("refs/heads/{b}"),
                    format!("refs/remotes/origin/{b}"),
                    git::stack_ref(n),
                ]
            }
            StackEnd::Base => vec![format!("refs/remotes/origin/{b}"), format!("refs/heads/{b}")],
        }
    }

    /// The command that would bring this end in by hand.
    #[must_use]
    pub fn fetch_hint(&self) -> String {
        format!("git fetch origin {}", self.branch)
    }
}

/// An end resolved to a commit: the oid and the spelling that named it, for the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub oid: String,
    pub via: String,
}

/// Resolve `spec` from what the repository already holds, in [`EndSpec::candidates`] order,
/// then the forge's head oid if that commit is present. `None` when nothing local names it:
/// the not-fetched state.
pub fn resolve(repo: &Path, spec: &EndSpec) -> Option<Resolved> {
    for candidate in spec.candidates() {
        if let Ok(Some(oid)) = git::resolve_commit(repo, &candidate) {
            let via = candidate
                .strip_prefix("refs/heads/")
                .or_else(|| candidate.strip_prefix("refs/remotes/"))
                .map_or_else(|| format!("{} fetched", spec.label), str::to_string);
            return Some(Resolved { oid, via });
        }
    }
    let oid = spec.head_oid.as_deref().filter(|o| git::commit_exists(repo, o))?;
    Some(Resolved { oid: oid.to_string(), via: git::abbreviate_oid(oid) })
}

/// A picked stack range, frozen at the pick: both ends' specs and the commits they resolved
/// to then. The range is place state — only the reader picks, follows, or leaves it — so a
/// world refresh never re-resolves the shown commits; it only notices when an end moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackRange {
    /// The compared PR: the diff's new side.
    pub to: EndSpec,
    /// What it is compared against: the old side is the two tips' merge-base.
    pub from: EndSpec,
    pub to_at: Option<Resolved>,
    pub from_at: Option<Resolved>,
}

impl StackRange {
    /// Resolve both ends now.
    pub fn resolve(repo: &Path, to: EndSpec, from: EndSpec) -> Self {
        let to_at = resolve(repo, &to);
        let from_at = resolve(repo, &from);
        Self { to, from, to_at, from_at }
    }

    /// The header's name for the range: `#14 vs #12`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} vs {}", self.to.label, self.from.label)
    }

    /// The first end that resolved to nothing, if any.
    #[must_use]
    pub fn missing(&self) -> Option<&EndSpec> {
        if self.to_at.is_none() {
            Some(&self.to)
        } else if self.from_at.is_none() {
            Some(&self.from)
        } else {
            None
        }
    }

    /// The labels of the ends a fresh resolution would move.
    pub fn moved(&self, repo: &Path) -> Vec<String> {
        [(&self.to, &self.to_at), (&self.from, &self.from_at)]
            .into_iter()
            .filter(|(spec, at)| {
                let fresh = resolve(repo, spec).map(|r| r.oid);
                at.is_some() && fresh != at.as_ref().map(|r| r.oid.clone())
            })
            .map(|(spec, _)| spec.label.clone())
            .collect()
    }
}

/// The `All files` tab's source while it browses a stack PR's tree: the PR's spec and the
/// commit it resolved to when picked, `None` while it is not fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSource {
    pub spec: EndSpec,
    pub at: Option<Resolved>,
}

impl TreeSource {
    pub fn resolve(repo: &Path, spec: EndSpec) -> Self {
        let at = resolve(repo, &spec);
        Self { spec, at }
    }

    /// Whether a fresh resolution would show another commit.
    pub fn moved(&self, repo: &Path) -> bool {
        self.at.is_some()
            && resolve(repo, &self.spec).map(|r| r.oid) != self.at.as_ref().map(|r| r.oid.clone())
    }
}

/// What one world build found a stack range to be: the merge-base its old side reads, and the
/// ends whose refs moved since the pick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackStatus {
    pub merge_base: Option<String>,
    pub moved: Vec<String>,
}

// --- opt-in fetch (`stack_fetch`) ----------------------------------------------------

/// One batched stack fetch: the stack's PR numbers (whose private refs survive the prune),
/// and the PRs to fetch with the head the forge last reported for each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchJob {
    pub tag: u64,
    pub keep: Vec<u64>,
    pub wanted: Vec<(u64, String)>,
}

/// A finished fetch: which PRs it fetched, and the error when the fetch failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchOutcome {
    pub tag: u64,
    pub fetched: Vec<u64>,
    pub error: Option<String>,
}

/// The refspec that fetches PR `number`'s head into its private ref. `refs/pull/N/head` is
/// GitHub's own ref for the PR, so a fork PR fetches the same way.
#[must_use]
pub fn refspec(number: u64) -> String {
    format!("+refs/pull/{number}/head:{}", git::stack_ref(number))
}

/// The `git` arguments of one batched fetch. No tags, no `FETCH_HEAD`, no submodules, no
/// auto-maintenance, no remote-HEAD follow, and an empty `--refmap`, which turns off git's
/// opportunistic tracking-ref update (a configured `+refs/pull/*/head:refs/remotes/origin/pr/*`
/// would otherwise write `refs/remotes/origin/pr/<N>` too), so nothing outside the private
/// refs and the object store moves: `refs/remotes/*`, the branches, the worktree, and the index stay as
/// they were.
#[must_use]
pub fn fetch_args(numbers: &[u64]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-c",
        "remote.origin.followRemoteHEAD=never",
        "-c",
        "fetch.writeCommitGraph=false",
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        "--no-auto-maintenance",
        "--refmap=",
        "--quiet",
        "origin",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend(numbers.iter().map(|&n| refspec(n)));
    args
}

/// The environment of the fetch: no terminal prompt for credentials (a pane cannot answer
/// one), and English messages for the hint.
pub const FETCH_ENV: [(&str, &str); 2] = [("GIT_TERMINAL_PROMPT", "0"), ("LC_ALL", "C")];

/// Run one job: prune the private refs of PRs that left the stack, then fetch, in one
/// `git fetch`, every wanted PR whose head is not already in the store. Runs on a worker
/// thread; a failure or a timeout is reported in the outcome, never raised.
pub fn fetch_stack_heads(repo: &Path, job: &FetchJob, timeout: Duration) -> FetchOutcome {
    if let Err(e) = git::prune_stack_refs(repo, &job.keep) {
        crate::logln!("stack ref prune failed: {}", e.0);
    }
    let numbers: Vec<u64> = job
        .wanted
        .iter()
        .filter(|(_, oid)| !git::commit_exists(repo, oid))
        .map(|(n, _)| *n)
        .collect();
    if numbers.is_empty() {
        return FetchOutcome { tag: job.tag, fetched: Vec::new(), error: None };
    }
    let error = run_git_timed(repo, &fetch_args(&numbers), timeout).err();
    FetchOutcome { tag: job.tag, fetched: numbers, error }
}

/// Run `git -C repo <args>` with [`FETCH_ENV`], no stdin, killed past `timeout`. The error is
/// git's own last stderr line, or the timeout.
fn run_git_timed(repo: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
    let mut cmd = crate::proc::command("git");
    cmd.arg("-C").arg(repo).args(args).stdin(Stdio::null()).stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    for (k, v) in FETCH_ENV {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("git fetch: {e}"))?;
    // Drain stderr on its own thread so a chatty remote can never fill the pipe and stall
    // the child past the deadline.
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut s) = stderr {
            let _ = std::io::Read::read_to_string(&mut s, &mut text);
        }
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("git fetch: {e}")),
        }
    };
    let stderr = reader.join().unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let line = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("git fetch failed");
    Err(line.trim().trim_start_matches("fatal: ").to_string())
}

#[cfg(test)]
mod tests {
    use super::{EndSpec, StackEnd, fetch_args, refspec};

    fn pr(n: u64, branch: &str) -> EndSpec {
        EndSpec {
            end: StackEnd::Pr(n),
            label: format!("#{n}"),
            branch: branch.into(),
            head_oid: None,
        }
    }

    #[test]
    fn a_pr_prefers_its_local_branch_then_origin_then_the_private_ref() {
        assert_eq!(
            pr(12, "feature-b").candidates(),
            [
                "refs/heads/feature-b",
                "refs/remotes/origin/feature-b",
                "refs/worktree/reviewr/stack/12"
            ]
        );
        let base = EndSpec {
            end: StackEnd::Base,
            label: "main".into(),
            branch: "main".into(),
            head_oid: None,
        };
        assert_eq!(base.candidates(), ["refs/remotes/origin/main", "refs/heads/main"]);
        assert_eq!(pr(12, "feature-b").fetch_hint(), "git fetch origin feature-b");
    }

    #[test]
    fn the_fetch_writes_only_private_refs_in_one_batch() {
        assert_eq!(refspec(7), "+refs/pull/7/head:refs/worktree/reviewr/stack/7");
        let args = fetch_args(&[7, 9]);
        for flag in [
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "--refmap=",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag} in {args:?}");
        }
        assert!(args.iter().any(|a| a == "remote.origin.followRemoteHEAD=never"));
        let fetch = args.iter().position(|a| a == "fetch").unwrap();
        assert!(
            args[..fetch].iter().all(|a| a == "-c" || a.contains('=')),
            "only -c config before fetch"
        );
        assert_eq!(&args[args.len() - 3..], ["origin", &refspec(7), &refspec(9)]);
        assert!(args.iter().all(|a| !a.contains("refs/remotes") && !a.contains("refs/heads")));
    }

    #[test]
    fn the_fetch_never_prompts() {
        assert!(super::FETCH_ENV.contains(&("GIT_TERMINAL_PROMPT", "0")));
    }
}
