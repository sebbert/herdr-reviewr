//! Render tests: drive `ui::render` through ratatui's `TestBackend` and assert on
//! the painted buffer, so the layout and component wiring are checked for real.

mod common;

use common::{Repo, app_on, enter_tab};
use herdr_reviewr::app::{App, BaseChoice, BasePicker, BaseProbe, Focus, Mode, Tab};
use herdr_reviewr::config::NavigatorPosition;
use herdr_reviewr::herdr::AgentChoice;
use herdr_reviewr::keymap::Keymap;
use herdr_reviewr::model::Scope;
use herdr_reviewr::ui::{self, HeaderHit};
use herdr_reviewr::{handle_key, handle_mouse};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

fn dump(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

fn render(app: &App) -> String {
    dump(&render_size(app, 140, 40))
}

/// Render and return the buffer, for cell-style assertions.
fn render_buffer(app: &App) -> Buffer {
    render_size(app, 140, 40)
}

/// Render at a specific width (height fixed), for footer fit-to-width assertions.
fn render_at(app: &App, width: u16) -> String {
    dump(&render_size(app, width, 12))
}

fn render_size(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// Catppuccin surface2 — the shared selection/cursor fill.
const SELECTION_BG: ratatui::style::Color = ratatui::style::Color::Rgb(0x58, 0x5b, 0x70);
/// Catppuccin orange — the comment-editor caret block.
const PEACH: ratatui::style::Color = ratatui::style::Color::Rgb(0xfa, 0xb3, 0x87);

/// The right `100-pct`% of every frame row, for pane-scoped assertions — one home for
/// the column math, so the two panes' cut points can't drift apart silently.
fn right_column(out: &str, pct: usize) -> String {
    out.lines()
        .map(|l| l.chars().skip(l.chars().count() * pct / 100).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The read pane's inner text on every frame row: the columns between the left pane's
/// outer borders (so never its scrollbar thumb), found from the top border's `┐┌` seam.
fn read_column(out: &str) -> Vec<String> {
    let seam = out.lines().nth(1).and_then(|l| l.chars().position(|c| c == '┐')).unwrap_or(0);
    out.lines()
        .map(|l| l.chars().skip(1).take(seam.saturating_sub(1)).collect::<String>())
        .collect()
}

/// The border colour of the read-pane box whose top border opens with `header`.
fn corner_of(buf: &Buffer, header: &str) -> Option<ratatui::style::Color> {
    let read = read_column(&dump(buf));
    // A foldable card's header opens with its fold marker.
    let y = read.iter().position(|l| {
        ["", "▸ ", "▾ "].iter().any(|mark| l.starts_with(&format!("╭─ {mark}{header}")))
    })?;
    Some(buf[(1, y as u16)].fg)
}

/// The first painted link region anywhere on the test frame, scanned over its grid.
fn first_painted_link(app: &App) -> Option<std::sync::Arc<str>> {
    (0..40u16)
        .flat_map(|y| (0..140u16).map(move |x| (x, y)))
        .find_map(|(x, y)| app.painted_link_at(x, y))
}

/// Open the comment composer on the first changed line of `edited_app`.
fn composing(app: &mut App) {
    app.focus = Focus::Diff;
    app.diff_cursor = app.visible.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
}

#[test]
fn invalid_config_replaces_the_entire_pane_with_its_error() {
    let mut app = edited_app();
    app.set_config_error(
        "config /tmp/reviewr/config.toml: invalid value for `theme`; expected a built-in theme name"
            .to_string(),
    );

    let out = render(&app);

    assert!(out.contains("config /tmp/reviewr/config.toml"));
    assert!(out.contains("expected a built-in theme name"));
    assert!(out.contains("The config reloads automatically."));
    assert!(!out.contains("Changes"), "normal reviewr chrome must be hidden");
}

#[test]
fn the_empty_comment_box_shows_a_placeholder() {
    let mut app = edited_app();
    composing(&mut app);
    assert!(render(&app).contains("Leave a comment…"), "an empty box shows the placeholder");
}

#[test]
fn the_caret_block_sits_on_the_character_at_the_caret() {
    let mut app = edited_app();
    composing(&mut app);
    app.input_push('a');
    app.input_push('b');
    app.caret_left(); // caret between 'a' and 'b' → block over 'b'
    let buf = render_buffer(&app);
    let mut found = false;
    for y in 0..40 {
        for x in 0..140 {
            if buf.cell((x, y)).is_some_and(|c| c.bg == PEACH && c.symbol() == "b") {
                found = true;
            }
        }
    }
    assert!(found, "the caret block highlights the character at the caret");
}

#[test]
fn backspacing_a_wide_character_leaves_the_terminal_cursor_unpainted() {
    let mut app = edited_app();
    composing(&mut app);
    app.input_push('日');
    app.input_push('本');
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    let before = terminal.backend().cursor_position();
    app.input_backspace();
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    let cursor = terminal.backend().cursor_position();
    assert_eq!(
        (cursor.x + 2, cursor.y),
        (before.x, before.y),
        "the cursor retreats one wide character"
    );
    let cell = terminal.backend().buffer().cell(cursor).unwrap();
    assert_eq!(cell.bg, ratatui::style::Color::Reset);
    assert_eq!(cell.symbol(), " ");
}

#[test]
fn the_base_picker_anchors_the_terminal_cursor_at_its_caret() {
    let mut app = edited_app();
    app.base_picker = Some(BasePicker {
        rows: vec![BaseChoice::Branch {
            name: "main".to_string(),
            pr_base: false,
            is_default: true,
            current: false,
            tip_secs: 1,
        }],
        cursor: 0,
        query: String::new(),
        caret: 0,
        probe: BaseProbe::Idle,
    });
    app.mode = Mode::BasePick;
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();
    let empty = terminal.backend().cursor_position();

    app.input_push('日');
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    let after = terminal.backend().cursor_position();
    assert_eq!(
        (after.x, after.y),
        (empty.x + 2, empty.y),
        "the cursor advances one wide character"
    );
    let cell = terminal.backend().buffer().cell(after).unwrap();
    assert_eq!(
        cell.bg,
        ratatui::style::Color::Reset,
        "end of input leaves the cursor cell unpainted"
    );
}

#[test]
fn a_height_capped_composer_scrolls_to_keep_the_caret_visible() {
    let mut app = edited_app();
    composing(&mut app);
    for _ in 0..599 {
        app.input_push('x');
    }
    app.input_push('z'); // the unique last character locates the caret in the buffer
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    let buffer = terminal.backend().buffer();
    let cursor = terminal.backend().cursor_position();
    let (zx, zy) = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
        .find(|&(x, y)| buffer.cell((x, y)).unwrap().symbol() == "z")
        .expect("the box scrolled the last typed character into view");
    // The cursor sits where the next character lands: right after `z`, or on the first
    // text column of the fresh row below when `z` exactly filled its row.
    let inline = (cursor.x, cursor.y) == (zx + 1, zy);
    let row_start =
        (0..buffer.area.width).find(|&x| buffer.cell((x, zy)).unwrap().symbol() == "x").unwrap();
    let wrapped = (cursor.x, cursor.y) == (row_start, zy + 1);
    assert!(
        inline || wrapped,
        "the cursor sits after the text (cursor {cursor:?}, z at ({zx},{zy}))"
    );
    assert_eq!(
        buffer.cell(cursor).unwrap().symbol(),
        " ",
        "end of input leaves the cursor cell blank"
    );
}

#[test]
fn the_find_band_anchors_the_terminal_cursor_at_its_caret() {
    let r = Repo::init();
    r.write("base.txt", "x\n");
    r.commit_all("init");
    r.write("m.rs", "let total = 1;\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 140, 40);
    handle_key(&mut app, KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), area, &keymap)
        .unwrap();

    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();
    let empty = terminal.backend().cursor_position();

    handle_key(&mut app, KeyEvent::from(KeyCode::Char('日')), area, &keymap).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    let after = terminal.backend().cursor_position();
    assert_eq!(
        (after.x, after.y),
        (empty.x + 2, empty.y),
        "the cursor advances one wide character"
    );
}

#[test]
fn caret_vertical_moves_between_wrapped_rows() {
    // "abcdef" hard-wraps at width 3 to "abc"/"def"; caret 4 (def col 1) up → 1; 1 down → 4.
    assert_eq!(ui::caret_vertical("abcdef", 4, 3, false), 1);
    assert_eq!(ui::caret_vertical("abcdef", 1, 3, true), 4);
    // Composer wrapping preserves repeated spaces so every caret index remains addressable.
    assert_eq!(ui::caret_vertical("ab  cd", 4, 2, false), 2);
    assert_eq!(ui::caret_vertical("ab  cd", 2, 2, true), 4);
    // A line exactly filling the width adds no phantom row, so one step crosses it.
    assert_eq!(ui::caret_vertical("abc\ndef", 0, 3, true), 4);
    assert_eq!(ui::caret_vertical("abc\ndef", 4, 3, false), 0);
    // The caret past the full line sits visually on the next row, and motion agrees.
    assert_eq!(ui::caret_vertical("abc\ndef", 3, 3, false), 0);
    assert_eq!(ui::caret_vertical("abc\ndef", 3, 3, true), 7);
}

#[test]
fn the_fold_hint_names_the_expand_binding() {
    use std::fmt::Write as _;
    let r = Repo::init();
    let mut body = String::new();
    for i in 0..30 {
        let _ = writeln!(body, "line {i}");
    }
    r.write("f.rs", &body);
    r.commit_all("init");
    r.write("f.rs", &body.replace("line 15", "LINE 15")); // one change, long runs fold
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    app.diff_cursor = app.visible.iter().position(|row| row.hidden() > 0).expect("a fold row");

    let out = render(&app);
    assert!(out.contains("→ expand"), "the fold hint names the `→` key");
    assert!(!out.contains("⏎ expand"), "no stale enter hint remains");

    // A rebound `expand` renames the fold row's inline label and the footer hint alike
    // (a hint shows the action's first bound key).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[keybindings]\nexpand = [\"x\"]\n").unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    let out = render(&app);
    assert!(out.contains("x expand"), "the rebound key names the hint:\n{out}");
    assert!(!out.contains("→ expand"), "the freed arrow leaves the hint");
}

fn edited_app() -> App {
    let r = Repo::init();
    r.write("hello.rs", "alpha\nbeta\n");
    r.commit_all("init");
    r.write("hello.rs", "alpha\nBETA\n");
    // The repo is only needed through reload(); rendering reads cached state, so
    // `r` can drop here and clean up its tempdir.
    app_on(&r)
}

#[test]
fn the_file_list_renders_as_a_directory_tree() {
    let r = Repo::init();
    r.write("src/app.rs", "x\n");
    r.write("src/ui.rs", "y\n");
    r.write("Cargo.toml", "[package]\n");
    r.commit_all("init");
    r.write("src/app.rs", "x2\n");
    r.write("src/ui.rs", "y2\n");
    r.write("Cargo.toml", "[package]\nname='z'\n");
    let app = app_on(&r);

    // Scan only the default-right navigator so the diff header — which does show
    // the open file's full path — doesn't confuse the assertions.
    let files_pane = right_column(&render(&app), 70);
    assert!(files_pane.contains("src/"), "the directory groups its files: {files_pane:?}");
    assert!(files_pane.contains("app.rs") && files_pane.contains("ui.rs"), "files by basename");
    assert!(!files_pane.contains("src/app.rs"), "a grouped file is not shown by full path");
    assert!(files_pane.contains("Cargo.toml"), "the top-level file shows too");
}

#[test]
fn an_expanded_directory_nests_its_children() {
    // All files paints unchanged rows without a marker. Those two columns must still
    // hold the chevron's width, or child names line up with the parent.
    let r = Repo::init();
    r.write("src/app.rs", "x\n");
    r.write("src/ui.rs", "y\n");
    r.write("tests/a.rs", "a\n");
    r.write("tests/b.rs", "b\n");
    r.write("README.md", "hi\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    app.focus = Focus::Files;
    app.file_cursor = app.file_rows.iter().position(|r| r.dir_path() == Some("src")).unwrap();
    app.expand_dir();

    let buf = render_buffer(&app);
    // Search from the files pane so a left-pane path cannot steal the match.
    let files_x0 = 140 - 140 * 32 / 100 + 1;
    let src = token_x(&buf, "src/", files_x0);
    let tests = token_x(&buf, "tests/", files_x0);
    let readme = token_x(&buf, "README.md", files_x0);
    let app_rs = token_x(&buf, "app.rs", files_x0);
    assert_eq!(src, tests, "sibling directories share a name column");
    assert_eq!(src, readme, "a root file name lines up with a root directory");
    assert!(app_rs > src, "a child file sits to the right of its parent: {app_rs} vs {src}");
}

/// First painted column of `token` in `buf` at or after `x0`. Panics if it never appears.
fn token_x(buf: &Buffer, token: &str, x0: u16) -> u16 {
    let chars: Vec<char> = token.chars().collect();
    let n = chars.len() as u16;
    for y in 0..buf.area.height {
        for x in x0..buf.area.width.saturating_sub(n) {
            let hit = (0..n).all(|i| {
                buf.cell((x + i, y)).is_some_and(|c| c.symbol() == chars[i as usize].to_string())
            });
            if hit {
                return x;
            }
        }
    }
    panic!("{token} was not painted at x>={x0}");
}

#[test]
fn a_saved_comment_renders_inline_as_a_card() {
    let r = Repo::init();
    r.write("a.rs", "alpha\nbeta\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nBETA\n");
    let mut app = app_on(&r);

    app.focus = Focus::Diff;
    app.diff_cursor = app.visible.iter().position(|row| row.marker() == '+').unwrap();
    app.start_comment();
    for ch in "memoize this".chars() {
        app.input_push(ch);
    }
    app.submit_comment(); // box closes, comment saved

    let out = render(&app);
    assert!(out.contains("memoize this"), "the saved comment stays visible inline: {out:?}");
    assert!(out.contains("comment ·"), "the inline card is titled with the location");
}

#[test]
fn a_renamed_file_shows_old_arrow_new_in_the_header() {
    let r = Repo::init();
    r.write("old_name.rs", "stable contents that survive the move\nplus a second line\n");
    r.commit_all("init");
    r.git(&["mv", "old_name.rs", "new_name.rs"]);
    r.write("new_name.rs", "stable contents that survive the move\nplus an edited line\n");
    let app = app_on(&r);

    let out = render(&app);
    assert!(out.contains("old_name.rs → new_name.rs"), "header shows the rename: {out:?}");
}

#[test]
fn tabs_expand_to_spaces_in_the_diff() {
    let r = Repo::init();
    r.write("t.rs", "x\n");
    r.commit_all("init");
    r.write("t.rs", "x\n\tindented\n"); // a tab-indented added line
    let app = app_on(&r);
    let out = render(&app);
    let line = out.lines().find(|l| l.contains("indented")).expect("the added line renders");
    // The literal tab is gone; the word is preceded by spaces (4-col tab stop).
    assert!(!line.contains('\t'), "no literal tab in the rendered line");
    assert!(line.contains("    indented") || line.contains("   indented"), "tab became spaces");
}

#[test]
fn a_long_line_wraps_across_display_rows() {
    let long: String = std::iter::repeat_n("abcd", 60).collect(); // 240 cols, wider than the pane
    let r = Repo::init();
    r.write("w.rs", "x\n");
    r.commit_all("init");
    r.write("w.rs", &format!("x\n{long}\n"));
    let app = app_on(&r); // wrap defaults on

    // The whole long line is visible (no truncation): every chunk renders.
    let shown: String = render(&app).chars().filter(|c| *c == 'a').collect();
    assert!(shown.len() >= 60, "all of the wrapped line is shown, not truncated");
    // The logical row reports a display height > 1 (it wraps).
    let heights = ui::diff_row_heights(&app, AREA);
    let wrapped = app.visible.iter().position(|r| r.text().starts_with("abcd")).unwrap();
    assert!(heights[wrapped] > 1, "the long line spans multiple display rows");
}

#[test]
fn wrapping_breaks_at_word_boundaries() {
    // Words sized so the line must wrap, but no word is wider than the pane: every break
    // should land on a space, so no word is split across two display rows.
    let words = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima \
                 mike november oscar papa quebec romeo sierra tango";
    let r = Repo::init();
    r.write("w.rs", "x\n");
    r.commit_all("init");
    r.write("w.rs", &format!("x\n{words}\n"));
    let app = app_on(&r); // wrap defaults on

    let heights = ui::diff_row_heights(&app, AREA);
    let wrapped = app.visible.iter().position(|r| r.text().starts_with("alpha")).unwrap();
    assert!(heights[wrapped] > 1, "the line wraps across rows");

    // Every word survives intact on some rendered line (none straddles a wrap break).
    let out = render(&app);
    for word in words.split(' ') {
        assert!(out.lines().any(|l| l.contains(word)), "word {word:?} is not split across rows");
    }
}

#[test]
fn wide_glyphs_wrap_by_column_width_not_char_count() {
    // 50 wide CJK glyphs span 100 columns; 50 ASCII chars span 50. Width-aware wrapping
    // must give the CJK line more display rows — a char-counting wrap would tie them.
    let cjk: String = std::iter::repeat_n('あ', 50).collect();
    let ascii: String = std::iter::repeat_n('a', 50).collect();
    let r = Repo::init();
    r.write("w.rs", "x\n");
    r.commit_all("init");
    r.write("w.rs", &format!("x\n{ascii}\n{cjk}\n"));
    let app = app_on(&r); // wrap defaults on

    let heights = ui::diff_row_heights(&app, AREA);
    let ascii_h = heights[app.visible.iter().position(|r| r.text().starts_with('a')).unwrap()];
    let cjk_h = heights[app.visible.iter().position(|r| r.text().starts_with('あ')).unwrap()];
    assert!(cjk_h > ascii_h, "wide glyphs wrap by columns: cjk {cjk_h} > ascii {ascii_h}");
}

#[test]
fn horizontal_scroll_shifts_the_diff_left() {
    let r = Repo::init();
    r.write("w.rs", "x\n");
    r.commit_all("init");
    r.write("w.rs", "x\nAAAABBBBCCCCDDDD_marker\n");
    let mut app = App::new(r.path_buf(), Scope::Uncommitted, None);
    app.wrap = false; // horizontal scroll applies only with wrap off
    app.reload().unwrap();
    assert!(render(&app).contains("AAAABBBB"), "the line head shows before scrolling");

    app.scroll_h(8); // drop the first 8 code columns
    let out = render(&app);
    assert!(!out.contains("AAAABBBB"), "the scrolled-off head is gone");
    assert!(out.contains("CCCCDDDD_marker"), "the later columns are now visible");
}

#[test]
fn a_changed_word_gets_the_emphasis_background() {
    const EMPH_INS_BG: ratatui::style::Color = ratatui::style::Color::Rgb(0x30, 0x55, 0x3f);
    let r = Repo::init();
    r.write("e.rs", "let x = foo(a);\n");
    r.commit_all("init");
    r.write("e.rs", "let x = bar(a, b);\n");
    let mut app = app_on(&r);
    app.focus = Focus::Files; // no diff cursor, so the emphasis bg shows
    let buf = render_buffer(&app);

    // Somewhere in the diff pane a cell carries the brighter insertion-emphasis bg,
    // and it sits under a changed character (a `b` from `bar`), not the shared prefix.
    let mut found = false;
    for y in 0..40 {
        for x in 0..95 {
            if let Some(c) = buf.cell((x, y))
                && c.bg == EMPH_INS_BG
                && c.symbol() == "b"
            {
                found = true;
            }
        }
    }
    assert!(found, "a changed word carries the emphasis background");
}

/// Catppuccin surface1 — the cursor fill of the pane that does not hold focus.
const UNFOCUSED_CURSOR_BG: ratatui::style::Color = ratatui::style::Color::Rgb(0x45, 0x47, 0x5a);

#[test]
fn the_diff_cursor_row_is_marked_from_either_pane() {
    // The diff pane's cursor row fills like the file list's: brightest when the pane holds
    // focus, a step softer when it does not. A hunk step driven from the file list moves this
    // cursor, so it has to be visible from there.
    let mut app = edited_app();
    app.focus = Focus::Diff;
    app.next_hunk();
    let cursor_y = |app: &App| 2 + app.diff_cursor as u16; // border at y=1, first row at y=2
    let fill = |app: &App, bg| {
        let buf = render_buffer(app);
        let y = cursor_y(app);
        (1..40u16).filter(|&x| buf.cell((x, y)).is_some_and(|c| c.bg == bg)).count()
    };

    assert!(fill(&app, SELECTION_BG) > 10, "the focused diff fills its cursor row with surface2");

    app.focus = Focus::Files;
    assert!(
        fill(&app, UNFOCUSED_CURSOR_BG) > 10,
        "and still marks it, a step softer, while the file list holds focus"
    );
}

#[test]
fn the_selected_file_row_fills_with_the_shared_selection_color() {
    let app = edited_app(); // one file, file_cursor = 0, Files focused
    let buf = render_buffer(&app);
    // Files pane: right 32% of 140 cols; its border is at y=1, first content row at y=2.
    let files_x0 = 140 - 140 * 32 / 100 + 1;
    let selected =
        (files_x0..139).filter(|&x| buf.cell((x, 2)).is_some_and(|c| c.bg == SELECTION_BG)).count();
    assert!(selected > 10, "the selected file row fills wide with surface2: {selected} cells");
}

#[test]
fn a_hidden_navigator_gives_the_read_pane_the_whole_body() {
    let mut app = edited_app();
    app.focus = Focus::Diff;
    app.next_hunk();
    let cursor_y = 2 + app.diff_cursor as u16;
    let fill = |app: &App| {
        let buf = render_buffer(app);
        (1..139u16)
            .filter(|&x| buf.cell((x, cursor_y)).is_some_and(|c| c.bg == SELECTION_BG))
            .count()
    };
    let visible_fill = fill(&app);
    let out = render(&app);
    assert!(!out.contains("z hide"), "visible and collapsed, the hide key waits under `?`");

    app.toggle_navigator_hidden();
    let hidden_fill = fill(&app);
    assert!(
        hidden_fill > visible_fill && hidden_fill > 120,
        "the cursor row fills the whole body with surface2: {hidden_fill} vs {visible_fill}"
    );
    let out = render(&app);
    assert!(out.contains("z show"), "the collapsed footer names the way back");

    app.toggle_keys();
    let out = render(&app);
    assert!(out.contains("z show"), "row 1 keeps the way back in the expansion");
    assert!(!out.contains("p layout"), "`p layout` drops while hidden");

    app.toggle_navigator_hidden();
    let out = render(&app);
    assert!(out.contains("z hide"), "visible, the `go` band lists the hide key");
    assert!(out.contains("p layout"), "`p layout` returns with the navigator");
}

#[test]
fn shows_tab_bar_file_list_and_diff() {
    let app = edited_app();
    let out = render(&app);
    assert!(out.contains("Changes"), "tab bar names the view");
    assert!(out.contains("uncommitted"), "current scope shown");
    assert!(out.contains("hello.rs"), "file appears in the list");
    assert!(out.contains("BETA"), "diff content is rendered");
    assert!(out.contains("changed"), "the header shows the changed count");
}

#[test]
fn the_header_totals_the_scope_and_hides_them_at_zero() {
    let r = Repo::init();
    r.write("edited.rs", "old\n");
    r.commit_all("init");
    r.write("edited.rs", "new\n");
    r.write("untracked.rs", "one\ntwo\n");
    let app = app_on(&r);

    // 64 columns is the exact fit (the tab strip ends in the two-column reserved
    // indicator cell). The totals' `−` is multi-byte, so this breaks if the header
    // measures bytes instead of display width.
    let header = render_at(&app, 64).lines().next().unwrap().to_string();
    assert!(header.contains("2 changed  +3 −1"), "count, then the totals:\n{header}");

    let clean = Repo::init();
    clean.write("clean.rs", "same\n");
    clean.commit_all("init");
    let app = app_on(&clean);
    let header = render_at(&app, 80).lines().next().unwrap().to_string();
    assert!(header.contains("0 changed"), "the bare count remains:\n{header}");
    assert!(!header.contains('+'), "an empty changeset shows no totals:\n{header}");
}

/// The last non-blank rendered row — the footer band.
fn footer_line(out: &str) -> String {
    out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string()
}

/// Focus the diff on its first changed line.
fn on_changed_line(app: &mut App) {
    app.focus = Focus::Diff;
    app.diff_cursor = app.visible.iter().position(|r| r.marker() == '+').unwrap();
}

#[test]
fn the_footer_offers_the_armed_crossing_in_both_directions() {
    // Two files, one hunk each, so a hunk step from either end has only a crossing left to offer.
    let r = Repo::init();
    r.write("a.rs", "one\ntwo\n");
    r.write("z.rs", "one\ntwo\n");
    r.commit_all("init");
    r.write("a.rs", "one\nEDIT A\n");
    r.write("z.rs", "one\nEDIT Z\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;

    app.next_hunk(); // onto a.rs's only hunk
    app.next_hunk(); // nothing below it: arms the crossing forward
    let footer = footer_line(&render(&app));
    assert!(footer.contains("] next file"), "the armed crossing leads the bar:\n{footer}");
    assert!(footer.contains("c comment"), "and the line's own action stays:\n{footer}");

    app.next_hunk(); // takes it
    app.prev_hunk(); // nothing above z.rs's hunk: arms the crossing back
    let footer = footer_line(&render(&app));
    assert!(footer.contains("[ prev file"), "armed backward, the bar names `[`:\n{footer}");
}

#[test]
fn the_footer_shows_the_action_for_the_context() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    let footer = footer_line(&render(&app));
    assert!(footer.contains("c comment"), "a diff line offers comment:\n{footer}");
    assert!(footer.contains("v select"), "and selecting a range:\n{footer}");
    assert!(!footer.contains("changed"), "the changed count is not in the footer:\n{footer}");
}

#[test]
fn the_footer_trims_trailing_actions_to_fit_keeping_the_primary_and_the_more_hint() {
    let mut app = edited_app();
    on_changed_line(&mut app); // diff focus, content line → c comment · v select … ?
    // Wide: every cursor action fits, and the `?` closes the row.
    let wide = footer_line(&render_at(&app, 120));
    assert!(
        wide.contains("c comment") && wide.contains("v select") && wide.trim_end().ends_with('?'),
        "wide footer shows all actions and the `?`:\n{wide}"
    );
    // Narrow: the primary survives, the trailing action drops, and the `?` stays at the right.
    let narrow = footer_line(&render_at(&app, 18));
    assert!(narrow.contains("c comment"), "the primary action is never dropped:\n{narrow}");
    assert!(narrow.trim_end().ends_with('?'), "the `?` never drops:\n{narrow}");
    assert!(!narrow.contains("v select"), "the trailing action is trimmed off row 1:\n{narrow}");
    // Too narrow for the primary and the `?` together: the primary sheds its label to its key, and
    // the `?` still survives at the right.
    let tiny = footer_line(&render_at(&app, 11));
    assert!(tiny.contains(" c "), "the primary keeps its key:\n{tiny}");
    assert!(!tiny.contains("comment"), "the primary sheds its label:\n{tiny}");
    assert!(tiny.trim_end().ends_with('?'), "the `?` still survives:\n{tiny}");
}

#[test]
fn a_narrow_row_keeps_send_and_the_more_hint_by_shedding_the_primary_label() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.start_comment();
    for ch in "n".chars() {
        app.input_push(ch);
    }
    app.submit_comment(); // a written comment adds `s send 1` to row 1
    // A pane too narrow for the full primary alongside `send` and `?` keeps all three by shedding
    // the primary's label — `send` and the `?` must never clip off the right edge.
    let narrow = footer_line(&render_at(&app, 16));
    assert!(narrow.contains("s send 1"), "send never drops:\n{narrow}");
    assert!(narrow.trim_end().ends_with('?'), "the `?` never drops:\n{narrow}");
    assert!(narrow.chars().count() <= 16, "the row never overflows its width:\n{narrow}");
}

#[test]
fn a_status_too_long_to_paint_never_costs_the_row_the_actions_that_fit() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.start_comment();
    app.input_push('n');
    app.submit_comment(); // a written comment adds `s send 1` to row 1

    // A herdr failure is the longest line the status ever carries. Where the row has no room to
    // paint any of it, the status must cost nothing: the row falls back to exactly what it shows
    // with no status at all. Reserving room for a message that then drops would spend the width
    // twice and paint neither.
    for w in 14..=140u16 {
        app.status = "z".repeat(60);
        let with = footer_line(&render_at(&app, w));
        app.status = String::new();
        let without = footer_line(&render_at(&app, w));
        if !with.contains('z') {
            assert_eq!(with, without, "width {w} paid for a status it never painted");
        }
    }
}

#[test]
fn the_footer_shows_the_sends_outcome_at_a_pane_width_by_yielding_the_cursor_actions() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.start_comment();
    app.input_push('n');
    app.submit_comment(); // a written comment adds `s send 1` to row 1

    // The status is the only answer `s` gives, and a reviewr pane is around 40 columns wide, so
    // the cursor's actions yield to it: the `?` panel repeats every action and nothing repeats the
    // status.
    app.status = "no agent here — copy to the clipboard instead".to_string();
    let narrow = footer_line(&render_at(&app, 40));
    assert!(narrow.contains("no agent here"), "the refusal shows at 40 columns:\n{narrow}");
    assert!(narrow.contains("s send 1"), "send never drops:\n{narrow}");
    assert!(narrow.trim_end().ends_with('?'), "the `?` never drops:\n{narrow}");
    assert!(!narrow.contains("d delete"), "the cursor's actions yield to the status:\n{narrow}");

    // With room for both, nothing yields.
    let wide = footer_line(&render_at(&app, 120));
    assert!(
        wide.contains("no agent here — copy to the clipboard instead"),
        "a wide row shows the whole refusal:\n{wide}"
    );
    assert!(wide.contains("d delete"), "and keeps the cursor's actions:\n{wide}");

    // Below a legible width the status drops rather than paint a lone `·` promising a message.
    let tiny = footer_line(&render_at(&app, 20));
    assert!(!tiny.contains("agent"), "no room for a legible message, so none is painted:\n{tiny}");
    assert!(tiny.contains("s send 1"), "send still never drops:\n{tiny}");

    // A truncated status never pushes the `?` off the right edge, at any width that fits row 1's
    // own fixed parts. Below 14 columns the shed primary and `send` overflow it on their own, with
    // no status in play at all.
    for w in 14..=140u16 {
        let row = footer_line(&render_at(&app, w));
        assert!(row.trim_end().ends_with('?'), "the `?` left the row at width {w}:\n{row}");
    }

    // `s` is also the comments list's primary, so a refusal has to reach the reviewer there too.
    // The list has no `?`, so its trailing `…` is the only promise the trimmed actions exist, and
    // the status leaves room for it.
    app.open_list();
    let listed = footer_line(&render_at(&app, 40));
    assert!(listed.contains("no agent here"), "the refusal shows in the list at 40:\n{listed}");
    assert!(listed.contains("s send 1"), "send never drops in the list either:\n{listed}");
    assert!(listed.trim_end().ends_with('…'), "the trimmed actions keep their `…`:\n{listed}");
}

#[test]
fn the_expansion_aligns_row_one_into_the_labeled_grid() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.toggle_keys();
    let out = render(&app);
    // Search only the footer rows, so a stray `move`/`go` in the diff or file list can't stand in.
    let footer_start = ui::body_rect(Rect::new(0, 0, 140, 40), &app);
    let footer_start = (footer_start.y + footer_start.height) as usize;
    let line_of = |lbl: &str| {
        out.lines()
            .skip(footer_start)
            .find(|l| l.trim_start().starts_with(lbl))
            .unwrap_or("")
            .to_string()
    };
    let (do_line, go_line, move_line) = (line_of("do"), line_of("go"), line_of("move"));

    // Row 1 is now the `do` band: the primary, and the `?` still at the right.
    assert!(
        do_line.contains("c comment") && do_line.trim_end().ends_with('?'),
        "row 1 is the `do` line with the primary and `?`:\n{do_line}"
    );
    assert!(go_line.contains("scope"), "the go band lists the always-there keys:\n{go_line}");
    assert!(
        move_line.contains("hunk") && move_line.contains("file"),
        "the move band names the hunk and file steps:\n{move_line}"
    );
    // The three labels share one gutter column, and their content aligns in the next.
    let at = |l: &str, s: &str| l.find(s).expect("token present");
    assert_eq!(at(&do_line, "do"), at(&go_line, "go"), "labels share a gutter column");
    assert_eq!(at(&go_line, "go"), at(&move_line, "move"), "labels share a gutter column");
    assert_eq!(
        at(&do_line, "c comment"),
        at(&go_line, "u/b/t"),
        "the primary aligns under the same column as the band keys"
    );
    assert_eq!(at(&go_line, "u/b/t"), at(&move_line, "j k"), "band keys align in one column");
}

#[test]
fn the_collapsed_footer_stays_a_flush_action_bar() {
    let mut app = edited_app();
    on_changed_line(&mut app); // collapsed: no expansion
    let footer = footer_line(&render(&app));
    assert!(
        footer.trim_start().starts_with("c comment"),
        "no `do` gutter when collapsed:\n{footer}"
    );
    assert!(!footer.contains(" do "), "the `do` label appears only when expanded:\n{footer}");
}

#[test]
fn the_expanded_row_one_never_drops_send_or_the_more_hint_on_a_narrow_pane() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.start_comment();
    for ch in "n".chars() {
        app.input_push(ch);
    }
    app.submit_comment(); // a written comment puts `s send 1` on row 1
    app.toggle_keys(); // expanded — the fixed `do` gutter cannot shed
    for w in [14u16, 16, 18, 20, 22, 30] {
        let out = dump(&render_size(&app, w, 40));
        let row1 = out.lines().find(|l| l.contains("send")).expect("row 1 carries send");
        let row1 = row1.trim_end();
        assert!(row1.contains("s send 1"), "send survives at w={w}: [{row1}]");
        assert!(row1.ends_with('?'), "the `?` survives at w={w}: [{row1}]");
        assert!(row1.chars().count() <= w as usize, "row 1 never overflows at w={w}: [{row1}]");
    }
}

