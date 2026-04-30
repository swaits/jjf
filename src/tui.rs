use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::jj::{self, PreviewParts, Row};

const PREVIEW_MIN_COLS: u16 = 80;

// SGR constants
const SGR_RESET: &[u8] = b"\x1b[0m";
const SGR_RESET_SHORT: &[u8] = b"\x1b[m";
const SGR_DIM: &[u8] = b"\x1b[2m";
const SGR_NORMAL_WEIGHT: &[u8] = b"\x1b[22m";
const SGR_BOLD: &[u8] = b"\x1b[1m";
const SGR_FG_CYAN: &[u8] = b"\x1b[36m";
const SGR_FG_YELLOW: &[u8] = b"\x1b[33m";
const ERASE_TO_EOL: &[u8] = b"\x1b[K";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

// Synchronized output (DEC Mode 2026): Kitty / WezTerm / foot / recent iTerm
// honor these brackets and present the buffered redraw atomically; other
// terminals ignore them. Eliminates tearing on supported terminals.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// The cursor-row "highlight" SGR. Dark gray bg on dark themes, reverse
/// video on light themes (since indexed-color 237 disappears on light).
fn cursor_bg() -> &'static [u8] {
    static CACHE: OnceLock<&'static [u8]> = OnceLock::new();
    CACHE.get_or_init(|| {
        if is_light_terminal() {
            b"\x1b[7m"
        } else {
            b"\x1b[48;5;237m"
        }
    })
}

fn is_light_terminal() -> bool {
    if let Ok(v) = std::env::var("COLORFGBG")
        && let Some(bg_str) = v.split(';').next_back()
        && let Ok(bg) = bg_str.parse::<u8>()
    {
        return bg == 7 || bg == 15;
    }
    false
}
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";

struct App {
    rows: Vec<Row>,
    filter: String,
    cursor: usize,
    view_offset: usize,
    selected: HashSet<usize>,
    filtered: Vec<usize>,
    matcher: Matcher,
    last_height: usize,
    preview_cache: HashMap<usize, PreviewParts>,
}

impl App {
    fn new(rows: Vec<Row>) -> Self {
        let n = rows.len();
        Self {
            rows,
            filter: String::new(),
            cursor: 0,
            view_offset: 0,
            selected: HashSet::new(),
            filtered: (0..n).collect(),
            matcher: Matcher::new(Config::DEFAULT),
            last_height: 0,
            preview_cache: HashMap::new(),
        }
    }

