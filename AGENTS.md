# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. AGENTS.md is the primary file. CLAUDE.md is a symlink to it.

herdr-reviewr is a Rust TUI (ratatui) code-review pane: it runs in a [herdr](https://herdr.dev) pane beside a coding agent, shows the agent's diff, takes line comments, and sends them back to the agent's input. One binary, one git worktree per pane. It also runs standalone (`cargo run` in any repo).

## Commands

- `just test` — full test suite. Single test: `cargo test <name>` (unit tests live beside the code, integration tests in `tests/`: `cargo test --test app_flow <name>`).
- `just lint` — clippy with warnings as errors. `just fmt` / `just fmt-check` — rustfmt.
- `just ci` — exactly what CI runs (fmt-check, lint, test, release build).
- `just qa-install` — put a local build into the user's real herdr panes. See "QA install" below before using it.
- `just smoke-edit` — PTY smoke test of the editor path (`e`) against a real release binary. Unit tests stop at the argv; everything after it is terminal state, so run this after any change to `run_editor`, the terminal mode stack, or the editor dialects. Not part of `just ci`: it drives a pty and takes about a minute.
- `python3 scripts/bench_tui.py --binary target/release/herdr-reviewr --fixture` — perceived-latency benchmark (keypress → painted frame, via PTY). Ad-hoc tooling, not a gate: run it when a change might feel slower. `cargo run --release --example bench_latency -- <repo>` attributes a slow number to its component calls. The one committed baseline is `scripts/bench-results/baseline.json` — replace it when a change moves the numbers, never add per-round runs. For an A/B, rebuild the old binary to a second target dir and interleave runs under the same system load — absolute numbers drift with background load.

## Invariants

New behavior is designed with `/brainstorming` and sequenced with `/planning` in the conversation; the repo keeps no spec tree. The commit message and the changelog carry the decisions.

Load-bearing invariants. Cite them by name:

- **No writes**: reviewr never mutates the worktree, index, or branches. Its only git writes are private refs under `refs/worktree/reviewr/`: the turn baseline and the base pick. One opt-in exception, `stack_fetch = true` (off by default): a `git fetch origin +refs/pull/<N>/head:refs/worktree/reviewr/stack/<N>` per stack PR whose head is not in the store, which writes only refs under `refs/worktree/reviewr/stack/` (deleted when the PR leaves the stack) and the objects the fetch adds to the store. No tags, no `FETCH_HEAD`, no `refs/remotes/*`, no `origin/HEAD`. Without that key reviewr never fetches.
- **Comments survive**: comments are never lost to a refresh or the agent's edits, and leave only by explicit export. The comment store is in-memory **by design** — do not propose persisting it.
- **Continuity**: place state (cursor, scroll, tab, scope, folds, selection, layout) moves only under the user's own input. World events (polls, refreshes, fetch results) may only *reconcile* it: match by identity first (path, comment author+anchor — never row index), fall back to the nearest surviving target, clamp last. Derived state on screen may be stale, never wrong: blank a view only when its identity changed, never because the same thing gained newer content.

## Architecture

The runtime is a single-threaded frame loop (`event_loop` in `src/lib.rs`): draw → wait for input or poll deadline → mutate `App` → draw. Clipboard, agent-send, and per-file diff builds run synchronously between frames, and a terminal editor holds the loop for its whole session by design (`policies/ux-responsiveness.md`). Eight things run on worker threads: the opt-in stack fetch (`src/stack.rs` — one batched `git fetch` at a time, tagged, timed out), the world worker (`src/world.rs` — the refresh build and turn tracking), the search worker (`src/search.rs` — the fff-search engine, which runs its own scan, watch, and content-index threads), the PR input probe, the PR forge fetch (`gh`/`glab`/`az`, plus the stack cache's batched read of the other stack PRs on its own thread), config recovery, the avatar fetcher (`src/avatar.rs` — opt-in `curl` downloads and decodes of PR authors' avatars, four threads, request order, nothing waits on them), and the inline-image fetcher (`src/images.rs` — opt-in `curl` downloads of the PR conversation's images, three threads, the `gh` token read there and only for the forge's own host, decode or SVG rasterise, PNG pack, plus one `Rasterizer` thread redrawing SVGs at a block's pixel size). Both fetchers are `graphics::Fetcher`. World results land through `land_world_completion`: input-tagged, latest-wins, reconciled only while the view still matches (see Continuity above).

- `src/app.rs` — the `App` state machine. Tabs (`Changes`/`AllFiles`/`Pr`), scopes (`Uncommitted`/`Branch`/`LastTurn`), `Focus` (files vs diff pane), `Mode` (`Normal`, the `Composing`/`List` overlays, and the body-replacing `Search` screen). `reconcile_world()` is the one place a world snapshot touches place state; `reload()` is the synchronous build+reconcile pair used at startup, first tab visits, and scope switches. Each file tab stashes its full place state on switch-away (`swap_active_with_stash`). While composing, the open diff is frozen (reconcile skips it) so a draft's anchor can't move.
- `src/world.rs` — the world worker: the pure snapshot build (`WorldInput` → `WorldSnapshot`), the request/completion channels (latest-wins by generation), and `TurnHost` (the turn tracker, worktree snapshots, and the baseline ref write, all worker-side).
- `src/git.rs` — every git subprocess. `changed_files` (scope changesets), `all_files` (tracked + untracked + ignored via `ls-files` — never use `git status --ignored`, it walks inside ignored trees and costs seconds), `snapshot_worktree` (temp-index `add -A` + `write-tree` for turn baselines), baseline refs.
- `src/diff.rs` — `FileDiff` build (syntect highlight both sides, similar-line pairing, word emphasis, folds) and `DiffCache`, keyed by path and gated by content hash. Cleared on scope switch and theme change.
- `src/ui.rs` — all rendering. Row heights and wrapping recompute per frame across the visible diff, so render cost scales with open-file size.
- `src/forge.rs` + the `PrRefresh`/`PrCoordinator` state machines in `lib.rs` — the PR snapshot. Fetches are tagged with the input (repository identity, pinned HEAD and base, the branch's published heads and pin) that produced them, and a result paints only if a fresh probe proves the input still matches. This generation/input-tag pattern is the template for moving other derived state off-thread.
- `src/gitlab.rs` / `src/azure_devops.rs` — the `glab` and `az` providers behind the forge boundary in `forge.rs`, each mapping its CLI's payloads onto the one `PrSnapshot` shape.
- `src/turn.rs` — the pure turn state machine: a resting→working edge starts a turn, and a pending candidate promotes to the `last-turn` baseline once the worktree diverges from it. The world worker's `TurnHost` drives it; `src/herdr.rs` holds the herdr CLI calls.
- `src/model.rs` — `CommentStore` (in-memory), comment anchoring (`diff_anchored` distinguishes diff comments from All-files content comments — each renders only in its own view).
- `src/stack.rs` + `src/app/stack_view.rs` — PR stacks on the file tabs: the navigator's stack list (`Focus::Stack`; carved out of the navigator in `ui::panes`, so every files hit-test shrinks with it), the `stack` scope's range (two stack PRs, or a PR and the stack's base, diffed merge-base → tip), the `All files` tab's stack PR tree (`ls-tree`/`show`, nothing checked out), the local-branch badges (world worker), and the opt-in fetch. A PR end is the forge's head unless the reader chose its local branch; the checked-out PR is `HEAD`. Both views are read-only and place state: ends resolve once at the pick, a world build only reports that an end moved, and only the reader's input (`r`, the local toggle) re-resolves them by PR number.
- `src/editor.rs` — the editor command: a name-keyed dialect table (how each editor takes a line, and whether it draws in the pane), quote-aware splitting, and the `editor` key's `{file}`/`{line}` template. Pure argv resolution, spawning nowhere. `run_editor` in `lib.rs` owns the spawn, and hands the pane over for a terminal editor, blocking the frame loop for that editor's whole session.
- `src/graphics.rs` — the pixel-graphics layer avatars and inline images share: Kitty graphics with Unicode placeholders (the image id rides a cell's foreground, row/column diacritics name the cell), chunked transmission and deletion, the non-blocking graphics probe read back out of the key stream, the generic download `Fetcher`, `curl` with size/time caps (a token goes through curl's stdin config, never argv), and `token_host` — the one rule for where a forge token may go. The loop (`GraphicsHost` in `lib.rs`) sends the one probe, transmits avatars before a draw, requests after it (painted first), places the images a frame painted after it (and repaints at once), and deletes everything on exit, editor handoff, and each kind when switched off. A resize forgets nothing: it re-measures the cell size, and a queued burst of resizes is drained into one draw.
- `src/avatar.rs` — opt-in PR avatars over `graphics`: the circle geometry and mask, the one-row placement, and the session store. A missing piece anywhere paints the dot.
- `src/images.rs` — opt-in inline PR images over `graphics`: destination resolution (absolute `http(s)`, GitLab `/uploads/`, GitHub `/raw/` and `?raw=true` file pages to the raw file a token can fetch), the cell sizing (`layout`: badges at their own size, block images `fill` the width or keep their own size (`fit`), within `inline_image_max_rows`), raster decode with limits, SVG rasterisation (`resvg`, every href refused), PNG packing, and the session store: one terminal image per URL, its pixels sent once and its one virtual placement moved on a new footprint, sharp SVG re-rasters debounced (`QUIET`) and tagged on the `Rasterizer` thread, at most `MAX_PLACED`/`MAX_PIXELS` held, least recently painted deleted first. The markdown renderer lays images out from the store through `markdown::ImageSource` (the memo re-renders a body when an image in it lands), image cells render as the alt text, and `ui::paint_image_runs` swaps in placeholders only once the terminal holds the picture. `settle_pr_read_scroll` carries the scroll by an image block's growth above the reader.
- `src/export.rs` — comment export: format all, send via `herdr agent send` or clipboard, consume-on-success only.
- `src/config.rs` — plugin config: the whole file validates before every frame/action. An invalid config blocks all review work until recovery, which carries authored state.
- `herdr-plugin.toml` + `herdr/pane.sh` — plugin packaging: pane, toggle/open/close actions, and worktree workspace-birth auto-open.

## QA install — putting a local build into the user's herdr panes

The user tests builds in real herdr panes. The panes run the GitHub-installed plugin's binary at `~/.config/herdr/plugins/github/persiyanov.reviewr-<hash>/bin/herdr-reviewr`, NOT anything in this worktree. Full procedure: `docs/qa-install.md`. Short form:

```
just qa-install
```

Then tell the user to close and reopen their reviewr panes with the toggle keybinding. Done.

Three rules. Each one has already burned a session:

1. **Never overwrite that binary in place.** `cp` onto the existing file keeps the inode and macOS SIGKILLs the binary at every launch (exit 137, blank panes, no log). Replace through a fresh inode and re-sign — which is exactly what `just qa-install` does. Do not improvise the swap by hand.
2. **Swapping the file does not restart running panes.** They keep the old binary image until closed and reopened. Refresh inside reviewr does nothing for this.
3. **Never script pane opens.** The plugin's `open`/`toggle` actions act on the currently focused workspace and ignore `HERDR_WORKSPACE_ID`. Automating reopens stacks panes into whatever space the user is looking at. Closing via `herdr/pane.sh close` is safe. Reopening is the user's keystroke, always.

Rollback: `bin/herdr-reviewr.release-backup` sits beside the installed binary, swap it back the same fresh-inode way (or `herdr plugin install` to restore the release).