#[test]
fn the_expansion_caps_so_the_body_keeps_its_rows() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.toggle_keys();
    // On a short pane the wrapped bands would want more rows than fit, but the footer is capped so
    // the body keeps its Min(3).
    let body = ui::body_rect(Rect::new(0, 0, 40, 6), &app);
    assert!(body.height >= 3, "the body keeps at least three rows: got {}", body.height);
}

#[test]
fn the_pr_footer_keeps_the_open_action_when_the_state_line_is_long() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::{Check, CheckStatus, Merge, PrSnapshot, PrView, Sync};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        number: 226,
        merge: Merge::Conflicting, // a long state line: conflicts · behind · failing · +more
        sync: Sync::Behind(3),
        checks: vec![Check { name: "ci".into(), status: CheckStatus::Failure, url: None }],
        comments_truncated: true,
        checks_truncated: true,
        ..common::pr_snapshot()
    }));
    // At narrow width the state line is capped so the primary `o open ↗` is never crowded off.
    let footer = footer_line(&render_at(&app, 60));
    assert!(footer.contains("o open"), "the open action survives a long state line:\n{footer}");
}

#[test]
fn the_pr_footer_names_a_capped_list_in_the_pane() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::{Check, CheckStatus, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::Pr(Box::new(PrSnapshot { comments_truncated: true, ..common::pr_snapshot() }));
    let footer = footer_line(&render(&app));
    assert!(footer.contains("newest 100 comments"), "{footer}");
    assert!(!footer.contains("+more on"), "{footer}");

    app.pr = PrView::Pr(Box::new(PrSnapshot {
        checks: vec![Check { name: "ci".into(), status: CheckStatus::Success, url: None }],
        checks_truncated: true,
        ..common::pr_snapshot()
    }));
    let footer = footer_line(&render(&app));
    assert!(footer.contains("newest 100 checks"), "{footer}");
    assert!(!footer.contains("+more on"), "{footer}");

    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments_truncated: true,
        checks_truncated: true,
        ..common::pr_snapshot()
    }));
    let footer = footer_line(&render(&app));
    assert!(footer.contains("newest 100 comments"), "{footer}");
    assert!(footer.contains("newest 100 checks"), "{footer}");
}

#[test]
fn pr_header_names_the_resolved_branch_and_marks_a_fork() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let snap = |fork: bool| {
        PrView::Pr(Box::new(PrSnapshot {
            number: 226,
            head_ref: "persiyanov/feature".into(),
            head_is_fork: fork,
            ..common::pr_snapshot()
        }))
    };
    // The header shows the branch that resolved — it can differ from the local branch —
    // and marks a fork head, so a same-named fork PR is visible.
    app.pr = snap(false);
    let header = render(&app).lines().next().unwrap().to_string();
    assert!(header.contains("persiyanov/feature"), "resolved branch in the header:\n{header}");
    assert!(!header.contains('⑂'), "no fork marker on a same-repo head:\n{header}");
    app.pr = snap(true);
    let header = render(&app).lines().next().unwrap().to_string();
    assert!(header.contains("⑂ persiyanov/feature"), "fork head is marked:\n{header}");
    // Narrow bars drop the branch first; the chip's number stays.
    app.pr = snap(false);
    let narrow = render_at(&app, 46).lines().next().unwrap().to_string();
    assert!(!narrow.contains("persiyanov/feature"), "branch drops when narrow:\n{narrow}");
    assert!(narrow.contains("#226"), "the chip survives a narrow bar:\n{narrow}");

    let width = 80;
    let area = Rect::new(0, 0, width, 12);
    let header = render_at(&app, width).lines().next().unwrap().to_string();
    let chip_start = header.find("open #226").unwrap() as u16;
    let number = header.find("#226").unwrap() as u16;
    assert!(ui::hit_pr_open(area, &app, chip_start, 0));
    assert!(ui::hit_pr_open(area, &app, number, 0));
    assert!(!ui::hit_pr_open(area, &app, chip_start - 1, 0));
    assert!(!ui::hit_pr_open(area, &app, number, 1));
}

#[test]
fn pr_empty_states_are_calm() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::PrView;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::NoPr;
    let out = render(&app);
    assert!(
        out.contains("No pull request yet. Ready to ship?"),
        "ordinary absence stays brief:\n{out}"
    );
    app.pr = PrView::Detached;
    let out = render(&app);
    assert!(
        out.contains("No pull request found — HEAD is detached."),
        "detached wording stays factual:\n{out}"
    );
    app.pr = PrView::GitError("git remote get-url upstream failed".to_string());
    let out = render(&app);
    assert!(out.contains("Git read failed"), "local failures stay factual:\n{out}");
    assert!(!out.contains("GitHub unavailable"), "a local failure is not blamed on GitHub:\n{out}");
}

#[test]
fn the_footer_keeps_its_actions_alongside_a_status() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.status = "comment added".to_string();
    let footer = footer_line(&render(&app));
    // A status sits among the actions, never replacing them.
    assert!(footer.contains("comment added"), "the status shows:\n{footer}");
    assert!(
        footer.contains("c comment"),
        "the primary action persists alongside a status:\n{footer}"
    );
}

/// The editor's failure has to reach the reviewer on the frame it happened, from either pane.
#[test]
fn the_footer_shows_an_editor_failure_from_either_pane() {
    let mut app = edited_app();
    on_changed_line(&mut app);
    app.status = "editor failed: No such file or directory (os error 2)".to_string();
    let footer = footer_line(&render(&app));
    assert!(footer.contains("editor failed"), "on the read pane:\n{footer}");

    app.focus = herdr_reviewr::app::Focus::Files;
    let footer = footer_line(&render(&app));
    assert!(footer.contains("editor failed"), "and on the navigator:\n{footer}");

    // At the pane width the reviewer actually runs, not only the test default.
    let footer = footer_line(&render_at(&app, 120));
    assert!(footer.contains("editor failed"), "at 120 columns:\n{footer}");
}

#[test]
fn empty_repo_shows_empty_states() {
    let r = Repo::init();
    r.write("seed.rs", "x\n");
    r.commit_all("init");
    let app = app_on(&r);

    let out = render(&app);
    assert!(out.contains("no changes"), "empty file list state");
}

#[test]
fn composing_renders_the_inline_multiline_box() {
    let mut app = edited_app();
    app.focus = Focus::Diff;
    app.diff_cursor = app.diff.rows.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
    for ch in "line one".chars() {
        app.input_push(ch);
    }
    app.input_push('\n');
    for ch in "line two".chars() {
        app.input_push(ch);
    }

    let out = render(&app);
    assert!(out.contains("comment ·"), "box titled with the location");
    assert!(out.contains("line one"), "first input line shown");
    assert!(out.contains("line two"), "second input line shown — the box is multi-line");
}

#[test]
fn the_box_grows_with_multiline_input_and_keeps_the_anchor_visible() {
    let r = Repo::init();
    r.write("mid.rs", "a\nb\nc\nd\ne\n");
    r.commit_all("init");
    r.write("mid.rs", "a\nB\nc\nd\ne\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    app.diff_cursor =
        app.diff.rows.iter().position(|r| r.marker() == '+' && r.text().contains('B')).unwrap();
    app.start_comment();
    for ch in "one\ntwo\nthree".chars() {
        app.input_push(ch);
    }

    let out = render(&app);
    assert!(out.contains("one") && out.contains("two") && out.contains("three"), "all box lines");
    let lines: Vec<&str> = out.lines().collect();
    // The inserted line is the only one carrying an uppercase `B` (no `+` glyph now).
    let anchor = lines.iter().position(|l| l.contains('B')).expect("anchor line visible");
    let box_row = lines.iter().position(|l| l.contains("comment ·")).expect("box");
    assert!(anchor < box_row, "the commented line stays above the box as it grows");
}

#[test]
fn the_box_is_inserted_under_the_selected_line() {
    let r = Repo::init();
    r.write("mid.rs", "alpha\nbeta\ngamma\n");
    r.commit_all("init");
    r.write("mid.rs", "alpha\nBETA\ngamma\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    app.diff_cursor = app.diff.rows.iter().position(|r| r.text().contains("BETA")).unwrap();
    app.start_comment();
    for ch in "note".chars() {
        app.input_push(ch);
    }

    let out = render(&app);
    let lines: Vec<&str> = out.lines().collect();
    let box_row = lines.iter().position(|l| l.contains("comment ·")).expect("box rendered");
    let below_row = lines.iter().position(|l| l.contains("gamma")).expect("context below shown");
    assert!(below_row > box_row, "the diff line below the selection is pushed under the box");
}

const AREA: Rect = Rect { x: 0, y: 0, width: 140, height: 40 };

#[test]
fn header_clicks_map_to_the_scope_chip() {
    let app = edited_app(); // scope uncommitted, no comments
    // Scan the header row instead of hardcoding columns, so the test survives changes
    // to the label text.
    let scope: Vec<u16> = (0..AREA.width)
        .filter(|&c| ui::hit_header(AREA, &app, app.keymap(), c, 0) == Some(HeaderHit::Scope))
        .collect();

    assert!(!scope.is_empty(), "scope chip is clickable");

    let gap = scope.iter().max().unwrap() + 1;
    assert_eq!(
        ui::hit_header(AREA, &app, app.keymap(), gap, 0),
        None,
        "the space right of the chip is inert"
    );
    assert_eq!(
        ui::hit_header(AREA, &app, app.keymap(), scope[0], 5),
        None,
        "only row 0 is the header"
    );
}

#[test]
fn file_and_diff_clicks_map_to_row_indices() {
    let app = edited_app();
    // Right pane: the first file row maps to index 0; clicking past the list misses.
    assert_eq!(ui::hit_file(AREA, &app, 120, 2, app.file_rows.len(), 0), Some(0));
    assert_eq!(ui::hit_file(AREA, &app, 120, 9, app.file_rows.len(), 0), None);
    // With the list scrolled down, the top visible row maps to that scrolled-to index.
    assert_eq!(ui::hit_file(AREA, &app, 120, 2, 50, 7), Some(7));
    assert_eq!(ui::hit_file(AREA, &app, 120, 3, 50, 7), Some(8));
    // The wheel routes by pointer: a column in the navigator is "in" the file list,
    // one in the read pane is not.
    assert!(ui::in_files_pane(AREA, &app, 120, 3));
    assert!(!ui::in_files_pane(AREA, &app, 10, 3));
    // Left pane: diff rows map top-down to diff-line indices.
    assert!(app.visible.len() > 1);
    let heights = ui::diff_row_heights(&app, AREA);
    assert_eq!(ui::hit_diff(AREA, &app, 10, 2, &heights, 0), Some(0));
    assert_eq!(ui::hit_diff(AREA, &app, 10, 3, &heights, 0), Some(1));
    // With a nonzero scroll and wrapped (multi-row) lines, the click must skip the
    // scrolled-off rows and account for each visible row's display height. Rows are
    // 2 tall each; diff_scroll=1 puts row index 1 at the top of the pane (inner.y == 2).
    let tall = [2usize, 2, 2, 2];
    assert_eq!(ui::hit_diff(AREA, &app, 10, 2, &tall, 1), Some(1)); // top visible row
    assert_eq!(ui::hit_diff(AREA, &app, 10, 3, &tall, 1), Some(1)); // its second display row
    assert_eq!(ui::hit_diff(AREA, &app, 10, 4, &tall, 1), Some(2)); // next logical row
}

#[test]
fn navigator_layout_rects_cover_every_position_and_tiny_axis() {
    let mut app = edited_app();
    let body = ui::body_rect(AREA, &app);

    for position in [
        NavigatorPosition::Right,
        NavigatorPosition::Bottom,
        NavigatorPosition::Left,
        NavigatorPosition::Top,
    ] {
        app.navigator_position = position;
        let _ = render_size(&app, AREA.width, AREA.height);
        let app_ref = &app;
        let files: Vec<(u16, u16)> = (body.y..body.y + body.height)
            .flat_map(|row| {
                (body.x..body.x + body.width)
                    .filter(move |&col| ui::in_files_pane(AREA, app_ref, col, row))
                    .map(move |col| (col, row))
            })
            .collect();
        let diff: Vec<(u16, u16)> = (body.y..body.y + body.height)
            .flat_map(|row| {
                (body.x..body.x + body.width)
                    .filter(move |&col| ui::in_diff_pane(AREA, app_ref, col, row))
                    .map(move |col| (col, row))
            })
            .collect();
        let files_x = (
            files.iter().map(|&(x, _)| x).min().unwrap(),
            files.iter().map(|&(x, _)| x).max().unwrap(),
        );
        let files_y = (
            files.iter().map(|&(_, y)| y).min().unwrap(),
            files.iter().map(|&(_, y)| y).max().unwrap(),
        );
        let diff_x = (
            diff.iter().map(|&(x, _)| x).min().unwrap(),
            diff.iter().map(|&(x, _)| x).max().unwrap(),
        );
        let diff_y = (
            diff.iter().map(|&(_, y)| y).min().unwrap(),
            diff.iter().map(|&(_, y)| y).max().unwrap(),
        );
        assert_eq!(files.len() + diff.len(), usize::from(body.width * body.height));
        assert!(!files.is_empty() && !diff.is_empty());
        assert!(
            (body.y..body.y + body.height).any(|row| {
                (body.x..body.x + body.width).any(|col| ui::hit_divider(AREA, &app, col, row))
            }),
            "divider is hittable for {position:?}"
        );
        match position {
            NavigatorPosition::Right => {
                assert!(files.iter().map(|(x, _)| x).min() > diff.iter().map(|(x, _)| x).min());
                assert_eq!(files.len() / usize::from(body.height), 44);
                assert!(!ui::hit_divider(AREA, &app, files_x.0 + 1, body.y + 4));
                assert!(!ui::hit_divider(AREA, &app, diff_x.1 - 1, body.y + 4));
            }
            NavigatorPosition::Left => {
                assert!(files.iter().map(|(x, _)| x).min() < diff.iter().map(|(x, _)| x).min());
                assert_eq!(files.len() / usize::from(body.height), 44);
                assert!(!ui::hit_divider(AREA, &app, files_x.1 - 1, body.y + 4));
                assert!(!ui::hit_divider(AREA, &app, diff_x.0 + 1, body.y + 4));
            }
            NavigatorPosition::Bottom => {
                assert!(files.iter().map(|(_, y)| y).min() > diff.iter().map(|(_, y)| y).min());
                assert_eq!(files.len() / usize::from(body.width), 9);
                assert!(!ui::hit_divider(AREA, &app, body.x + 4, files_y.0 + 1));
                assert!(!ui::hit_divider(AREA, &app, body.x + 4, diff_y.1 - 1));
            }
            NavigatorPosition::Top => {
                assert!(files.iter().map(|(_, y)| y).min() < diff.iter().map(|(_, y)| y).min());
                assert_eq!(files.len() / usize::from(body.width), 9);
                assert!(!ui::hit_divider(AREA, &app, body.x + 4, files_y.1 - 1));
                assert!(!ui::hit_divider(AREA, &app, body.x + 4, diff_y.0 + 1));
            }
        }
    }

    app.navigator_position = NavigatorPosition::Right;
    let six = Rect::new(0, 0, 6, 10);
    let row = ui::body_rect(six, &app).y;
    assert_eq!((0..6).filter(|&col| ui::in_files_pane(six, &app, col, row)).count(), 3);
    assert_eq!((0..6).filter(|&col| ui::in_diff_pane(six, &app, col, row)).count(), 3);

    let five = Rect::new(0, 0, 5, 10);
    let row = ui::body_rect(five, &app).y;
    assert_eq!((0..5).filter(|&col| ui::in_files_pane(five, &app, col, row)).count(), 2);
    assert_eq!((0..5).filter(|&col| ui::in_diff_pane(five, &app, col, row)).count(), 3);

    app.navigator_position = NavigatorPosition::Top;
    let eight = Rect::new(0, 0, 10, 10); // body height 8
    let col = ui::body_rect(eight, &app).x;
    assert_eq!((1..9).filter(|&row| ui::in_files_pane(eight, &app, col, row)).count(), 3);
    assert_eq!((1..9).filter(|&row| ui::in_diff_pane(eight, &app, col, row)).count(), 5);

    let seven = Rect::new(0, 0, 10, 7); // body height 5: navigator gets floor(5 / 2)
    let col = ui::body_rect(seven, &app).x;
    assert_eq!((1..6).filter(|&row| ui::in_files_pane(seven, &app, col, row)).count(), 2);
    assert_eq!((1..6).filter(|&row| ui::in_diff_pane(seven, &app, col, row)).count(), 3);
}

#[test]
fn pr_focus_border_tracks_tab_between_navigator_and_read_pane() {
    let mut app = edited_app();
    app.set_tab(Tab::Pr).unwrap();
    app.focus = Focus::Files;
    let body = ui::body_rect(AREA, &app);
    let nav_x = (body.x..body.x + body.width)
        .find(|&x| ui::in_files_pane(AREA, &app, x, body.y + 4))
        .unwrap();
    let read_x = (body.x..body.x + body.width)
        .find(|&x| ui::in_diff_pane(AREA, &app, x, body.y + 4))
        .unwrap();
    let (blue, surface2) = (app.palette().blue, app.palette().surface2);

    let focused_nav = render_buffer(&app);
    assert_eq!(focused_nav.cell((nav_x, body.y + 4)).unwrap().fg, blue);
    assert_eq!(focused_nav.cell((read_x, body.y + 4)).unwrap().fg, surface2);

    handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), AREA, &Keymap::default())
        .unwrap();
    let focused_read = render_buffer(&app);
    assert_eq!(focused_read.cell((nav_x, body.y + 4)).unwrap().fg, surface2);
    assert_eq!(focused_read.cell((read_x, body.y + 4)).unwrap().fg, blue);
}

#[test]
fn a_zero_height_pr_navigator_does_not_consume_selection_reveal() {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.navigator_position = NavigatorPosition::Top;
    let comments = (0..30)
        .map(|i| Comment { author: format!("author-{i:02}"), ..common::comment() })
        .collect();
    app.pr = PrView::Pr(Box::new(PrSnapshot { comments, ..common::pr_snapshot() }));
    app.pr_move(10);

    let _ = render_size(&app, 80, 3); // the top navigator has no inner viewport
    let useful = dump(&render_size(&app, 80, 40));

    assert!(useful.contains("@author-10"), "the pending reveal survives the tiny frame:\n{useful}");
}

#[test]
fn a_loading_pr_navigator_does_not_consume_selection_reveal() {
    use herdr_reviewr::forge::{Check, CheckStatus, Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.clear_pr();
    let _ = render(&app); // a useful viewport, but no selected row yet

    let checks = (0..20)
        .map(|i| Check { name: format!("check-{i:02}"), status: CheckStatus::Success, url: None })
        .collect();
    let comments = vec![Comment { author: "selected".into(), ..common::comment() }];
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot { checks, comments, ..common::pr_snapshot() })));
    let populated = render(&app);

    assert!(populated.contains("@selected"), "the first selectable row is revealed:\n{populated}");
}

#[test]
fn a_binary_file_shows_the_no_line_comments_message() {
    let r = Repo::init();
    r.write("logo.bin", "\0\0\0\0seed\0\0");
    r.commit_all("init");
    r.write("logo.bin", "\0\0\0\0changed\0\0\0");
    let mut app = app_on(&r);
    let idx = app.entries.iter().position(|f| f.path == "logo.bin").expect("binary file listed");
    app.select_file(idx).unwrap();

    let out = render(&app);
    assert!(out.contains("binary — no line comments"), "binary diff message shown:\n{out}");
}

#[test]
fn the_comments_list_flags_a_stale_comment() {
    let r = Repo::init();
    r.write("a.rs", "alpha\nbeta\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nBETA\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    app.diff_cursor = app.diff.rows.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
    for ch in "look here".chars() {
        app.input_push(ch);
    }
    app.submit_comment();

    // a.rs reverts to its committed state → leaves the changeset → the comment is stale.
    r.write("a.rs", "alpha\nbeta\n");
    app.reload().unwrap();
    app.open_list();

    let out = render(&app);
    assert!(out.contains("(stale)"), "stale comment flagged in the list:\n{out}");
}

#[test]
fn open_list_renders_the_comments_overlay() {
    let mut app = edited_app();
    app.focus = Focus::Diff;
    app.diff_cursor = app.diff.rows.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
    for ch in "overlay note".chars() {
        app.input_push(ch);
    }
    app.submit_comment();
    app.open_list();

    let out = render(&app);
    assert!(out.contains("Comments ("), "overlay titled with a count");
    assert!(out.contains("overlay note"), "comment text listed");
}

#[test]
fn last_turn_without_an_agent_says_the_worktree_is_empty() {
    // owns when membership counts as observed; owns the
    // wording. Only a sample that found no member may say the worktree is empty.
    let r = Repo::init();
    r.write("a.rs", "a\n");
    r.commit_all("init");
    let mut app = App::new(r.path_buf(), Scope::LastTurn, None);
    app.reload().unwrap();
    app.sync_agents_present(Some(false));
    let out = render(&app);
    assert!(out.contains("[last turn]"), "the scope chip reads last turn");
    assert!(out.contains("no agent works here"), "the empty-worktree state shows");
}

#[test]
fn last_turn_with_an_agent_and_no_turn_yet_waits_for_the_first() {
    let r = Repo::init();
    r.write("a.rs", "a\n");
    r.commit_all("init");
    let mut app = App::new(r.path_buf(), Scope::LastTurn, None);
    app.reload().unwrap();
    app.sync_agents_present(Some(true));
    let out = render(&app);
    assert!(out.contains("waiting for the first turn"), "the pre-turn state shows");
}

#[test]
fn last_turn_before_the_first_sample_waits_rather_than_asserting_emptiness() {
    // The pre-poll frame has observed nothing, so it may wait but not claim the worktree
    // is empty — stale is allowed, wrong is not (Continuity).
    let r = Repo::init();
    r.write("a.rs", "a\n");
    r.commit_all("init");
    let mut app = App::new(r.path_buf(), Scope::LastTurn, None);
    app.reload().unwrap();
    let out = render(&app);
    assert!(out.contains("waiting for the first turn"), "the unknown state waits");
}

#[test]
fn all_files_tab_bar_footer_and_count_read_for_the_tab() {
    use herdr_reviewr::app::Tab;
    let r = Repo::init();
    r.write("a.rs", "one\n");
    r.commit_all("init");
    r.write("a.rs", "ONE\n"); // one change
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);

    let out = render(&app);
    assert!(out.contains("1 Changes"), "tab labels carry their switch digit:\n{out}");
    assert!(out.contains("2 Files"));
    assert!(
        out.contains("1 changed"),
        "the changed count stays in the header on All files:\n{out}"
    );
    let footer = footer_line(&out);
    assert!(
        footer.trim_end().ends_with('?'),
        "the collapsed footer closes with the `?`:\n{footer}"
    );
    assert!(
        !footer.contains("changed"),
        "the changed count is not repeated in the footer:\n{footer}"
    );
    // `scope` is a `go` key now, revealed by the `?` expansion rather than crowding row 1.
    app.toggle_keys();
    let expanded = render(&app);
    assert!(expanded.contains("scope"), "the `?` expansion lists the scope keys:\n{expanded}");
    assert!(expanded.contains("move"), "and labels the movement band:\n{expanded}");
}

#[test]
fn all_files_empty_pane_reads_select_a_file() {
    use herdr_reviewr::app::Tab;
    let r = Repo::init();
    r.write("src/a.rs", "x\n");
    r.write("src/b.rs", "y\n"); // two children so src/ is a real collapsed dir, not a folded file
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles); // clean repo: no seed; cursor rests on collapsed src/

    let out = render(&app);
    assert!(out.contains("select a file to read"), "the empty All files read-pane copy:\n{out}");
    assert!(!out.contains("no diff"), "no diff vocabulary in the content browser:\n{out}");
}

#[test]
fn renders_a_light_theme_without_panic() {
    let mut app = edited_app();
    app.set_cli_theme(Some("catppuccin-latte".to_string()));
    // Driving the full render path with a derived light palette must not panic, and a Latte
    // color (the focused pane's blue border) reaches the painted buffer.
    let buf = render_buffer(&app);
    let latte_blue = herdr_reviewr::theme::resolve(Some("catppuccin-latte")).palette.blue;
    let painted = (0..40)
        .flat_map(|y| (0..140).map(move |x| (x, y)))
        .any(|(x, y)| buf.cell((x, y)).is_some_and(|c| c.fg == latte_blue));
    assert!(painted, "the Latte palette reaches the painted buffer");
}

/// An `edited_app` running under `[keybindings]` from a real config file.
fn rebound_app(keybindings: &str) -> App {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), format!("[keybindings]\n{keybindings}"))
        .unwrap();
    let config = herdr_reviewr::config::plugin_config_in(dir.path()).unwrap();
    let mut app = edited_app();
    app.set_plugin_config(config);
    app.focus = Focus::Diff;
    app
}

#[test]
fn hints_show_the_first_bound_key() {
    let app = rebound_app("comment = [\"ㅊ\", \"c\"]\ntab-pr = [\"x\"]\n");
    let out = render(&app);
    let footer = footer_line(&out);
    // A wide hint key spans two buffer cells, so the dump carries a placeholder space after it.
    assert!(footer.contains("ㅊ  comment"), "the hint is the first bound key:\n{footer}");
    assert!(out.contains("x PR"), "the header tab hint follows its binding:\n{out}");
    assert!(!out.contains("3 PR"), "the replaced digit is gone:\n{out}");
}

/// The header columns `hit_header` maps to `tab` under `keymap`, scanned instead of hardcoded
/// so the tests survive changes to the label text and gaps.
fn tab_hit_cols(app: &App, keymap: &herdr_reviewr::keymap::Keymap, tab: Tab) -> Vec<u16> {
    let area = Rect::new(0, 0, 140, 40);
    (0..140)
        .filter(|&c| ui::hit_header(area, app, keymap, c, 0) == Some(HeaderHit::Tab(tab)))
        .collect()
}

#[test]
fn header_hits_use_the_frame_keymap_not_the_live_one() {
    use herdr_reviewr::keymap::default_keymap;
    // The live keymap has a wide tab-changes hint, shifting every span right by one column.
    let app = rebound_app("tab-changes = [\"ㅊ\"]\n");
    for tab in [Tab::Changes, Tab::AllFiles, Tab::Pr] {
        assert_ne!(
            tab_hit_cols(&app, default_keymap(), tab),
            tab_hit_cols(&app, app.keymap(), tab),
            "the passed frame keymap decides the spans, not the app's live one"
        );
    }
}

#[test]
fn header_tab_hits_align_with_wide_hint_keys() {
    let app = rebound_app("tab-changes = [\"ㅊ\"]\n");
    let out = render(&app);
    // The wide hint spans two buffer cells, so the dump shows a placeholder space after it.
    assert!(out.contains("ㅊ  Changes"), "the wide hint renders:\n{out}");
    // Each rendered label must be clickable at its own drawn column (one dumped char per cell,
    // so the char offset of the label in row 0 is its column).
    let row0 = out.lines().next().unwrap().to_string();
    let col_of = |needle: &str| row0[..row0.find(needle).unwrap()].chars().count() as u16;
    let area = Rect::new(0, 0, 140, 40);
    for (needle, tab) in [("Changes", Tab::Changes), ("2 Files", Tab::AllFiles), ("3 PR", Tab::Pr)]
    {
        assert_eq!(
            ui::hit_header(area, &app, app.keymap(), col_of(needle), 0),
            Some(HeaderHit::Tab(tab)),
            "the drawn {needle:?} label answers its own click"
        );
    }
}