    fn preview_for(&mut self, row_idx: usize) -> &PreviewParts {
        if !self.preview_cache.contains_key(&row_idx) {
            let cid = self.rows[row_idx].change_id_short.clone();
            self.preview_cache.insert(row_idx, jj::show_summary(&cid));
        }
        self.preview_cache.get(&row_idx).unwrap()
    }

    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.filtered = (0..self.rows.len()).collect();
        } else {
            let App {
                rows,
                matcher,
                filter,
                ..
            } = self;
            let mut hbuf: Vec<char> = Vec::new();
            let mut nbuf: Vec<char> = Vec::new();
            let q_lower = filter.to_lowercase();
            let mut scored: Vec<(usize, u32)> = rows
                .iter()
                .enumerate()
                .filter_map(|(i, row)| {
                    score_row(row, filter, &q_lower, matcher, &mut hbuf, &mut nbuf).map(|s| (i, s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        }
        self.cursor = 0;
        self.view_offset = 0;
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let mut c = self.cursor as isize + delta;
        if c < 0 {
            c = 0;
        }
        let max = len as isize - 1;
        if c > max {
            c = max;
        }
        self.cursor = c as usize;
        self.adjust_view();
    }

    fn jump_top(&mut self) {
        self.cursor = 0;
        self.adjust_view();
    }

    fn jump_bottom(&mut self) {
        if !self.filtered.is_empty() {
            self.cursor = self.filtered.len() - 1;
            self.adjust_view();
        }
    }

    fn adjust_view(&mut self) {
        if self.last_height == 0 {
            return;
        }
        if self.cursor < self.view_offset {
            self.view_offset = self.cursor;
        } else if self.cursor >= self.view_offset + self.last_height {
            self.view_offset = self.cursor + 1 - self.last_height;
        }
    }

    fn toggle_select(&mut self) {
        if let Some(&row_idx) = self.filtered.get(self.cursor)
            && !self.selected.insert(row_idx)
        {
            self.selected.remove(&row_idx);
        }
    }

    fn pending_ids(&self) -> Vec<String> {
        if self.selected.is_empty() {
            if let Some(&row_idx) = self.filtered.get(self.cursor) {
                return vec![short_id(&self.rows[row_idx])];
            }
            return Vec::new();
        }
        let mut indices: Vec<usize> = self.selected.iter().copied().collect();
        indices.sort_unstable();
        indices
            .into_iter()
            .map(|i| short_id(&self.rows[i]))
            .collect()
    }
}

fn short_id(row: &Row) -> String {
    if row.change_id_prefix.is_empty() {
        row.change_id_short.clone()
    } else {
        row.change_id_prefix.clone()
    }
}

fn score_row(
    row: &Row,
    query: &str,
    q_lower: &str,
    matcher: &mut Matcher,
    hbuf: &mut Vec<char>,
    nbuf: &mut Vec<char>,
) -> Option<u32> {
    let mut best: u32 = 0;
    let mut hit = false;

    let cp = row.change_id_prefix.to_lowercase();
    let cs = row.change_id_short.to_lowercase();
    let kp = row.commit_id_prefix.to_lowercase();
    let ks = row.commit_id_short.to_lowercase();

    if !cp.is_empty() && cp.starts_with(q_lower) {
        best = best.max(1_000_000);
        hit = true;
    }
    if cs.starts_with(q_lower) {
        best = best.max(100_000);
        hit = true;
    }
    if !kp.is_empty() && kp.starts_with(q_lower) {
        best = best.max(10_000);
        hit = true;
    }
    if ks.starts_with(q_lower) {
        best = best.max(1_000);
        hit = true;
    }

    let h = Utf32Str::new(&row.plain, hbuf);
    let n = Utf32Str::new(query, nbuf);
    if let Some(score) = matcher.fuzzy_match(h, n) {
        best = best.max(score as u32);
        hit = true;
    }

    if hit { Some(best) } else { None }
}

pub fn run(
    rows: Vec<Row>,
    subcommand: &str,
    passthrough: &[String],
) -> Result<Option<Vec<String>>> {
    install_panic_hook();
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open /dev/tty")?;
    crossterm::terminal::enable_raw_mode().context("enable raw mode")?;
    // Full-screen alt-screen mode: saves the user's screen, gives us the
    // whole terminal as a clean canvas, restores on exit. Resize behavior
    // becomes trivial because we own the entire display.
    tty.write_all(b"\x1b[?1049h").context("enter alt screen")?;
    tty.write_all(HIDE_CURSOR).context("hide cursor")?;
    tty.flush().ok();

    let result = run_loop(&mut tty, rows, subcommand, passthrough);

    let _ = tty.write_all(SHOW_CURSOR);
    let _ = tty.write_all(b"\x1b[?1049l");
    let _ = tty.flush();
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        if let Ok(mut tty) = OpenOptions::new().write(true).open("/dev/tty") {
            let _ = tty.write_all(SHOW_CURSOR);
            let _ = tty.write_all(b"\x1b[?1049l");
            let _ = tty.flush();
        }
        original(info);
    }));
}

