use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::jj::{self, Row};

const PREVIEW_MIN_COLS: u16 = 80;
const VIEWPORT_MIN: u16 = 15;

// SGR constants
const SGR_RESET: &[u8] = b"\x1b[0m";
const SGR_RESET_SHORT: &[u8] = b"\x1b[m";
const SGR_DIM: &[u8] = b"\x1b[2m";
const SGR_NORMAL_WEIGHT: &[u8] = b"\x1b[22m";
const SGR_BOLD: &[u8] = b"\x1b[1m";
const SGR_FG_CYAN: &[u8] = b"\x1b[36m";
const SGR_FG_YELLOW: &[u8] = b"\x1b[33m";
const ERASE_TO_EOL: &[u8] = b"\x1b[K";
const ERASE_TO_END: &[u8] = b"\x1b[J";
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
    preview_cache: HashMap<usize, Vec<u8>>,
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

    fn preview_for(&mut self, row_idx: usize) -> &[u8] {
        if !self.preview_cache.contains_key(&row_idx) {
            let cid = self.rows[row_idx].change_id_short.clone();
            let bytes = jj::show_summary(&cid);
            self.preview_cache.insert(row_idx, bytes);
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

pub fn run(rows: Vec<Row>) -> Result<Option<Vec<String>>> {
    install_panic_hook();
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open /dev/tty")?;
    crossterm::terminal::enable_raw_mode().context("enable raw mode")?;

    let mut viewport_y: u16 = 0;
    let result = (|| {
        let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let needed = (rows.len() as u16).saturating_add(2);
        let viewport_height = needed.max(VIEWPORT_MIN).min(term_rows.max(1));

        viewport_y = allocate_viewport(&mut tty, viewport_height, term_rows)?;
        tty.write_all(HIDE_CURSOR)?;
        tty.flush()?;

        run_loop(&mut tty, rows, viewport_y, viewport_height, term_cols)
    })();

    let _ = cleanup_at(&mut tty, viewport_y);
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

fn allocate_viewport(tty: &mut File, viewport_height: u16, term_rows: u16) -> Result<u16> {
    let (_, mut cursor_row) = crossterm::cursor::position().context("query cursor position")?;
    let bottom_excl = cursor_row + viewport_height;
    if bottom_excl > term_rows {
        let scroll = bottom_excl - term_rows;
        for _ in 0..scroll {
            tty.write_all(b"\n")?;
        }
        tty.flush()?;
        cursor_row = cursor_row.saturating_sub(scroll);
    }
    Ok(cursor_row)
}

fn cleanup_at(tty: &mut File, viewport_y: u16) -> std::io::Result<()> {
    write!(tty, "\x1b[{};1H", viewport_y + 1)?;
    tty.write_all(ERASE_TO_END)?;
    tty.write_all(SHOW_CURSOR)?;
    tty.flush()
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        if let Ok(mut tty) = OpenOptions::new().write(true).open("/dev/tty") {
            let _ = tty.write_all(SHOW_CURSOR);
            let _ = tty.flush();
        }
        original(info);
    }));
}

fn run_loop(
    tty: &mut File,
    rows: Vec<Row>,
    viewport_y: u16,
    viewport_height: u16,
    term_cols: u16,
) -> Result<Option<Vec<String>>> {
    let mut app = App::new(rows);
    app.last_height = (viewport_height as usize).saturating_sub(2); // input + hint
    loop {
        render(tty, &mut app, viewport_y, viewport_height, term_cols)?;
        if !event::poll(Duration::from_millis(250)).context("event poll failed")? {
            continue;
        }
        match event::read().context("event read failed")? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(action) = handle_key(&mut app, key) {
                    return Ok(action);
                }
            }
            Event::Resize(_, _) => {
                // Just redraw on next loop iteration; viewport stays the same.
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
    viewport_y: u16,
    viewport_height: u16,
    term_cols: u16,
) -> Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    buf.extend_from_slice(SYNC_BEGIN);
    let log_height = app.last_height;

    let preview_enabled = term_cols >= PREVIEW_MIN_COLS;
    let (left_w, right_x, right_w): (usize, u16, usize) = if preview_enabled {
        let lw = (term_cols / 2).saturating_sub(1) as usize;
        let rx = term_cols / 2 + 1;
        let rw = (term_cols as usize).saturating_sub(rx as usize);
        (lw, rx, rw)
    } else {
        (term_cols as usize, term_cols, 0)
    };

    // Pre-compute preview lines (cached) for the cursor row, before borrowing app immutably.
    // The first preview row is a dim header anchoring the pane to the cursor's change ID.
    let (preview_header, preview_lines): (Option<Vec<u8>>, Vec<Vec<u8>>) = if preview_enabled {
        if let Some(&row_idx) = app.filtered.get(app.cursor) {
            let cid = if app.rows[row_idx].change_id_prefix.is_empty() {
                app.rows[row_idx].change_id_short.clone()
            } else {
                app.rows[row_idx].change_id_prefix.clone()
            };
            let label = format!("── {cid} ");
            let dashes = right_w.saturating_sub(label.chars().count());
            let header = format!(
                "\x1b[2m{label}{}\x1b[0m",
                "─".repeat(dashes)
            );
            (Some(header.into_bytes()), split_lines(app.preview_for(row_idx)))
        } else {
            (None, Vec::new())
        }
    } else {
        (None, Vec::new())
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

        if let Some(&(filt_idx, row_idx)) = visible.get(i) {
            let row = &app.rows[row_idx];
            let is_cursor = filt_idx == app.cursor;
            let is_selected = app.selected.contains(&row_idx);
            render_log_row(&mut buf, row, is_cursor, is_selected, left_w);
        }

        if preview_enabled {
            write!(buf, "\x1b[{};{}H", row_y, right_x + 1)?;
            buf.extend_from_slice(ERASE_TO_EOL);
            if i == 0 {
                if let Some(h) = &preview_header {
                    buf.extend_from_slice(h);
                }
            } else {
                buf.extend_from_slice(SGR_DIM);
                buf.extend_from_slice("│ ".as_bytes());
                buf.extend_from_slice(SGR_RESET);
                if let Some(line) = preview_lines.get(i - 1) {
                    let clipped = truncate_ansi(line, right_w.saturating_sub(2));
                    buf.extend_from_slice(&clipped);
                    buf.extend_from_slice(SGR_RESET);
                }
            }
        }
    }

    // Hint row (left only).
    let hint_y = viewport_y + 1 + log_height as u16 + 1;
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
    // Two-tone hint: keys at normal weight, labels dim. Gives the eye an anchor
    // to scan by without removing any information.
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

    if preview_enabled {
        write!(buf, "\x1b[{};{}H", hint_y, right_x + 1)?;
        buf.extend_from_slice(ERASE_TO_EOL);
    }

    // Park cursor at end of input line.
    let input_col = 2 + app.filter.chars().count() as u16 + 1;
    write!(buf, "\x1b[{};{}H", viewport_y + 1, input_col + 1)?;

    let _ = viewport_height;
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

fn render_log_row(buf: &mut Vec<u8>, row: &Row, is_cursor: bool, is_selected: bool, width: usize) {
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
        let pad = content_w.saturating_sub(used);
        for _ in 0..pad {
            buf.push(b' ');
        }
        buf.extend_from_slice(SGR_RESET);
    } else {
        buf.extend_from_slice(&truncated);
        buf.extend_from_slice(SGR_RESET);
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