#[test]
fn the_markdown_preview_renders_styled_lines_without_a_gutter() {
    let r = Repo::init();
    r.write("README.md", "# Install\n\nRun `cargo test` for **all** checks.\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);

    // Source view: raw markdown, and the footer surfaces the way into the preview.
    app.focus = Focus::Diff;
    let source = render(&app);
    assert!(source.contains("# Install"), "source shows raw markdown:\n{source}");
    let footer = source.lines().last().unwrap();
    assert!(footer.contains("m preview"), "source discovers the preview:\n{footer}");

    app.toggle_preview();
    let out = render(&app);
    assert!(out.contains("Install"), "the heading text renders:\n{out}");
    assert!(!out.contains("# Install"), "the # markers are gone in the preview:\n{out}");
    assert!(!out.contains("**all**"), "emphasis markers are consumed:\n{out}");
    assert!(!out.contains("  1 "), "the preview has no line-number gutter:\n{out}");
    let footer = out.lines().last().unwrap();
    assert!(footer.contains("m source"), "the footer leads back to source:\n{footer}");
    assert!(!footer.contains("c comment"), "no comment key in the preview:\n{footer}");
}

#[test]
fn a_deleted_markdown_file_offers_no_preview_in_the_footer() {
    let r = Repo::init();
    r.write("gone.md", "# Doc\n\nbody\n");
    r.commit_all("init");
    r.remove("gone.md");
    let mut app = app_on(&r);
    assert_eq!(app.diff_path.as_deref(), Some("gone.md"));
    app.focus = Focus::Diff;

    // The deletion rows are commentable, but a deleted file has no current content, so
    // the footer never offers the inert preview toggle.
    let out = render(&app);
    let footer = out.lines().last().unwrap();
    assert!(footer.contains("c comment"), "a deletion row is commentable:\n{footer}");
    assert!(!footer.contains("m preview"), "a deleted file offers no preview:\n{footer}");
}

#[test]
fn pr_bodies_render_as_markdown_and_the_description_row_pins_first() {
    use herdr_reviewr::forge::{Comment, CommentKind, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let place = herdr_reviewr::forge::FindingPlace::from_anchor(
        "x.rs:1",
        Some(herdr_reviewr::model::Side::New),
    );
    let finding = Comment {
        kind: CommentKind::Finding,
        author: "codex".into(),
        author_is_bot: true,
        anchor: place.anchor(),
        place: Some(place),
        body: "Avoid **panics** in `parse`.".into(),
        snippet: Some("-    old\n+    new".into()),
        ..common::comment()
    };
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        number: 226,
        body: "## Summary\nThis PR adds *markdown*.".into(),
        comments: vec![finding],
        ..common::pr_snapshot()
    }));

    // The cursor starts on the pinned description row; its body renders as markdown.
    let out = render(&app);
    assert!(out.contains("description"), "the description row shows:\n{out}");
    assert!(out.contains("Summary"), "the description heading renders:\n{out}");
    assert!(!out.contains("## Summary"), "markers are consumed:\n{out}");
    assert!(!out.contains("*markdown*"), "emphasis markers are consumed:\n{out}");

    // The navigator orders the PR itself first: description above checks above comments.
    let nav = right_column(&out, 68);
    let desc_at = nav.find("description").expect("description row in the nav");
    let checks_at = nav.find("checks").expect("checks section in the nav");
    let comments_at = nav.find("comments ·").expect("comments header in the nav");
    assert!(desc_at < checks_at && checks_at < comments_at, "nav order:\n{nav}");

    // The finding: the snippet paints as Diff-view rows, the body renders as markdown.
    app.pr_move(1);
    let out = render(&app);
    assert!(out.contains("old"), "the deletion row paints:\n{out}");
    assert!(out.contains("new"), "the insertion row paints:\n{out}");
    assert!(!out.contains("+    new"), "the hunk is not raw +/- text:\n{out}");
    assert!(out.contains("Avoid panics in parse."), "the body renders styled:\n{out}");
    assert!(!out.contains("**panics**"), "markers are consumed:\n{out}");
}

#[test]
fn the_read_pane_shows_the_description_then_every_comment_oldest_first() {
    use herdr_reviewr::forge::{Comment, CommentKind, PrSnapshot, PrView, Reply, ReviewState};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let place = herdr_reviewr::forge::FindingPlace::from_anchor(
        "x.rs:1",
        Some(herdr_reviewr::model::Side::New),
    );
    let comments = vec![
        Comment {
            author: "ann".into(),
            body: "FIRST_COMMENT".into(),
            created_at: "2026-06-27T09:00:00Z".into(),
            ..common::comment()
        },
        Comment {
            kind: CommentKind::Finding,
            author: "bob".into(),
            anchor: place.anchor(),
            place: Some(place),
            body: "THREAD_ROOT".into(),
            created_at: "2026-06-27T10:00:00Z".into(),
            is_resolved: true,
            replies: vec![Reply {
                author: "ann".into(),
                author_is_bot: false,
                body: "THREAD_REPLY".into(),
                created_at: "2026-06-27T10:30:00Z".into(),
                avatar_url: None,
                links: herdr_reviewr::forge::Links::default(),
            }],
            ..common::comment()
        },
        Comment {
            kind: CommentKind::Review,
            author: "cat".into(),
            anchor: "review".into(),
            body: String::new(),
            created_at: "2026-06-27T11:00:00Z".into(),
            review_state: Some(ReviewState::Approved),
            ..common::comment()
        },
    ];
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        body: "DESCRIPTION_BODY".into(),
        comments,
        ..common::pr_snapshot()
    }));

    // The resolved thread paints folded: its header, then its root's first line in the
    // bottom border, the turns hidden.
    let out = render(&app);
    let read = read_column(&out);
    let head = read.iter().position(|l| l.starts_with("╭─ ▸ x.rs:1 · resolved · 1 reply "));
    let head = head.unwrap_or_else(|| panic!("the folded header:\n{out}"));
    assert!(read[head + 1].starts_with("╰─ @bob THREAD_ROOT ─"), "the summary edge:\n{out}");
    assert!(read[head + 1].trim_end().ends_with('╯'), "the folded box closes:\n{out}");
    assert!(!out.contains("THREAD_REPLY"), "the turns stay hidden:\n{out}");
    let thread = app.pr_snapshot().unwrap().comments[1].key();
    app.toggle_pr_card(&thread);

    let out = render(&app);
    let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{out}"));
    // One conversation: the description, a labelled separator, then each card oldest first,
    // with the thread's reply grouped under its root.
    let order = [
        at("DESCRIPTION_BODY"),
        at("━━ 3 comments"),
        at("FIRST_COMMENT"),
        at("x.rs:1 · resolved · 1 reply"),
        at("THREAD_ROOT"),
        at("THREAD_REPLY"),
        at("review · ✓ approved"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "conversation order:\n{out}");
    // Each card is its own rounded box, its header in the top border; the thread's turns
    // share one box, a dot at each byline and the rail joining them.
    let read = read_column(&out);
    let tops = read.iter().filter(|l| l.starts_with("╭─ ")).count();
    let bottoms =
        read.iter().filter(|l| l.starts_with("╰─") && l.trim_end().ends_with('╯')).count();
    assert_eq!((tops, bottoms), (3, 3), "one box per card:\n{out}");
    // Boxes stack flush: every box after the first opens on the line after the last closes.
    let flush =
        read.windows(2).filter(|w| w[0].starts_with("╰─") && w[1].starts_with("╭─ ")).count();
    assert_eq!(flush, 2, "no blank line between boxes:\n{out}");
    assert!(read.iter().any(|l| l.starts_with("╭─ ▾ x.rs:1 · resolved · 1 reply ")), "{out}");
    let thread: Vec<&String> = read
        .iter()
        .skip_while(|l| !l.contains("x.rs:1 · resolved"))
        .skip(1)
        .take_while(|l| !l.starts_with("╰"))
        .collect();
    let line = |needle: &str| thread.iter().find(|l| l.contains(needle)).unwrap().as_str();
    assert!(line("@bob").starts_with("│ ● @bob"), "the root's dot:\n{out}");
    assert!(line("THREAD_ROOT").starts_with("│ │ THREAD_ROOT"), "the rail runs on:\n{out}");
    assert!(line("@ann").starts_with("│ ● @ann"), "the reply's dot:\n{out}");
    assert!(line("THREAD_REPLY").starts_with("│   THREAD_REPLY"), "the rail ends:\n{out}");
    assert!(thread.iter().all(|l| l.trim_end().ends_with('│')), "the box closes:\n{out}");

    // The selected card's border takes the accent; the others stay dim.
    app.pr_move(2);
    let buf = render_buffer(&app);
    let corner = |text: &str| corner_of(&buf, text).expect("the box paints");
    assert_ne!(corner("x.rs:1"), corner("comment"), "the selected box stands out");
    assert_eq!(corner("comment"), corner("review"), "unselected boxes share one border");

    // The navigator names the review by its verdict, and counts a thread's replies.
    let nav = right_column(&out, 68);
    assert!(nav.contains("@cat ✓ approved"), "the verdict replaces the bare word:\n{nav}");
    assert!(nav.contains("↳1 resolved"), "the thread's reply count:\n{nav}");
    let (ann, cat) = (nav.find("@ann").unwrap(), nav.find("@cat").unwrap());
    assert!(ann < cat, "the navigator lists oldest first:\n{nav}");
}

#[test]
fn selecting_a_comment_scrolls_the_conversation_to_it_and_a_refresh_keeps_it_anchored() {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let body =
        |tag: &str| (0..12).map(|n| format!("{tag}-line-{n:02}")).collect::<Vec<_>>().join("\n\n");
    let snapshot = |description: &str| PrSnapshot {
        body: description.into(),
        comments: (0..4)
            .map(|i| Comment {
                author: format!("author-{i}"),
                body: body(&format!("c{i}")),
                created_at: format!("2026-06-27T1{i}:00:00Z"),
                ..common::comment()
            })
            .collect(),
        ..common::pr_snapshot()
    };
    app.apply_pr(PrView::Pr(Box::new(snapshot("short description"))));
    let read = |app: &App| -> Vec<String> {
        read_column(&render(app)).iter().map(|l| l.trim_end().to_string()).collect()
    };

    // Selecting the third comment brings its card to the top of the read pane.
    app.pr_move(3);
    assert_eq!(app.pr_selected_comment().map(|c| c.author.as_str()), Some("author-2"));
    let jumped = read(&app);
    // Below the tab strip and the pane's title border.
    let first = jumped.iter().skip(2).find(|l| !l.trim().is_empty()).unwrap();
    assert!(first.starts_with("╭─ comment"), "the card's box top opens the pane: {jumped:#?}");
    assert!(jumped.iter().any(|l| l.contains("c2-line-00")), "{jumped:#?}");
    assert!(!jumped.iter().any(|l| l.contains("c1-line-11")), "earlier cards scroll away");

    // The reader scrolls into the card; a refresh that grows the description above it
    // leaves exactly the same lines on screen (Continuity: anchored by identity).
    let area = Rect::new(0, 0, 140, 40);
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 10,
            modifiers: KeyModifiers::NONE,
        },
        area,
        &[],
        &Keymap::default(),
        &herdr_reviewr::export::Clipboard,
    )
    .unwrap();
    let before = read(&app);
    assert_ne!(before, jumped, "the wheel scrolled the read pane");
    let before_scroll = app.pr_read_scroll();
    let longer = (0..10).map(|n| format!("para {n}")).collect::<Vec<_>>().join("\n\n");
    app.apply_pr(PrView::Pr(Box::new(snapshot(&longer))));
    let after = read(&app);
    assert_eq!(app.pr_selected_comment().map(|c| c.author.as_str()), Some("author-2"));
    assert!(app.pr_read_scroll() > before_scroll, "the absolute scroll absorbs the growth");
    assert_eq!(before, after, "the reader's view does not move");
}

#[test]
fn comment_boxes_fit_their_pane_and_go_flat_when_it_is_too_narrow() {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let long = format!(
        "see [the docs](https://example.com/docs) then `{}`\n\n```\n{}\n```",
        "y".repeat(150),
        "z".repeat(150)
    );
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments: vec![Comment { body: long, ..common::comment() }],
        ..common::pr_snapshot()
    }));

    // Every box row closes on the same column however long its content runs: markdown wraps
    // to the inner width and an unbreakable run clips inside the border.
    for width in [40u16, 60, 140] {
        let read = read_column(&dump(&render_size(&app, width, 40)));
        let rows: Vec<&String> = read
            .iter()
            .skip_while(|l| !l.starts_with("╭─"))
            .take_while(|l| !l.starts_with("╰"))
            .collect();
        assert!(rows.len() > 2, "the box paints at {width}: {read:#?}");
        let edge = rows[0].trim_end().chars().count();
        for row in &rows[1..] {
            let row = row.trim_end();
            assert!(row.ends_with('│'), "the border closes at {width}: {read:#?}");
            assert_eq!(row.chars().count(), edge, "one right edge at {width}: {read:#?}");
        }
    }
    // The link inside the box stays clickable at its shifted column.
    let _ = render(&app);
    assert_eq!(first_painted_link(&app).as_deref(), Some("https://example.com/docs"));

    // Too narrow for a box: the card paints flat, with no chrome, and nothing panics down to
    // a sliver of a pane.
    let read = read_column(&dump(&render_size(&app, 30, 40)));
    assert!(!read.iter().any(|l| l.contains('╭')), "no box in a narrow pane: {read:#?}");
    assert!(read.iter().any(|l| l.contains("@ann")), "the card still reads: {read:#?}");
    for width in 1..30u16 {
        let _ = render_size(&app, width, 12);
    }
}

#[test]
fn a_finding_range_paints_as_diff_rows() {
    use herdr_reviewr::forge::{Comment, CommentKind, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let hunk = format!(
        concat!(
            "@@ -16,10 +16,10 @@\n",
            " OUT_ABOVE\n",
            " a\n",
            " b\n",
            " c\n",
            " ctx\n",
            "-    let x = foo(a);\n",
            "+    let x = bar(a); SNIP_HEAD{}SNIP_TAIL\n",
            " tail\n",
            " d\n",
            " e\n",
            " OUT_BELOW\n",
        ),
        "x".repeat(80),
    );
    let finding = |anchor: &str, body: &str| {
        let place = herdr_reviewr::forge::FindingPlace::from_anchor(
            anchor,
            Some(herdr_reviewr::model::Side::New),
        );
        Comment {
            kind: CommentKind::Finding,
            author: "codex".into(),
            author_is_bot: true,
            anchor: place.anchor(),
            place: Some(place),
            body: body.into(),
            snippet: Some(hunk.clone()),
            ..common::comment()
        }
    };
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments: vec![finding("x.rs:21", "keep this"), finding("x.rs:16", "second finding")],
        ..common::pr_snapshot()
    }));
    app.wrap = false;
    let out = render(&app);
    // The nav label is `x.rs:21`; the gutter must also paint in the read pane.
    let read = out
        .lines()
        .map(|l| l.chars().take(l.chars().count() * 68 / 100).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        read.lines().any(|l| l.contains("21") && l.contains("foo")),
        "the gutter 21 sits on the deletion row:\n{read}"
    );
    assert!(out.contains("foo"), "the deletion in range paints:\n{out}");
    assert!(out.contains("bar"), "the insertion in range paints:\n{out}");
    assert!(out.contains("SNIP_TAIL"), "the snippet wraps even when wrap is off:\n{out}");
    assert!(out.contains("ctx") && out.contains("tail"), "the three-line margin paints:\n{out}");
    // Both findings paint in the one conversation; the margin claims are about the first card.
    let first_card = &read[..read.find("Comment on line 16").expect("the second card paints")];
    assert!(!first_card.contains("OUT_ABOVE"), "context beyond the margin is omitted:\n{read}");
    assert!(
        !first_card.contains("OUT_BELOW"),
        "following context beyond the margin is omitted:\n{read}"
    );
    assert!(!out.contains("@@"), "the hunk header does not paint:\n{out}");
    assert!(
        out.contains("Comment on line +21"),
        "an insertion in the range keeps the + sign:\n{out}"
    );
    assert!(out.contains("keep this"), "the body follows the range:\n{out}");

    app.pr_move(1);
    let out = render(&app);
    assert!(out.contains("Comment on line 16"), "a context range has no sign:\n{out}");
    assert!(out.contains("OUT_ABOVE"), "the other finding's range paints:\n{out}");
    // The conversation keeps both cards; the accent border moves to the second one.
    let buf = render_buffer(&app);
    let accent = corner_of(&buf, "x.rs:16");
    assert!(accent.is_some(), "the selected box paints:\n{out}");
    assert_ne!(accent, corner_of(&buf, "x.rs:21"), "the selection's border moves with it:\n{out}");
    app.pr_move(-1);
    assert_eq!(corner_of(&render_buffer(&app), "x.rs:21"), accent, "and back");
    assert!(out.contains("second finding"), "the selected body follows its range:\n{out}");
}

#[test]
fn pr_nav_clicks_map_the_description_and_comment_rows() {
    use herdr_reviewr::forge::{Check, CheckStatus, Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let comment = |author: &str| Comment { author: author.into(), ..common::comment() };
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        body: "the description".into(),
        checks: vec![Check { name: "ci".into(), status: CheckStatus::Success, url: None }],
        comments: vec![comment("ann"), comment("bob")],
        ..common::pr_snapshot()
    }));

    // Nav layout: description, blank, checks header, 1 check, blank, comments header,
    // then the comments. The nav inner starts one row under the tab bar's border. A click
    // resolves through the display-row map and the row's cursor, the same pair the release
    // path uses.
    let area = Rect::new(0, 0, 140, 40);
    let x = 130; // inside the nav pane
    let hit = |app: &App, y: u16| {
        ui::pr_nav_display_row(area, app, x, y, false)
            .and_then(|row| ui::pr_nav_cursor_at(app, row))
    };
    assert_eq!(hit(&app, 2), Some(0), "click on the description row");
    assert_eq!(hit(&app, 5), None, "a check row is not a cursor stop");
    assert_eq!(hit(&app, 8), Some(1), "first comment maps past the offset");
    assert_eq!(hit(&app, 9), Some(2), "second comment follows");
    assert_eq!(hit(&app, 10), None, "past the last comment is dead");
}

#[test]
fn pr_navigator_scroll_is_independent_and_preserved() {
    use herdr_reviewr::forge::{Check, CheckStatus, Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.navigator_position = NavigatorPosition::Bottom;
    let checks: Vec<Check> = (0..14)
        .map(|i| Check { name: format!("check-{i:02}"), status: CheckStatus::Success, url: None })
        .collect();
    let comments: Vec<Comment> = (0..8)
        .map(|i| Comment {
            author: format!("author-{i:02}"),
            body: (0..50).map(|line| format!("line-{line:02}")).collect::<Vec<_>>().join("  \n"),
            ..common::comment()
        })
        .collect();
    let snapshot = || PrSnapshot {
        checks: checks.clone(),
        comments: comments.clone(),
        ..common::pr_snapshot()
    };
    app.pr = PrView::Pr(Box::new(snapshot()));

    let selected = app.pr_selected_comment().map(|c| c.author.clone());
    let area = Rect::new(0, 0, 140, 40);
    let body = ui::body_rect(area, &app);
    let (column, row) = (body.y..body.y + body.height)
        .flat_map(|row| (body.x..body.x + body.width).map(move |column| (column, row)))
        .find(|&(column, row)| ui::in_files_pane(area, &app, column, row))
        .unwrap();
    let keymap = Keymap::default();
    let _ = render(&app); // establishes the navigator's scroll bound
    for _ in 0..10 {
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &[],
            &keymap,
            &herdr_reviewr::export::Clipboard,
        )
        .unwrap();
    }
    let before = render(&app);
    assert!(before.contains("check-00"));
    assert!(!before.contains("check-13"));
    for _ in 0..5 {
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &[],
            &keymap,
            &herdr_reviewr::export::Clipboard,
        )
        .unwrap();
    }
    let scrolled = render(&app);
    assert!(scrolled.contains("@author-00"), "the wheel exposes overflowed comments:\n{scrolled}");
    assert_eq!(app.pr_selected_comment().map(|c| c.author.clone()), selected);

    // Click a comment that is not already selected: the click acts at the release
    // , so it takes a press and its same-row release.
    let current = app.pr_selected_comment().map(|c| c.author.clone());
    let (clicked_row, clicked_author) = scrolled
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            comments
                .iter()
                .find(|comment| line.contains(&format!("@{}", comment.author)))
                .filter(|comment| Some(&comment.author) != current.as_ref())
                .map(|comment| (row as u16, comment.author.clone()))
        })
        .expect("a scrolled, unselected comment row is painted");
    for kind in [
        MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left),
        MouseEventKind::Up(ratatui::crossterm::event::MouseButton::Left),
    ] {
        handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: body.x + 2,
                row: clicked_row,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &[],
            &keymap,
            &herdr_reviewr::export::Clipboard,
        )
        .unwrap();
    }
    assert_eq!(app.pr_selected_comment().map(|c| c.author.as_str()), Some(clicked_author.as_str()));

    app.apply_pr(PrView::Pr(Box::new(snapshot())));
    let refetched = render(&app);
    assert!(refetched.contains("@author-00"), "a refetch preserves navigator scroll:\n{refetched}");

    app.focus = Focus::Files;
    handle_key(&mut app, KeyEvent::from(KeyCode::PageUp), area, &keymap).unwrap();
    let paged = render(&app);
    assert!(paged.contains("check-00"), "page keys scroll the focused navigator:\n{paged}");
    assert_eq!(app.pr_selected_comment().map(|c| c.author.as_str()), Some(clicked_author.as_str()));

    handle_key(&mut app, KeyEvent::from(KeyCode::Tab), area, &keymap).unwrap();
    let nav_before_read_page = render(&app);
    assert!(nav_before_read_page.contains("line-00"));
    handle_key(&mut app, KeyEvent::from(KeyCode::PageDown), area, &keymap).unwrap();
    let read_paged = render(&app);
    assert!(!read_paged.contains("line-00"), "the focused read pane leaves its first line");
    assert!(read_paged.contains("line-20"), "PageDown advances the PR read body:\n{read_paged}");
    assert!(read_paged.contains("check-00"), "read paging leaves navigator paging unchanged");
}

#[test]
fn the_refresh_glyph_lives_in_the_tab_strip_not_the_content() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr =
        PrView::Pr(Box::new(PrSnapshot { body: "steady content".into(), ..common::pr_snapshot() }));

    let steady = render(&app);
    let row_of = |out: &str, needle: &str| {
        out.lines().position(|l| l.contains(needle)).unwrap_or(usize::MAX)
    };
    let before = row_of(&steady, "steady content");
    assert!(!steady.contains('⟳'), "the reserved cell is blank while idle");

    // The reserved cell means the glyph's appearance shifts nothing.
    app.refresh_indicator = true;
    let refreshing = render(&app);
    let header = refreshing.lines().next().unwrap();
    assert!(header.contains('⟳'), "the glyph shows in the tab strip:\n{header}");
    assert_eq!(
        row_of(&refreshing, "steady content"),
        before,
        "a refetch never shifts the content the reader is on"
    );
    assert_eq!(
        steady.lines().next().unwrap().replace(' ', "").len(),
        header.replace(' ', "").len() - '⟳'.len_utf8(),
        "the glyph fills the reserved blank cell instead of inserting one"
    );
}

#[test]
fn a_retry_notice_stays_visible_above_a_scrolled_pr_body() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.focus = Focus::Diff;
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        body: (0..80).map(|line| format!("line-{line:02}")).collect::<Vec<_>>().join("  \n"),
        ..common::pr_snapshot()
    }));

    let area = Rect::new(0, 0, 140, 40);
    let _ = render(&app);
    handle_key(&mut app, KeyEvent::from(KeyCode::PageDown), area, &Keymap::default()).unwrap();
    let scrolled = render(&app);
    assert!(!scrolled.contains("line-00"), "the setup scrolls away from the top");

    app.apply_pr(PrView::GitError("git rev-parse HEAD failed".to_string()));
    let failed = render(&app);
    assert!(failed.contains("Git read failed"), "the recovery action remains visible:\n{failed}");
    assert!(!failed.contains("line-00"), "showing the notice does not reset the reader");
}

#[test]
fn a_gitlab_repository_renders_merge_request_nouns_and_remedies() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    use herdr_reviewr::git::Forge;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr_forge = Forge::GitLab;

    // The empty state speaks the forge's noun.
    app.apply_pr(PrView::NoPr);
    let out = render(&app);
    assert!(out.contains("No merge request yet"), "GitLab empty state:\n{out}");

    // The chip uses GitLab's reference form.
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot { number: 42, ..common::pr_snapshot() })));
    let out = render(&app);
    assert!(out.contains("!42"), "MR reference form:\n{out}");
    assert!(!out.contains("#42"), "no GitHub reference form on GitLab:\n{out}");

    // Each failure names its own CLI and login command.
    app.apply_pr(PrView::NoCli(Forge::GitLab));
    let out = render(&app);
    assert!(out.contains("Install `glab`"), "glab install step:\n{out}");
    app.apply_pr(PrView::NotAuthed(Forge::GitLab, "git.corp.example".to_string()));
    let out = render(&app);
    assert!(out.contains("glab auth login --hostname git.corp.example"), "login remedy:\n{out}");
}

#[test]
fn an_unsupported_host_points_at_the_per_forge_host_keys() {
    use herdr_reviewr::forge::PrView;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.apply_pr(PrView::UnsupportedHost("code.corp.example".to_string()));
    let out = render(&app);
    assert!(out.contains("code.corp.example"), "the host is named:\n{out}");
    assert!(out.contains("github_host"), "GitHub key offered:\n{out}");
    assert!(out.contains("gitlab_host"), "GitLab key offered:\n{out}");
    assert!(out.contains("azure_devops_host"), "Azure DevOps key offered:\n{out}");
}

#[test]
fn an_azure_devops_repository_renders_pr_nouns_and_remedies() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    use herdr_reviewr::git::Forge;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr_forge = Forge::AzureDevOps;

    // The empty state speaks the forge's noun.
    app.apply_pr(PrView::NoPr);
    let out = render(&app);
    assert!(out.contains("No pull request yet"), "Azure DevOps empty state:\n{out}");

    // The chip uses the `#` reference form.
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot { number: 12, ..common::pr_snapshot() })));
    let out = render(&app);
    assert!(out.contains("#12"), "PR reference form:\n{out}");

    // Each failure names its own CLI, extension, and login command.
    app.apply_pr(PrView::NoCli(Forge::AzureDevOps));
    let out = render(&app);
    assert!(out.contains("Install `az`"), "az install step:\n{out}");
    app.apply_pr(PrView::NoExtension(Forge::AzureDevOps));
    let out = render(&app);
    assert!(out.contains("az extension add --name azure-devops"), "extension install step:\n{out}");
    app.apply_pr(PrView::NotAuthed(Forge::AzureDevOps, "dev.azure.com".to_string()));
    let out = render(&app);
    assert!(out.contains("`az login`"), "login remedy:\n{out}");
    assert!(out.contains("az devops login"), "the PAT alternative is offered:\n{out}");
}

#[test]
fn a_short_narrow_pr_pane_keeps_the_retry_action_and_one_body_row() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr =
        PrView::Pr(Box::new(PrSnapshot { body: "steady body".into(), ..common::pr_snapshot() }));
    app.apply_pr(PrView::NotAuthed(
        herdr_reviewr::git::Forge::GitHub,
        "github.example.com".to_string(),
    ));

    let out = dump(&render_size(&app, 30, 7));
    assert!(out.contains("Not signed"), "the failure state remains visible:\n{out}");
    assert!(out.contains("press r"), "the actionable tail remains visible:\n{out}");
    assert!(out.contains("steady body"), "the preserved snapshot keeps one readable row:\n{out}");
}

#[test]
fn markdown_links_paint_click_regions_and_the_guard_gates_them() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        body: "see [the run](https://ci.example/1)".into(),
        ..common::pr_snapshot()
    }));

    let _ = render(&app);
    let hit = first_painted_link(&app);
    assert_eq!(hit.as_deref(), Some("https://ci.example/1"), "a painted region resolves");

    // The guard gates what a click can open; a refused destination is silently inert.
    app.status.clear();
    app.open_link("javascript:alert(1)");
    assert_eq!(app.status, "", "an unsupported scheme does nothing");
    app.open_link("https://a\u{202e}b");
    assert_eq!(app.status, "", "a bidi-carrying destination does nothing");
    app.open_link("#no-such-anchor");
    assert_eq!(app.status, "", "a missing anchor does nothing");
}

#[test]
fn an_anchor_click_scrolls_the_preview_to_its_heading() {
    let mut md = String::from(
        "# Top

jump [go](#section-two)

",
    );
    for i in 0..40 {
        use std::fmt::Write as _;
        let _ = write!(md, "filler paragraph {i}\n\n");
    }
    md.push_str(
        "## Section Two

the target body
",
    );
    let r = Repo::init();
    r.write("doc.md", &md);
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);

    // In source view an anchor click is inert: no anchors are painted there.
    let _ = render(&app);
    app.open_link("#section-two");
    assert_eq!(app.preview_scroll, 0, "source view ignores anchor destinations");

    app.toggle_preview();
    let _ = render(&app); // paint: anchors and link regions note themselves

    assert_eq!(app.preview_scroll, 0);
    app.open_link("#section-two");
    assert!(app.preview_scroll > 40, "the preview jumped to the heading: {}", app.preview_scroll);
    let out = render(&app);
    assert!(
        out.contains("Section Two"),
        "the heading is on screen:
{out}"
    );
    assert!(
        !out.contains("# Top"),
        "the top scrolled away:
{out}"
    );
    assert!(out.contains('┃'), "an overflowing preview shows the scrollbar thumb:\n{out}");
}

#[test]
fn a_body_that_fits_the_pane_shows_no_scrollbar() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    use std::fmt::Write as _;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr =
        PrView::Pr(Box::new(PrSnapshot { body: "one short line".into(), ..common::pr_snapshot() }));
    let out = render(&app);
    assert!(!out.contains('┃'), "content that fits paints no thumb:\n{out}");

    // The same pane paints the thumb once its body overflows, so the absence above
    // proves fitting content, not a dead scrollbar.
    let mut long = String::new();
    for i in 0..80 {
        let _ = writeln!(long, "line {i}\n");
    }
    app.pr = PrView::Pr(Box::new(PrSnapshot { body: long, ..common::pr_snapshot() }));
    let out = render(&app);
    assert!(out.contains('┃'), "an overflowing PR body shows the thumb:\n{out}");
}