fn run_loop(
    tty: &mut File,
    rows: Vec<Row>,
    subcommand: &str,
    passthrough: &[String],
) -> Result<Option<Vec<String>>> {
    let mut app = App::new(rows);
    let (mut term_cols, mut term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    // Chrome rows: input(1) + cmd_preview(1) + hint(1) = 3.
    app.last_height = (term_rows as usize).saturating_sub(3);
    loop {
        render(tty, &mut app, term_rows, term_cols, subcommand, passthrough)?;
        if !event::poll(Duration::from_millis(250)).context("event poll failed")? {
            continue;
        }
        match event::read().context("event read failed")? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(action) = handle_key(&mut app, key) {
                    return Ok(action);
                }
            }
            Event::Resize(new_cols, new_rows) => {
                // Alt-screen mode: trivial. Just update dims and redraw.
                term_cols = new_cols;
                term_rows = new_rows;
                app.last_height = (term_rows as usize).saturating_sub(3);
                let _ = write!(tty, "\x1b[2J");
                let _ = tty.flush();
            }
            _ => {}
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Option<Option<Vec<String>>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl {
        match key.code {
            KeyCode::Char('c') => return Some(None),
            KeyCode::Char('n') | KeyCode::Char('j') => {
                app.move_cursor(1);
                return None;
            }
            KeyCode::Char('p') | KeyCode::Char('k') => {
                app.move_cursor(-1);
                return None;
            }
            KeyCode::Char('a') => {
                app.jump_top();
                return None;
            }
            KeyCode::Char('e') => {
                app.jump_bottom();
                return None;
            }
            KeyCode::Char('h') => {
                app.filter.pop();
                app.refilter();
                return None;
            }
            KeyCode::Char('u') => {
                app.filter.clear();
                app.refilter();
                return None;
            }
            KeyCode::Char('w') => {
                delete_word(&mut app.filter);
                app.refilter();
                return None;
            }
            _ => return None,
        }
    }

    match key.code {
        KeyCode::Esc => Some(None),
        KeyCode::Enter => Some(Some(app.pending_ids())),
        KeyCode::Tab => {
            app.toggle_select();
            None
        }
        KeyCode::Up => {
            app.move_cursor(-1);
            None
        }
        KeyCode::Down => {
            app.move_cursor(1);
            None
        }
        KeyCode::PageUp => {
            app.move_cursor(-(app.last_height.max(1) as isize));
            None
        }
        KeyCode::PageDown => {
            app.move_cursor(app.last_height.max(1) as isize);
            None
        }
        KeyCode::Home => {
            app.jump_top();
            None
        }
        KeyCode::End => {
            app.jump_bottom();
            None
        }
        KeyCode::Backspace => {
            app.filter.pop();
            app.refilter();
            None
        }
        KeyCode::Char(c) => {
            app.filter.push(c);
            app.refilter();
            None
        }
        _ => None,
    }
}

fn delete_word(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
    while let Some(c) = s.chars().last() {
        if c.is_whitespace() {
            break;
        }
        s.pop();
    }
}

fn render(
    tty: &mut File,
    app: &mut App,
    term_rows: u16,
    term_cols: u16,
    subcommand: &str,
    passthrough: &[String],
) -> Result<()> {
    let viewport_y: u16 = 0;
    let viewport_height = term_rows;
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    buf.extend_from_slice(SYNC_BEGIN);
    let log_height = app.last_height;

    let preview_enabled = term_cols >= PREVIEW_MIN_COLS;
    // Layout (1-indexed columns), with a 2-col separator between picker and box:
    //   cols 1..=left_w               picker pane (text + padding for non-cursor;
    //                                 text only, no bg padding, for cursor)
    //   cols left_w+1 .. left_w+2     2-col separator
    //   col  left_w+3                 box left edge `│` (non-cursor)
    //   cols left_w+4 .. left_w+5     2-col gutter inside box
    //   cols left_w+6 .. term_cols    preview content
    //
    // On the cursor row the picker text is NOT padded; instead a dynamic
    // arrow fills from the text end through the separator and peeks into
    // the box by exactly one column:
    //   col Y+1                       ` `   (one space after text)
    //   col Y+2                       `├`   (arrow start)
    //   cols Y+3 .. left_w+3          `─`   (variable-length body)
    //   col left_w+4                  `►`   (peeks 1 col into the box)
    //   col left_w+5                  ` `
    //   col left_w+6                  preview content (aligned with non-cursor)
    let (left_w, right_x, right_w): (usize, u16, usize) = if preview_enabled {
        let lw = (term_cols / 2).saturating_sub(3) as usize;
        let rx = (lw + 2) as u16; // col where `│` lives (1-indexed): lw+3 = rx+1
        let rw = (term_cols as usize).saturating_sub(lw + 2);
        (lw, rx, rw)
    } else {
        (term_cols as usize, term_cols, 0)
    };

    // Preview pane layout: top border (`┌─────`) sits on the *input row*,
    // not the first log row, so log rows align 1:1 with preview content
    // rows. Each preview content row is `│  <line>` (3-char gutter) or
    // `├─►<line>` on the cursor's row, drawing the eye from picker
    // selection into preview content.
    let preview_gutter_w = 3;
    let preview_content_w = right_w.saturating_sub(preview_gutter_w);
    let preview_rows = log_height; // top border is on input row, not log area
    let preview_lines: Vec<Vec<u8>> = if preview_enabled {
        if let Some(&row_idx) = app.filtered.get(app.cursor) {
            let parts = app.preview_for(row_idx);
            build_preview_lines(parts, preview_rows, preview_content_w)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Input row (left only).
    write!(buf, "\x1b[{};1H", viewport_y + 1)?;
    buf.extend_from_slice(SGR_BOLD);
    buf.extend_from_slice(SGR_FG_CYAN);
    buf.extend_from_slice("❯ ".as_bytes());
    buf.extend_from_slice(SGR_RESET);
    buf.extend_from_slice(app.filter.as_bytes());
    // Reverse-video caret on a single space — matches fzf/lazygit/telescope
    // convention; survives any colorscheme without competing with the cyan ❯.
    buf.extend_from_slice(b"\x1b[7m \x1b[27m");
    buf.extend_from_slice(ERASE_TO_EOL);

    // Top border for the preview pane — drawn at the input row's vertical
    // position so the box "starts" alongside the search field. Lives at
    // col left_w+3 (= right_x + 1), past the 2-col separator from the picker.
    if preview_enabled {
        write!(buf, "\x1b[{};{}H", viewport_y + 1, right_x + 1)?;
        buf.extend_from_slice(ERASE_TO_EOL);
        buf.extend_from_slice(SGR_DIM);
        buf.extend_from_slice("┌".as_bytes());
        for _ in 1..right_w {
            buf.extend_from_slice("─".as_bytes());
        }
        buf.extend_from_slice(SGR_RESET);
    }

    // Log rows.
    let visible: Vec<(usize, usize)> = app
        .filtered
        .iter()
        .enumerate()
        .skip(app.view_offset)
        .take(log_height)
        .map(|(filt_idx, &row_idx)| (filt_idx, row_idx))
        .collect();

    for i in 0..log_height {
        let row_y = viewport_y + 2 + i as u16; // input row + offset
        write!(buf, "\x1b[{};1H", row_y)?;
        buf.extend_from_slice(ERASE_TO_EOL);

        let on_cursor = visible
            .get(i)
            .is_some_and(|&(filt_idx, _)| filt_idx == app.cursor);

        let visible_used: usize = if let Some(&(filt_idx, row_idx)) = visible.get(i) {
            let row = &app.rows[row_idx];
            let is_cursor = filt_idx == app.cursor;
            let is_selected = app.selected.contains(&row_idx);
            render_log_row(&mut buf, row, is_cursor, is_selected, left_w)
        } else {
            0
        };

        if preview_enabled {
            if on_cursor {
                // Dynamic arrow inline, starting at col visible_used+1:
                //   ` ` ├ ─×N ► ` `   then preview content at col left_w+6.
                buf.push(b' ');
                buf.extend_from_slice(SGR_DIM);
                buf.extend_from_slice("├".as_bytes());
                let dash_count = (left_w + 1).saturating_sub(visible_used);
                for _ in 0..dash_count {
                    buf.extend_from_slice("─".as_bytes());
                }
                buf.extend_from_slice(SGR_RESET);
                buf.extend_from_slice(SGR_FG_CYAN);
                buf.extend_from_slice("►".as_bytes());
                buf.extend_from_slice(SGR_RESET);
                buf.push(b' ');
            } else {
                // Position past the picker pane: 2-col sep, then `│  `, then content.
                write!(buf, "\x1b[{};{}H", row_y, left_w as u16 + 1)?;
                buf.push(b' ');
                buf.push(b' ');
                buf.extend_from_slice(SGR_DIM);
                buf.extend_from_slice("│  ".as_bytes());
                buf.extend_from_slice(SGR_RESET);
            }
            if let Some(line) = preview_lines.get(i) {
                buf.extend_from_slice(line);
                buf.extend_from_slice(SGR_RESET);
            }
        }
    }

    // Command preview row — `▶ jj describe -m 'foo' -r 'vx'` in bright green.
    // Tells the user exactly what Enter will run, with the resolved `-r '<id>'`
    // updated live as the cursor moves or selections toggle.
    let cmd_y = viewport_y + 1 + log_height as u16 + 1;
    write!(buf, "\x1b[{};1H", cmd_y)?;
    buf.extend_from_slice(ERASE_TO_EOL);
    let cmd = jj::command_line(subcommand, passthrough, &app.pending_ids());
    let cmd_max = (term_cols as usize).saturating_sub(2); // for "▶ "
    let cmd_visible: String = if cmd.chars().count() <= cmd_max {
        cmd
    } else if cmd_max == 0 {
        String::new()
    } else {
        let mut s: String = cmd.chars().take(cmd_max.saturating_sub(1)).collect();
        s.push('…');
        s
    };
    buf.extend_from_slice(b"\x1b[1;32m"); // bold green
    buf.extend_from_slice("▶ ".as_bytes());
    buf.extend_from_slice(cmd_visible.as_bytes());
    buf.extend_from_slice(SGR_RESET);

    // Hint row (full width, below cmd preview).
    let hint_y = cmd_y + 1;
    write!(buf, "\x1b[{};1H", hint_y)?;
    let counts = if app.selected.is_empty() {
        format!("[{}/{}]  ", app.filtered.len(), app.rows.len())
    } else {
        format!(
            "[{}/{} · {} selected]  ",
            app.filtered.len(),
            app.rows.len(),
            app.selected.len()
        )
    };
    buf.extend_from_slice(SGR_DIM);
    buf.extend_from_slice(counts.as_bytes());
    buf.extend_from_slice(b"type filter");
    write_hint_pair(&mut buf, "↑↓/^N^P".as_bytes(), b"nav");
    write_hint_pair(&mut buf, b"tab", b"select");
    write_hint_pair(&mut buf, b"enter", b"run");
    write_hint_pair(&mut buf, b"^U", b"clear");
    write_hint_pair(&mut buf, b"esc", b"quit");
    buf.extend_from_slice(SGR_RESET);
    buf.extend_from_slice(ERASE_TO_EOL);

    // Park cursor at end of input line.
    let input_col = 2 + app.filter.chars().count() as u16 + 1;
    write!(buf, "\x1b[{};{}H", viewport_y + 1, input_col + 1)?;

    let _ = viewport_height;
    let _ = right_x;
    buf.extend_from_slice(SYNC_END);
    tty.write_all(&buf)?;
    tty.flush()?;
    Ok(())
}

fn write_hint_pair(buf: &mut Vec<u8>, key: &[u8], label: &[u8]) {
    buf.extend_from_slice(" · ".as_bytes());
    buf.extend_from_slice(SGR_NORMAL_WEIGHT);
    buf.extend_from_slice(key);
    buf.extend_from_slice(SGR_DIM);
    buf.push(b' ');
    buf.extend_from_slice(label);
}

/// Pack description + files into `available_rows`. The file list always wins —
/// description is clipped (with a dim ellipsis appended to its last kept line)
/// to whatever room is left after the files (and a blank separator) are placed.
/// Long description/file lines are word-wrapped to `width` columns.
fn build_preview_lines(parts: &PreviewParts, available_rows: usize, width: usize) -> Vec<Vec<u8>> {
    let mut desc_lines: Vec<Vec<u8>> = Vec::new();
    if !parts.description.is_empty() {
        for line in split_lines(&parts.description) {
            desc_lines.extend(wrap_ansi(&line, width));
        }
    }
    let mut file_lines: Vec<Vec<u8>> = Vec::new();
    if !parts.files.is_empty() {
        for line in split_lines(&parts.files) {
            file_lines.extend(wrap_ansi(&line, width));
        }
    }

    let blank_sep = if !desc_lines.is_empty() && !file_lines.is_empty() {
        1
    } else {
        0
    };
    let max_desc = available_rows.saturating_sub(file_lines.len() + blank_sep);
    let kept_desc = desc_lines.len().min(max_desc);
    let clipped = desc_lines.len() > kept_desc;

    let mut out: Vec<Vec<u8>> = Vec::with_capacity(kept_desc + blank_sep + file_lines.len());
    for (i, line) in desc_lines.iter().take(kept_desc).enumerate() {
        if clipped && i + 1 == kept_desc {
            let mut last = line.clone();
            last.extend_from_slice(b" \x1b[2m\xe2\x80\xa6\x1b[0m");
            out.push(last);
        } else {
            out.push(line.clone());
        }
    }
    if blank_sep == 1 && kept_desc > 0 {
        out.push(Vec::new());
    }
    for line in file_lines {
        out.push(line);
    }
    out
}

/// Word-wrap a line containing ANSI escapes to `width` visible columns.
/// Splits on spaces; a word longer than `width` overflows on its own line
/// rather than mid-word breaking. ANSI escape sequences carry through and
/// don't count toward visible width.
fn wrap_ansi(line: &[u8], width: usize) -> Vec<Vec<u8>> {
    if width == 0 || visible_width(line) <= width {
        return vec![line.to_vec()];
    }

    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut current_visible = 0usize;
    let mut word: Vec<u8> = Vec::new();
    let mut word_visible = 0usize;

    let mut i = 0;
    while i < line.len() {
        if line[i] == 0x1b && line.get(i + 1) == Some(&b'[') {
            let start = i;
            i += 2;
            while i < line.len() && !line[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < line.len() {
                i += 1;
            }
            // Append ANSI to the in-progress word so styling travels with it.
            word.extend_from_slice(&line[start..i]);
        } else if line[i] == b' ' {
            // End-of-word: try to fit `word` into `current`.
            let sep = if current_visible > 0 { 1 } else { 0 };
            if current_visible + sep + word_visible <= width {
                if sep == 1 {
                    current.push(b' ');
                    current_visible += 1;
                }
                current.extend_from_slice(&word);
                current_visible += word_visible;
            } else {
                // Doesn't fit: flush current, start new line with the word.
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                current.extend_from_slice(&word);
                current_visible = word_visible;
            }
            word.clear();
            word_visible = 0;
            i += 1;
        } else {
            let lead = line[i];
            let len = utf8_char_len(lead);
            let end = (i + len).min(line.len());
            word.extend_from_slice(&line[i..end]);
            word_visible += 1;
            i = end;
        }
    }
    // Final word
    if word_visible > 0 {
        let sep = if current_visible > 0 { 1 } else { 0 };
        if current_visible + sep + word_visible <= width {
            if sep == 1 {
                current.push(b' ');
            }
            current.extend_from_slice(&word);
        } else {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            current.extend_from_slice(&word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Render the picker portion of one log row. Returns the 1-indexed column
/// after the rendered content (i.e. the col where the arrow's leading space
/// would be drawn for the cursor row).
///
/// Non-cursor rows are padded with normal spaces to fill `width`. Cursor rows
/// have bg highlight on actual text only and are NOT padded — the dynamic
/// arrow fills the rest.
fn render_log_row(
    buf: &mut Vec<u8>,
    row: &Row,
    is_cursor: bool,
    is_selected: bool,
    width: usize,
) -> usize {
    let gutter_w = 2;
    let content_w = width.saturating_sub(gutter_w);

    // Gutter: yellow `▎ ` if selected, else two spaces.
    if is_selected {
        buf.extend_from_slice(SGR_BOLD);
        buf.extend_from_slice(SGR_FG_YELLOW);
        buf.extend_from_slice("▎ ".as_bytes());
        buf.extend_from_slice(SGR_RESET);
    } else {
        buf.extend_from_slice(b"  ");
    }

    let truncated = truncate_ansi(&row.styled, content_w);
    let used = visible_width(&truncated);

    if is_cursor {
        let bg = cursor_bg();
        buf.extend_from_slice(bg);
        inject_bg_into(buf, &truncated, bg);
        buf.extend_from_slice(SGR_RESET);
        gutter_w + used
    } else {
        buf.extend_from_slice(&truncated);
        buf.extend_from_slice(SGR_RESET);
        let pad = content_w.saturating_sub(used);
        for _ in 0..pad {
            buf.push(b' ');
        }
        width
    }
}

fn split_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes.split(|&b| b == b'\n').map(|l| l.to_vec()).collect()
}

/// Count visible chars (post-CSI-strip) in a UTF-8 byte slice.
fn visible_width(bytes: &[u8]) -> usize {
    let mut w = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            let lead = bytes[i];
            let len = utf8_char_len(lead);
            i = (i + len).min(bytes.len());
            w += 1;
        }
    }
    w
}

/// Truncate `bytes` so visible (non-CSI, non-continuation) chars fit in `max`.
/// All CSI sequences are preserved; only printable chars are counted.
fn truncate_ansi(bytes: &[u8], max: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut visible = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let start = i;
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            out.extend_from_slice(&bytes[start..i]);
        } else {
            if visible >= max {
                break;
            }
            let lead = bytes[i];
            let len = utf8_char_len(lead);
            let end = (i + len).min(bytes.len());
            out.extend_from_slice(&bytes[i..end]);
            visible += 1;
            i = end;
        }
    }
    out
}

fn utf8_char_len(lead: u8) -> usize {
    if lead < 0x80 || (0x80..0xc0).contains(&lead) {
        1
    } else if lead < 0xe0 {
        2
    } else if lead < 0xf0 {
        3
    } else {
        4
    }
}

/// Copy `src` into `dst`, but after every embedded `ESC [ 0 m` or `ESC [ m`
/// reset, append `bg` so the cursor-row background survives mid-row resets.
fn inject_bg_into(dst: &mut Vec<u8>, src: &[u8], bg: &[u8]) {
    let mut i = 0;
    while i < src.len() {
        if src[i] == 0x1b && src.get(i + 1) == Some(&b'[') {
            if src[i..].starts_with(SGR_RESET) {
                dst.extend_from_slice(SGR_RESET);
                dst.extend_from_slice(bg);
                i += SGR_RESET.len();
                continue;
            }
            if src[i..].starts_with(SGR_RESET_SHORT) {
                dst.extend_from_slice(SGR_RESET_SHORT);
                dst.extend_from_slice(bg);
                i += SGR_RESET_SHORT.len();
                continue;
            }
        }
        dst.push(src[i]);
        i += 1;
    }
}
