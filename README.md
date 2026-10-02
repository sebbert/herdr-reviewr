# herdr-reviewr

[![CI](https://github.com/persiyanov/herdr-reviewr/actions/workflows/ci.yml/badge.svg)](https://github.com/persiyanov/herdr-reviewr/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/persiyanov/herdr-reviewr)](https://github.com/persiyanov/herdr-reviewr/releases/latest)
[![License](https://img.shields.io/github/license/persiyanov/herdr-reviewr)](LICENSE)

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#controls">controls</a> · <a href="#diff-scopes">scopes</a> · <a href="#configuration">configuration</a> · <a href="#limitations">limitations</a> · <a href="CHANGELOG.md">changelog</a>
</p>

A code-review pane for [herdr](https://herdr.dev). Your agent writes the code. You read its
diff in a pane beside the chat, comment on the lines, and send the notes back. You never leave
the terminal.

![demo](assets/demo.gif)

One persistent pane, pointed at a git worktree:

- **Diff review** — the agent's changed files, syntax-highlighted.
- **Four diff scopes** — uncommitted, branch, last turn, commits — plus any stacked PR against
  another or the stack's base.
- **Last-turn diff** — what the worktree's latest turn changed, on its own.
- **Line comments** — comment on a line or a range. Then send it to the agent.
- **Text selection** — drag over any text to copy it, like an editor.
- **File viewer** — any file's current content from the whole worktree.
- **Search** — fuzzy file names and live code grep across the worktree, powered by [fff](https://github.com/dmtrKovalenko/fff).
- **Find in file** — search the open file and step between every match.
- **PR view** — the branch's pull request in the pane, read-only.
- **Markdown preview** — flip a `.md` file between source and rendered view.
- **Themes** — 18 palettes in dark and light.

It never edits your worktree and sends nothing on its own. The **PR** tab reads GitHub,
GitLab, or Azure DevOps and never posts.

## Requirements

- **herdr ≥ 0.7.5** (the plugin system).
- **git** on `PATH`.
- A **truecolor** terminal with Unicode box-drawing.
- **macOS or Linux.**
- **`gh`** (GitHub), **`glab`** (GitLab), or **`az`** (Azure DevOps, with the `azure-devops` extension), authenticated. Only the **PR** tab needs one.

## Install

Prebuilt binaries, no Rust toolchain needed:

```bash
herdr plugin install persiyanov/herdr-reviewr
```

Open it in the current workspace:

```bash
herdr plugin action invoke open --plugin persiyanov.reviewr
```

reviewr auto-opens when herdr creates a workspace for a worktree, whether the checkout is new or
opened from disk. `auto_open = false` keeps it hidden until you ask
([Configuration](#configuration)).

**To update**, reinstall. Your config is keyed by plugin id and survives:

```bash
herdr plugin uninstall persiyanov.reviewr && herdr plugin install persiyanov/herdr-reviewr
```

**Without herdr**, reviewr runs as a plain terminal app. Grab a
[release binary](https://github.com/persiyanov/herdr-reviewr/releases/latest) and point it at a
repo:

```bash
herdr-reviewr ~/some/repo
```

Everything works except **Send** and the **last turn** scope. Those need herdr around.

## Quick start

Open reviewr next to your agent:

1. **Pick a file.** Changed files are in the navigator. `j` / `k` moves, the diff follows. Or
   `]` walks the changes hunk by hunk, file after file.
2. **Focus the diff.** `Tab` switches panes.
3. **Select lines.** `v`, then `j` / `k` to extend (or click or drag the gutter).
4. **Comment.** `c`, type, `Enter`.
5. **Send.** `s` sends every comment to the agent's input.

The footer shows the next step. Press `?` for every key that works right now.

For a shortcut, bind a key to the toggle in your herdr config (user config, not the plugin manifest):

```toml
[[keys.command]]
key = "cmd+r"
type = "plugin_action"
command = "persiyanov.reviewr.toggle"   # <plugin_id>.<action_id> — note the id, not the name
```

`cmd+…` chords reach herdr. Many macOS terminals swallow `alt+…` themselves.

## Controls

The keys below are defaults. You can rebind every action, even to several keys at once
([Keybindings](#keybindings)).

**Getting around**

| Key | Action |
| --- | --- |
| `1` `2` `3` | Switch tab — Changes / All files / PR |
| `u` `b` `t` `g` | Switch scope — uncommitted / branch / last turn / commits |
| `B` | Pick the base branch |
| `G` | Pick the commits to review |
| `P` | Move into the stack list, or back to the files ([Stacked PRs](#stacked-prs)) |
| `A` | In the stack list: compare the shown PR against the highlighted row |
| `W` | In the stack list: use the row's local branch instead of the PR head, or back |
| `0` | Back from a stack range or tree to the checked-out work |
| `j` `k` · `↑` `↓` | Move cursor |
| `]` `[` | Jump to next / previous hunk |
| `f` `F` | Jump to next / previous file |
| `PageUp` `PageDown` | Move a page |
| `Ctrl+U` `Ctrl+D` | Move a half-page |
| `Tab` | Switch focus |
| `→` `←` | Expand / collapse, or scroll sideways |
| `/` | Search files and code |
| `Ctrl+F` | Find in file |
| `w` | Toggle line wrap |
| `m` | Preview markdown file |
| `p` | Rotate navigator |
| `z` | Hide / show navigator |
| `<` `>` | Grow / shrink navigator |
| `r` | Refresh |
| `?` | Open shortcuts helper |
| `q` | Quit |

**Reviewing** (in the diff)

| Key | Action |
| --- | --- |
| `v` | Select lines |
| `c` | Comment on line or selection |
| `e` | Edit the comment under the cursor, or open the file in your editor |
| `d` | Delete comment |
| `n` `N` | Jump to next / previous comment |
| `l` | List all comments |
| `s` | Send comments to agent |
| `y` | Copy comments to clipboard |
| `esc` | Clear selection |

**In the comment box**

| Key | Action |
| --- | --- |
| `Enter` | Save comment |
| `Esc` | Cancel |
| `Shift+Enter` · `Alt+Enter` · `Ctrl+J` | Insert newline |

Plus the usual caret moves: arrows, `Home` / `End`, `Ctrl+A` / `Ctrl+E`, `Alt+b` / `Alt+f` word
jumps, and `Ctrl+W` / `Ctrl+U` / `Ctrl+K` deletes.

**PR tab** (read-only)

| Key | Action |
| --- | --- |
| `j` `k` | Jump to the description, a stack PR, or a comment |
| `Enter` | View the selected stack PR ([Stacked PRs](#stacked-prs)) |
| `0` | Back to the checked-out branch's PR |
| `PageUp` `PageDown` | Scroll focused pane |
| `a` | Fold or unfold the selected thread |
| `o` | Open the PR you are viewing in the browser |
| `r` | Refresh |

The mouse works too. Drag over any text to select and copy it, double-click a word,
triple-click a line. Click or drag the line-number gutter to comment. Click files, tabs, and
links, and scroll with the wheel. On the PR tab, click a thread's header to fold or unfold it,
and click a stack row to view that PR.

## The three tabs

- **Changes** — the active scope's changed files with `+/-` stats and totals in the header.
- **All files** — any file's current content from the whole worktree, comments too. A collapsed
  folder with a changed file under it shows a dot. Ignored paths show dimmed. A row of the
  stack list switches it to that PR's tree, read-only, and `0` back
  ([Stacked PRs](#stacked-prs)).
- **PR** — a read-only mirror of the branch's pull request (GitHub, Azure DevOps) or merge
  request (GitLab): state, checks, description, and comments, rendered as markdown. A GitHub
  PR that is part of a stack lists the stack too ([Stacked PRs](#stacked-prs)). The read
  pane is one conversation: the description, then every comment oldest first, each in its own
  box. Conversation comments, review verdicts (`✓ approved`, `✗ changes requested`), and inline
  threads (`path:line`) share the one list. A thread's replies sit in its root's box along a
  timeline. A resolved thread starts folded to two lines, `▸ path:line · resolved` and its
  root's first line. Click the header or press `a` to unfold it (`▾`), and again to fold it.
  Any thread folds this way, on every forge. Your folds stick to their threads across
  refreshes. A thread resolved while you read it stays open until you move to another one.
  Names, ages, checks, and PR numbers are [links](#hyperlinks) to the forge.
  reviewr never writes to the forge.

## Diff scopes

- **uncommitted** — the working tree vs `HEAD` (staged, unstaged, and untracked).
- **branch** — the working tree vs the merge-base with the base branch: **uncommitted** plus
  the branch's commits. The base is your repo's default branch, or a stacked PR's parent
  branch, until you pick another with
  `B` ([Base branch](#base-branch)).
- **last turn** — everything that changed in this worktree since its most recent turn started
  ([Limitations](#limitations)).
- **commits** — one commit, or several in a row, picked with `G`. Read what the agent
  committed one step at a time, without its unsaved edits mixed in.
- **stack** — one PR of the branch's stack against another, or against the stack's base,
  picked in the navigator's stack list (GitHub). Read-only: it is not your checked-out work
  ([Stacked PRs](#stacked-prs)).

reviewr starts in **uncommitted**. `default_scope` changes that. Switching with `u`/`b`/`t`/`g`
wins for the rest of the session. `g` without a pick opens the picker.

Every scope respects `.gitignore`, so build output never clutters **Changes**. To review a file,
track it. **All files** still browses any ignored path.

## Configuration

CLI flags on the pane command:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--poll <ms>` | `2000` | worktree poll interval (min `200`) |
| `--base <ref>` | auto | base for `branch` scope, any rev, overrides the pick |
| `--theme <name>` | `catppuccin` | UI + syntax theme (see below) |
| `--wrap <on\|off>` | `on` | soft-wrap long diff lines (`w` toggles at runtime) |

Everything else lives in reviewr's config file:

```text
~/.config/herdr/plugins/config/persiyanov.reviewr/config.toml
```

Create it if missing. It is reviewr's file. Settings in herdr's `~/.config/herdr/config.toml`
never reach it. reviewr re-reads it on every refresh and toggle, so edits apply without a
relaunch.

The file accepts these keys:

```toml
theme = "tokyo-night"
default_scope = "branch"
navigator_position = "right"
toggle_placement = "overlay"
toggle_direction = "down"
split_ratio = 0.33
auto_open = false
pane_outer_borders = false
pr_nav_separators = true
hyperlinks = false
stack_fetch = true
stack_list_position = "bottom"
avatars = true
github_host = "github.example.com"
editor = "code -g {file}:{line}"

[keybindings]
comment = ["c", "ㅊ"]
select  = ["v", "ㅍ"]
```

A missing file or omitted key uses its default. An invalid file is rejected whole — the pane
shows the error and recovers on the next refresh after you fix it.

### Theme

One theme colors the whole UI, chrome and syntax together:

```toml
theme = "tokyo-night"
```

`--theme` overrides the file. Match your terminal's light or dark background. Available:

- **Dark:** `catppuccin`, `catppuccin-frappe`, `catppuccin-macchiato`, `dracula`, `nord`,
  `gruvbox`, `one-dark`, `solarized`, `monokai`, `tokyo-night`, `rose-pine`.
- **Light:** `catppuccin-latte`, `gruvbox-light`, `one-light`, `solarized-light`,
  `github-light`, `tokyo-night-day`, `rose-pine-dawn`.

Names match herdr's where both ship a palette.

### Navigator position

The navigator starts on the right. Set `navigator_position` to `right`, `bottom`, `left`, or
`top`, or press `p` to cycle clockwise:

```toml
navigator_position = "bottom"
```

`<` grows, `>` shrinks, or drag the divider. `z` hides the navigator altogether and brings it
back.

### Outer borders

reviewr frames each pane by default. If herdr runs with `[ui] pane_outer_borders = false`,
set the same key in reviewr's file so its panes match:

```toml
pane_outer_borders = false   # default: true
```

The frames go. One divider line stays between the file list and the diff. Each pane keeps
its title on a top row of its own, and the focused pane's title is lit instead of its
border. Popups, the comment box, and the keys help keep their frames: they float over the
panes rather than sitting against the edge.

### Avatars

Opt in to show each comment author's avatar in place of the `●` on the PR tab's thread
timeline:

```toml
avatars = true          # default: false (dots)
avatar_width = 1        # 1 or 2 cells; default 1
avatar_fit = "height"   # "height" or "width"; default "height"
```

This needs a terminal that speaks the Kitty graphics protocol with Unicode placeholders.
Ghostty and kitty do, and so does herdr's own pane renderer. reviewr asks the terminal once,
and anywhere it gets no answer, the dots stay. The avatars load lazily: the PR paints with
dots straight away, the visible cards fetch first, and each avatar swaps in when it arrives.
A failed download stays a dot. Nothing moves when one lands.

`avatar_width = 1` takes the dot's own cell. `2` takes the dot and the space after it.
`avatar_fit = "height"` makes the circle as tall as the row, so it can spill into the blank
cells beside the dot, cut off before any border or text. `"width"` makes it exactly as wide
as its cells, with clear space above and below. Avatars on GitHub Enterprise or Azure DevOps
that need a sign-in stay dots.

To check a terminal, run `cargo run --example avatar_check` inside a herdr pane. It should
draw four round pictures.

### Base branch

The **branch** scope diffs against the merge-base with your repo's default branch, with or
without a remote. The header shows the resolved base, `vs main`.

When the branch's open PR targets another branch than the default — a stacked PR — the
scope diffs against that branch instead, once the PR has loaded, and the header says where
the base came from: `vs feature-a (pr base)`. You review this PR's own commits, not its
parent's. When the PR retargets (its parent merged), the base follows on the next PR
refresh. A merged or closed PR, or one from a fork, leaves the default in place. If the
parent branch is not fetched, the header says so (`vs main · feature-a missing`).

When the trunk is something else, press `B` (or click the
base name) and pick the branch. Every branch is a row with its age, and a row says when it
is the open PR's target (`pr base`), the repo's `default`, or the branch checked out here
(`current`). Type to narrow the list, fuzzily. The pick is stored for this worktree and holds
until you pick again, over the PR's target too. Other worktrees on the same clone keep their
own pick. To go back, pick the branch reviewr would choose on its own: the stacked PR's
target when there is one, else the default branch.

You can also type any revision, like `HEAD~2`, a tag, or a SHA prefix. It appears as one more
row under the matches, and the header shows what resolved: `vs HEAD~2 (a1b2c3d)`.

`--base <ref>` sets the base for this pane. It wins over the pick and the PR's target, and
disables the picker.

### Stacked PRs

On GitHub, the PR tab's navigator lists the stack a PR sits in: the PRs below it, down to the
one that targets the default branch, and the open PRs stacked on top of it — the chains
`gh stack` builds, or any PR whose base is another PR's
branch. Each row shows the number, state, and title, top of the stack first, with the trunk
last. The checked-out branch's PR wears a filled `●` and a `checked out` tag:

```text
stack · 3
   #12 open   Add the settings page
 ● #11 open   Add the settings API    checked out
   #10 merged Add the settings table
   └ main
```

Move onto a stack row with `j`/`k` and press `Enter` (or click it) to view that PR: its state,
checks, description, and conversation. Nothing is checked out, and nothing outside the PR tab
changes. The file tabs, the branch scope's base, and your comments stay with the checked-out
branch. The header says `viewing #12 · not checked out` while you look. The row of the PR on
screen, checked out or not, is filled with a violet tint, a step stronger when the cursor
sits on it. The list stays the checked-out PR's stack whichever PR you view. `o` opens the PR
you are viewing. `0` (`checked-out-pr`), or the checked-out PR's own row, takes you back to
where you were on it.

reviewr reads every PR in the stack ahead of time and keeps it in memory, so a switch shows
the PR at once. Only a PR never read yet shows `loading`, and then only in its own sections:
the stack and the header stay. The PR you view refreshes on the PR tab's cadence. The others
refresh every 5 minutes while the PR tab is showing. A refresh lands in place without moving
your cursor or scroll. A failed refresh keeps the last good read, with a notice when it is the
PR you are viewing. The checked-out PR keeps its own refresh, which never takes you back. A
viewed PR stays on screen until you leave it, even if it drops out of the stack. Each PR you
open from the stack starts at its top.

A PR that stacks on nothing shows no stack section. Reading the stack costs one extra GitHub
query per refresh, plus one per further level. The stack's PRs are read in batches of up to 5
per GitHub query, one batch at a time. GitLab and Azure DevOps show no stack, but their
MR or PR target still sets the [base](#base-branch).

**The stack list.** On the Changes and All files tabs, the navigator shows the stack above the
file list (`stack_list_position = "bottom"` puts it below): each PR top first, with its number,
state, and title, the checked-out one marked `●`, and the stack's base last. It is as tall as
the stack, up to six rows, and scrolls beyond. A short navigator shrinks it first, and one too
short for both shows only the files. Framed, it is a box of its own. With
`pane_outer_borders = false`, a one-row divider parts it from the files. Without a stack
(GitLab, Azure DevOps, or a PR that stacks on nothing) there is no list.

```text
┌ Stack · 3 ────────────────────┐
│   #12 open   Add the page  head│
│ ● #11 open   Add the API       │
│   #10 merged Add the table     │
│   └ main stack base   against  │
└───────────────────────────────┘
┌ Files ────────────────────────┐
```

`P` (`stack-list`) moves the keyboard into the list, and again (or `tab`, or `esc`) back to the
files. `j`/`k` move, and a click works too.

**Compare stack PRs.** On the Changes tab, `Enter` on a PR shows it against its parent, the
common case. `A` (`stack-against`), or a ctrl- or alt-click, on another row sets what it is
compared against: another PR, or the base. With no range shown, `A` compares the checked-out
PR against that row. The rows wear their roles: the shown PR is filled violet like the PR
tab's viewed row and tagged `head`, the other end tagged `against`. The **stack** scope shows
the range, and the header names it:

```text
[stack] #12 vs #11 · read-only
```

`Enter` on the checked-out PR's own row, or `0` (`checked-out-pr`), goes back to the scope you
came from. `u`/`b`/`t`/`g` work too.

The diff runs from the two tips' merge-base to the PR's tip: what the PR adds over the other
end, the way GitHub shows a PR over its base. A parent that moved on after the PR branched off
it never reads as the PR's own change. A PR compared against one stacked on top of it adds
nothing, and the panes say so.

**Which commit a row is.** A PR row means the PR as its reviewers see it: the head commit the
forge reports, when it is in your repository, then its `origin/` branch, then reviewr's own
fetched ref (below). A local branch of the same name never stands in for it. The checked-out
PR's row is always your worktree's `HEAD`. The base prefers `origin/<base>`. An end nothing
local names shows how to fetch it: `#12's branch isn't fetched — git fetch origin feature-b`.

When a local branch named like the PR's head exists and differs from the PR head, the row says
so: `local +2 -3`, its commits ahead of and behind the PR head (`local` alone when the PR head
is not in your repository). `W` (`stack-local`), or a click on the badge, uses that local
branch for the row instead, and again goes back to the PR head. The badge then reads
`[local +2 -3]`, and the header names it: `[stack] #12 (local) vs #11 · read-only`. The choice
is per PR and survives refreshes. The checked-out PR has no toggle. If the local branch
disappears, the row falls back to the PR head and the header says
`#12's local branch is gone — PR head`. The counts are read off the UI thread, once per pair
of commits.

A range is read-only. Nothing is checked out, the comment key says why it does nothing, the
editor key does nothing, and your comments, which belong to the checked-out work, don't show
on it. They are all there again when you go back. To review your own work against the parent,
use the **branch** scope, which already diffs against the stacked PR's parent.

The range stays on the commits it was picked at. When a branch moves, or the PR tab's stack
read sees a push, the header adds `#12 moved — r follows`. `r` re-reads both ends by PR
number, and the open file stays open.

**Browse a stack PR's tree.** On the All files tab, `Enter` on a row browses that PR's tree
from the object store: the same tree, preview, and find, nothing checked out. Its row is
tagged `tree`. The header reads `#12 tree 1a2b3c4 · read-only`, and comments are off. The
checked-out PR's row, or `0`, goes back to the worktree, on the same file. A PR whose branch is
not fetched says so, the same way.

**Fetch stack PRs automatically.** `stack_fetch = true` (off by default) lets reviewr fetch
every stack PR whose head is not in your repository yet, once the stack read names the head.
Each PR's head goes into reviewr's private ref `refs/worktree/reviewr/stack/<N>`, read from
GitHub's `refs/pull/<N>/head`, so a fork PR fetches too:

```text
git fetch --no-tags --no-write-fetch-head --refmap= origin +refs/pull/<N>/head:refs/worktree/reviewr/stack/<N>
```

Nothing else moves: no branch, no `refs/remotes/` ref, no `origin/HEAD`, not the index or the
worktree, so the **branch** scope's base never shifts because of it. The empty `--refmap`
keeps a configured `refs/pull/*` fetch refspec from writing its own `refs/remotes/` copy. All the PRs go in one
`git fetch`, one at a time, off the UI thread, with no credential prompt and a one-minute
limit. A PR is fetched again only when the stack read reports a new head. A failed fetch shows
its error in the not-fetched message; `r` tries again. When a PR leaves the stack, its private
ref is deleted.

### Hyperlinks

The PR tab's text that names something on the forge is a terminal hyperlink (OSC 8). How you
open one depends on your terminal: in Ghostty it is ⌘-click on macOS and ctrl-click elsewhere.
A plain click still does what it always did in reviewr.

| Text | Leads to |
| --- | --- |
| `@author` in a byline, a folded thread, or the navigator | the author's profile |
| a byline's age (`2d`) | that comment or reply |
| a card's `path:line`, `review`, or `comment` header, and the navigator's anchor or verdict | that comment |
| a check in the navigator | its run or details page |
| the header's title and `open #12 ↗` chip, and a stack row's `#12` | that PR |
| the header's branch | the branch (not a fork's) |
| a markdown link in a description or comment, and in a file's preview | its target |

Only `http(s)` URLs link. What a forge doesn't name stays plain text: an Azure DevOps author
(it has no public profile page), an Azure DevOps policy check, or a GitLab approval's own page.
The link never changes what the cell shows, and copying text never copies a link. To paint
plain text instead:

```toml
hyperlinks = false   # default: true
```

### Navigator separators

The PR tab's navigator parts its stack, checks, and comments sections with blank rows. To rule
them apart with a horizontal line instead:

```toml
pr_nav_separators = true   # default: false
```

### Editor

`e` opens the file at the line you're on, or the navigator's selected file. On a line you have
already commented, `e` edits the comment instead.

Set `$EDITOR` (or `$VISUAL`) and reviewr opens it at the right line. It knows vim, neovim,
helix, emacs, nano, VS Code and its forks, Zed, Sublime Text, JetBrains, and the rest of the
usual set.

A terminal editor takes the pane, and reviewr refreshes when you quit it. A window editor opens
its own window, so the diff stays up and your save turns up in it on the next poll.

Write the command yourself when you need to. `{file}` and `{line}` are reviewr's, everything
else is your editor's:

```toml
editor = "code -g {file}:{line}"
```

### URL opener

`o` and link clicks open URLs with `open` (macOS) or `xdg-open` (Linux). Under `herdr --remote`
that runs on the server, so point `url_opener` at a command that reaches your browser:

```toml
url_opener = "browser-bridge --new-tab {url}"
```

It reads like the `editor` key: quotes group words, and `{url}` goes where you put it, or at the
end. The URL always arrives as one argument, never through a shell. So don't use
`ssh host open {url}`: ssh hands its arguments to the remote shell, where a crafted link could run
commands.

### Keybindings

`[keybindings]` maps an action name to an array of keys. The array replaces that action's
defaults, actions you don't mention keep theirs, and hints show the first key:

```toml
[keybindings]
comment = ["c", "ㅊ"]
select  = ["v", "ㅍ"]
```

Several keys per action serves CJK input sources — bind the character your layout produces
on the same physical key.

The action names and their defaults:

| Action | Default |
| --- | --- |
| `down` / `up` | `j` / `k` |
| `next-hunk` / `prev-hunk` | `]` / `[` |
| `next-file` / `prev-file` | `f` / `F` |
| `scope-uncommitted` / `scope-branch` / `scope-last-turn` / `scope-commits` | `u` / `b` / `t` / `g` |
| `base-pick` / `commit-pick` | `B` / `G` |
| `stack-list` / `stack-against` / `stack-local` | `P` / `A` / `W` |
| `tab-changes` / `tab-all-files` / `tab-pr` | `1` / `2` / `3` |
| `wrap` | `w` |
| `preview` | `m` |
| `navigator-position` | `p` |
| `navigator-hide` | `z` |
| `navigator-grow` / `navigator-shrink` | `<` / `>` |
| `select` | `v` |
| `comment` | `c` |
| `edit` / `delete` | `e` / `d` |
| `next-comment` / `prev-comment` | `n` / `N` |
| `comments` | `l` |
| `search` | `/` |
| `find` | `ctrl+f` |
| `keys` | `?` |
| `send` | `s`, `S` |
| `copy` | `y`, `Y` |
| `open-pr` | `o` |
| `toggle-thread` | `a` |
| `checked-out-pr` | `0` (also leaves a stack range or tree) |
| `refresh` | `r` |
| `quit` | `q` |

A key is one printable character, or a `ctrl+`/`alt+` chord like `ctrl+f`. `Tab`, `Esc`, and
`Enter` are fixed. Keys still type normally in the comment box.

### Forge repositories and hosts

The PR tab reads `upstream` when you have one, otherwise `origin`. A standard fork clone works
without setup. Checking out a contributor PR (`gh pr checkout`, `glab mr checkout`) in an
upstream clone attaches it too.

GitHub.com, GitLab.com, dev.azure.com, and the `*.visualstudio.com` organization hosts work
without configuration. For one self-hosted instance per forge, set its bare hostname:

```toml
github_host = "github.example.com"
gitlab_host = "git.corp.example"
azure_devops_host = "tfs.corp.example"
```

Matching is exact. reviewr does not infer SSH aliases like `github.com-work` — use a
canonical-host remote or an `insteadOf` rewrite. Authenticate with
`gh auth login --hostname github.example.com`, `glab auth login --hostname git.corp.example`,
or `az login`.

### Pane placement

The toggle opens reviewr as a split to the right of your agent. `toggle_placement` changes the
shape:

```toml
toggle_placement = "overlay"   # split | overlay | zoomed | tab   (default: split)
toggle_direction = "down"      # right | down — split only        (default: right)
split_ratio = 0.33             # reviewr's share, 0.1–0.9 — split only (default: 0.4)
```

- **`split`** sits next to your agent. `toggle_direction` puts reviewr on the right (default) or below.
  `split_ratio` sizes it: the split opens even, then resizes to reviewr's share, 0.4 unless set.
  Auto-open sizes it too.
- **`overlay`** covers the tab. Toggle again to drop back.
- **`zoomed`** fills the tab.
- **`tab`** opens its own tab.

Every placement takes the keyboard on toggle. New worktree workspaces auto-open only `split` and
`tab`, and never steal focus.

### Auto-open and layout plugins

reviewr auto-opens when herdr creates a workspace for a new or existing worktree checkout. Opening
an already-live workspace does not resurrect a reviewr pane you closed there. `auto_open = false`
makes it wait for the toggle:

```toml
auto_open = false   # default: true
```

A layout places reviewr like any other program. Give one pane the command:

```toml
command = "herdr-reviewr"
```

That pane is a full reviewr pane. The install links the binary at `~/.local/bin/herdr-reviewr`
and at `~/.local/state/herdr/plugins/persiyanov.reviewr/bin/herdr-reviewr`. Use the long path
if `~/.local/bin` is not on your `PATH`.

A layout hook can also invoke the actions, once its panes are in place:

```bash
herdr plugin action invoke open --plugin persiyanov.reviewr
```

`open` ignores `auto_open`, and both actions are safe to repeat. They target the focused
workspace. Put `herdr-reviewr` itself in a layout pane, never the invoke.

## Limitations

The known constraints:

**Terminal & theme**
- **Truecolor required** — colors are 24-bit RGB with no 256/8-color fallback. Basic terminals
  render wrong colors.
- **Theme must match the terminal** — the pane keeps the terminal's background, and there is no
  auto light/dark detection yet. You match the theme by hand.
- **Add / remove are red / green** — no secondary cue for colorblind users yet.
- **Box-drawing glyphs required**, but no Nerd Font.

**Platform**
- **macOS and Linux only** — no Windows.
- **Clipboard export** uses `pbcopy`, `wl-copy`, `xclip`, or `xsel`. With none installed it
  says so, and **Send** still works.

**herdr coupling**
- **Send needs an agent in the workspace** — one agent takes the comments straight away, and
  several open a picker so you choose. With no agent, Send says so and keeps your comments.
- **last turn relies on polling** (2 s default) — a turn that starts and finishes inside one
  poll is missed, and the scope shows everything since the last *observed* turn start, your
  own edits included.

**PR tab (GitHub, GitLab, and Azure DevOps)**
- **Read-only** — needs the forge's authenticated CLI (`gh`, `glab`, or `az`) and a
  recognized `upstream` or `origin`. Without either it tells you what to fix, and the other
  tabs keep working. Other forges are not supported.
- **One repository, never a cross-repository search** — a readable, recognized `upstream` is
  authoritative, otherwise `origin`. Clones that target different parent repositories stay
  separate.
- **Mirrors the branch's *open* PR or MR** — merged or closed shows as history. Each comment
  surface caps at its newest 100 rows, with a `+more` marker naming the forge when there is
  more.

**Review model**
- **Comments are in-memory and single-session** — closing the pane loses any you haven't sent
  or copied out.
- **Sending is all-or-nothing** — Send (or copy) delivers the whole set and clears it. A
  failure leaves everything in place.
- **No line-number rebasing** — a comment stays locatable by its diff snippet, not its line
  number. reviewr flags a stale comment instead of dropping it.

**Budgets**
- Files over 2 MB or 50,000 lines show a "too large" notice. Binary files get no diff.

## Building from source

For the dev setup, tests, and benchmarks, see [CONTRIBUTING.md](CONTRIBUTING.md). To run your
own build inside herdr panes, link the checkout. `herdr plugin link` runs the binary you build
at `bin/herdr-reviewr`:

```bash
git clone https://github.com/persiyanov/herdr-reviewr
cd herdr-reviewr
just install   # build release → bin/herdr-reviewr, ad-hoc re-signed on macOS
herdr plugin link .
```

After every `just install`, toggle the reviewr pane off and on. An open pane keeps running the old
process. The loop only works while the plugin is linked: a `github:…` source in
`herdr plugin list` runs a downloaded binary that local rebuilds never touch. Switch with:

```bash
herdr plugin uninstall persiyanov.reviewr   # config is keyed by id and survives
herdr plugin link .
```

## Roadmap

Structured (JSON) export, a side-by-side split view, mark-file-reviewed,
named-key notation for keybindings, OSC light/dark theme autodetect, more themes
(`kanagawa`, `vesper`, `everforest`, `ayu`, a dark `github`), a `terminal`-following palette,
and OSC 52 clipboard.

## License

[MIT](LICENSE). Syntax highlighting comes from [syntect](https://github.com/trishume/syntect)
and [two-face](https://github.com/CosmicHorrorDev/two-face). Most themes' syntax colors come
from two-face's bundled set.

Bundled `.tmTheme` syntax files in `assets/`, each under its own license:

- [Catppuccin Mocha](https://github.com/catppuccin/bat) — MIT.
- [Tokyo Night](https://github.com/folke/tokyonight.nvim) (`tokyo-night`, `tokyo-night-day`) — Apache-2.0.
- [Rosé Pine](https://github.com/rose-pine/tm-theme) (`rose-pine`, `rose-pine-dawn`) — MIT.