#[test]
fn the_preview_paints_link_regions_and_names_itself_in_the_title() {
    let r = Repo::init();
    r.write("README.md", "# Install\n\nsee [docs](https://docs.example/x)\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);

    let source = render(&app);
    assert!(!source.contains("· preview"), "source view has no preview marker");
    let miss = first_painted_link(&app);
    assert_eq!(miss, None, "raw source paints no link regions");

    app.toggle_preview();
    let out = render(&app);
    assert!(out.contains("README.md · preview"), "the title names the mode:\n{out}");
    let hit = first_painted_link(&app);
    assert_eq!(hit.as_deref(), Some("https://docs.example/x"));
}

#[test]
fn the_changes_tab_paints_the_markdown_preview() {
    let r = Repo::init();
    r.write("README.md", "# Install\n");
    r.commit_all("init");
    r.write("README.md", "# Install\n\nRun `cargo test` for **all** checks.\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;

    // The Changes diff shows raw markdown, and the footer surfaces the way into the preview.
    let source = render(&app);
    assert!(source.contains("# Install"), "the diff shows raw markdown:\n{source}");
    let footer = source.lines().last().unwrap();
    assert!(footer.contains("m preview"), "the diff discovers the preview:\n{footer}");

    // The toggle paints the rendered document over the diff and names the mode in the title.
    app.toggle_preview();
    let out = render(&app);
    assert!(out.contains("README.md · preview"), "the title names the mode:\n{out}");
    assert!(out.contains("Install"), "the heading text renders:\n{out}");
    assert!(!out.contains("# Install"), "the # markers are gone in the preview:\n{out}");
    // "checks" is on the new side only (the committed side is the bare heading), so this
    // proves the preview renders current content, not the old version being diffed.
    assert!(out.contains("checks"), "the preview renders the new-side content:\n{out}");
    let footer = out.lines().last().unwrap();
    assert!(footer.contains("m source"), "the footer leads back to the diff:\n{footer}");
}

#[test]
fn an_uppercase_unicode_anchor_still_finds_its_heading() {
    use std::fmt::Write as _;
    let mut md = String::from("# Über Top\n\njump [go](#ÜBER-TOP)\n\n");
    for i in 0..40 {
        let _ = writeln!(md, "filler {i}\n");
    }
    md.push_str("## Über Ziel\n\nend\n");
    let r = Repo::init();
    r.write("doc.md", &md);
    r.commit_all("init");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    app.toggle_preview();
    let _ = render(&app);

    // The click side must Unicode-lowercase like the slugger: #ÜBER-ZIEL → über-ziel.
    app.open_link("#ÜBER-ZIEL");
    assert!(app.preview_scroll > 40, "the jump matched the slug: {}", app.preview_scroll);
}

#[test]
fn an_anchor_in_a_comment_body_jumps_past_the_snippet_offset() {
    use herdr_reviewr::forge::{Comment, CommentKind, PrSnapshot, PrView};
    use std::fmt::Write as _;
    let mut body = String::from("jump [go](#target)\n\n");
    for i in 0..60 {
        let _ = writeln!(body, "line {i}\n");
    }
    body.push_str("## Target\n\nTARGET-BODY\n");
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments: vec![Comment {
            kind: CommentKind::Finding,
            author: "codex".into(),
            author_is_bot: true,
            anchor: "x.rs:1".into(),
            place: Some(herdr_reviewr::forge::FindingPlace::from_anchor(
                "x.rs:1",
                Some(herdr_reviewr::model::Side::New),
            )),
            body,
            snippet: Some("-    old\n+    new".into()),
            ..common::comment()
        }],
        ..common::pr_snapshot()
    }));
    let out = render(&app);
    assert!(out.contains("new"), "the snippet paints above the body:\n{out}");

    // The anchor stores its content line snippet-offset included, so the jump lands on
    // the heading, scrolling the snippet and the body's top out of view.
    app.open_link("#target");
    let out = render(&app);
    assert!(out.contains("Target"), "the heading is on screen:\n{out}");
    assert!(!out.contains("new"), "the snippet scrolled away:\n{out}");
    assert!(!out.contains("jump go"), "the body's top scrolled away:\n{out}");
}

#[test]
fn a_finding_paints_its_replies_in_the_read_pane() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::{Comment, CommentKind, PrSnapshot, PrView, Reply};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments: vec![Comment {
            kind: CommentKind::Finding,
            author: "codex".into(),
            author_is_bot: true,
            anchor: "x.rs:1".into(),
            place: Some(herdr_reviewr::forge::FindingPlace::from_anchor(
                "x.rs:1",
                Some(herdr_reviewr::model::Side::New),
            )),
            body: "the finding".into(),
            replies: vec![Reply {
                author: "persijano".into(),
                author_is_bot: false,
                body: "Addressed in abc".into(),
                created_at: "2026-06-27T11:30:00Z".into(),
                avatar_url: None,
                links: herdr_reviewr::forge::Links::default(),
            }],
            ..common::comment()
        }],
        ..common::pr_snapshot()
    }));
    let out = render(&app);
    assert!(out.contains("the finding"), "{out}");
    assert!(out.contains("@codex · "), "root byline with age:\n{out}");
    assert!(out.contains("@persijano · "), "reply byline with age:\n{out}");
    assert!(out.contains("Addressed in abc"), "{out}");
    assert!(out.contains('─'), "a rule separates turns:\n{out}");
    assert!(!out.contains("open on"), "{out}");
    // The old `↳ N replies` pointer line is gone from the read pane; the navigator's `↳1`
    // reply count is the only one.
    let read = out
        .lines()
        .map(|l| l.chars().take(l.chars().count() * 68 / 100).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!read.contains("↳"), "{read}");
}

#[test]
fn details_expand_on_the_pr_tab_and_reset_on_row_change() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let body = "<details> <summary>About Codex</summary>\n\nchrome lives here\n\n</details>";
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments: vec![
            Comment { author: "codex".into(), body: body.into(), ..common::comment() },
            Comment { author: "ann".into(), body: "plain".into(), ..common::comment() },
        ],
        ..common::pr_snapshot()
    }));
    let out = render(&app);
    assert!(out.contains("About Codex"), "{out}");
    assert!(!out.contains("chrome lives here"), "{out}");

    app.expand_pr_details();
    let out = render(&app);
    assert!(out.contains("chrome lives here"), "{out}");

    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        comments: vec![
            Comment { author: "codex".into(), body: body.into(), ..common::comment() },
            Comment { author: "ann".into(), body: "plain".into(), ..common::comment() },
        ],
        ..common::pr_snapshot()
    })));
    let out = render(&app);
    assert!(out.contains("chrome lives here"), "same thread keeps it open:\n{out}");

    app.pr_move(1);
    let out = render(&app);
    assert!(out.contains("plain"), "{out}");
    assert!(!out.contains("chrome lives here"), "row change collapses:\n{out}");

    app.pr_move(-1);
    let _ = render(&app);
    let hit = (0..40u16)
        .flat_map(|y| (0..140u16).map(move |x| (x, y)))
        .find_map(|(x, y)| app.painted_details_at(x, y));
    let summary = hit.expect("summary is clickable after a paint");
    app.toggle_details(&summary);
    let out = render(&app);
    assert!(out.contains("chrome lives here"), "click opens:\n{out}");
}

// In-file find rendering.
#[test]
fn the_find_band_and_match_highlight_paint() {
    let r = Repo::init();
    r.write("base.txt", "x\n");
    r.commit_all("init");
    r.write("m.rs", "let total = 1;\ncompute();\ntotal += 2;\n");
    let mut app = app_on(&r);
    app.focus = Focus::Diff;
    app.diff_cursor = 0; // the first "total" row
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 140, 40);

    handle_key(&mut app, KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), area, &keymap)
        .unwrap();
    for ch in "total".chars() {
        handle_key(&mut app, KeyEvent::from(KeyCode::Char(ch)), area, &keymap).unwrap();
    }

    let buf = render_buffer(&app);
    let out = dump(&buf);
    // The band carries the label, the query, and the count: two matches, the cursor on the first.
    assert!(out.contains("find"), "the band shows the find label:\n{out}");
    assert!(out.contains("1/2"), "the band shows the cursor's ordinal over the total:\n{out}");

    // A matched character reverses to the bright fill with dark text, so it reads over any row.
    let fill = app.palette().yellow;
    let ink = app.palette().surface0;
    let highlighted = (0..40u16).flat_map(|y| (0..140u16).map(move |x| (x, y))).any(|(x, y)| {
        buf.cell((x, y)).is_some_and(|c| c.symbol() == "t" && c.bg == fill && c.fg == ink)
    });
    assert!(highlighted, "a matched character reverses to the bright find highlight");
}

// Search screen rendering.
mod search_screen_render {
    use super::{common, dump, render, render_size};
    use common::{Repo, app_on, enter_tab};
    use herdr_reviewr::app::{App, Mode, Tab};
    use herdr_reviewr::keymap::default_keymap;
    use herdr_reviewr::land_search_completion;
    use herdr_reviewr::search::{CodeHit, FileHit, SearchCompletion, SearchOutcome, SearchResults};
    use herdr_reviewr::{handle_key, handle_mouse, ui};
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    const AREA: Rect = Rect { x: 0, y: 0, width: 140, height: 40 };

    fn open_on_all_files(repo: &Repo) -> App {
        let mut app = app_on(repo);
        enter_tab(&mut app, Tab::AllFiles);
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('/')), AREA, default_keymap()).unwrap();
        assert_eq!(app.mode, Mode::Search);
        app
    }

    fn key(app: &mut App, code: KeyCode) {
        handle_key(app, KeyEvent::from(code), AREA, default_keymap()).unwrap();
    }

    fn land(app: &mut App, results: SearchResults) {
        let completion = SearchCompletion { generation: 1, outcome: SearchOutcome::Ready(results) };
        land_search_completion(app, completion, 1);
    }

    #[test]
    fn the_band_anchors_the_terminal_cursor_at_its_caret() {
        let repo = Repo::init();
        repo.write("src/registry.rs", "fn resolve() {}\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|f| ui::render(f, &app)).unwrap();
        let empty = terminal.backend().cursor_position();

        key(&mut app, KeyCode::Char('日'));
        terminal.draw(|f| ui::render(f, &app)).unwrap();

        let after = terminal.backend().cursor_position();
        assert_eq!(
            (after.x, after.y),
            (empty.x + 2, empty.y),
            "the cursor advances one wide character"
        );
    }

    #[test]
    fn screen_shows_band_chips_and_both_modes() {
        let repo = Repo::init();
        repo.write("src/registry.rs", "fn resolve() {}\nregistry.resolve()\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        for c in "reg".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        land(
            &mut app,
            SearchResults {
                files: vec![FileHit { path: "src/registry.rs".into(), spans: vec![(4, 7)] }],
                code: vec![
                    CodeHit {
                        path: "src/registry.rs".into(),
                        line: 1,
                        text: "fn resolve() {}".into(),
                        spans: vec![(3, 6)],
                    },
                    CodeHit {
                        path: "src/registry.rs".into(),
                        line: 2,
                        text: "registry.resolve()".into(),
                        spans: vec![(0, 3)],
                    },
                ],
                file_total: 4,
                code_more: true,
            },
        );

        // Files mode: the band, both chips with live counts, path rows, the Files clip.
        let out = render(&app);
        let band = out.lines().find(|l| l.contains("> reg")).expect("the band row renders");
        assert!(band.contains("files 4 │ code 2+"), "both chips carry a live count: {band}");
        assert!(!band.contains('⇥'), "the chips drop the flip glyph — the footer owns the key");
        assert!(out.contains("src/registry.rs"), "a path match renders as a file row");
        assert!(out.contains("… more"), "a clipped list marks that there is more");
        assert!(out.contains("─ results"), "the results pane carries a titled rule");
        assert!(out.contains("─ preview"), "the divider row carries the preview title");
        assert!(out.contains("↑↓ move") && out.contains("enter open"), "the screen's footer shows");

        // Code mode: grouped rows under a header, `line:` locators, the clip.
        key(&mut app, KeyCode::Tab);
        let out = render(&app);
        assert!(out.contains("> reg"), "the flip keeps the query");
        assert!(out.contains("1: fn resolve"), "a match row shows its line number");
        assert!(out.contains("2: registry.resolve"), "grouped rows keep engine order");
        assert!(out.contains("… more"), "a cut-short grep shows there is more");
        let header_rows =
            out.lines().filter(|l| l.contains("src/registry.rs") && !l.contains(':')).count();
        assert!(header_rows >= 1, "the file emits one header row: {out}");
    }

    #[test]
    fn screen_shows_indexing_until_warm() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit_all("c");
        let app = open_on_all_files(&repo);
        let out = render(&app);
        assert!(out.contains("indexing…"), "the screen reads indexing… before the first scan");
        assert!(out.contains("files │ code"), "the count slots stay empty while warming");
    }

    #[test]
    fn no_matches_only_where_the_engine_looked() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(&mut app, SearchResults::default());
        let out = render(&app);
        assert!(out.contains("no matches"), "an empty warm Files result reads no matches");

        // An empty query lists nothing in Code mode — no copy at all.
        key(&mut app, KeyCode::Tab);
        let out = render(&app);
        assert!(!out.contains("no matches"), "an empty query in Code mode lists nothing");
    }

    #[test]
    fn click_picks_then_opens_and_chip_click_flips() {
        let repo = Repo::init();
        repo.write("a.rs", "one\ntwo\n");
        repo.write("b.rs", "three\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(
            &mut app,
            SearchResults {
                files: vec![
                    FileHit { path: "a.rs".into(), spans: vec![] },
                    FileHit { path: "b.rs".into(), spans: vec![] },
                ],
                code: Vec::new(),
                file_total: 2,
                code_more: false,
            },
        );
        // Paint once so the screen scroll settles, then resolve rows the frame mapped.
        let _ = dump(&render_size(&app, 140, 40));
        let hit_row = |app: &App, pick: usize| {
            (0..40u16)
                .find(|&y| ui::search_target(app, AREA, 30, y) == Some(ui::SearchTarget::Row(pick)))
                .expect("the result row is clickable")
        };
        let click = |app: &mut App, row: u16| {
            let event = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 30,
                row,
                modifiers: KeyModifiers::NONE,
            };
            handle_mouse(
                app,
                event,
                AREA,
                &[],
                default_keymap(),
                &herdr_reviewr::export::Clipboard,
            )
            .unwrap();
        };

        // A click on an unpicked row picks it; a second click opens it.
        let row = hit_row(&app, 1);
        click(&mut app, row);
        assert_eq!(app.mode, Mode::Search, "the first click only picks");
        assert_eq!(app.search.as_ref().unwrap().pick, 1);
        click(&mut app, row);
        assert_eq!(app.mode, Mode::Normal, "the second click opens the pick");
        assert_eq!(app.diff_path.as_deref(), Some("b.rs"));

        // A chip click flips the mode.
        let mut app = open_on_all_files(&repo);
        let _ = dump(&render_size(&app, 140, 40));
        let band_y = ui::body_rect(AREA, &app).y;
        let chip_x = (0..140u16)
            .find(|&x| ui::search_target(&app, AREA, x, band_y) == Some(ui::SearchTarget::Chips))
            .expect("the chips are clickable");
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: chip_x,
            row: band_y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(
            &mut app,
            event,
            AREA,
            &[],
            default_keymap(),
            &herdr_reviewr::export::Clipboard,
        )
        .unwrap();
        assert_eq!(
            app.search.as_ref().unwrap().search_mode,
            herdr_reviewr::app::SearchMode::Code,
            "a chip click flips the mode"
        );
    }

    #[test]
    fn preview_centers_and_bands_the_hit() {
        let repo = Repo::init();
        let lines: Vec<String> = (1..=60).map(|i| format!("line_{i}")).collect();
        let body = lines.join("\n") + "\n";
        repo.write("a.rs", &body);
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(
            &mut app,
            SearchResults {
                files: Vec::new(),
                code: vec![CodeHit {
                    path: "a.rs".into(),
                    line: 30,
                    text: "line_30".into(),
                    spans: vec![(0, 7)],
                }],
                file_total: 0,
                code_more: false,
            },
        );
        key(&mut app, KeyCode::Tab);
        app.build_search_preview();

        let buf = render_size(&app, 140, 40);
        let out = dump(&buf);
        assert!(out.contains("─ preview · a.rs"), "the pane title names the previewed file");
        let y = out
            .lines()
            .position(|l| l.contains("30 line_30"))
            .expect("the hit line is visible with its number") as u16;
        let x = out.lines().nth(y as usize).unwrap().find("line_30").unwrap() as u16;
        let style = buf.cell((x, y)).expect("cell").style();
        assert_eq!(
            style.bg,
            Some(app.palette().match_hl),
            "the hit's matched span wears the match highlight: {style:?}"
        );
        assert!(!out.contains(" 1 line_1\n"), "the hit is centered, not previewed from the top");

        // PageDown moves the pane; the scroll survives the next paint.
        key(&mut app, KeyCode::PageDown);
        let scrolled = app.search.as_ref().unwrap().preview.as_ref().unwrap().scroll.get();
        let _ = render_size(&app, 140, 40);
        assert!(scrolled > 0, "PageDown scrolls the preview");
    }

    #[test]
    fn preview_highlight_lands_on_the_match_under_indentation() {
        // The worker trims each grep line's leading indentation and reports offsets into the
        // trimmed text; the preview keeps the true indentation, so the highlight must shift
        // over it and still cover the match, not slide left into the whitespace or the
        // preceding tokens.
        let repo = Repo::init();
        let mut lines: Vec<String> = (1..=60).map(|i| format!("let x{i} = {i};")).collect();
        lines[29] = "    fn resolve() {}".to_string(); // line 30, four-space indented
        repo.write("a.rs", &(lines.join("\n") + "\n"));
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(
            &mut app,
            SearchResults {
                files: Vec::new(),
                // As the worker emits it: the trimmed line, offsets into the trimmed text.
                code: vec![CodeHit {
                    path: "a.rs".into(),
                    line: 30,
                    text: "fn resolve() {}".into(),
                    spans: vec![(3, 10)], // "resolve" within the trimmed line
                }],
                file_total: 0,
                code_more: false,
            },
        );
        key(&mut app, KeyCode::Tab);
        app.build_search_preview();

        let buf = render_size(&app, 140, 40);
        let out = dump(&buf);
        // The results row above is correctly trimmed; assert on the preview row, which keeps
        // the true indentation — that is where the trimmed spans had to be shifted.
        let preview_at =
            out.lines().position(|l| l.contains("─ preview")).expect("the preview divider");
        let below = out
            .lines()
            .skip(preview_at + 1)
            .position(|l| l.contains("fn resolve() {}"))
            .expect("the hit line previews with its indentation");
        let y = (preview_at + 1 + below) as u16;
        let line = out.lines().nth(y as usize).unwrap();
        let rx = line.find("resolve").unwrap() as u16;
        assert_eq!(
            buf.cell((rx, y)).unwrap().style().bg,
            Some(app.palette().match_hl),
            "the highlight lands on the match under indentation",
        );
        // The indentation and the preceding `fn ` keep the cursor band, not the match highlight.
        let fx = line.find("fn ").unwrap() as u16;
        assert_ne!(
            buf.cell((fx, y)).unwrap().style().bg,
            Some(app.palette().match_hl),
            "the highlight did not slide left into the un-trimmed indentation",
        );
    }

    #[test]
    fn tiny_screen_keeps_the_band() {
        let repo = Repo::init();
        repo.write("a.rs", "one\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(
            &mut app,
            SearchResults {
                files: vec![FileHit { path: "a.rs".into(), spans: vec![] }],
                code: Vec::new(),
                file_total: 1,
                code_more: false,
            },
        );
        app.build_search_preview();
        let out = dump(&render_size(&app, 24, 6));
        assert!(out.contains('>'), "the input band keeps its one row at tiny sizes");
    }

    #[test]
    fn empty_query_shows_a_placeholder_and_no_preview() {
        let repo = Repo::init();
        repo.write("a.rs", "one\n");
        repo.commit_all("c");
        let app = open_on_all_files(&repo);
        // Warm but no results landed yet: the band teaches, the preview isn't blank.
        let out = render(&app);
        assert!(out.contains("Search files and code…"), "the empty query shows a placeholder");
        let mut app = app;
        land(&mut app, SearchResults::default());
        let out = render(&app);
        assert!(out.contains("no preview"), "nothing to preview shows a dim notice, not a blank");
    }

    #[test]
    fn an_elided_file_result_still_highlights_the_visible_match() {
        // A head-elided path must still mark a match that survives in the shown tail — the
        // highlight is unconditional, remapped across the elision, not dropped.
        let repo = Repo::init();
        let path = "aaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbb/target_match.rs";
        repo.write(path, "x\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        let at = path.find("target").unwrap() as u32;
        land(
            &mut app,
            SearchResults {
                files: vec![FileHit { path: path.into(), spans: vec![(at, at + 6)] }],
                code: Vec::new(),
                file_total: 1,
                code_more: false,
            },
        );
        // A pane narrow enough to head-elide the long path onto its tail.
        let buf = render_size(&app, 44, 20);
        let out = dump(&buf);
        let y = out
            .lines()
            .position(|l| l.contains('…') && l.contains("target"))
            .expect("the elided path row shows its tail") as u16;
        let line = out.lines().nth(y as usize).unwrap();
        let tx = line.find("target").unwrap() as u16;
        assert_eq!(
            buf.cell((tx, y)).unwrap().style().bg,
            Some(app.palette().match_hl),
            "the match highlight survives the head-elision on the visible tail",
        );
    }

    #[test]
    fn long_query_scrolls_to_keep_the_caret_visible() {
        let repo = Repo::init();
        repo.write("a.rs", "one\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(&mut app, SearchResults::default()); // warm, so the chips have a fixed width
        // The caret sits at the end of the query; a band narrower than the query must
        // scroll its head off and keep the tail (and caret) on screen.
        let query = "aaaaHEAD_bbbbccccddddeeeeffffgggg_TAILzzzz";
        for c in query.chars() {
            key(&mut app, KeyCode::Char(c));
        }
        let out = dump(&render_size(&app, 44, 12));
        let band = out.lines().find(|l| l.contains("TAIL")).expect("the caret end stays visible");
        assert!(!band.contains("HEAD"), "the overflowing head scrolls off the band: {band:?}");
    }

    #[test]
    fn a_changed_file_result_shows_its_marker_and_stats() {
        // A Files result on an uncommitted file wears the same change marker and stats as the
        // file list, alongside the match highlight.
        let repo = Repo::init();
        repo.write("a.rs", "one\n");
        repo.commit_all("c");
        repo.write("a.rs", "one\ntwo\n"); // uncommitted: one added line
        let mut app = open_on_all_files(&repo);
        land(
            &mut app,
            SearchResults {
                files: vec![FileHit { path: "a.rs".into(), spans: vec![(0, 1)] }],
                code: Vec::new(),
                file_total: 1,
                code_more: false,
            },
        );
        let buf = render_size(&app, 140, 40);
        let out = dump(&buf);
        let row = out.lines().find(|l| l.contains("a.rs")).expect("the file row renders");
        assert!(row.contains("+1"), "the changed file's stats render on its row: {row:?}");
        // The match highlight coexists with the marker and stats.
        let y = out.lines().position(|l| l.contains("a.rs")).unwrap() as u16;
        let x = row.find("a.rs").unwrap() as u16;
        assert_eq!(
            buf.cell((x, y)).expect("cell").style().bg,
            Some(app.palette().match_hl),
            "the match highlight lands on the matched path character",
        );
    }

    #[test]
    fn a_poll_refreshes_the_open_preview_in_place() {
        // A landed poll rebuilds the previewed file's diff in place, so the preview follows
        // the worktree while the held results stay as queried (Continuity). Exercises the real reload → reconcile_world → refresh_search_preview
        // wiring, not the method in isolation.
        let repo = Repo::init();
        repo.write("a.rs", "alpha\n");
        repo.commit_all("c");
        let mut app = open_on_all_files(&repo);
        land(
            &mut app,
            SearchResults {
                files: Vec::new(),
                code: vec![CodeHit {
                    path: "a.rs".into(),
                    line: 1,
                    text: "alpha".into(),
                    spans: vec![(0, 5)],
                }],
                file_total: 0,
                code_more: false,
            },
        );
        key(&mut app, KeyCode::Tab);
        app.build_search_preview();
        assert!(render(&app).contains("alpha"), "the preview shows the file's content");

        // The worktree changes, then a poll lands through the synchronous reload path.
        repo.write("a.rs", "alpha\nBETA_LINE\n");
        app.reload().unwrap();
        assert!(render(&app).contains("BETA_LINE"), "the poll refreshed the preview in place");
    }
}

// Style-level emphasis coverage for the match rows.
mod search_row_emphasis {
    use super::{common, dump, render_size};
    use common::{Repo, app_on, enter_tab};
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::handle_key;
    use herdr_reviewr::keymap::default_keymap;
    use herdr_reviewr::land_search_completion;
    use herdr_reviewr::search::{CodeHit, SearchCompletion, SearchOutcome, SearchResults};
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::layout::Rect;

    const AREA: Rect = Rect { x: 0, y: 0, width: 140, height: 40 };

    fn code_only(hit: CodeHit) -> SearchCompletion {
        let results =
            SearchResults { files: Vec::new(), code: vec![hit], file_total: 0, code_more: false };
        SearchCompletion { generation: 1, outcome: SearchOutcome::Ready(results) }
    }

    /// A code row too wide for the pane clips around its first matched span, keeping the
    /// `line:` locator and marking the cut head with `…`.
    #[test]
    fn clipped_code_row_keeps_and_emphasizes_the_match() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit_all("c");
        let mut app = app_on(&repo);
        enter_tab(&mut app, Tab::AllFiles);
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('/')), AREA, default_keymap()).unwrap();

        // A long head of `x`s pushes the match past the pane, so the row clips around the
        // first matched span (`needle_marker`, 13 bytes) rather than the un-shown head.
        let text = format!("{}needle_marker tail", "x".repeat(200));
        let start = 200u32;
        let hit = CodeHit { path: "a.rs".into(), line: 1, text, spans: vec![(start, start + 13)] };
        land_search_completion(&mut app, code_only(hit), 1);
        handle_key(&mut app, KeyEvent::from(KeyCode::Tab), AREA, default_keymap()).unwrap();

        let buf = render_size(&app, 140, 40);
        let out = dump(&buf);
        let row = out
            .lines()
            .find(|l| l.contains("needle_marker"))
            .expect("the clipped row keeps the first matched span visible");
        assert!(row.contains("1:"), "the line locator survives the clip: {row}");
        assert!(row.contains("…x"), "the cut head is marked with an ellipsis: {row}");

        let y = out.lines().position(|l| l.contains("needle_marker")).unwrap() as u16;
        // Cell column = char count before the token (every cell here is one column wide).
        let byte = row.find("needle_marker").unwrap();
        let x = row[..byte].chars().count() as u16;
        assert_eq!(
            buf.cell((x, y)).expect("cell").style().bg,
            Some(app.palette().match_hl),
            "the matched span wears the match highlight",
        );
        // A cell in the clipped `…x` head keeps the selection fill, not the match highlight —
        // the band covers the match only, never spilling left across the cut.
        let ell = row.find('…').unwrap();
        let head_x = row[..ell].chars().count() as u16 + 1;
        assert_ne!(
            buf.cell((head_x, y)).expect("cell").style().bg,
            Some(app.palette().match_hl),
            "the clipped head is not highlighted",
        );
    }

    /// A tab-indented code row expands its tabs to spaces, so the indentation shows and
    /// the emphasis lands on the matched word, not shifted by the collapsed tabs
    #[test]
    fn tab_indented_code_row_expands_and_emphasizes() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit_all("c");
        let mut app = app_on(&repo);
        enter_tab(&mut app, Tab::AllFiles);
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('/')), AREA, default_keymap()).unwrap();

        // Two leading tabs, then `needle` — the match is at bytes 2..8 of the raw line.
        let hit = CodeHit {
            path: "a.rs".into(),
            line: 1,
            text: "\t\tneedle here".into(),
            spans: vec![(2, 8)],
        };
        land_search_completion(&mut app, code_only(hit), 1);
        handle_key(&mut app, KeyEvent::from(KeyCode::Tab), AREA, default_keymap()).unwrap();

        let buf = render_size(&app, 140, 40);
        let out = dump(&buf);
        let y = out.lines().position(|l| l.contains("needle")).unwrap();
        let row = out.lines().nth(y).unwrap();
        // Eight spaces of expanded indent sit between the locator and `needle`.
        assert!(row.contains("1:         needle"), "tabs expand to spaces: {row:?}");
        let x = row.find("needle").unwrap() as u16;
        let style = buf.cell((x, y as u16)).expect("cell").style();
        assert_eq!(
            style.bg,
            Some(app.palette().match_hl),
            "the highlight tracks the word past the expanded tabs: {style:?}"
        );
    }

    /// A multi-byte head forced through the clip path must paint, not panic — the
    /// engine's span offsets are bytes and the cut walks char boundaries.
    #[test]
    fn clipped_multibyte_code_row_paints() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit_all("c");
        let mut app = app_on(&repo);
        enter_tab(&mut app, Tab::AllFiles);
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('/')), AREA, default_keymap()).unwrap();

        // A wide multi-byte head (each `中` is 3 bytes, 2 columns) forces the clip's
        // char-boundary walk onto boundaries a byte/column confusion would land off.
        let head = "中".repeat(200);
        let start = head.len() as u32; // 600 bytes in
        let hit = CodeHit {
            path: "a.rs".into(),
            line: 1,
            text: format!("{head}needle tail"),
            spans: vec![(start, start + 6)],
        };
        land_search_completion(&mut app, code_only(hit), 1);
        handle_key(&mut app, KeyEvent::from(KeyCode::Tab), AREA, default_keymap()).unwrap();

        let buf = render_size(&app, 140, 40);
        let out = dump(&buf);
        let y = out
            .lines()
            .position(|l| l.contains("needle"))
            .expect("the clipped multibyte row paints without panicking") as u16;
        // The highlight starts exactly on the match, not shifted onto the multibyte head:
        // the first highlighted cell on the row is `needle`'s `n`.
        let hx = (0..buf.area.width)
            .find(|&x| buf.cell((x, y)).expect("cell").style().bg == Some(app.palette().match_hl))
            .expect("the match is highlighted");
        assert_eq!(
            buf.cell((hx, y)).expect("cell").symbol(),
            "n",
            "the match highlight lands on the match, past the multibyte head",
        );
    }
}

// --- Agent picker ----------------------------------------------------

fn agent_row(pane: &str, name: &str, state: &str, tab: &str) -> AgentChoice {
    AgentChoice { pane_id: pane.into(), name: name.into(), state: state.into(), tab: tab.into() }
}

/// One saved comment on the first added line, so the picker has a count to title itself with.
fn write_comment(app: &mut App, text: &str) {
    composing(app);
    app.input = text.to_string();
    app.submit_comment();
}

/// An app with three comments and the picker open, matching the spec's mockup.
fn picker_app() -> App {
    let mut app = edited_app();
    for text in ["one", "two", "three"] {
        write_comment(&mut app, text);
    }
    app.open_picker(vec![
        agent_row("w8:p1", "claude", "idle", "Grip Outreach"),
        agent_row("w8:p2", "release-bot", "idle", "Grip Outreach Campaign"),
        agent_row("w8:p3", "codex", "working", "3"),
    ]);
    app
}

#[test]
fn the_last_sent_row_carries_its_tag_and_no_other_row_does() {
    let mut app = edited_app();
    write_comment(&mut app, "one");
    // A prior send to release-bot arms the highlight there and tags the row, so the
    // remembered default reads before `enter` fires it.
    app.last_sent_pane = Some("w8:p2".to_string());
    app.open_picker(vec![
        agent_row("w8:p1", "claude", "idle", "1"),
        agent_row("w8:p2", "release-bot", "idle", "2"),
    ]);
    assert_eq!(app.picker_cursor, 1, "the highlight arms on the last-sent agent");
    let out = render(&app);

    let tagged = out.lines().find(|l| l.contains("release-bot")).unwrap_or_default();
    assert!(tagged.contains("· last used"), "the last-sent row is tagged: {tagged:?}");
    let plain = out.lines().find(|l| l.contains("claude")).unwrap_or_default();
    assert!(!plain.contains("last used"), "no other row is tagged: {plain:?}");
}

#[test]
fn an_open_picker_dims_the_view_behind_it_but_never_the_footer() {
    let mut app = edited_app();
    write_comment(&mut app, "one");
    let plain = render_buffer(&app);
    app.open_picker(vec![
        agent_row("w8:p1", "claude", "idle", "1"),
        agent_row("w8:p2", "codex", "idle", "2"),
    ]);
    let dimmed = render_buffer(&app);

    // The tab bar recedes toward the theme base while the picker is up.
    // Locate a lettered header cell rather than assuming a column, so a header layout
    // change cannot silently repoint the assertion.
    let x = (0..plain.area.width)
        .find(|&x| {
            plain
                .cell((x, 0))
                .is_some_and(|c| c.symbol().chars().all(char::is_alphanumeric) && c.symbol() != " ")
        })
        .expect("a lettered cell in the tab bar");
    let cell = |buf: &Buffer, x: u16, y: u16| buf.cell((x, y)).unwrap().clone();
    assert_eq!(cell(&plain, x, 0).symbol(), cell(&dimmed, x, 0).symbol());
    assert_ne!(cell(&plain, x, 0).fg, cell(&dimmed, x, 0).fg, "the header cell is scrimmed");

    // The footer is the picker's own key bar, so its primary hint keeps full brightness.
    let footer_y = dimmed.area.height - 1;
    let bright =
        (0..dimmed.area.width).any(|x| dimmed.cell((x, footer_y)).is_some_and(|c| c.fg == PEACH));
    assert!(bright, "the footer's primary key hint stays at full brightness");
}

#[test]
fn neither_popup_reaches_the_footer_that_advertises_its_keys() {
    let mut app = edited_app();
    for text in ["one", "two", "three"] {
        write_comment(&mut app, text);
    }
    let rows = vec![
        agent_row("w8:p1", "claude", "idle", "Grip Outreach"),
        agent_row("w8:p2", "release-bot", "idle", "Grip Outreach Campaign"),
    ];

    // Both popups place through one rule, `body_popup`, so at every pane size the footer keeps
    // naming the keys the popup is listening for — it is the only surface that does
    for h in 8..=30u16 {
        app.open_list();
        let listed = dump(&render_size(&app, 44, h));
        app.close_list();
        app.open_picker(rows.clone());
        let picked = dump(&render_size(&app, 44, h));
        app.close_picker();

        for (name, out) in [("comments list", listed), ("agent picker", picked)] {
            let footer = out.lines().last().unwrap_or_default().to_string();
            assert!(
                footer.contains("esc"),
                "the {name} popup covered the footer at height {h}:\n{out}"
            );
        }
    }
}

#[test]
fn the_picker_titles_the_count_and_aligns_the_dim_trail_in_one_column() {
    let app = picker_app();
    let out = render(&app);

    assert!(out.contains("send 3 comments to"), "the title counts the comments:\n{out}");

    let rows: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("claude") || l.contains("release-bot") || l.contains("codex"))
        .collect();
    assert_eq!(rows.len(), 3, "one row per agent:\n{out}");

    // The names pad to the widest, so every dim trail starts in the same column.
    let starts: Vec<usize> = rows
        .iter()
        .map(|l| l.find("idle").or_else(|| l.find("working")).expect("a state on every row"))
        .collect();
    assert!(starts.windows(2).all(|w| w[0] == w[1]), "trails misaligned at {starts:?}:\n{out}");

    // The tab trails behind the state, separated by the dim dot.
    assert!(rows[0].contains("idle · Grip Outreach"), "{:?}", rows[0]);
    assert!(rows[2].contains("working · 3"), "{:?}", rows[2]);
}

#[test]
fn the_picker_numbers_only_the_rows_a_digit_key_can_reach() {
    let mut app = edited_app();
    write_comment(&mut app, "one");
    let rows: Vec<AgentChoice> = (1..=11)
        .map(|i| agent_row(&format!("w8:p{i}"), &format!("agent{i}"), "idle", "1"))
        .collect();
    app.open_picker(rows);
    let out = render(&app);

    for i in 1..=9 {
        let row = out.lines().find(|l| l.contains(&format!("agent{i} "))).unwrap_or_default();
        assert!(row.contains(&format!(" {i}  ")), "row {i} carries its digit: {row:?}");
    }
    // Rows past the ninth are reached by movement, so they carry no number to press.
    let tenth = out.lines().find(|l| l.contains("agent10")).unwrap_or_default();
    assert!(!tenth.contains(" 10 "), "row 10 must not advertise an unreachable key: {tenth:?}");
}

#[test]
fn a_picker_taller_than_the_pane_scrolls_to_keep_the_highlight_visible() {
    let mut app = edited_app();
    write_comment(&mut app, "one");
    let rows: Vec<AgentChoice> = (1..=20)
        .map(|i| agent_row(&format!("w8:p{i}"), &format!("agent{i}"), "idle", "1"))
        .collect();
    app.open_picker(rows);

    // A short frame cannot show twenty rows; the last one is still reachable.
    let short = dump(&render_size(&app, 80, 12));
    assert!(!short.contains("agent20"), "the tail is clipped at this height:\n{short}");

    app.picker_goto(19);
    let scrolled = dump(&render_size(&app, 80, 12));
    assert!(scrolled.contains("agent20"), "the view follows the highlight:\n{scrolled}");

    // The popup clamps to the body band, so even this over-tall picker never covers the
    // footer — the one surface advertising its keys.
    let last_row = scrolled.lines().last().unwrap_or_default().to_string();
    assert!(last_row.contains("enter"), "the footer keeps the picker's keys: {last_row:?}");
}

#[test]
fn a_click_on_a_picker_row_moves_the_highlight_and_misses_stay_inert() {
    let mut app = picker_app();
    let area = Rect::new(0, 0, 140, 40);
    let out = render(&app);

    let (row_y, line) = out
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("codex"))
        .map(|(y, l)| (y as u16, l.to_string()))
        .expect("the codex row is painted");
    let col = line.find("codex").expect("a column inside the row") as u16;

    assert_eq!(ui::hit_picker_row(area, &app, col, row_y), Some(2));
    // The title row and everything outside the popup are inert.
    assert_eq!(ui::hit_picker_row(area, &app, col, row_y - 3), None);
    assert_eq!(ui::hit_picker_row(area, &app, 0, 0), None);

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left),
            column: col,
            row: row_y,
            modifiers: KeyModifiers::NONE,
        },
        area,
        &[],
        &Keymap::default(),
        &herdr_reviewr::export::Clipboard,
    )
    .unwrap();
    assert_eq!(app.picker_cursor, 2, "a click moves the highlight to the clicked row");
}

// --- Header base label ------------------------------------------------------

/// A repo on branch `feature` past `main`, with `origin/HEAD` naming `main` the default.
/// The repo rides along: opening the picker shells out to git at click time.
fn based_app() -> (Repo, App) {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["branch", "dev"]);
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    (r, app)
}

#[test]
fn the_branch_header_names_the_base_and_its_click_opens_the_picker() {
    let (_repo, mut app) = based_app();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("[branch] vs main"), "the bare base name follows the scope: {line0}");

    let base: Vec<u16> = (0..AREA.width)
        .filter(|&c| ui::hit_header(AREA, &app, app.keymap(), c, 0) == Some(HeaderHit::Base))
        .collect();
    assert!(!base.is_empty(), "the base label is clickable");
    let click = MouseEvent {
        kind: MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left),
        column: base[0],
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let keymap = app.keymap().clone();
    handle_mouse(&mut app, click, AREA, &[], &keymap, &herdr_reviewr::export::Clipboard).unwrap();
    let frame = render(&app);
    assert!(frame.contains("base · 3 branches"), "the click opens the picker popup");
    assert!(frame.contains("dev"), "the sibling branch is a row");
    assert!(frame.contains("default"), "the default branch is marked");
    assert!(frame.contains("current"), "the checked-out branch is marked");
    assert!(!frame.contains('★'), "no glyph: the trail words carry the facts");

    // The box is sized to its rows: the filter line, three branch rows, two borders, and
    // no blank row held for a probe that is not showing.
    let top = frame.lines().position(|l| l.contains("┌ base")).unwrap();
    let bottom = frame.lines().skip(top).position(|l| l.contains("└────")).unwrap();
    assert_eq!(bottom, 5, "top border, filter line, three rows, bottom border: {frame}");
}

#[test]
fn the_picker_title_counts_matches_while_filtering() {
    let (r, mut app) = based_app();
    app.open_base_picker();
    assert!(render(&app).contains("base · 3 branches"));
    app.input_push('d');
    assert!(render(&app).contains("base · 1/3"), "matched over total: {}", render(&app));
    app.close_base_picker();

    // A current non-branch pick is a row, never a count: `HEAD~1` is listed under the
    // three branches but both numbers still say three.
    herdr_reviewr::git::write_base_pick(r.path(), "HEAD~1").unwrap();
    app.set_scope(Scope::Branch).unwrap();
    app.open_base_picker();
    let frame = render(&app);
    assert!(frame.contains("HEAD~1"), "{frame}");
    assert!(frame.contains("base · 3 branches"), "{frame}");
    app.input_push('e');
    let frame = render(&app);
    assert!(
        frame.contains("base · 2/3"),
        "dev and feature match, the rev row counts nowhere: {frame}"
    );
}

#[test]
fn a_probe_row_still_fits_when_every_branch_matches() {
    // One branch, and it matches the query: the frozen full-list height has no spare
    // row, so the box must grow by the hit's row or the tag is unpainted and unclickable.
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["checkout", "-q", "-b", "v1.2-hotfix"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    r.git(&["branch", "-D", "main"]);
    r.git(&["tag", "v1.2", "HEAD~1"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    app.open_base_picker();
    for ch in "v1.2".chars() {
        app.input_push(ch);
    }
    app.run_base_probe();
    let frame = render(&app);
    assert!(frame.contains("v1.2-hotfix"), "{frame}");
    assert!(frame.contains('(') && frame.contains("v1.2 "), "the tag row paints: {frame}");
    let rows: std::collections::BTreeSet<usize> = (0..AREA.height)
        .flat_map(|row| (0..AREA.width).map(move |col| (col, row)))
        .filter_map(|(col, row)| ui::hit_base_picker_row(AREA, &app, col, row))
        .collect();
    assert_eq!(rows.into_iter().collect::<Vec<_>>(), [0, 1], "the hit row is inside the box");
}

fn now_minus(secs: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .saturating_sub(secs)
}

/// A row carrying every trail word, painted at a width that fits and at widths that do not.
fn worst_case_picker(app: &mut App) {
    app.base_picker = Some(BasePicker {
        rows: vec![BaseChoice::Branch {
            name: "main".to_string(),
            pr_base: true,
            is_default: true,
            current: true,
            tip_secs: now_minus(2 * 3600),
        }],
        cursor: 0,
        query: String::new(),
        caret: 0,
        probe: BaseProbe::Idle,
    });
    app.mode = Mode::BasePick;
}

#[test]
fn a_narrow_pane_sheds_trail_words_before_the_name() {
    let mut app = edited_app();
    worst_case_picker(&mut app);
    let wide = dump(&render_size(&app, 80, 20));
    assert!(wide.contains("main"), "{wide}");
    assert!(wide.contains("pr base · default · current · 2h"), "every word fits at 80: {wide}");

    // The popup never exceeds the body, so a narrow pane narrows the row. Words drop from
    // the right, the age first, and the name is the last thing to clip.
    let narrow = dump(&render_size(&app, 34, 20));
    assert!(narrow.contains("main"), "{narrow}");
    assert!(narrow.contains("pr base"), "the first word survives: {narrow}");
    assert!(!narrow.contains("2h"), "the age goes first: {narrow}");
    let tiny = dump(&render_size(&app, 14, 20));
    assert!(tiny.contains("main"), "the name outlives every word: {tiny}");
    assert!(!tiny.contains("pr base"), "{tiny}");
}

#[test]
fn a_probe_row_is_clickable_below_the_matches() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["branch", "v1.2-hotfix"]);
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    r.git(&["tag", "v1.2", "HEAD~1"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    app.open_base_picker();
    for ch in "v1.2".chars() {
        app.input_push(ch);
    }
    app.run_base_probe();
    let frame = render(&app);
    assert!(frame.contains("v1.2-hotfix"), "{frame}");
    let hits: Vec<(u16, u16, usize)> = (0..AREA.height)
        .flat_map(|row| (0..AREA.width).map(move |col| (col, row)))
        .filter_map(|(col, row)| {
            ui::hit_base_picker_row(AREA, &app, col, row).map(|i| (col, row, i))
        })
        .collect();
    let rows: std::collections::BTreeSet<usize> = hits.iter().map(|h| h.2).collect();
    assert_eq!(
        rows.into_iter().collect::<Vec<_>>(),
        [0, 1],
        "both rows hit-test, the probe row too"
    );
    let probe_y = hits.iter().find(|h| h.2 == 1).unwrap().1;
    let branch_y = hits.iter().find(|h| h.2 == 0).unwrap().1;
    assert_eq!(probe_y, branch_y + 1, "the probe row sits under the match");
}

#[test]
fn a_named_rev_paints_the_spelling_and_abbrev() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    herdr_reviewr::git::write_base_pick(r.path(), "HEAD~1").unwrap();
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    assert!(
        line0.contains(&format!("vs HEAD~1 ({short})")),
        "a named rev paints the spelling and the abbreviated SHA: {line0}"
    );
}

#[test]
fn a_sha_pick_paints_once() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    herdr_reviewr::git::write_base_pick(r.path(), &parent).unwrap();
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains(&format!("vs {short}")), "a SHA spelling paints once: {line0}");
    assert!(
        !line0.contains(&format!("vs {short} (")),
        "a SHA spelling does not repeat as a marker: {line0}"
    );

    herdr_reviewr::git::write_base_pick(r.path(), &short).unwrap();
    app.reload().unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(
        line0.contains(&format!("vs {short}")),
        "an abbreviated SHA spelling paints once: {line0}"
    );
    assert!(
        !line0.contains(&format!("vs {short} (")),
        "an abbreviated SHA spelling does not repeat as a marker: {line0}"
    );
}

#[test]
fn a_flag_named_rev_paints_the_same_form() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let mut app = App::new(r.path_buf(), Scope::Branch, Some("HEAD~1".to_string()));
    app.reload().unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    assert!(
        line0.contains(&format!("vs HEAD~1 ({short})")),
        "the --base flag uses the same paint: {line0}"
    );
}

#[test]
fn a_probe_row_is_the_typed_spelling() {
    let (r, mut app) = based_app();
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    app.open_base_picker();
    for ch in "HEAD~1".chars() {
        app.input_push(ch);
    }
    app.run_base_probe();
    let frame = render(&app);
    assert!(frame.contains("HEAD~1"), "the probe row is the typed spelling:\n{frame}");
    assert!(
        frame.contains(&format!("({short})")),
        "a named rev is marked with the abbreviated SHA:\n{frame}"
    );
}

#[test]
fn a_probe_row_right_aligns_the_sha_like_the_open_list() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["branch", "dev"]);
    r.git(&["branch", &format!("release/{}", "x".repeat(40))]);
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    app.open_base_picker();
    for ch in "HEAD~1".chars() {
        app.input_push(ch);
    }
    app.run_base_probe();
    let marker = format!("({short})");
    let picker_row = |frame: &str| {
        frame
            .lines()
            .find(|l| l.contains(&marker) && !l.contains("vs "))
            .expect("the picker row paints")
            .to_string()
    };
    let probe = picker_row(&render(&app));
    let probe_at = probe.find(&marker).unwrap();
    app.base_picker_pick().unwrap();
    app.open_base_picker();
    let open = picker_row(&render(&app));
    let open_at = open.find(&marker).unwrap();
    assert_eq!(
        probe_at, open_at,
        "typing a rev puts `(sha)` in the same column as opening the list:\nopen: {open}\nprobe: {probe}"
    );
    let name_end = probe.find("HEAD~1").expect("the spelling paints") + "HEAD~1".len();
    assert!(probe_at > name_end + 2, "the SHA is right-aligned, not glued to the name:\n{probe}");
}

#[test]
fn a_short_sha_prefix_probe_completes_to_the_abbrev() {
    let (r, mut app) = based_app();
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    let prefix = short[..4].to_string();
    app.open_base_picker();
    for ch in prefix.chars() {
        app.input_push(ch);
    }
    app.run_base_probe();
    let bp = app.base_picker.as_ref().unwrap();
    assert_eq!(bp.visible()[0].name(), short, "the row is the abbreviated SHA, not the prefix");
    let frame = render(&app);
    assert!(
        frame.lines().any(|l| l.contains(&short) && !l.contains(&format!("({short})"))),
        "a unique prefix completes to the abbreviated SHA with no marker:\n{frame}"
    );
}

#[test]
fn a_seven_char_sha_probe_is_not_marked() {
    let (r, mut app) = based_app();
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    app.open_base_picker();
    for ch in short.chars() {
        app.input_push(ch);
    }
    app.run_base_probe();
    let frame = render(&app);
    assert!(frame.contains(&short), "the probe row is the abbreviated SHA:\n{frame}");
    assert!(
        !frame.contains(&format!("({short})")),
        "a spelling that already is that SHA carries no marker:\n{frame}"
    );
}

#[test]
fn a_skipped_named_rev_uses_the_stored_spelling() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    herdr_reviewr::git::write_base_pick(r.path(), "HEAD~1").unwrap();
    r.git(&["checkout", "-q", "-b", "feature"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(
        line0.contains("vs main · HEAD~1 missing"),
        "a skipped non-branch spelling uses the stored spelling: {line0}"
    );
}

#[test]
fn a_skipped_pick_warns_beside_the_resolved_base() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    herdr_reviewr::git::write_base_pick(r.path(), "gone").unwrap();
    r.git(&["checkout", "-q", "-b", "feature"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("vs main · gone missing"), "the dormant pick reads as skipped: {line0}");
}

#[test]
fn without_a_resolving_base_the_header_reads_no_base() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.git(&["branch", "-m", "main", "trunk"]); // no `main`/`master`: no default to fall back on
    r.git(&["checkout", "-q", "-b", "feature"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let frame = render(&app);
    let line0 = frame.lines().next().unwrap().to_string();
    assert!(line0.contains("[branch] no base"), "the empty state is named: {line0}");
    assert!(frame.contains("B base"), "the footer advertises the picker");
}

#[test]
fn a_local_only_repo_has_its_main_as_the_base() {
    // No remote at all: `origin/HEAD` names nothing, and the local `main` is the default,
    // so the header never reads `no base` in a repo that plainly has a trunk.
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("[branch] vs main"), "the local main is the base: {line0}");
    assert!(!line0.contains("missing"), "nothing is skipped: {line0}");
    assert!(line0.contains("1 changed"), "the branch diffs against it: {line0}");

    // On `main` itself the base is still `main`: the scope is the uncommitted diff.
    r.git(&["checkout", "-q", "main"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("[branch] vs main"), "{line0}");
}

#[test]
fn a_dormant_pick_shows_beside_the_empty_state() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.git(&["branch", "-m", "main", "trunk"]); // no `main`/`master`: no default to fall back on
    herdr_reviewr::git::write_base_pick(r.path(), "gone").unwrap();
    r.git(&["checkout", "-q", "-b", "feature"]);
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(
        line0.contains("no base · gone missing"),
        "a dormant choice never reads as never-chosen: {line0}"
    );
}

#[test]
fn a_named_rev_clips_the_spelling_and_keeps_the_sha() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let parent = r.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let long = format!("release-{}", "x".repeat(80));
    r.git(&["tag", &long, &parent]);
    herdr_reviewr::git::write_base_pick(r.path(), &long).unwrap();
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = dump(&render_size(&app, 80, 20)).lines().next().unwrap().to_string();
    let short = herdr_reviewr::git::abbreviate_oid(&parent);
    assert!(line0.contains(&format!("({short})")), "the SHA marker survives the clip: {line0}");
    assert!(line0.contains('…'), "the spelling truncates: {line0}");
    assert!(line0.contains("1 changed"), "the right-aligned stats survive: {line0}");
}

#[test]
fn an_overlong_base_name_truncates_with_an_ellipsis() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    let long = format!("feature/{}", "x".repeat(80));
    r.git(&["branch", &long]);
    herdr_reviewr::git::write_base_pick(r.path(), &long).unwrap();
    r.git(&["checkout", "-q", "-b", "work"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = dump(&render_size(&app, 80, 20)).lines().next().unwrap().to_string();
    assert!(line0.contains("vs feature/x"), "the name paints up to the fit: {line0}");
    assert!(line0.contains('…'), "the overflow truncates with a trailing ellipsis: {line0}");
    assert!(line0.contains("1 changed"), "the right-aligned stats survive the long name: {line0}");
}

#[test]
fn a_narrow_header_never_maps_a_click_outside_the_painted_base() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    let long = format!("feature/{}", "x".repeat(60));
    r.git(&["branch", &long]);
    herdr_reviewr::git::write_base_pick(r.path(), &long).unwrap();
    r.git(&["checkout", "-q", "-b", "work"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();

    // The base label truncates to its budget at a narrow width, and the hit test walks the
    // same arithmetic the paint does: every column it claims carries painted label, and the
    // claim is one unbroken run.
    for width in [40u16, 56, 72] {
        let area = Rect { x: 0, y: 0, width, height: 12 };
        let line0 = dump(&render_size(&app, width, 12)).lines().next().unwrap().to_string();
        let cells: Vec<char> = line0.chars().collect();
        let hits: Vec<u16> = (0..width)
            .filter(|&c| ui::hit_header(area, &app, app.keymap(), c, 0) == Some(HeaderHit::Base))
            .collect();
        let Some((&first, &last)) = hits.first().zip(hits.last()) else {
            // Too narrow for even one column of the name: the base left the header whole,
            // so nothing paints a nameless `vs` and nothing claims it.
            assert!(!line0.contains("vs"), "width {width}: a nameless `vs` paints: {line0}");
            assert!(line0.contains("[branch]"), "width {width}: the scope survives: {line0}");
            continue;
        };
        assert_eq!(
            hits.len() as u16,
            last - first + 1,
            "width {width}: the base claims one unbroken run"
        );
        let claimed: String = hits.iter().map(|&c| cells[c as usize]).collect();
        assert_ne!(claimed.trim(), "vs", "width {width}: a nameless `vs` is claimed: {line0}");
        assert!(
            !claimed.ends_with(' '),
            "width {width}: the claim runs past the painted label: {line0}"
        );
        assert_eq!(
            cells.get(last as usize + 1).copied(),
            Some(' '),
            "width {width}: the claim stops short of the painted label: {line0}"
        );
    }
}

#[test]
fn an_overlong_skipped_tail_never_evicts_the_base_name() {
    let r = Repo::init();
    r.write("hello.rs", "alpha\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    let long = format!("feature/{}", "x".repeat(80));
    herdr_reviewr::git::write_base_pick(r.path(), &long).unwrap();
    r.git(&["checkout", "-q", "-b", "work"]);
    r.write("hello.rs", "alpha\nBETA\n");
    r.commit_all("edit");
    let mut app = app_on(&r);
    app.set_scope(Scope::Branch).unwrap();
    let line0 = dump(&render_size(&app, 80, 20)).lines().next().unwrap().to_string();
    assert!(line0.contains("vs main"), "the resolved name keeps first claim: {line0}");
    assert!(line0.contains("· feature/x"), "the skipped tail paints in what remains: {line0}");
    assert!(line0.contains('…'), "the tail truncates with a trailing ellipsis: {line0}");
    assert!(line0.contains("1 changed"), "the right-aligned stats survive the long tail: {line0}");
}

// ---- mouse text selection ----

/// A repo with one uncommitted three-line file, for selection geometry.
fn selection_app() -> (Repo, App) {
    let r = Repo::init();
    r.write("base.rs", "fn main() {}\n");
    r.commit_all("init");
    r.write("m.rs", "alpha beta\n\tif x {\n日本 z\n");
    let app = app_on(&r);
    (r, app)
}

#[test]
fn the_hovered_row_shows_a_plus_in_its_change_bar_cell() {
    let (_repo, mut app) = selection_app();
    let area = Rect::new(0, 0, 140, 40);
    let inner = ui::read_inner_rect(area, &app);

    // No hover: the insertion row paints its change bar.
    let buf = render_buffer(&app);
    assert_eq!(buf.cell((inner.x, inner.y)).unwrap().symbol(), "▌");

    // Hovering anywhere on the row puts the `[+]` button over the number field; the
    // change bar stays, so the diff signal never blinks.
    app.hover = Some((inner.x + 8, inner.y));
    let buf = render_buffer(&app);
    assert_eq!(buf.cell((inner.x, inner.y)).unwrap().symbol(), "▌");
    assert_eq!(buf.cell((inner.x + 1, inner.y)).unwrap().symbol(), "[");
    assert_eq!(buf.cell((inner.x + 2, inner.y)).unwrap().symbol(), "+");
    assert_eq!(buf.cell((inner.x + 3, inner.y)).unwrap().symbol(), "]");
    // The unhovered row below keeps its bar and number.
    assert_eq!(buf.cell((inner.x, inner.y + 1)).unwrap().symbol(), "▌");
}

#[test]
fn the_plus_button_right_aligns_in_a_wide_number_field() {
    // A 1000-line file widens the number field past the 3-column minimum, so the
    // right-aligned `[+]` leaves blank padding on its left, like the numbers it
    // replaces.
    let r = Repo::init();
    r.write("base.rs", "fn main() {}\n");
    r.commit_all("init");
    let body = (1..=1000).fold(String::new(), |mut s, i| {
        use std::fmt::Write;
        let _ = writeln!(s, "line {i}");
        s
    });
    r.write("long.rs", &body);
    let mut app = app_on(&r);
    let area = Rect::new(0, 0, 140, 40);
    let inner = ui::read_inner_rect(area, &app);

    app.hover = Some((inner.x + 8, inner.y));
    let buf = render_buffer(&app);
    assert_eq!(buf.cell((inner.x, inner.y)).unwrap().symbol(), "▌");
    assert_eq!(buf.cell((inner.x + 1, inner.y)).unwrap().symbol(), " ", "left pad, not `[`");
    assert_eq!(buf.cell((inner.x + 2, inner.y)).unwrap().symbol(), "[");
    assert_eq!(buf.cell((inner.x + 3, inner.y)).unwrap().symbol(), "+");
    assert_eq!(buf.cell((inner.x + 4, inner.y)).unwrap().symbol(), "]");
    // The unhovered row below right-aligns its number in the same field.
    assert_eq!(buf.cell((inner.x + 4, inner.y + 1)).unwrap().symbol(), "2");
}

#[test]
fn the_text_selection_highlights_the_dragged_span() {
    use herdr_reviewr::selection::{Point, Surface, TextDrag};
    let (_repo, mut app) = selection_app();
    let area = Rect::new(0, 0, 140, 40);
    let inner = ui::read_inner_rect(area, &app);
    let sel_bg = app.palette().sel_bg;
    // The selection fill is its own slot, distinct by hue from the cursor fills, so a
    // selection reads inside a cursor row. Park the cursor on the fully
    // selected middle row so its cells still assert the selection fill won.
    app.diff_cursor = 1;

    // `beta` on row 0 through char 1 (`本`) of row 2: a three-row stream selection.
    app.gesture = herdr_reviewr::selection::Gesture::Text {
        drag: TextDrag {
            surface: Surface::Read,
            anchor: Point { row: 0, chr: 6 },
            extent: Point { row: 2, chr: 1 },
        },
        count: 1,
    };
    let buf = render_buffer(&app);
    let bg = |x: u16, y: u16| buf.cell((x, y)).unwrap().style().bg;
    // The `b` of beta is selected; the chars before the anchor are not — the first row runs
    // from its start character, not whole.
    assert_eq!(bg(inner.x + 5 + 6, inner.y), Some(sel_bg));
    assert_ne!(bg(inner.x + 5, inner.y), Some(sel_bg));
    assert_ne!(bg(inner.x + 5 + 5, inner.y), Some(sel_bg));
    // Row 1 lies whole between the endpoints: tab expansion through its last char.
    assert_eq!(bg(inner.x + 5, inner.y + 1), Some(sel_bg));
    assert_eq!(bg(inner.x + 5 + 6, inner.y + 1), Some(sel_bg));
    // Row 2 runs up to its end character: both wide glyphs (each asserted at its first
    // cell — the buffer diff skips a wide char's hidden continuation cell), and nothing
    // past them.
    assert_eq!(bg(inner.x + 5, inner.y + 2), Some(sel_bg));
    assert_eq!(bg(inner.x + 5 + 2, inner.y + 2), Some(sel_bg));
    assert_ne!(bg(inner.x + 5 + 4, inner.y + 2), Some(sel_bg));
}

// --- Commit picker and the commits header --------------------

/// `main` with four commits, root first, plus an uncommitted edit. Returns the shas root
/// first.
fn commits_app() -> (Repo, App, Vec<String>) {
    let r = Repo::init();
    r.write("root.rs", "r\n");
    r.commit_all("root");
    r.write("one.rs", "1\n");
    r.commit_all("one");
    r.write("two.rs", "2\n");
    r.commit_all("two");
    r.write("three.rs", "3\n");
    r.commit_all("Stop counting git's own lock files");
    r.write("root.rs", "dirty\n");
    let shas: Vec<String> =
        r.git(&["rev-list", "--reverse", "HEAD"]).lines().map(str::to_string).collect();
    let app = app_on(&r);
    (r, app, shas)
}

fn short(sha: &str) -> &str {
    &sha[..7]
}

#[test]
fn the_commit_picker_paints_rows_a_run_bar_and_its_count() {
    let (r, mut app, shas) = commits_app();
    app.open_commit_picker();
    app.commit_picker_anchor();
    app.commit_picker_move(2);
    let buf = render_buffer(&app);
    let out = dump(&buf);
    assert!(out.contains("commits · last 50"), "the title names the universe:\n{out}");
    assert!(out.contains(short(&shas[3])), "a row leads with the sha:\n{out}");
    assert!(out.contains("Stop counting git's own lock files"), "then the subject:\n{out}");
    // The author sits in its own right-aligned column, the checked-out branch is not a ref.
    let rows: Vec<&str> = out.lines().filter(|l| l.contains("  Test ")).collect();
    assert_eq!(rows.len(), 4, "every row carries the author:\n{out}");
    let ends: std::collections::HashSet<usize> =
        rows.iter().map(|l| l[..l.find("  Test ").unwrap()].chars().count()).collect();
    assert_eq!(ends.len(), 1, "the author column is one edge:\n{out}");
    assert!(!out.contains("· main"), "the checked-out branch is never a ref:\n{out}");
    let rows: Vec<&str> = out.lines().filter(|l| l.contains("▎")).collect();
    assert_eq!(rows.len(), 3, "the run carries a bar:\n{out}");
    assert!(
        !out.lines().any(|l| l.contains("▎") && l.contains(short(&shas[0]))),
        "the root is outside the run"
    );
    let footer = footer_line(&out);
    assert!(footer.contains("enter open 3"), "the footer counts the run: {footer}");
    assert!(footer.contains("v select") && footer.contains("esc clear"), "{footer}");
    // The footer stays bright, the view behind recedes.
    let plain = {
        let mut a = app_on(&r);
        a.focus = Focus::Files;
        render_buffer(&a)
    };
    let x = (0..plain.area.width)
        .find(|&x| {
            plain
                .cell((x, 0))
                .is_some_and(|c| c.symbol().chars().all(char::is_alphanumeric) && c.symbol() != " ")
        })
        .expect("a lettered cell in the tab bar");
    assert_ne!(
        plain.cell((x, 0)).unwrap().fg,
        buf.cell((x, 0)).unwrap().fg,
        "the header is scrimmed"
    );

    // Without an anchor the hint is a plain `open` and `esc` cancels.
    app.commit_picker_escape();
    let footer = footer_line(&render(&app));
    assert!(footer.contains("enter open") && !footer.contains("open 3"), "{footer}");
    assert!(footer.contains("esc cancel"), "{footer}");
    // A click on a row moves the highlight, a click on the highlight picks.
    let row_y = (0..40u16)
        .find(|&y| {
            (0..140u16).any(|x| {
                buf.cell((x, y)).is_some_and(|c| {
                    c.symbol() == short(&shas[1]).chars().next().unwrap().to_string()
                })
            }) && dump(&buf).lines().nth(y as usize).is_some_and(|l| l.contains(short(&shas[1])))
        })
        .expect("the row for `one`");
    let col = dump(&buf).lines().nth(row_y as usize).unwrap().find(short(&shas[1])).unwrap() as u16;
    let hit = ui::hit_commit_picker_row(AREA, &app, col, row_y);
    assert_eq!(hit, Some(2), "the row under the pointer");
}

#[test]
fn the_commits_header_names_the_pick_and_its_verdict() {
    let (r, mut app, shas) = commits_app();
    app.open_commit_picker();
    app.commit_picker_pick().unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(
        line0
            .contains(&format!("[commits] {} Stop counting git's own lock files", short(&shas[3]))),
        "a run of one paints sha and subject: {line0}"
    );
    assert!(line0.contains("1 changed"), "{line0}");
    // The pick name is clickable and opens the picker.
    let cols: Vec<u16> = (0..AREA.width)
        .filter(|&c| ui::hit_header(AREA, &app, app.keymap(), c, 0) == Some(HeaderHit::Pick))
        .collect();
    assert!(!cols.is_empty(), "the pick name is a header hit");
    let footer = footer_line(&render(&app));
    assert!(
        !footer.contains("G commits"),
        "row 1 never carries the picker key while picking works"
    );
    app.keys_expanded = true;
    let expanded = render(&app);
    assert!(expanded.contains("u/b/t/g scope"), "the go band names four scopes:\n{expanded}");
    assert!(expanded.contains("G commits"), "and the picker key:\n{expanded}");
    app.keys_expanded = false;

    // A run paints `a..b (N)`.
    app.open_commit_picker();
    app.commit_picker_anchor();
    app.commit_picker_move(2);
    app.commit_picker_pick().unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(
        line0.contains(&format!("[commits] {}..{} (3)", short(&shas[1]), short(&shas[3]))),
        "a run paints its ends and count: {line0}"
    );

    // Truncation keeps the sha and the marker, the subject clips.
    app.open_commit_picker();
    app.commit_picker_escape(); // drop the restored anchor: a run of one again
    app.commit_picker_pick().unwrap();
    let narrow = render_at(&app, 72).lines().next().unwrap().to_string();
    assert!(narrow.contains(short(&shas[3])), "the sha survives: {narrow}");
    assert!(narrow.contains('…'), "the subject clips: {narrow}");

    // Off branch: the marker follows the pick.
    r.git(&["reset", "-q", "--hard", &shas[2]]);
    r.write("three.rs", "again\n");
    r.commit_all("three again");
    common::land_world(&mut app);
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("· off branch"), "{line0}");
    let narrow = render_at(&app, 72).lines().next().unwrap().to_string();
    assert!(narrow.contains("· off branch"), "the marker survives truncation: {narrow}");
    // And the picker shows the pick as a row above the list.
    app.open_commit_picker();
    let out = render(&app);
    let pick_line = out.lines().find(|l| l.contains("· off branch")).expect("the pick row");
    assert!(pick_line.contains(short(&shas[3])), "{pick_line}");
    app.close_commit_picker();

    // Gone: both panes read the message, row 1 leads with the picker key.
    r.git(&["reflog", "expire", "--expire=now", "--all"]);
    r.git(&["gc", "-q", "--prune=now"]);
    common::land_world(&mut app);
    let out = render(&app);
    let line0 = out.lines().next().unwrap().to_string();
    assert!(line0.contains("· gone"), "{line0}");
    assert_eq!(
        out.matches(&format!("commit {} is gone", short(&shas[3]))).count(),
        2,
        "both panes:\n{out}"
    );
    let footer = footer_line(&out);
    assert!(footer.trim_start().starts_with("G commits"), "{footer}");
    assert!(footer.contains("u/b/t scope"), "the other three scopes: {footer}");
}

#[test]
fn a_gone_run_paints_no_count_and_a_verdict_paints_only_in_commits() {
    let (r, mut app, shas) = commits_app();
    app.open_commit_picker();
    app.commit_picker_anchor();
    app.commit_picker_move(1);
    app.commit_picker_pick().unwrap();
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("(2)"), "{line0}");
    // Rewrite the tip: the run is off branch, and the header says so in `commits` only.
    r.git(&["reset", "-q", "--hard", &shas[1]]);
    r.write("three.rs", "rewritten\n");
    r.commit_all("three again");
    common::land_world(&mut app);
    assert!(render(&app).lines().next().unwrap().contains("· off branch"));
    app.set_scope(Scope::Uncommitted).unwrap();
    app.open_commit_picker();
    let out = render(&app);
    let pick_line = out.lines().find(|l| l.contains("..")).expect("the pick row");
    assert!(pick_line.contains("(2)") && !pick_line.contains("off branch"), "{pick_line}");
    app.close_commit_picker();
    // Pruned: no count beside `· gone`.
    app.set_scope(Scope::Commits).unwrap();
    r.git(&["reflog", "expire", "--expire=now", "--all"]);
    r.git(&["gc", "-q", "--prune=now"]);
    common::land_world(&mut app);
    let line0 = render(&app).lines().next().unwrap().to_string();
    assert!(line0.contains("· gone") && !line0.contains("(0)"), "{line0}");
}

#[test]
fn a_wide_glyph_author_keeps_the_age_column() {
    let (r, mut app, _) = commits_app();
    r.write("four.rs", "4\n");
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "four", "--author=田中太郎 <t@example.com>"]);
    app.open_commit_picker();
    let out = render(&app);
    let ages: Vec<usize> = out
        .lines()
        .skip(1)
        .filter(|l| l.contains("  ") && (l.contains("Test") || l.contains("田")))
        // The test backend dumps one char per cell, so a char index is a column.
        .map(|l| {
            let cells: Vec<char> = l.trim_end_matches([' ', '│']).chars().collect();
            cells.iter().rposition(|c| *c == ' ').unwrap()
        })
        .collect();
    assert!(out.contains("田"), "{out}");
    assert!(ages.len() >= 2 && ages.iter().all(|&a| a == ages[0]), "ages align:\n{out}");
}

#[test]
fn the_picker_trail_counts_comments_and_a_tall_list_says_more() {
    let (r, mut app, shas) = commits_app();
    // A comment on `two`.
    app.open_commit_picker();
    app.commit_picker_move(1);
    app.commit_picker_pick().unwrap();
    app.select_file(0).unwrap();
    app.focus = Focus::Diff;
    app.diff_cursor = app.diff.rows.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
    app.input_push('x');
    app.submit_comment();
    app.open_commit_picker();
    let out = render(&app);
    let two = out.lines().skip(1).find(|l| l.contains(&shas[2][..7])).unwrap();
    assert!(two.contains("✎ 1"), "the commented commit counts its comments: {two}");
    let three = out.lines().skip(1).find(|l| l.contains(&shas[3][..7])).unwrap();
    assert!(!three.contains('✎'), "an uncommented one does not: {three}");
    app.close_commit_picker();

    // Forty more commits: a 12-row terminal clips the list and says so.
    for i in 0..40 {
        r.write("n.rs", &format!("{i}\n"));
        r.commit_all(&format!("n{i}"));
    }
    app.open_commit_picker();
    let buf = render_size(&app, 140, 12);
    let out = dump(&buf);
    let more = out.lines().find(|l| l.contains("… ")).expect("the clip line");
    assert!(more.contains("more"), "{more}");
    assert!(!out.contains(&shas[0][..7]), "the root is below the clip");
    // The clip line is not a row: a click on it is inert.
    let y = out.lines().position(|l| l.contains("… ")).unwrap() as u16;
    let x = more.find('…').unwrap() as u16;
    assert_eq!(ui::hit_commit_picker_row(Rect::new(0, 0, 140, 12), &app, x, y), None);
}

#[test]
fn an_empty_universe_names_itself() {
    let r = Repo::init();
    let mut app = app_on(&r);
    app.open_commit_picker();
    assert_eq!(app.mode, Mode::CommitPick);
    let out = render(&app);
    assert!(out.contains("no commits yet"), "{out}");
    let footer = footer_line(&out);
    assert!(
        footer.contains("esc cancel") && !footer.contains("enter"),
        "only the exit offers: {footer}"
    );
    app.commit_picker_pick().unwrap();
    assert_eq!(app.mode, Mode::CommitPick, "enter does nothing");
    app.close_commit_picker();

    // On the base branch itself the range is empty, so the universe is the last 50.
    r.write("a.rs", "a\n");
    r.commit_all("init");
    r.set_origin_default("main", "main");
    let mut app = app_on(&r);
    app.open_commit_picker();
    let out = render(&app);
    assert!(out.contains("commits · last 50"), "{out}");
    assert!(out.contains("init"), "{out}");
}

#[test]
fn a_row_shows_one_ref_by_what_matters_most() {
    let (r, mut app, shas) = commits_app();
    r.set_origin_default("main", &shas[1]);
    r.git(&["update-ref", "refs/remotes/origin/feature", &shas[3]]);
    r.git(&["branch", "spike", &shas[3]]);
    r.git(&["tag", "v1", &shas[3]]);
    r.git(&["branch", "other", &shas[2]]);
    app.open_commit_picker();
    let out = render(&app);
    let top = out.lines().skip(1).find(|l| l.contains(&shas[3][..7])).unwrap();
    assert!(top.contains("origin/feature"), "a remote tip outranks a tag and a branch: {top}");
    assert!(!top.contains("spike") && !top.contains("v1"), "one ref only: {top}");
    let two = out.lines().skip(1).find(|l| l.contains(&shas[2][..7])).unwrap();
    assert!(two.contains("other"), "a lone local branch shows: {two}");
    app.close_commit_picker();
    r.git(&["branch", "feat/two", &shas[2]]);
    r.git(&["tag", "v0", &shas[2]]);
    app.open_commit_picker();
    let out = render(&app);
    let two = out.lines().skip(1).find(|l| l.contains(&shas[2][..7])).unwrap();
    assert!(two.contains("tag: v0") && !two.contains("feat/two"), "a slash is no remote: {two}");
    app.close_commit_picker();

    // The open PR's head outranks every ref.
    app.pr = herdr_reviewr::forge::PrView::Pr(Box::new(herdr_reviewr::forge::PrSnapshot {
        head_oid: shas[3].clone(),
        ..common::pr_snapshot()
    }));
    app.open_commit_picker();
    let out = render(&app);
    let top = out.lines().skip(1).find(|l| l.contains(&shas[3][..7])).unwrap();
    assert!(top.contains("  pr ") && !top.contains("origin/feature"), "{top}");
}

/// The default-right navigator's first inner column on the 140-wide test frame.
const FILES_X0: u16 = 140 - 140 * 32 / 100 + 1;
/// Its last inner column: the frame edge less the right border.
const FILES_X1: u16 = 140 - 2;

/// The files-pane row holding `token`: its y, and its text from the pane's first inner
/// column to its last, untrimmed, so a test can check both what a row ends in and where.
fn files_row_at(buf: &Buffer, token: &str) -> (u16, String) {
    let row = |y: u16| -> String {
        (FILES_X0..=FILES_X1).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect()
    };
    let y = (0..buf.area.height)
        .find(|&y| row(y).contains(token))
        .unwrap_or_else(|| panic!("no files-pane row holds {token:?}"));
    (y, row(y))
}

/// The files-pane row holding `token`, trailing padding trimmed.
fn files_row(app: &App, token: &str) -> String {
    files_row_at(&render_buffer(app), token).1.trim_end().to_string()
}

/// Whether the row holding `token` ends in the dot, painted in the pane's last inner column
/// and in the `M` marker's color (the cell style of `m_marker_fg`).
fn dot_at_edge(app: &App, token: &str) -> bool {
    let buf = render_buffer(app);
    let (y, _) = files_row_at(&buf, token);
    let cell = buf.cell((FILES_X1, y)).unwrap();
    cell.symbol() == "•" && cell.style().fg == Some(m_marker_fg(&buf))
}

/// The color of the `M` marker on the fixtures' edited `zz.rs` row.
fn m_marker_fg(buf: &Buffer) -> ratatui::style::Color {
    let (y, text) = files_row_at(buf, "M zz.rs");
    let x = FILES_X0 + text.find("M zz.rs").unwrap() as u16;
    buf.cell((x, y)).unwrap().style().fg.expect("the marker is colored")
}

/// A worktree with `src/{app.rs,ui.rs}` and `docs/{a.md,b.md}` committed and `src/ui.rs`
/// edited, plus a top-level edited `zz.rs` so an `M` marker is always painted for the color
/// reference.
fn dotted_repo() -> Repo {
    let r = Repo::init();
    r.write("src/app.rs", "x\n");
    r.write("src/ui.rs", "y\n");
    r.write("docs/a.md", "a\n");
    r.write("docs/b.md", "b\n");
    r.write("zz.rs", "z\n");
    r.commit_all("init");
    r.write("src/ui.rs", "y2\n");
    r.write("zz.rs", "z2\n");
    r
}

#[test]
fn a_collapsed_all_files_folder_with_a_change_wears_a_dot() {
    let r = dotted_repo();
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    assert!(dot_at_edge(&app, "src/"), "src/ holds the edit: {:?}", files_row(&app, "src/"));
    assert!(!files_row(&app, "docs/").contains('•'), "docs/ holds no change");
    assert!(files_row(&app, "src/").starts_with("▸ src/"), "the chevron is the first column");

    // Expanded, the children carry their own marker and the folder drops the dot.
    app.focus = Focus::Files;
    app.file_cursor = app.file_rows.iter().position(|r| r.dir_path() == Some("src")).unwrap();
    app.expand_dir();
    assert!(!files_row(&app, "src/").contains('•'), "an expanded folder wears no dot");
    assert!(files_row(&app, "ui.rs").starts_with("  M ui.rs"), "the child carries the marker");
}

#[test]
fn a_kept_change_under_an_ignored_folder_wears_the_same_dot() {
    // The folder name dims, the dot keeps its color.
    let r = dotted_repo();
    r.write(".gitignore", "vendor/\n");
    r.write("vendor/lib.rs", "v\n");
    r.write("vendor/other.rs", "o\n");
    r.git(&["add", "-f", "vendor/lib.rs", "vendor/other.rs", ".gitignore"]);
    r.commit_all("vendor");
    r.write("vendor/lib.rs", "v2\n");
    r.write("zz.rs", "z3\n");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    assert!(dot_at_edge(&app, "vendor/"), "{:?}", files_row(&app, "vendor/"));
}

#[test]
fn a_folder_whose_only_change_has_no_row_wears_no_dot() {
    // A staged deletion leaves the index, so `All files` has no row for it and the folder
    // stays quiet: a dot there would open onto nothing. A plain `rm` keeps the index entry,
    // so its row stays, marked `D`, and the folder wears the dot.
    let r = dotted_repo();
    r.write("gone/a.rs", "a\n");
    r.write("gone/b.rs", "b\n");
    r.write("rmd/a.rs", "a\n");
    r.write("rmd/b.rs", "b\n");
    r.commit_all("more");
    r.git(&["rm", "-q", "gone/a.rs"]);
    std::fs::remove_file(r.path_buf().join("rmd/a.rs")).unwrap();
    r.write("zz.rs", "z3\n");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    assert!(!files_row(&app, "gone/").contains('•'), "{:?}", files_row(&app, "gone/"));
    assert!(dot_at_edge(&app, "rmd/"), "{:?}", files_row(&app, "rmd/"));
}

#[test]
fn a_collapsed_changes_folder_wears_no_dot_and_reserves_nothing() {
    // On `Changes` every folder holds a change, so the dot would say nothing, and the row
    // keeps its full width for the name.
    let r = dotted_repo();
    r.write("src/app.rs", "x2\n"); // two changed files keep `src/` a directory row
    let mut app = app_on(&r);
    app.focus = Focus::Files;
    app.file_cursor = app.file_rows.iter().position(|r| r.dir_path() == Some("src")).unwrap();
    app.collapse_dir();
    assert!(!files_row(&app, "src/").contains('•'), "no dot on Changes");
    let wide = "n".repeat((FILES_X1 - FILES_X0) as usize - 2); // `▸ ` + name + `/` fills the row
    r.write(&format!("{wide}/a.rs"), "a\n");
    r.write(&format!("{wide}/b.rs"), "b\n");
    app.reload().unwrap();
    let row = files_row(&app, &wide[..20]);
    assert_eq!(row, format!("▾ {wide}/"), "the exact-fit name is whole on Changes");
    enter_tab(&mut app, Tab::AllFiles);
    assert!(dot_at_edge(&app, "…"), "{:?}", files_row(&app, "…"));
    assert_eq!(
        files_row(&app, "…"),
        format!("▸ …{}/ •", &wide[3..]),
        "the reserve elides two columns"
    );
}

#[test]
fn the_folder_dot_follows_the_scope() {
    let r = Repo::init();
    r.write("src/a.rs", "x\n");
    r.write("src/b.rs", "y\n");
    r.write("zz.rs", "z\n");
    r.commit_all("base");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("src/a.rs", "x2\n");
    r.write("zz.rs", "z2\n");
    r.commit_all("feature work"); // committed on the branch, worktree clean
    let mut app = App::new(r.path_buf(), Scope::Uncommitted, Some("main".to_string()));
    app.reload().unwrap();
    enter_tab(&mut app, Tab::AllFiles);
    assert!(!files_row(&app, "src/").contains('•'), "nothing uncommitted under src/");

    app.set_scope(Scope::Branch).unwrap();
    common::land_world(&mut app);
    assert!(dot_at_edge(&app, "src/"), "the branch scope changed src/a.rs");

    app.set_scope(Scope::Uncommitted).unwrap();
    common::land_world(&mut app);
    assert!(!files_row(&app, "src/").contains('•'), "back to uncommitted, the dot clears");
}

#[test]
fn a_long_folder_name_leaves_room_for_the_dot() {
    let r = Repo::init();
    let dir = "a_directory_name_far_wider_than_the_files_pane_can_ever_hold_at_this_width";
    r.write(&format!("{dir}/one.rs"), "1\n");
    r.write(&format!("{dir}/two.rs"), "2\n");
    r.write("zz.rs", "z\n");
    r.commit_all("init");
    r.write(&format!("{dir}/one.rs"), "1b\n");
    r.write("zz.rs", "z2\n");
    let mut app = app_on(&r);
    enter_tab(&mut app, Tab::AllFiles);
    assert!(dot_at_edge(&app, "…"), "{:?}", files_row(&app, "…"));
    let row = files_row(&app, "…");
    assert!(row.starts_with("▸ …") && row.contains("this_width/ •"), "head-elided: {row:?}");
    let collapsed_name = row.trim_end_matches(" •").to_string();

    app.focus = Focus::Files;
    app.file_cursor = app.file_rows.iter().position(|r| r.dir_path() == Some(dir)).unwrap();
    app.expand_dir();
    let row = files_row(&app, "…");
    assert_eq!(row.replacen('▾', "▸", 1), collapsed_name, "the name reads the same expanded");
}

#[test]
fn a_stacked_prs_target_names_its_source_in_the_header() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let (r, mut app) = based_app();
    r.git(&["checkout", "-q", "-b", "child"]);
    r.write("b.rs", "child\n");
    r.commit_all("child work");
    app.set_scope(Scope::Branch).unwrap();
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        base_ref: "feature".to_string(),
        ..common::pr_snapshot()
    })));
    app.reload().unwrap();
    let frame = render(&app);
    assert!(frame.contains("vs feature (pr base)"), "{frame}");

    // A pick of its own carries no source marker.
    app.open_base_picker();
    for ch in "dev".chars() {
        app.input_push(ch);
    }
    app.base_picker_pick().unwrap();
    let frame = render(&app);
    assert!(frame.contains("vs dev") && !frame.contains("(pr base)"), "{frame}");
}

#[test]
fn the_pr_navigator_lists_the_stack_top_first_with_this_pr_marked() {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::{PrSnapshot, PrState, PrView, StackEntry};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let entry = |number, title: &str, base: &str, state, level| StackEntry {
        number,
        title: title.to_string(),
        state,
        is_draft: false,
        head_ref: String::new(),
        base_ref: base.to_string(),
        level,
        url: None,
    };
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        number: 11,
        stack: vec![
            entry(10, "Parent work", "main", PrState::Merged, -1),
            entry(11, "This work", "feature-a", PrState::Open, 0),
            entry(12, "Child work", "feature-b", PrState::Open, 1),
        ],
        ..common::pr_snapshot()
    })));
    let frame = render_size(&app, 120, 30);
    let text = dump(&frame);
    assert!(text.contains("stack · 3"), "{text}");
    let row = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap();
    assert!(row("#12 open") < row("#11 open") && row("#11 open") < row("#10 merged"), "{text}");
    let checked_out = text.lines().nth(row("#11 open")).unwrap();
    assert!(checked_out.contains("● #11") && checked_out.contains("checked out"), "{text}");
    assert!(row("└ main") == row("#10 merged") + 1, "the trunk closes the stack: {text}");

    // A PR alone lists no stack section.
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot { number: 11, ..common::pr_snapshot() })));
    assert!(!dump(&render_size(&app, 120, 30)).contains("stack ·"));
}

/// Load a config whose `pane_outer_borders` is off, the way the event loop applies a reread.
fn borderless(app: &mut App) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "pane_outer_borders = false\n").unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
}

/// The painted text on frame row `y` from column `x`, `len` cells long.
fn cells(buf: &Buffer, x: u16, y: u16, len: u16) -> String {
    (x..(x + len).min(buf.area.width)).map(|cx| buf[(cx, y)].symbol().to_string()).collect()
}

const POSITIONS: [NavigatorPosition; 4] = [
    NavigatorPosition::Right,
    NavigatorPosition::Bottom,
    NavigatorPosition::Left,
    NavigatorPosition::Top,
];

#[test]
fn pane_outer_borders_off_drops_the_frames_and_keeps_one_divider_in_every_position() {
    let mut framed = edited_app();
    let mut app = edited_app();
    borderless(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    for position in POSITIONS {
        framed.navigator_position = position;
        app.navigator_position = position;
        let boxed = dump(&render_size(&framed, 80, 24));
        assert!(boxed.contains('┌') && boxed.contains('┘'), "on: today's frames ({position:?})");

        let buf = render_size(&app, 80, 24);
        let out = dump(&buf);
        for corner in ['┌', '┐', '└', '┘'] {
            assert!(!out.contains(corner), "off: no frame corner {corner} ({position:?}):\n{out}");
        }
        // One divider line between the panes, not the two meeting borders.
        let body = ui::body_rect(area, &app);
        let hits: Vec<(u16, u16)> = (body.y..body.y + body.height)
            .flat_map(|y| (body.x..body.x + body.width).map(move |x| (x, y)))
            .filter(|&(x, y)| ui::hit_divider(area, &app, x, y))
            .collect();
        let (line, glyph) = if position.stacked() {
            (hits.iter().map(|h| h.1).collect::<std::collections::BTreeSet<_>>(), "─")
        } else {
            (hits.iter().map(|h| h.0).collect::<std::collections::BTreeSet<_>>(), "│")
        };
        assert_eq!(line.len(), 1, "a one-cell divider ({position:?}):\n{out}");
        assert!(hits.iter().all(|&(x, y)| buf[(x, y)].symbol() == glyph), "{position:?}:\n{out}");

        // Painted and hit-tested geometry agree: each content rect's first cell is the pane's
        // first content, and the title sits on the row above it.
        let files = ui::files_inner_rect(area, &app);
        assert_eq!(cells(&buf, files.x, files.y, 10), "M hello.rs", "{position:?}:\n{out}");
        assert!(cells(&buf, files.x, files.y - 1, 7).contains("Files"), "{position:?}:\n{out}");
        let read = ui::read_inner_rect(area, &app);
        let first = cells(&buf, read.x, read.y, 12);
        assert!(first.trim_start().starts_with("1 alpha"), "{position:?}: {first:?}\n{out}");
        assert!(first.starts_with(' '), "the gutter opens the row, no border before it");
        assert!(cells(&buf, read.x, read.y - 1, 10).contains("hello.rs"), "{position:?}");
        assert!(!ui::hit_divider(area, &app, files.x, files.y), "content is not divider");
        let row = ui::hit_file(area, &app, files.x, files.y, 1, 0);
        assert_eq!(row, Some(0), "a click on the first painted file row hits it ({position:?})");
    }

    // A hidden navigator leaves the read pane alone: no divider, no frame.
    app.navigator_hidden = true;
    let out = dump(&render_size(&app, 80, 24));
    assert!(!out.contains('┌') && !out.contains('│'), "{out}");
}

#[test]
fn borderless_focus_shows_in_the_title_and_overlays_keep_their_frames() {
    let mut app = edited_app();
    borderless(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    let title_fg = |app: &App, inner: Rect| {
        let buf = render_size(app, 80, 24);
        // The title text starts one cell in, after its breathing space.
        buf[(inner.x + 1, inner.y - 1)].fg
    };
    app.focus = Focus::Diff;
    let (read, files) = (ui::read_inner_rect(area, &app), ui::files_inner_rect(area, &app));
    let (lit, dim) = (title_fg(&app, read), title_fg(&app, files));
    assert_ne!(lit, dim, "the focused pane's title stands out");
    app.focus = Focus::Files;
    assert_eq!(title_fg(&app, files), lit, "focus moves the accent to the navigator title");
    assert_eq!(title_fg(&app, read), dim);

    // Floating overlays keep full borders: they float over content, not against the edge.
    app.mode = Mode::List;
    let out = dump(&render_size(&app, 80, 24));
    assert!(out.contains('┌') && out.contains('┘'), "the comments list keeps its frame:\n{out}");
}

#[test]
fn a_borderless_divider_drags_to_the_mouse_and_the_read_thumb_never_covers_text() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let mut app = edited_app();
    borderless(&mut app);
    // A 100-cell split axis either way, so the drag's whole-percent share is exact.
    let area = Rect::new(0, 0, 100, 102);
    let body = ui::body_rect(area, &app);
    let event = |kind, column, row| MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE };
    for (position, target) in [
        (NavigatorPosition::Right, (body.x + 60, body.y + 5)),
        (NavigatorPosition::Left, (body.x + 40, body.y + 5)),
        (NavigatorPosition::Bottom, (body.x + 5, body.y + 60)),
        (NavigatorPosition::Top, (body.x + 5, body.y + 40)),
    ] {
        app.navigator_position = position;
        let start = (body.y..body.y + body.height)
            .flat_map(|y| (body.x..body.x + body.width).map(move |x| (x, y)))
            .find(|&(x, y)| ui::hit_divider(area, &app, x, y))
            .unwrap();
        for (kind, (x, y)) in [
            (MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left), start),
            (MouseEventKind::Drag(ratatui::crossterm::event::MouseButton::Left), target),
            (MouseEventKind::Up(ratatui::crossterm::event::MouseButton::Left), target),
        ] {
            handle_mouse(
                &mut app,
                event(kind, x, y),
                area,
                &[],
                &Keymap::default(),
                &herdr_reviewr::export::Clipboard,
            )
            .unwrap();
        }
        assert!(
            ui::hit_divider(area, &app, target.0, target.1),
            "the divider lands under the mouse ({position:?})"
        );
    }

    // The PR read pane's scrollbar: on the divider when the navigator is right of it, else
    // in a column of its own that the content never reaches.
    app.set_tab(Tab::Pr).unwrap();
    let long = (0..80).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n\n");
    app.pr = PrView::Pr(Box::new(PrSnapshot { body: long, ..common::pr_snapshot() }));
    for position in POSITIONS {
        app.navigator_position = position;
        let buf = render_size(&app, 100, 102);
        let read = ui::read_inner_rect(area, &app);
        let thumb: Vec<u16> = (read.y..read.y + read.height)
            .flat_map(|y| (0..100u16).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() == "┃")
            .map(|(x, _)| x)
            .collect();
        assert!(!thumb.is_empty(), "the overflow paints a thumb ({position:?})");
        let col = thumb[0];
        assert!(thumb.iter().all(|&x| x == col), "one track column ({position:?})");
        assert!(col >= read.x + read.width, "the thumb sits outside the text ({position:?})");
        if position == NavigatorPosition::Right {
            assert!(ui::hit_divider(area, &app, col, read.y), "on the divider");
        }
    }
}

/// Load a config with avatars on at `width` and `fit`, the terminal's graphics answer `graphics`.
fn with_avatars(app: &mut App, width: u8, fit: &str, graphics: Option<bool>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!("avatars = true\navatar_width = {width}\navatar_fit = \"{fit}\"\n"),
    )
    .unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    app.graphics = graphics;
    app.cell_px = Some((10, 20));
}

/// A PR tab whose one comment and its reply carry avatar URLs.
fn avatar_pr_app() -> App {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView, Reply};
    let mut app = edited_app();
    app.set_tab(Tab::Pr).unwrap();
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        comments: vec![Comment {
            body: "ROOT_BODY".into(),
            avatar_url: Some("https://avatars.example/ann.png".into()),
            replies: vec![Reply {
                author: "bob".into(),
                author_is_bot: false,
                body: "REPLY_BODY".into(),
                created_at: "2026-06-27T11:00:00Z".into(),
                avatar_url: Some("https://avatars.example/bob.png".into()),
                links: herdr_reviewr::forge::Links::default(),
            }],
            ..common::comment()
        }],
        ..common::pr_snapshot()
    }));
    app
}

#[test]
fn a_pr_paints_its_dots_while_avatars_are_unavailable_or_still_downloading() {
    let plain = avatar_pr_app();
    let today = render_buffer(&plain);

    // Opted in, but the terminal has not answered the probe (or said no): today's frame.
    for graphics in [None, Some(false)] {
        let mut app = avatar_pr_app();
        with_avatars(&mut app, 1, "height", graphics);
        assert_eq!(render_buffer(&app), today, "graphics {graphics:?}: byte-identical");
    }

    // Graphics on, the worker stalled: the snapshot paints straight away, every turn a dot.
    let mut app = avatar_pr_app();
    with_avatars(&mut app, 2, "height", Some(true));
    let requests = app.avatar_requests();
    assert_eq!(requests.len(), 2, "both turns' avatars are wanted: {requests:?}");
    for url in &requests {
        app.avatars.mark_requested(url);
    }
    assert!(app.avatars.pending());
    assert_eq!(render_buffer(&app), today, "pending downloads paint dots");
    assert!(app.avatar_requests().is_empty(), "nothing asked for twice");

    // A failed download stays a dot.
    for url in requests {
        app.avatars.land(url, None);
    }
    assert_eq!(render_buffer(&app), today);
}

#[test]
fn a_landed_avatar_covers_the_dots_cells_and_its_blank_neighbours_only() {
    let plain = avatar_pr_app();
    let today = dump(&render_buffer(&plain));
    let row_of = |out: &str, needle: &str| out.lines().position(|l| l.contains(needle)).unwrap();
    let ann_row = row_of(&today, "● @ann");
    let border_x = today.lines().nth(ann_row).unwrap().chars().position(|c| c == '●').unwrap() - 2;

    // (width, fit, the covered cells after the border as placeholder column numbers).
    for (width, fit, expected) in [
        (1, "height", [Some(0), Some(1), Some(2)]),
        (2, "height", [None, Some(0), Some(1)]),
        (1, "width", [None, Some(0), None]),
        (2, "width", [None, Some(0), Some(1)]),
    ] {
        let mut app = avatar_pr_app();
        with_avatars(&mut app, width, fit, Some(true));
        let source = image::RgbaImage::from_pixel(16, 16, image::Rgba([9, 9, 9, 255]));
        for url in app.avatar_requests() {
            app.avatars.mark_requested(&url);
            app.avatars.land(url, Some(source.clone()));
        }
        let geometry = app.avatar_geometry().unwrap();
        assert!(!app.avatars.transmissions(geometry).is_empty());

        let buf = render_buffer(&app);
        let out = dump(&buf);
        let y = ann_row as u16;
        let id = buf[(border_x as u16 + 2, y)].fg;
        for (cell, col) in expected.iter().enumerate() {
            let c = &buf[(border_x as u16 + 1 + cell as u16, y)];
            match col {
                Some(col) => {
                    let mark = ['\u{0305}', '\u{030D}', '\u{030E}'][*col];
                    let want = format!("\u{10EEEE}\u{0305}{mark}");
                    assert_eq!(c.symbol(), want, "{width}/{fit}: cell {cell}");
                    assert_eq!(c.fg, id, "one image across the placement");
                }
                None => assert_eq!(c.symbol(), " ", "{width}/{fit}: cell {cell} stays blank"),
            }
        }
        assert!(matches!(id, ratatui::style::Color::Rgb(..)), "the id rides the foreground");
        // No layout shift: the border, the byline text, and the body sit where they did.
        assert_eq!(buf[(border_x as u16, y)].symbol(), "│");
        let today_buf = render_buffer(&plain);
        let text_at = |b: &Buffer| cells(b, border_x as u16 + 4, y, 40);
        assert_eq!(text_at(&buf), text_at(&today_buf), "{width}/{fit}: the text never moves");
        assert!(text_at(&buf).starts_with("@ann · "));
        assert_eq!(row_of(&out, "REPLY_BODY"), row_of(&today, "REPLY_BODY"));
        // The reply's byline has its own avatar, a different image.
        let bob_row = row_of(&today, "● @bob") as u16;
        assert_ne!(buf[(border_x as u16 + 2, bob_row)].fg, id, "each author their own image");
    }
}

/// The frame cell where `needle` first paints, scanning row by row.
fn find_cell(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let out = dump(buf);
    out.lines().enumerate().find_map(|(y, line)| {
        let at = line.find(needle)?;
        Some((line[..at].chars().count() as u16, y as u16))
    })
}

#[test]
fn a_resolved_thread_folds_to_a_two_line_box_and_its_header_hit_tests_where_it_paints() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let long = (0..30).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ");
    for borders in [true, false] {
        let mut app = app_on(&r);
        if !borders {
            borderless(&mut app);
        }
        app.set_tab(Tab::Pr).unwrap();
        // A long description wraps above the cards, so the hit rows sit past wrapped lines.
        app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
            body: long.clone(),
            comments: vec![common::thread("ann", 3, 9, true), common::thread("bob", 5, 10, false)],
            ..common::pr_snapshot()
        })));
        let buf = render_buffer(&app);
        let out = dump(&buf);
        let read: Vec<&str> = out.lines().collect();
        let head = read
            .iter()
            .position(|l| l.contains("╭─ ▸ x.rs:3 · resolved · 1 reply "))
            .unwrap_or_else(|| panic!("the folded header (borders {borders}):\n{out}"));
        assert!(
            read[head + 1].contains("╰─ @ann ann root first line ─"),
            "the summary edge (borders {borders}):\n{out}"
        );
        assert!(read[head + 2].contains("╭─ ▾ x.rs:5"), "the open thread follows flush:\n{out}");
        assert!(!out.contains("ann reply"), "a folded thread hides its turns:\n{out}");
        assert!(out.contains("bob reply"), "an unresolved thread stays open:\n{out}");

        // The click boxes sit exactly on the painted header and summary rows.
        let ann = app.pr_snapshot().unwrap().comments[0].key();
        let bob = app.pr_snapshot().unwrap().comments[1].key();
        let (x, y) = find_cell(&buf, "▸ x.rs:3").unwrap();
        assert_eq!(app.painted_card_toggle_at(x, y), Some(ann.clone()));
        assert_eq!(app.painted_card_toggle_at(x + 20, y + 1), Some(ann.clone()), "the summary");
        assert_eq!(app.painted_card_toggle_at(x, y - 1), None, "nothing above the card");
        let (x, y) = find_cell(&buf, "▾ x.rs:5").unwrap();
        assert_eq!(app.painted_card_toggle_at(x, y), Some(bob));
        assert_eq!(app.painted_card_toggle_at(x, y + 1), None, "an open card's body is text");

        // Unfolded, the resolved thread paints whole under an open marker.
        app.toggle_pr_card(&ann);
        let out = render(&app);
        assert!(out.contains("▾ x.rs:3 · resolved"), "{out}");
        assert!(out.contains("ann reply"), "the turns are back:\n{out}");
    }
}

#[test]
fn a_folded_thread_paints_flat_in_a_narrow_pane_and_hit_tests_there() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    app.navigator_hidden = true;
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        comments: vec![common::thread("ann", 3, 9, true)],
        ..common::pr_snapshot()
    })));
    let buf = render_size(&app, 22, 20);
    let out = dump(&buf);
    assert!(!out.contains('╭'), "too narrow for a box:\n{out}");
    let (x, y) = find_cell(&buf, "▸ x.rs:3").unwrap_or_else(|| panic!("flat header:\n{out}"));
    let summary = out.lines().nth(y as usize + 1).unwrap();
    assert!(summary.contains("  @ann ann"), "the indented summary line:\n{out}");
    assert!(summary.contains('…'), "a summary cut at the edge says so:\n{out}");
    assert!(!out.contains("ann reply"), "{out}");
    let ann = app.pr_snapshot().unwrap().comments[0].key();
    assert_eq!(app.painted_card_toggle_at(x, y), Some(ann.clone()));
    assert_eq!(app.painted_card_toggle_at(x, y + 1), Some(ann));
    assert_eq!(app.painted_card_toggle_at(x, y + 2), None);
}

#[test]
fn a_thread_resolved_above_the_reader_folds_without_moving_their_view() {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    app.set_tab(Tab::Pr).unwrap();
    let long = |author: &str, line: u32, created: u8, resolved: bool| Comment {
        body: (0..12).map(|n| format!("{author}-line-{n:02}")).collect::<Vec<_>>().join("\n\n"),
        ..common::thread(author, line, created, resolved)
    };
    let snapshot = |first_resolved: bool, third_resolved: bool| {
        PrView::Pr(Box::new(PrSnapshot {
            comments: vec![
                long("ann", 1, 9, first_resolved),
                long("bob", 2, 10, false),
                long("cat", 3, 11, third_resolved),
            ],
            ..common::pr_snapshot()
        }))
    };
    app.apply_pr(snapshot(false, false));
    let read = |app: &App| -> Vec<String> {
        read_column(&render(app)).iter().map(|l| l.trim_end().to_string()).collect()
    };
    // Select bob and scroll into his card; ann's card is above, off screen.
    app.pr_move(1);
    let _ = read(&app);
    for _ in 0..2 {
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 5,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 140, 40),
            &[],
            &Keymap::default(),
            &herdr_reviewr::export::Clipboard,
        )
        .unwrap();
    }
    let before = read(&app);
    assert!(!before.iter().any(|l| l.contains("ann-line-")), "ann is off screen:\n{before:#?}");

    // A refresh resolves ann, above the reader and off screen: her card folds by default, and
    // the reader's lines stay put (the offset anchoring absorbs the shrink above).
    app.apply_pr(snapshot(true, false));
    let after = read(&app);
    assert_eq!(before, after, "the reader's view does not move");
    assert!(app.pr_card_collapsed(&app.pr_snapshot().unwrap().comments[0]), "ann folded");

    // A refresh resolves cat, whose card is on screen: it says so, but stays open under the
    // reader, and so does every line above it.
    app.apply_pr(snapshot(true, true));
    let after = read(&app);
    let cat = after.iter().position(|l| l.contains("x.rs:3 · resolved")).expect("cat's header");
    assert!(after[cat].contains("▾"), "held open:\n{after:#?}");
    assert_eq!(before[..cat], after[..cat], "nothing above it moves");
    assert!(after.iter().any(|l| l.contains("cat-line-00")), "its turns stay:\n{after:#?}");

    // The reader's next move releases the hold: cat shows its resolved default.
    app.pr_move(1);
    let moved = read(&app);
    assert!(app.pr_card_collapsed(app.pr_selected_comment().unwrap()), "cat folds");
    assert!(!moved.iter().any(|l| l.contains("cat-line-01")), "{moved:#?}");
}

/// Checked-out #11 in a three-PR stack (#10 below, #12 above), on the `PR` tab.
fn stack_app(config: &str) -> (Repo, App) {
    use herdr_reviewr::app::Tab;
    use herdr_reviewr::forge::PrView;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    app.set_tab(Tab::Pr).unwrap();
    app.apply_pr(PrView::Pr(Box::new(stack_pr(11))));
    (r, app)
}

fn stack_pr(number: u64) -> herdr_reviewr::forge::PrSnapshot {
    use herdr_reviewr::forge::{Check, CheckStatus, PrState, StackEntry};
    let entry = |n: u64, base: &str| StackEntry {
        number: n,
        title: format!("Work {n}"),
        state: PrState::Open,
        is_draft: false,
        head_ref: format!("head-{n}"),
        base_ref: base.to_string(),
        level: i32::try_from(n).unwrap() - i32::try_from(number).unwrap(),
        url: None,
    };
    herdr_reviewr::forge::PrSnapshot {
        number,
        title: format!("Work {number}"),
        stack: vec![entry(10, "main"), entry(11, "head-10"), entry(12, "head-11")],
        checks: vec![Check { name: "ci".into(), status: CheckStatus::Success, url: None }],
        comments: vec![common::comment()],
        repo: herdr_reviewr::git::RepoTarget::new("github.com", "o", "r"),
        ..common::pr_snapshot()
    }
}

fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle}:\n{text}"))
}

/// The background of the first cell of `needle` on screen.
fn bg_at(buf: &Buffer, needle: &str) -> ratatui::style::Color {
    let text = dump(buf);
    let y = text.lines().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{text}"));
    let line = text.lines().nth(y).unwrap();
    let x = line[..line.find(needle).unwrap()].chars().count();
    buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].bg
}

/// Land a batch carrying every stack PR the app asks for.
fn land_stack(app: &mut App) {
    use herdr_reviewr::forge::PrView;
    let now = std::time::Instant::now();
    while let Some(batch) = app.take_stack_batch(now, None) {
        let results =
            batch.numbers.iter().map(|&n| (n, PrView::Pr(Box::new(stack_pr(n))))).collect();
        app.land_stack_batch(batch.tag, results, now);
    }
}

#[test]
fn the_shown_prs_stack_row_is_filled_and_reads_apart_under_the_cursor() {
    for borders in ["", "pane_outer_borders = false\n"] {
        let (_r, mut app) = stack_app(borders);
        let p = *app.palette();
        land_stack(&mut app);
        // On the checked-out PR, its own row carries the fill: it is what you look at.
        let buf = render_size(&app, 140, 30);
        assert_eq!(bg_at(&buf, "#11 open"), p.view_bg, "{borders:?}");
        assert_ne!(bg_at(&buf, "#12 open"), p.view_bg);
        let text = dump(&buf);
        assert!(line_with(&text, "#11 open").contains("● #11"));
        assert!(!text.contains('◆'), "no separate viewing dot: {text}");

        // View #12 by clicking its row (top of the stack): the cursor and the fill share it.
        app.pr_view_stack_row(0);
        let buf = render_size(&app, 140, 30);
        assert_eq!(bg_at(&buf, "#12 open"), p.view_cursor_bg, "{borders:?}");
        assert_ne!(p.view_cursor_bg, p.cursor_bg(true));
        assert_ne!(bg_at(&buf, "#11 open"), p.view_bg, "the fill moved with the view");
        let header = dump(&buf).lines().next().unwrap().to_string();
        assert!(header.contains("viewing #12 · not checked out"), "{header}");

        // The cursor moves on: the viewed row keeps its fill, the cursor row its own.
        app.pr_move(1);
        let buf = render_size(&app, 140, 30);
        assert_eq!(bg_at(&buf, "#12 open"), p.view_bg);
        assert_eq!(bg_at(&buf, "#11 open"), p.cursor_bg(true));
        let text = dump(&buf);
        let home = line_with(&text, "#11 open");
        assert!(home.contains("● #11") && home.contains("checked out"), "{text}");
        assert!(!text.contains("sync unknown"), "a browsed PR has no local sync: {text}");
    }
}

#[test]
fn a_never_read_stack_pr_keeps_the_stack_and_header_painted_while_it_loads() {
    for borders in ["", "pane_outer_borders = false\n"] {
        let (_r, mut app) = stack_app(borders);
        app.pr_view_number(12);
        let text = dump(&render_size(&app, 140, 30));
        let header = text.lines().next().unwrap();
        assert!(header.contains("viewing #12 · not checked out"), "{header}");
        assert!(!header.contains("#11 ↗"), "never the checked-out PR's chip: {header}");
        assert!(text.contains("stack · 3"), "the stack stays: {text}");
        assert!(line_with(&text, "#11 open").contains("● #11"));
        assert!(text.contains("loading #12…"), "its own sections wait in one line: {text}");
        let footer = footer_line(&text);
        assert!(!footer.trim_start().starts_with('·'), "no dangling separator: {footer:?}");
        assert!(footer.contains("0 checked out"), "{footer:?}");

        // Narrow, the tags give way and the marks stay.
        land_stack(&mut app);
        let buf = render_size(&app, 70, 30);
        let text = dump(&buf);
        assert!(line_with(&text, "#11 open").contains("● #11"), "{text}");
        assert!(text.lines().next().unwrap().contains("viewing #12"), "{text}");
        assert_eq!(bg_at(&buf, "#12 open"), app.palette().view_bg);
    }
}

/// The navigator rows above and below the `checks` header, from its own column.
fn around_checks(buf: &Buffer) -> (String, String) {
    let text = dump(buf);
    let y = text.lines().position(|l| l.contains("checks  ✓")).expect("checks header");
    let line = text.lines().nth(y).unwrap();
    let x = line[..line.find("checks  ✓").unwrap()].chars().count();
    let x = u16::try_from(x).unwrap();
    let y = u16::try_from(y).unwrap();
    // Above: the stack's parting row. Below the one check: the comments' parting row.
    (cells(buf, x, y - 1, 6), cells(buf, x, y + 2, 6))
}

#[test]
fn navigator_separators_rule_the_sections_apart_only_when_configured() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    for borders in ["", "pane_outer_borders = false\n"] {
        // Off (the default): the sections part with blank rows, as ever.
        let (_r, app) = stack_app(borders);
        let (above, below) = around_checks(&render_size(&app, 140, 30));
        assert_eq!((above.trim(), below.trim()), ("", ""), "no rule by default ({borders:?})");

        // On: a rule between stack and checks, and between checks and comments.
        let (_r, app) = stack_app(&format!("{borders}pr_nav_separators = true\n"));
        let (above, below) = around_checks(&render_size(&app, 140, 30));
        assert_eq!(above, "──────", "stack | checks ({borders:?})");
        assert_eq!(below, "──────", "checks | comments ({borders:?})");
        // A rule is no cursor stop and copies as nothing.
        let text = dump(&render_size(&app, 50, 30));
        assert!(text.contains("──"), "a narrow pane still rules: {text}");
    }

    // With no stack, only checks | comments is ruled.
    let (_r, mut app) = stack_app("pr_nav_separators = true\n");
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot { stack: Vec::new(), ..stack_pr(11) })));
    let (above, below) = around_checks(&render_size(&app, 140, 30));
    assert!(!above.contains('─'), "nothing above checks to part from: {above:?}");
    assert_eq!(below, "──────");
}

/// A `Write` sink shared with the test, so the bytes a real crossterm backend emits can be
/// replayed after each draw.
#[derive(Clone, Default)]
struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A grapheme-aware terminal grid, the way Ghostty lays text out: cursor moves (`CSI row;col H`)
/// position absolutely, every other escape is ignored, a printable codepoint takes its
/// `unicode-width` cells, a zero-width one joins the cell before it, and VS16 (U+FE0F) widens
/// that cell to two — the emoji presentation the terminal paints.
///
/// It also keeps OSC 8 the way Ghostty does: `ESC ] 8 ; params ; URL ST` sets the pen's link
/// (an empty URL clears it), and every cell printed takes the pen's link.
struct GraphemeTerminal {
    cells: Vec<Vec<String>>,
    links: Vec<Vec<Option<String>>>,
    pen: Option<String>,
    x: usize,
    y: usize,
    last: Option<(usize, usize)>,
}

impl GraphemeTerminal {
    fn new(w: usize, h: usize) -> Self {
        Self {
            cells: vec![vec![" ".to_string(); w]; h],
            links: vec![vec![None; w]; h],
            pen: None,
            x: 0,
            y: 0,
            last: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        use unicode_width::UnicodeWidthChar;
        let text = String::from_utf8_lossy(bytes);
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    let mut params = String::new();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() || c == '~' {
                            if c == 'H' {
                                let mut it =
                                    params.split(';').map(|n| n.parse::<usize>().unwrap_or(1));
                                self.y = it.next().unwrap_or(1).saturating_sub(1);
                                self.x = it.next().unwrap_or(1).saturating_sub(1);
                                self.last = None;
                            }
                            break;
                        }
                        params.push(c);
                    }
                } else if chars.peek() == Some(&']') {
                    chars.next();
                    // An OSC runs to ST (`ESC \`) or BEL.
                    let mut body = String::new();
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                        body.push(c);
                    }
                    if let Some(rest) = body.strip_prefix("8;") {
                        let url = rest.split_once(';').map_or("", |(_, url)| url);
                        self.pen = (!url.is_empty()).then(|| url.to_string());
                    }
                }
                continue;
            }
            let (w, h) = (self.cells[0].len(), self.cells.len());
            match ch.width().unwrap_or(0) {
                0 => {
                    if let Some((lx, ly)) = self.last {
                        self.cells[ly][lx].push(ch);
                        if ch == '\u{FE0F}' && lx + 1 < w && self.x == lx + 1 {
                            self.cells[ly][lx + 1] = String::new();
                            self.links[ly][lx + 1].clone_from(&self.pen);
                            self.x += 1;
                        }
                    }
                }
                cw => {
                    if self.y < h && self.x < w {
                        // Writing over either half of a wide glyph erases the whole glyph.
                        let row = &mut self.cells[self.y];
                        if self.x + 1 < w && row[self.x + 1].is_empty() {
                            row[self.x + 1] = " ".to_string();
                        }
                        if row[self.x].is_empty() && self.x > 0 {
                            row[self.x - 1] = " ".to_string();
                        }
                        self.cells[self.y][self.x] = ch.to_string();
                        self.links[self.y][self.x].clone_from(&self.pen);
                        for k in 1..cw {
                            if self.x + k < w {
                                self.cells[self.y][self.x + k] = String::new();
                                self.links[self.y][self.x + k].clone_from(&self.pen);
                            }
                        }
                        self.last = Some((self.x, self.y));
                    }
                    self.x += cw;
                }
            }
        }
    }

    /// Every cell whose link differs from the frame's runs: `(x, y, wanted, shown)`.
    fn link_disagreements(
        &self,
        runs: &[herdr_reviewr::hyperlink::LinkRun],
    ) -> Vec<(usize, usize, Option<String>, Option<String>)> {
        let mut want = vec![vec![None; self.links[0].len()]; self.links.len()];
        for run in runs {
            for x in run.x0..run.x1 {
                want[run.y as usize][x as usize] = Some(run.url.to_string());
            }
        }
        let mut out = Vec::new();
        for (y, row) in self.links.iter().enumerate() {
            for (x, got) in row.iter().enumerate() {
                if *got != want[y][x] {
                    out.push((x, y, want[y][x].clone(), got.clone()));
                }
            }
        }
        out
    }

    /// Every cell the terminal shows that disagrees with the buffer ratatui believes is there.
    fn disagreements(&self, buf: &Buffer) -> Vec<(u16, u16, String, String)> {
        use unicode_width::UnicodeWidthStr;
        let mut out = Vec::new();
        for y in 0..buf.area.height {
            let mut x = 0;
            while x < buf.area.width {
                let want = buf[(x, y)].symbol();
                let got = &self.cells[y as usize][x as usize];
                if got != want {
                    out.push((x, y, want.to_string(), got.clone()));
                }
                x += want.width().max(1) as u16;
            }
        }
        out
    }
}

type LinkTerminal = Terminal<herdr_reviewr::hyperlink::HyperlinkBackend<Sink>>;

/// The production terminal stack — crossterm under the OSC 8 backend — writing into `sink`.
fn link_terminal(sink: &Sink, area: Rect) -> LinkTerminal {
    use ratatui::{TerminalOptions, Viewport};
    Terminal::with_options(
        herdr_reviewr::hyperlink::HyperlinkBackend::new(sink.clone()),
        TerminalOptions { viewport: Viewport::Fixed(area) },
    )
    .unwrap()
}

/// One frame the way the event loop draws it: render, hand the backend the frame's links,
/// flush. Returns the buffer ratatui believes is on screen.
fn draw_linked(terminal: &mut LinkTerminal, app: &App) -> Buffer {
    let links = terminal.backend().links();
    let frame = terminal
        .draw(|f| {
            ui::render(f, app);
            *links.borrow_mut() = app.painted_hyperlinks();
        })
        .unwrap();
    frame.buffer.clone()
}

/// Draw `app` through a real crossterm backend into a grapheme-aware terminal model, scroll
/// the read pane by `pages` between frames, and report what the terminal ends up showing
/// differently from ratatui's buffer.
fn replay_scrolled(app: &mut App, w: u16, h: u16, pages: usize) -> Vec<(u16, u16, String, String)> {
    let sink = Sink::default();
    let area = Rect::new(0, 0, w, h);
    let mut terminal = link_terminal(&sink, area);
    let mut screen = GraphemeTerminal::new(w as usize, h as usize);
    let mut worst = Vec::new();
    for _ in 0..=pages {
        let buffer = draw_linked(&mut terminal, app);
        screen.feed(&sink.0.borrow_mut().split_off(0));
        let bad = screen.disagreements(&buffer);
        if bad.len() > worst.len() {
            worst = bad;
        }
        handle_key(app, KeyEvent::from(KeyCode::PageDown), area, &Keymap::default()).unwrap();
    }
    worst
}

/// The PR conversation from the report: the dbschema plan with its 🗄️ heading and 🟡 table,
/// and the reviewer's 🤖/✅/⚫ round.
fn emoji_pr_app(framed: bool) -> App {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let plan = "### 🗄️ dbschema plan (preview only — no changes applied)\n\n\
        | Target | Path | Env | Result |\n|---|---|---|---|\n\
        | acme_shop_staging/public | db/shop/ | pp | 🟡 changes |\n\
        | acme_shop_prod/public | db/shop/ | p | 🟡 changes |\n\n\
        <details><summary>🟡 shop-public-pp — changes</summary>\n\nbody\n</details>\n\n\
        Full plan ⚠️ ℹ️ ✔️ done";
    let codex = "🤖 Example Review v2 — Round 1 — ✅ patch is correct\n\n\
        | Severity | Category | Finding |\n|---|---|---|\n| ⚫ | security | Remove remote image |";
    let mut app = edited_app();
    if !framed {
        borderless(&mut app);
    }
    app.set_tab(Tab::Pr).unwrap();
    let comments: Vec<Comment> = (0..4)
        .map(|i| Comment {
            author: format!("u{i}"),
            body: if i % 2 == 0 { plan.into() } else { codex.into() },
            created_at: format!("2026-06-27T1{i}:00:00Z"),
            ..common::comment()
        })
        .collect();
    app.pr = PrView::Pr(Box::new(PrSnapshot {
        body: "🗄️ desc ⚠️ x".into(),
        comments,
        ..common::pr_snapshot()
    }));
    app.focus = Focus::Diff;
    app
}

#[test]
fn emoji_rows_paint_where_ratatui_put_them_in_a_grapheme_aware_terminal() {
    // A VS16 emoji (`🗄️`) is the cell that drifted: ratatui writes its hidden second cell
    // right after it with no cursor move, and a terminal that draws the emoji two wide puts
    // that blank — and the rest of the run — one column right, over the box border, the
    // divider, and the scrollbar thumb. Every frame of a scroll must land cell for cell.
    for framed in [false, true] {
        for pos in [NavigatorPosition::Right, NavigatorPosition::Bottom] {
            let mut app = emoji_pr_app(framed);
            app.navigator_position = pos;
            let bad = replay_scrolled(&mut app, 70, 24, 12);
            assert!(
                bad.is_empty(),
                "framed={framed} {pos:?}: the terminal shows {} cells ratatui never painted, \
                 first {:?}",
                bad.len(),
                &bad[..bad.len().min(4)]
            );
        }
    }
}

#[test]
fn no_painted_cell_is_a_grapheme_terminals_measure_differently() {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let app = emoji_pr_app(false);
    let buf = render_size(&app, 70, 40);
    for y in 0..40u16 {
        for x in 0..70u16 {
            let symbol = buf[(x, y)].symbol();
            let parts: usize = symbol.chars().map(|c| c.width().unwrap_or(0)).sum();
            assert_eq!(symbol.width(), parts, "({x},{y}) {symbol:?} measures two ways");
        }
    }
    // The layout is unchanged: the emoji keeps its two-column slot, the hidden half now a
    // plain blank the terminal is told to paint.
    let out = dump(&buf);
    assert!(out.contains("🗄  dbschema plan"), "{out}");
    assert!(out.contains("🟡  changes"), "{out}");
}

#[test]
fn the_read_thumb_tracks_top_middle_and_end_on_its_one_column_in_every_layout() {
    // Both border settings, every navigator position and a hidden navigator, at a narrow and
    // a wide pane: the thumb is one contiguous run in the track column — the frame border, the
    // divider beside the read pane, or the read pane's reserved last column — and it opens at
    // the top, moves, and closes at the bottom at the end of the scroll.
    let doc = (0..60).map(|n| format!("para {n}")).collect::<Vec<_>>().join("\n\n");
    let positions = [
        None,
        Some(NavigatorPosition::Right),
        Some(NavigatorPosition::Left),
        Some(NavigatorPosition::Bottom),
        Some(NavigatorPosition::Top),
    ];
    for framed in [true, false] {
        for pos in positions {
            for (w, h) in [(44u16, 16u16), (100, 40)] {
                let r = Repo::init();
                r.write("doc.md", "# T\n");
                r.commit_all("init");
                r.write("doc.md", &doc);
                let mut app = app_on(&r);
                if !framed {
                    borderless(&mut app);
                }
                match pos {
                    Some(p) => app.navigator_position = p,
                    None => app.navigator_hidden = true,
                }
                app.toggle_preview();
                app.focus = Focus::Diff;
                let area = Rect::new(0, 0, w, h);
                let mut starts = Vec::new();
                for presses in [0, 2, 200] {
                    for _ in 0..presses {
                        handle_key(
                            &mut app,
                            KeyEvent::from(KeyCode::PageDown),
                            area,
                            &Keymap::default(),
                        )
                        .unwrap();
                    }
                    let buf = render_size(&app, w, h);
                    let read = ui::read_inner_rect(area, &app);
                    let label = format!("framed={framed} {pos:?} {w}x{h} after {presses}");
                    let thumb: Vec<(u16, u16)> = (0..h)
                        .flat_map(|y| (0..w).map(move |x| (x, y)))
                        .filter(|&(x, y)| buf[(x, y)].symbol() == "┃")
                        .collect();
                    assert!(!thumb.is_empty(), "{label}: a thumb");
                    let col = thumb[0].0;
                    assert_eq!(col, read.x + read.width, "{label}: the track hugs the text");
                    assert!(thumb.iter().all(|t| t.0 == col), "{label}: one column");
                    let rows: Vec<u16> = thumb.iter().map(|t| t.1).collect();
                    assert!(rows.windows(2).all(|p| p[1] == p[0] + 1), "{label}: contiguous");
                    for y in read.y..read.y + read.height {
                        let cell = buf[(col, y)].symbol();
                        assert!(
                            ["┃", "│", " "].contains(&cell),
                            "{label}: track row {y} holds {cell:?}"
                        );
                    }
                    starts.push((rows[0], *rows.last().unwrap()));
                    if presses == 0 {
                        assert_eq!(rows[0], read.y, "{label}: opens at the top");
                    }
                    if presses == 200 {
                        assert_eq!(
                            *rows.last().unwrap(),
                            read.y + read.height - 1,
                            "{label}: closes at the end"
                        );
                    }
                }
                assert!(
                    starts[0].0 < starts[1].0 && starts[1].0 < starts[2].0,
                    "framed={framed} {pos:?} {w}x{h}: moves {starts:?}"
                );
            }
        }
    }
}

const PR_URL: &str = "https://github.com/o/r/pull/1";

/// A PR whose every navigable piece names its page: the PR and its branch, a check, a stack
/// PR, a review, a thread with a reply, and a comment with a markdown link.
fn linked_snapshot(check_url: &str) -> herdr_reviewr::forge::PrSnapshot {
    use herdr_reviewr::forge::{
        Check, CheckStatus, Comment, CommentKind, Links, PrSnapshot, PrState, Reply, ReviewState,
        StackEntry,
    };
    let links = |author: &str, anchor: &str| Links {
        permalink: Some(format!("{PR_URL}#{anchor}")),
        author: Some(format!("https://github.com/{author}")),
    };
    let entry = |number: u64, level: i32| StackEntry {
        number,
        title: format!("stack pr {number}"),
        state: PrState::Open,
        is_draft: false,
        head_ref: format!("b{number}"),
        base_ref: "main".into(),
        url: Some(format!("https://github.com/o/r/pull/{number}")),
        level,
    };
    let long = (0..14).map(|n| format!("para {n}")).collect::<Vec<_>>().join("\n\n");
    PrSnapshot {
        title: "Add feature".into(),
        url: PR_URL.into(),
        head_ref: "feat".into(),
        checks: vec![Check {
            name: "build".into(),
            status: CheckStatus::Success,
            url: Some(check_url.into()),
        }],
        stack: vec![entry(1, 0), entry(2, 1)],
        comments: vec![
            Comment {
                kind: CommentKind::Review,
                author: "ann".into(),
                anchor: "review".into(),
                body: "Looks good.".into(),
                review_state: Some(ReviewState::Approved),
                created_at: "2026-06-27T09:00:00Z".into(),
                links: links("ann", "review-1"),
                ..common::comment()
            },
            Comment {
                kind: CommentKind::Finding,
                author: "bob".into(),
                anchor: "x.rs:3".into(),
                body: "Thread root.".into(),
                created_at: "2026-06-27T10:00:00Z".into(),
                replies: vec![Reply {
                    author: "cat".into(),
                    author_is_bot: false,
                    body: "Reply.".into(),
                    created_at: "2026-06-27T10:30:00Z".into(),
                    avatar_url: None,
                    links: links("cat", "discussion-r2"),
                }],
                links: links("bob", "discussion-r1"),
                ..common::comment()
            },
            Comment {
                // A wide-glyph name: its link covers both halves of every glyph.
                author: "dan日本".into(),
                body: format!("See [the docs](https://docs.example/x) first.\n\n{long}"),
                created_at: "2026-06-27T11:00:00Z".into(),
                links: links("dan", "issuecomment-3"),
                ..common::comment()
            },
        ],
        ..common::pr_snapshot()
    }
}

fn linked_app(framed: bool) -> App {
    use herdr_reviewr::forge::PrView;
    let r = Repo::init();
    r.write("x.rs", "y\n");
    r.commit_all("init");
    let mut app = app_on(&r);
    if !framed {
        borderless(&mut app);
    }
    app.set_tab(Tab::Pr).unwrap();
    app.apply_pr(PrView::Pr(Box::new(linked_snapshot("https://ci.example/build/1"))));
    app.focus = Focus::Diff;
    app
}

/// Each link's painted texts, as the frame's runs cover them.
fn link_texts(app: &App, buf: &Buffer) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out = std::collections::BTreeMap::<String, Vec<String>>::new();
    for run in app.painted_hyperlinks() {
        // A wide glyph's hidden trailing cell is part of its run but not of its text.
        let (mut text, mut x) = (String::new(), run.x0);
        while x < run.x1 {
            let symbol = buf[(x, run.y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        out.entry(run.url.to_string()).or_default().push(text);
    }
    out
}

#[test]
fn every_navigable_piece_carries_its_link_and_nothing_else_does() {
    for framed in [true, false] {
        let app = linked_app(framed);
        let buf = render_buffer(&app);
        let out = dump(&buf);
        let texts = link_texts(&app, &buf);
        let of = |url: &str| texts.get(url).cloned().unwrap_or_default();
        let profile = |who: &str| of(&format!("https://github.com/{who}"));
        // Authors lead to their profiles: in the navigator row and in the byline.
        assert_eq!(profile("ann"), ["@ann", "@ann"], "{out}");
        assert_eq!(profile("bob"), ["@bob", "@bob"], "{out}");
        assert_eq!(profile("cat"), ["@cat"], "a reply's byline:\n{out}");
        assert_eq!(profile("dan"), ["@dan日本", "@dan日本"], "{out}");
        // A comment's anchor and its age lead to the comment.
        let thread = of(&format!("{PR_URL}#discussion-r1"));
        assert!(thread.iter().filter(|t| *t == "x.rs:3").count() == 2, "{thread:?}\n{out}");
        assert!(thread.iter().any(|t| t.ends_with('w') || t.ends_with('d')), "the age: {thread:?}");
        let review = of(&format!("{PR_URL}#review-1"));
        assert!(review.contains(&"review".to_string()), "the review's header: {review:?}");
        assert!(review.contains(&"✓ approved".to_string()), "its navigator verdict: {review:?}");
        // The check, the stack PR, the PR itself, its branch, and a markdown link.
        assert_eq!(of("https://ci.example/build/1"), ["build"], "{out}");
        assert_eq!(of("https://github.com/o/r/pull/2"), ["#2"], "{out}");
        // The PR itself: the header's title and chip, and its own stack row.
        assert_eq!(of(PR_URL), ["Add feature", "open #1 ↗", "#1"], "{out}");
        assert_eq!(of("https://github.com/o/r/tree/feat"), ["feat"], "{out}");
        // A markdown link carries its target over the region a click opens.
        assert_eq!(of("https://docs.example/x"), ["the docs (https://docs.example/x)"], "{out}");
        // No link reaches past its text into a border, a separator, or padding.
        for (url, runs) in &texts {
            for t in runs {
                assert_eq!(t.trim(), t, "{url} links padding: {t:?}");
                assert!(!t.contains('│') && !t.contains('╮') && !t.contains('·'), "{url}: {t:?}");
            }
        }
        // The tags that carried the links never reach the terminal.
        assert!(buf.content.iter().all(|c| c.underline_color == ratatui::style::Color::Reset));
    }
}

#[test]
fn hyperlinks_off_paints_the_same_frame_without_a_link() {
    let on = linked_app(true);
    let mut off = linked_app(true);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "hyperlinks = false\n").unwrap();
    off.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    let (a, b) = (render_buffer(&on), render_buffer(&off));
    assert!(!on.painted_hyperlinks().is_empty());
    assert!(off.painted_hyperlinks().is_empty(), "off paints no link");
    assert_eq!(a, b, "and the cells are exactly the linked frame's");
}

#[test]
fn osc8_links_open_and_close_on_their_runs_without_moving_a_cell() {
    use herdr_reviewr::forge::PrView;
    for framed in [true, false] {
        let mut app = linked_app(framed);
        let (w, h) = (120, 30);
        let area = Rect::new(0, 0, w, h);
        let sink = Sink::default();
        let mut terminal = link_terminal(&sink, area);
        let mut screen = GraphemeTerminal::new(w as usize, h as usize);
        let mut frame = |app: &App, step: &str| {
            let buffer = draw_linked(&mut terminal, app);
            let bytes = sink.0.borrow_mut().split_off(0);
            screen.feed(&bytes);
            let drift = screen.disagreements(&buffer);
            assert!(drift.is_empty(), "{step} (framed {framed}): cells drifted: {drift:?}");
            let links = screen.link_disagreements(&app.painted_hyperlinks());
            assert!(links.is_empty(), "{step} (framed {framed}): links wrong: {links:?}");
            assert_eq!(screen.pen, None, "{step}: the draw left a link open");
            let unlinked = screen.links.iter().flatten().all(Option::is_none);
            (bytes, unlinked)
        };
        let (first, _) = frame(&app, "first frame");
        assert!(String::from_utf8_lossy(&first).contains("\x1b]8;;https://github.com/ann\x1b\\"));
        // Scrolling moves linked text under the diff; a selection moves the accent.
        for step in ["page 1", "page 2", "page 3"] {
            handle_key(&mut app, KeyEvent::from(KeyCode::PageDown), area, &Keymap::default())
                .unwrap();
            frame(&app, step);
        }
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('j')), area, &Keymap::default()).unwrap();
        frame(&app, "next comment");
        // A refresh that changes only a link: the text is identical, so ratatui's diff is
        // empty there, and the backend must reprint the cells under the new link.
        app.apply_pr(PrView::Pr(Box::new(linked_snapshot("https://ci.example/build/2"))));
        let (bytes, _) = frame(&app, "link-only refresh");
        assert!(String::from_utf8_lossy(&bytes).contains("https://ci.example/build/2"));
        // Switched off, every link leaves the screen; back on, they all return.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "hyperlinks = false\n").unwrap();
        app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
        if !framed {
            borderless(&mut app);
            std::fs::write(
                dir.path().join("config.toml"),
                "hyperlinks = false\npane_outer_borders = false\n",
            )
            .unwrap();
            app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
        }
        assert!(frame(&app, "hyperlinks off").1, "no link left on screen");
        let dir = tempfile::tempdir().unwrap();
        let on = if framed { "" } else { "pane_outer_borders = false\n" };
        std::fs::write(dir.path().join("config.toml"), on).unwrap();
        app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
        frame(&app, "hyperlinks back on");
        // A tab away and back repaints every row, links and all.
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('1')), area, &Keymap::default()).unwrap();
        common::land_world(&mut app);
        assert!(frame(&app, "changes tab").1, "the diff tab links nothing");
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('3')), area, &Keymap::default()).unwrap();
        frame(&app, "back to PR");
    }
}

// --- PR stacks on the file tabs: the navigator's stack list -----------------------

fn with_config(app: &mut App, toml: &str) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), toml).unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
}

/// `main` ← `head-10` ← `head-11` (checked out) ← `head-12`, the stack known and read.
fn stack_render_app(fetched_twelve: bool) -> (Repo, App) {
    use herdr_reviewr::forge::{PrSnapshot, PrState, PrView, StackEntry};
    let r = Repo::init();
    r.write("base.rs", "base\n");
    r.commit_all("init");
    for n in [10, 11, 12] {
        r.git(&["checkout", "-q", "-b", &format!("head-{n}")]);
        r.write(&format!("f{n}.rs"), &format!("pr {n}\n"));
        r.commit_all(&format!("pr {n}"));
    }
    r.git(&["checkout", "-q", "head-11"]);
    let tip = |n: u64| r.git(&["rev-parse", &format!("head-{n}")]).trim().to_string();
    let heads: Vec<(u64, String)> = vec![(10, tip(10)), (11, tip(11)), (12, tip(12))];
    if !fetched_twelve {
        r.git(&["branch", "-q", "-D", "head-12"]);
        r.git(&["reflog", "expire", "--expire=now", "--all"]);
        r.git(&["gc", "-q", "--prune=now"]);
    }
    let entry = |number, base: &str, level| StackEntry {
        number,
        title: format!("pr {number} title"),
        state: PrState::Open,
        is_draft: false,
        head_ref: format!("head-{number}"),
        base_ref: base.to_string(),
        level,
        url: None,
    };
    let snapshot = |number: u64, base: &str| PrSnapshot {
        number,
        head_ref: format!("head-{number}"),
        head_oid: heads.iter().find(|(n, _)| *n == number).unwrap().1.clone(),
        base_ref: base.into(),
        stack: vec![entry(10, "main", -1), entry(11, "head-10", 0), entry(12, "head-11", 1)],
        repo: herdr_reviewr::git::RepoTarget::new("github.com", "o", "r"),
        ..common::pr_snapshot()
    };
    let mut app = app_on(&r);
    app.apply_pr(PrView::Pr(Box::new(snapshot(11, "head-10"))));
    let batch = app.take_stack_batch(std::time::Instant::now(), None).unwrap();
    let results = batch
        .numbers
        .iter()
        .map(|&n| (n, PrView::Pr(Box::new(snapshot(n, &format!("head-{}", n - 1))))))
        .collect();
    app.land_stack_batch(batch.tag, results, std::time::Instant::now());
    common::land_world(&mut app);
    (r, app)
}

fn stack_line<'a>(out: &'a str, needle: &str) -> Option<(usize, &'a str)> {
    out.lines().enumerate().find(|(_, l)| l.contains(needle))
}

#[test]
fn the_stack_list_sits_above_the_file_list_by_default_and_below_on_request() {
    let (_r, mut app) = stack_render_app(true);
    let out = render(&app);
    let (stack_y, _) = stack_line(&out, "Stack · 3").expect("the stack list's title");
    let (files_y, _) = stack_line(&out, "┌ Files").expect("the file list's title");
    let (twelve_y, twelve) = stack_line(&out, "#12 open").expect("#12's row");
    let (base_y, _) = stack_line(&out, "└ main").expect("the base row");
    assert!(stack_y < twelve_y && twelve_y < base_y && base_y < files_y, "{out}");
    assert!(twelve.contains("pr 12 title"), "{twelve}");
    let (_, eleven) = stack_line(&out, "#11 open").unwrap();
    assert!(eleven.contains('●'), "the checked-out PR is marked: {eleven}");

    with_config(&mut app, "stack_list_position = \"bottom\"\n");
    let out = render(&app);
    let (stack_y, _) = stack_line(&out, "Stack · 3").unwrap();
    let (files_y, _) = stack_line(&out, "┌ Files").unwrap();
    assert!(files_y < stack_y, "below the file list:\n{out}");
}

#[test]
fn the_stack_list_parts_from_the_file_list_with_and_without_outer_borders() {
    let (_r, mut app) = stack_render_app(true);
    let out = render(&app);
    // Framed: two boxes, their borders meeting.
    let (base_y, _) = stack_line(&out, "└ main").unwrap();
    let below = out.lines().nth(base_y + 1).unwrap();
    assert!(below.contains('╰') || below.contains('└'), "the stack box closes: {below}");

    with_config(&mut app, "pane_outer_borders = false\n");
    let out = render(&app);
    let (base_y, base) = stack_line(&out, "└ main").unwrap();
    let col = base.chars().position(|c| c == '└').unwrap();
    let divider: Vec<char> = out.lines().nth(base_y + 1).unwrap().chars().collect();
    assert_eq!(divider[col], '─', "a one-row divider under the list:\n{out}");
    assert!(out.lines().nth(base_y + 2).unwrap().contains("Files"), "{out}");
}

#[test]
fn the_rows_wear_their_roles_and_the_header_names_the_range() {
    let (_r, mut app) = stack_render_app(true);
    app.pick_stack_range(
        herdr_reviewr::stack::StackEnd::Pr(12),
        herdr_reviewr::stack::StackEnd::Base,
    )
    .unwrap();
    let buf = render_buffer(&app);
    let out = dump(&buf);
    let header = out.lines().next().unwrap();
    assert!(header.contains("[stack] #12 vs main · read-only"), "{header}");
    let (y12, twelve) = stack_line(&out, "#12 open").unwrap();
    assert!(twelve.contains("head"), "{twelve}");
    let (_, base) = stack_line(&out, "└ main").unwrap();
    assert!(base.contains("against"), "{base}");
    // The head row wears the PR tab's viewed-row fill.
    let x = twelve.chars().position(|c| c == '#').unwrap() as u16;
    assert_eq!(buf[(x, y12 as u16)].bg, app.palette().view_bg);
    // A click on the range's name puts the keys on the stack list.
    let keymap = Keymap::default();
    let col = header.find("#12 vs").unwrap() as u16;
    let area = Rect::new(0, 0, 140, 40);
    assert_eq!(ui::hit_header(area, &app, &keymap, col, 0), Some(HeaderHit::Stack));
    // The footer from the diff: the list leads, never the comment key.
    app.focus = Focus::Diff;
    let out = render(&app);
    let footer = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap();
    assert!(footer.contains("P stack") && footer.contains("0 checked out"), "{footer}");
    assert!(!footer.contains("c comment"), "{footer}");
}

#[test]
fn a_local_branch_that_moved_on_shows_its_badge_and_the_choice_names_it() {
    let (r, mut app) = stack_render_app(true);
    // `head-12` gains a commit locally that the PR does not have.
    let other = tempfile::tempdir().unwrap();
    let wt = other.path().join("wt");
    let wt_s = wt.to_str().unwrap().to_string();
    r.git(&["worktree", "add", "-q", &wt_s, "head-12"]);
    std::fs::write(wt.join("l.rs"), "l\n").unwrap();
    r.git(&["-C", &wt_s, "add", "-A"]);
    r.git(&["-C", &wt_s, "commit", "-q", "-m", "local"]);
    common::land_world(&mut app);
    let out = render(&app);
    let (_, twelve) = stack_line(&out, "#12 open").unwrap();
    assert!(twelve.contains("local +1 -0"), "{twelve}");

    app.stack_local.insert(12);
    app.pick_stack_range(
        herdr_reviewr::stack::StackEnd::Pr(12),
        herdr_reviewr::stack::StackEnd::Pr(11),
    )
    .unwrap();
    let out = render(&app);
    assert!(
        out.lines().next().unwrap().contains("[stack] #12 (local) vs #11 · read-only"),
        "{out}"
    );
    let (_, twelve) = stack_line(&out, "#12 open").unwrap();
    assert!(twelve.contains("[local +1 -0]"), "the chosen badge reads as on: {twelve}");
}

#[test]
fn a_not_fetched_end_paints_how_to_fetch_it() {
    let (_r, mut app) = stack_render_app(false);
    app.pick_stack_range(
        herdr_reviewr::stack::StackEnd::Pr(12),
        herdr_reviewr::stack::StackEnd::Pr(11),
    )
    .unwrap();
    let out = render(&app);
    assert!(out.contains("#12's branch isn't fetched — `git fetch origin head-12`"), "{out}");
}

#[test]
fn without_a_stack_there_is_no_stack_box() {
    let r = Repo::init();
    r.write("a.rs", "a\n");
    r.commit_all("init");
    let app = app_on(&r);
    let out = render(&app);
    assert!(!out.contains("Stack ·"), "{out}");
}

#[test]
fn a_narrow_or_short_navigator_keeps_the_roles_and_the_file_list() {
    let (_r, mut app) = stack_render_app(true);
    app.pick_stack_range(
        herdr_reviewr::stack::StackEnd::Pr(12),
        herdr_reviewr::stack::StackEnd::Pr(11),
    )
    .unwrap();
    let out = dump(&render_size(&app, 60, 30));
    let (_, twelve) = stack_line(&out, "#12 ").unwrap();
    assert!(twelve.contains("head"), "the role outlasts the title: {twelve}");
    // Short: the list shrinks to what fits and scrolls, the file list keeps its rows.
    let out = dump(&render_size(&app, 120, 9));
    assert!(out.contains("Stack · 3") && out.contains("A f12.rs"), "{out}");
    assert_eq!(out.matches(" open ").count(), 1, "one stack row shows:\n{out}");
    // Too short for both: the file list keeps the navigator.
    let out = dump(&render_size(&app, 120, 7));
    assert!(!out.contains("Stack ·") && out.contains("Files"), "{out}");
}

#[test]
fn all_files_names_the_browsed_tree_and_marks_its_row() {
    let (_r, mut app) = stack_render_app(true);
    enter_tab(&mut app, Tab::AllFiles);
    app.set_files_source(Some(herdr_reviewr::stack::StackEnd::Pr(10))).unwrap();
    let out = render(&app);
    let header = out.lines().next().unwrap();
    assert!(header.contains("#10 tree ") && header.contains("· read-only"), "{header}");
    let (_, ten) = stack_line(&out, "#10 open").unwrap();
    assert!(ten.contains("tree"), "{ten}");
    assert!(out.contains("f10.rs") && !out.contains("f12.rs"), "{out}");
}

#[test]
fn a_cut_stack_shows_more_below_and_no_base_on_the_pr_tab() {
    use herdr_reviewr::forge::{NativeStack, PrView};
    for borders in ["", "pane_outer_borders = false\n"] {
        let (_r, mut app) = stack_app(borders);
        let mut cut = stack_pr(11);
        cut.stack[0].base_ref = "head-9".to_string();
        cut.stack_shape.more_below = true;
        cut.stack_shape.more_above = true;
        app.apply_pr(PrView::Pr(Box::new(cut)));
        let text = dump(&render_size(&app, 140, 30));
        let below = text.lines().position(|l| l.contains("… more below — not read"));
        let above = text.lines().position(|l| l.contains("… more above — not read"));
        let top = text.lines().position(|l| l.contains("#12 open"));
        let bottom = text.lines().position(|l| l.contains("#10 open"));
        assert!(above < top && bottom < below && below.is_some(), "{text}");
        assert!(!text.contains("└ head-9") && !text.contains("└ main"), "no fake base: {text}");

        // GitHub's own stack names itself, counts its members, and marks a PR merely on top.
        let mut native = stack_pr(11);
        native.stack_shape.native =
            Some(NativeStack { number: 1153, members: vec![10, 11], base_ref: "main".into() });
        app.apply_pr(PrView::Pr(Box::new(native)));
        let text = dump(&render_size(&app, 140, 30));
        assert!(text.contains("stack #1153 · 2"), "{text}");
        assert!(line_with(&text, "#12 open").contains("not in stack #1153"), "{text}");
        assert!(!line_with(&text, "#10 open").contains("not in"), "{text}");
        assert!(text.contains("└ main"), "{text}");
    }
}

#[test]
fn a_cut_stack_shows_more_below_in_the_navigator_stack_list() {
    use herdr_reviewr::forge::PrView;
    let (_r, mut app) = stack_render_app(true);
    let mut home = app.pr_checked_out_snapshot().unwrap().clone();
    home.stack[0].base_ref = "head-9".to_string();
    home.stack_shape.more_below = true;
    app.apply_pr(PrView::Pr(Box::new(home)));
    let out = render(&app);
    let (ten_y, _) = stack_line(&out, "#10 open").expect("#10's row");
    let (more_y, _) = stack_line(&out, "… more below — not read").expect("the cut row");
    assert_eq!(more_y, ten_y + 1, "{out}");
    assert!(!out.contains("└ head-9") && !out.contains("└ main"), "no fake base: {out}");
    assert!(out.contains("Stack · 3"), "the cut row is no PR: {out}");
}

/// Load a config with inline images on, the terminal's graphics answer `graphics`.
fn with_inline_images(app: &mut App, graphics: Option<bool>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "inline_images = true\n").unwrap();
    app.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    app.graphics = graphics;
    app.cell_px = Some((10, 20));
}

const SHOT: &str = "https://img.example/shot.png";
const BADGE: &str = "https://img.shields.io/badge/ci-passing-green.svg";

/// A GitHub PR whose description holds a screenshot and whose comment holds a badge.
fn image_pr_app() -> App {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let mut app = edited_app();
    app.set_tab(Tab::Pr).unwrap();
    app.pr_forge = herdr_reviewr::git::Forge::GitHub;
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        url: "https://github.com/o/r/pull/7".into(),
        body: format!("Intro line.\n\n![the screenshot]({SHOT})\n\nOutro line."),
        comments: vec![Comment { body: format!("![ci]({BADGE}) green"), ..common::comment() }],
        ..common::pr_snapshot()
    })));
    app
}

fn prepared(size: (u32, u32)) -> herdr_reviewr::images::Prepared {
    herdr_reviewr::images::Prepared { size, pixels: 100, payload: std::sync::Arc::from("QUJD") }
}

/// Render, put what the frame painted on the terminal as the loop would, render again.
fn render_placed(app: &mut App) -> Buffer {
    let _ = render_buffer(app);
    let painted = app.take_painted_images();
    let _ = app.images.place(&painted);
    render_buffer(app)
}

#[test]
fn an_image_is_its_alt_link_while_graphics_are_off_unanswered_loading_or_failed() {
    let plain = image_pr_app();
    let today = render_buffer(&plain);
    assert!(dump(&today).contains("⧉ the screenshot"));

    for graphics in [None, Some(false)] {
        let mut app = image_pr_app();
        with_inline_images(&mut app, graphics);
        assert_eq!(render_buffer(&app), today, "graphics {graphics:?}: byte-identical");
        assert!(app.image_requests().is_empty(), "nothing downloads without graphics");
    }

    let mut app = image_pr_app();
    with_inline_images(&mut app, Some(true));
    let _ = render_buffer(&app);
    let requests: Vec<String> = app.image_requests().into_iter().map(|r| r.url).collect();
    assert_eq!(requests, [SHOT, BADGE], "the visible images, top down");
    for url in &requests {
        app.images.mark_requested(url);
    }
    assert_eq!(render_buffer(&app), today, "loading: the alt link, never a gap");
    assert!(app.image_requests().is_empty(), "nothing asked for twice");
    for url in requests {
        app.images.land(url, None);
    }
    assert_eq!(render_buffer(&app), today, "a failure stays the alt link");
    assert_eq!(render_placed(&mut app), today, "a failed image places nothing");
}

#[test]
fn a_landed_image_paints_its_block_of_placeholder_cells_once_on_the_terminal() {
    let mut app = image_pr_app();
    with_inline_images(&mut app, Some(true));
    let _ = render_buffer(&app);
    for request in app.image_requests() {
        app.images.mark_requested(&request.url);
    }
    // 120×60 px at 10×20-px cells: 12 columns by 3 rows. The badge: 9×1.
    app.images.land(SHOT.into(), Some(prepared((120, 60))));
    app.images.land(BADGE.into(), Some(prepared((90, 20))));

    // The first frame lays the block out (its alt on top) before the terminal holds it.
    let first = dump(&render_buffer(&app));
    assert!(first.contains("⧉ the screen"), "{first}");
    assert!(!first.contains(herdr_reviewr::graphics::PLACEHOLDER));

    let buf = render_placed(&mut app);
    let shot = herdr_reviewr::images::Cells { cols: 12, rows: 3 };
    let id = app.images.placed_id(SHOT, shot).expect("placed");
    let (r, g, b) = herdr_reviewr::graphics::id_rgb(id);
    let cells: Vec<(u16, u16)> = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf[(x, y)].fg == ratatui::style::Color::Rgb(r, g, b))
        .collect();
    assert_eq!(cells.len(), 36, "12 × 3 cells");
    let (x0, y0) = cells[0];
    for &(x, y) in &cells {
        let want = herdr_reviewr::graphics::placeholder((y - y0) as usize, (x - x0) as usize);
        assert_eq!(buf[(x, y)].symbol(), want, "cell ({x}, {y})");
    }
    let out = dump(&buf);
    assert!(out.contains("Intro line.") && out.contains("Outro line."));
    assert!(!out.contains("⧉ the screen"), "the picture replaced its alt");
    // The badge rides its comment's line, then the text.
    let badge = herdr_reviewr::images::Cells { cols: 9, rows: 1 };
    let bid = app.images.placed_id(BADGE, badge).expect("the badge placed too");
    let (r, g, b) = herdr_reviewr::graphics::id_rgb(bid);
    let row = (0..buf.area.height)
        .find(|&y| {
            (0..buf.area.width).any(|x| buf[(x, y)].fg == ratatui::style::Color::Rgb(r, g, b))
        })
        .unwrap();
    assert!(out.lines().nth(row as usize).unwrap().contains(" green"));

    // Switched off: the alt links again, and the loop deletes what it sent.
    with_inline_images(&mut app, Some(true));
    let mut off = app;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "inline_images = false\n").unwrap();
    off.set_plugin_config(herdr_reviewr::config::plugin_config_in(dir.path()).unwrap());
    assert!(dump(&render_buffer(&off)).contains("⧉ the screenshot"));
    assert!(!off.images.deletions().is_empty());
}

#[test]
fn the_forge_token_rides_only_requests_to_the_forge_host() {
    let mut app = image_pr_app();
    let req = |app: &App, url: &str| app.image_request(url).token_host;
    assert_eq!(
        req(&app, "https://github.com/user-attachments/assets/1").as_deref(),
        Some("github.com")
    );
    assert_eq!(req(&app, "https://private-user-images.githubusercontent.com/1.png"), None);
    assert_eq!(req(&app, BADGE), None);
    assert_eq!(req(&app, "http://github.com/user-attachments/assets/1"), None);
    // A GitLab or Azure PR never sends a `gh` token.
    app.pr_forge = herdr_reviewr::git::Forge::GitLab;
    assert_eq!(req(&app, "https://github.com/user-attachments/assets/1"), None);
}

#[test]
fn an_image_landing_above_the_reader_leaves_their_view_still() {
    use herdr_reviewr::forge::{PrSnapshot, PrView};
    let mut app = image_pr_app();
    let paras = (0..30).map(|n| format!("para-{n:02}")).collect::<Vec<_>>().join("\n\n");
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        url: "https://github.com/o/r/pull/7".into(),
        body: format!("Intro.\n\n![shot]({SHOT})\n\n{paras}"),
        ..common::pr_snapshot()
    })));
    with_inline_images(&mut app, Some(true));
    let read = |app: &mut App| -> Vec<String> {
        let out = dump(&render_placed(app));
        read_column(&out).iter().map(|l| l.trim_end().to_string()).collect()
    };
    let _ = read(&mut app);
    let area = Rect::new(0, 0, 140, 40);
    for _ in 0..4 {
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 5,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &[],
            &Keymap::default(),
            &herdr_reviewr::export::Clipboard,
        )
        .unwrap();
    }
    let before = read(&mut app);
    assert!(!before.iter().any(|l| l.contains("⧉ shot")), "the image is above the pane");
    let scroll = app.pr_read_scroll();

    for request in app.image_requests() {
        app.images.mark_requested(&request.url);
    }
    // 100×200 px: ten rows where the alt link was one.
    app.images.land(SHOT.into(), Some(prepared((100, 200))));
    let after = read(&mut app);
    assert_eq!(before, after, "the reader's view does not move");
    assert_eq!(app.pr_read_scroll(), scroll + 9, "the scroll absorbs the growth");

    // From the top, the same landing simply shows the block.
    let mut top = image_pr_app();
    with_inline_images(&mut top, Some(true));
    let _ = read(&mut top);
    assert_eq!(top.pr_read_scroll(), 0);
}

const DIAGRAM_BOT: &str = include_str!("fixtures/diagram_bot_comment.md");
const LENS_RAW: &str = "https://raw.githubusercontent.com/o/r/diagrams/7/0123abcd";

/// A GitHub PR whose one comment is the diagram-bot bot's: pictures in an open `<details>`, a
/// nested closed one, and a closed one after it.
fn diagram_bot_app() -> App {
    use herdr_reviewr::forge::{Comment, PrSnapshot, PrView};
    let mut app = edited_app();
    app.set_tab(Tab::Pr).unwrap();
    app.pr_forge = herdr_reviewr::git::Forge::GitHub;
    app.apply_pr(PrView::Pr(Box::new(PrSnapshot {
        url: "https://github.com/o/r/pull/7".into(),
        comments: vec![Comment { body: DIAGRAM_BOT.into(), ..common::comment() }],
        ..common::pr_snapshot()
    })));
    with_inline_images(&mut app, Some(true));
    app
}

#[test]
fn a_diagram_bot_comment_paints_its_open_picture_from_the_raw_file_with_the_token() {
    let mut app = diagram_bot_app();
    let _ = render_buffer(&app);
    let requests = app.image_requests();
    // The default theme is dark: the dark source, at its raw URL, with github.com's token.
    let want = format!("{LENS_RAW}/containers-dark-bb22.svg");
    assert_eq!(requests.len(), 1, "closed details fetch nothing: {requests:?}");
    assert_eq!(requests[0].url, want);
    assert_eq!(requests[0].token_host.as_deref(), Some("github.com"));
    app.images.mark_requested(&want);
    app.images.land(want.clone(), Some(prepared((1284, 600))));

    let buf = render_placed(&mut app);
    let out = dump(&buf);
    assert!(out.contains("▾ Pipeline containers"), "{out}");
    assert!(out.contains("▸ Sorting components") && out.contains("▸ Intake flow"));
    assert!(out.contains(herdr_reviewr::graphics::PLACEHOLDER), "the picture paints");
    assert!(!out.contains("⧉ Pipeline containers"), "its alt link is gone");

    // Expanding the nested one asks for its picture next, never before.
    app.toggle_details("Sorting components");
    let _ = render_buffer(&app);
    let next: Vec<String> = app.image_requests().into_iter().map(|r| r.url).collect();
    assert_eq!(next, [format!("{LENS_RAW}/components-dark-dd44.svg")]);
}

#[test]
fn a_light_theme_takes_the_pictures_light_source() {
    let mut app = diagram_bot_app();
    app.set_cli_theme(Some("github-light".into()));
    let _ = render_buffer(&app);
    let urls: Vec<String> = app.image_requests().into_iter().map(|r| r.url).collect();
    assert_eq!(urls, [format!("{LENS_RAW}/containers-light-aa11.svg")]);
}
