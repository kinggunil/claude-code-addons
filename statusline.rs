// Claude Code status line — zero-dependency single-file Rust implementation.
//
// Part of https://github.com/kinggunil/claude-code-addons (installer + claude-notify).
// Install: macOS/Linux  curl -sSfL https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.sh | sh
//          Windows      irm https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.ps1 | iex
// Build:   rustc --edition 2021 -O statusline.rs -o "$HOME/.claude/statusline-rs"
// Test:    rustc --edition 2021 --test statusline.rs -o /tmp/sl-test && /tmp/sl-test
// ~/.claude/settings.json:
//   "statusLine": { "type": "command", "command": "$HOME/.claude/statusline-rs",
//                   "refreshInterval": 1 }
//   Windows: "command": "\"C:/Users/you/.claude/statusline-rs.exe\"" — Claude Code runs
//   it through Git Bash, which drops unquoted backslashes: C:\Users\you\... would become
//   C:Usersyou... ("command not found") and the status line would silently never show.
//
// Reads Claude Code's JSON on stdin and prints a two-line colored status bar:
//   line 1 (dim):  claude --resume <sid> | elapsed | cost | rate limits
//   line 2 (live): model | effort | think | dir | context gauge | CPU | RAM | VM | version
//
// Zero subprocess spawns: macOS stats via mach host_statistics/host_statistics64
//   + sysctlbyname FFI, Linux via /proc, Windows via kernel32.
//   Whole run is ~2-3 ms, so refreshInterval=1 is free.
// Single static binary, no interpreter startup, ~1-2 MB RSS.
// Compile-time type checks + built-in unit tests (see mod tests at the bottom).
//
// Glyphs: emoji + block characters by default. `--ascii` (or STATUSLINE_GLYPHS=ascii)
//   swaps every non-ASCII character for a plain one; the default `auto` does that
//   by itself where emoji widths are known to be mis-measured: a non-UTF-8 locale,
//   TERM=linux/dumb, the legacy Windows console, or a Linux box whose libc
//   wcwidth() tables predate these emoji while a local terminal or tmux/screen/mosh
//   on that box re-measures the line (a one-column disagreement after a glyph plus
//   cell-level redraws leaves stale digits behind every refresh).
// Every changing field is fixed-width ("  4%", "0h 42m", "136.8K", " 1d06h") so
//   the line never shifts as numbers grow, and a cell-diffing layer repaints only
//   the digits that changed.
// Width: Claude Code puts the terminal size in COLUMNS before running the script.
//   Each line is fitted to it (minus a 2-column margin for Claude Code's framing)
//   by shedding pieces in a fixed order — resume command, pace detail, cost,
//   version, elapsed, think, pace words, VM, effort, 5h countdown, gauge, 7d
//   countdown, RAM, dir, token count — so a narrow window never wraps the line.
//   `--width N` (or STATUSLINE_WIDTH) overrides; 0 disables fitting.
//
// CPU% is the average load since the previous refresh: cumulative kernel counters
// are cached in ~/.claude/.statusline-cpu.json. First run falls back to a 0.1 s
// inline sample. Counters reset on reboot are detected and discarded.
//
// Context tokens: the last message's usage in the transcript tail (input +
// cache_read + cache_creation == tokens actually sent == current context, always
// <= the window). The harness context_window.* fields are sometimes cumulative
// garbage on long sessions (e.g. "4457.1K/1M"), so they are only a
// sanity-checked fallback; displayed tokens and % come from the same number.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Status line source version — shown dim at the far right of line 2. Bump this
// on every push so each server/PC displays which revision it is running;
// comparing the on-screen version against the repo reveals stale machines.
// Format: YY.MM.DD for the first build of a day, then .2/.3/... appended for
// each further same-day revision (so a newer same-day build sorts after an
// older one). Date-based means a glance tells you how old a build is without
// looking anything up.
const VERSION: &str = "26.10.20.3";

// ---------------- ANSI palette ----------------
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const C_DIM: &str = "\x1b[38;5;240m"; // gray   (line 1: resume cmd, elapsed, cost, hint)
const C_MODEL: &str = "\x1b[38;5;44m"; // cyan
const C_EFFORT: &str = "\x1b[38;5;176m"; // violet (unknown effort levels)
const C_ON: &str = "\x1b[38;5;42m"; // green
const C_OFF: &str = "\x1b[38;5;244m"; // muted gray
const C_DIR: &str = "\x1b[38;5;75m"; // blue
const C_SEP: &str = "\x1b[38;5;238m"; // faint separator
const C_HOST: &str = "\x1b[38;5;44m"; // cyan — local host (cool complement to SSH orange)
const C_ERR: &str = "\x1b[38;5;196m"; // red

fn fg(n: u16) -> String {
    format!("\x1b[38;5;{}m", n)
}

fn color_for_pct(p: f64) -> String {
    fg(if p >= 90.0 {
        196 // red
    } else if p >= 75.0 {
        202 // red-orange
    } else if p >= 60.0 {
        214 // orange
    } else if p >= 45.0 {
        220 // yellow
    } else if p >= 30.0 {
        148 // yellow-green
    } else {
        42 // green
    })
}

fn color_for_effort(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        "low" => fg(42),
        "medium" => fg(220),
        "high" => fg(214),
        "xhigh" => fg(202),
        "max" => fg(196),
        _ => C_EFFORT.to_string(),
    }
}

fn ctx_emoji(p: f64) -> &'static str {
    if p >= 90.0 {
        "🚨"
    } else if p >= 75.0 {
        "🔥"
    } else if p >= 50.0 {
        "⚡"
    } else {
        "🟢"
    }
}

fn clip(v: f64) -> f64 {
    v.max(0.0).min(100.0)
}

// ---------------- glyph set: emoji vs plain ASCII ----------------

/// Every non-ASCII character the status line can print is chosen here, so one
/// switch replaces them all. The ASCII set exists because a terminal, or a
/// tmux/screen/mosh layer in front of it, that measures an emoji as 1 column
/// while it is drawn 2 wide shifts the rest of the line, and a cell-level redraw
/// then leaves stale digits behind on every refresh (old Ubuntu: glibc 2.27 has
/// Unicode 10 tables; 🟢 is Unicode 12). Colors are identical in both sets.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glyphs {
    ascii: bool,
}

impl Glyphs {
    const UNICODE: Glyphs = Glyphs { ascii: false };
    const ASCII: Glyphs = Glyphs { ascii: true };

    /// Within-sector separator, colored: " · " or ", ".
    fn dot(&self) -> String {
        if self.ascii {
            format!("{},{} ", C_SEP, RESET)
        } else {
            format!(" {}·{} ", C_SEP, RESET)
        }
    }

    /// Marks a reset countdown: "⏳3d 14h" / "~3d 14h".
    fn hourglass(&self) -> &'static str {
        if self.ascii {
            "~"
        } else {
            "⏳"
        }
    }

    /// Gauge cells (filled, empty) and the frame drawn around them.
    fn bar_cells(&self) -> (&'static str, &'static str) {
        if self.ascii {
            ("#", ".")
        } else {
            ("█", "░")
        }
    }

    fn bar_frame(&self) -> (&'static str, &'static str) {
        if self.ascii {
            ("[", "]")
        } else {
            ("", "")
        }
    }

    /// Context-state marker in front of the gauge, trailing space included. The
    /// gauge color already says ok/warm/hot, so ASCII only flags the alarm state.
    fn ctx_marker(&self, pct: f64, alarm: bool) -> String {
        if self.ascii {
            if alarm || pct >= 90.0 {
                "!! ".to_string()
            } else {
                String::new()
            }
        } else {
            format!("{} ", if alarm { "🚨" } else { ctx_emoji(pct) })
        }
    }

    fn effort(&self, level: &str) -> String {
        if self.ascii {
            format!("eff {}", level)
        } else {
            format!("⚡{}", level)
        }
    }

    /// Host segment: local 🏠 / "@", remote 🌐 / "ssh:".
    fn host(&self, name: &str, remote: bool) -> String {
        match (self.ascii, remote) {
            (true, true) => format!("ssh:{}", name),
            (true, false) => format!("@{}", name),
            (false, true) => format!("🌐 {}", name),
            (false, false) => format!("🏠 {}", name),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum GlyphMode {
    Auto,
    Unicode,
    Ascii,
}

/// `--ascii` / `--unicode` / `--glyphs=auto|ascii|unicode` on the command line,
/// else STATUSLINE_GLYPHS, else auto. Unknown arguments are ignored.
fn glyph_mode_from<I: IntoIterator<Item = String>>(args: I, env: Option<&str>) -> GlyphMode {
    fn parse_mode(s: &str) -> Option<GlyphMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ascii" | "plain" => Some(GlyphMode::Ascii),
            "unicode" | "emoji" => Some(GlyphMode::Unicode),
            "auto" => Some(GlyphMode::Auto),
            _ => None,
        }
    }
    let mut mode = env.and_then(parse_mode);
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let m = match a.as_str() {
            "--ascii" => Some(GlyphMode::Ascii),
            "--unicode" | "--emoji" => Some(GlyphMode::Unicode),
            "--glyphs" => args.next().as_deref().and_then(parse_mode),
            s => s.strip_prefix("--glyphs=").and_then(parse_mode),
        };
        if m.is_some() {
            mode = m;
        }
    }
    mode.unwrap_or(GlyphMode::Auto)
}

/// What `auto` looks at. Collected once per run; the pure `auto_ascii` decides.
#[derive(Clone, Debug)]
struct TermEnv {
    windows: bool,
    /// LC_ALL, else LC_CTYPE, else LANG (None when none is set)
    lang: Option<String>,
    term: String,
    /// tmux/screen (env or ancestry) or mosh-server: something on this box
    /// re-measures the line with this box's width tables
    in_mux: bool,
    remote: bool,
    /// Some(true) when this box's libc wcwidth() agrees with how terminals draw
    /// our glyphs; None where there is no libc to ask (Windows, macOS)
    libc_ok: Option<bool>,
    /// Windows: a terminal that renders emoji (Windows Terminal, VS Code, mintty, ...)
    win_modern: bool,
}

fn env_set(key: &str) -> bool {
    std::env::var_os(key).map(|v| !v.is_empty()).unwrap_or(false)
}

fn term_env(ancestors: &[String], host: Option<&str>) -> TermEnv {
    let lang = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    TermEnv {
        windows: cfg!(windows),
        lang,
        term: std::env::var("TERM").unwrap_or_default(),
        in_mux: in_mux(ancestors),
        remote: is_remote(ancestors, host),
        libc_ok: libc_widths_ok(),
        win_modern: ["WT_SESSION", "TERM_PROGRAM", "WEZTERM_PANE", "ALACRITTY_WINDOW_ID"]
            .iter()
            .any(|k| env_set(k)),
    }
}

/// The auto rule. Only this box can be inspected, so it is conservative: ASCII
/// when something here is known to mis-measure, the emoji set otherwise.
fn auto_ascii(e: &TermEnv) -> bool {
    match &e.lang {
        Some(l) => {
            if !l.to_ascii_lowercase().replace('-', "").contains("utf8") {
                return true; // C / POSIX / EUC-KR ...: can't even encode the glyphs
            }
        }
        // tmux/screen treat input as non-UTF-8 without a locale; Windows rarely sets one
        None if e.in_mux && !e.windows => return true,
        None => {}
    }
    if e.windows {
        return !e.win_modern; // the legacy console draws emoji badly
    }
    if matches!(e.term.as_str(), "dumb" | "linux" | "vt100" | "vt102" | "vt220") {
        return true; // console / no emoji fonts
    }
    // A multiplexer here, or a local terminal on this box, measures with this
    // box's libc tables; over plain SSH the far terminal decides and we can't see it.
    (e.in_mux || !e.remote) && e.libc_ok == Some(false)
}

fn resolve_glyphs(mode: GlyphMode, ancestors: &[String], host: Option<&str>) -> Glyphs {
    match mode {
        GlyphMode::Ascii => Glyphs::ASCII,
        GlyphMode::Unicode => Glyphs::UNICODE,
        GlyphMode::Auto => {
            if auto_ascii(&term_env(ancestors, host)) {
                Glyphs::ASCII
            } else {
                Glyphs::UNICODE
            }
        }
    }
}

/// Names of this process's ancestors ("bash", "node", "tmux: server",
/// "sshd-session", ...), nearest first, read from /proc. Empty elsewhere.
#[cfg(target_os = "linux")]
fn ancestor_comms() -> Vec<String> {
    let mut out = Vec::new();
    let mut pid = std::process::id();
    for _ in 0..64 {
        let stat = match fs::read_to_string(format!("/proc/{}/stat", pid)) {
            Ok(s) => s,
            Err(_) => break,
        };
        // "<pid> (<comm>) <state> <ppid> ..." — comm may itself contain spaces or ')'
        let (open, close) = match (stat.find('('), stat.rfind(')')) {
            (Some(a), Some(b)) if b > a => (a, b),
            _ => break,
        };
        out.push(stat[open + 1..close].to_string());
        let ppid: u32 = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        if ppid <= 1 {
            break;
        }
        pid = ppid;
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn ancestor_comms() -> Vec<String> {
    Vec::new()
}

fn in_mux(ancestors: &[String]) -> bool {
    env_set("TMUX")
        || env_set("STY")
        || ancestors.iter().any(|c| {
            c.starts_with("tmux") || c.eq_ignore_ascii_case("screen") || c.starts_with("mosh-server")
        })
}

/// SSH/mosh/SSM session, or a cloud VM nobody sits at (EC2 by DMI vendor or its
/// default ip-A-B-C-D hostname): the box is remote even when a multiplexer or a
/// service manager started the shell without the SSH_* variables.
fn is_remote(ancestors: &[String], host: Option<&str>) -> bool {
    if env_set("SSH_CONNECTION") || env_set("SSH_TTY") || env_set("SSH_CLIENT") {
        return true;
    }
    if ancestors.iter().any(|c| {
        c.starts_with("sshd") || c.starts_with("mosh-server") || c.starts_with("ssm-session")
    }) {
        return true;
    }
    host.map(is_ec2_hostname).unwrap_or(false) || dmi_says_ec2()
}

/// EC2's default hostname, "ip-172-31-16-160".
fn is_ec2_hostname(h: &str) -> bool {
    match h.strip_prefix("ip-") {
        Some(rest) => {
            let parts: Vec<&str> = rest.split('-').collect();
            parts.len() == 4
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    }
}

#[cfg(target_os = "linux")]
fn dmi_says_ec2() -> bool {
    fs::read_to_string("/sys/class/dmi/id/sys_vendor")
        .map(|v| v.contains("Amazon EC2"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn dmi_says_ec2() -> bool {
    false
}

/// Does this box's libc measure our glyphs the way modern terminals draw them
/// (emoji 2 columns, block/dot 1)? None when there is nothing to ask.
#[cfg(target_os = "linux")]
fn libc_widths_ok() -> Option<bool> {
    extern "C" {
        fn setlocale(category: i32, locale: *const u8) -> *mut u8;
        fn wcwidth(c: i32) -> i32;
    }
    const LC_CTYPE: i32 = 0;
    // the environment's locale, else any UTF-8 one: the width table is per glibc
    // version, not per locale, it only has to be a UTF-8 charmap
    let locales: [&[u8]; 3] = [b"\0", b"C.UTF-8\0", b"en_US.UTF-8\0"];
    let loaded = locales
        .iter()
        .any(|l| !unsafe { setlocale(LC_CTYPE, l.as_ptr()) }.is_null());
    if !loaded {
        return None;
    }
    let wide = [0x1F7E2, 0x23F3, 0x1F3E0, 0x1F310, 0x26A1, 0x1F525, 0x1F6A8]; // 🟢 ⏳ 🏠 🌐 ⚡ 🔥 🚨
    let narrow = [0x2588, 0x2591, 0x00B7]; // █ ░ ·
    Some(
        wide.iter().all(|&c| unsafe { wcwidth(c) } == 2)
            && narrow.iter().all(|&c| unsafe { wcwidth(c) } == 1),
    )
}

#[cfg(not(target_os = "linux"))]
fn libc_widths_ok() -> Option<bool> {
    None
}

// ---------------- width-aware layout ----------------

// Drop ranks: on a terminal narrower than the line, pieces are shed in this order
// (lowest first) until it fits. The live budget numbers survive longest.
const R_RESUME: u8 = 5;
const R_PACE_TIME: u8 = 10;
const R_COST: u8 = 15;
const R_VERSION: u8 = 20;
const R_ELAPSED: u8 = 25;
const R_THINK: u8 = 30;
const R_PACE_WORDS: u8 = 35;
const R_VM: u8 = 36;
const R_EFFORT: u8 = 40;
const R_RESET_5H: u8 = 45;
const R_GAUGE: u8 = 50;
const R_RESET_7D: u8 = 55;
const R_RAM: u8 = 60;
const R_DIR: u8 = 65;
const R_TOKENS: u8 = 70;
const R_HOST: u8 = 75;
const R_CPU: u8 = 80;
const R_USED_7D: u8 = 85;
const R_USED_5H: u8 = 90;

/// One piece of a sector with progressively shorter renderings: `cands[0]` is the
/// full text and moving on to `cands[i + 1]` has priority `ranks[i]` (lower goes
/// first). An empty candidate removes the piece, separator included.
struct Item {
    cands: Vec<String>,
    ranks: Vec<u8>,
    idx: usize,
}

impl Item {
    /// Never shortened or dropped.
    fn fixed(text: String) -> Item {
        Item {
            cands: vec![text],
            ranks: Vec::new(),
            idx: 0,
        }
    }
    /// Dropped whole at `rank`.
    fn optional(text: String, rank: u8) -> Item {
        Item {
            cands: vec![text, String::new()],
            ranks: vec![rank],
            idx: 0,
        }
    }
    /// Shortened step by step: `cands` from longest to shortest, one rank per step.
    fn steps(cands: Vec<String>, ranks: Vec<u8>) -> Item {
        debug_assert_eq!(cands.len(), ranks.len() + 1);
        Item {
            cands,
            ranks,
            idx: 0,
        }
    }
    fn text(&self) -> &str {
        &self.cands[self.idx]
    }
    fn next_rank(&self) -> Option<u8> {
        self.ranks.get(self.idx).copied()
    }
}

/// A group of pieces with its own joiner (" · " within a sector, " " in the
/// context and 5h sectors). Sectors are joined by " | " and vanish when empty.
struct Sector {
    items: Vec<Item>,
    joiner: String,
}

impl Sector {
    fn new(joiner: &str) -> Sector {
        Sector {
            items: Vec::new(),
            joiner: joiner.to_string(),
        }
    }
    fn one(item: Item) -> Sector {
        Sector {
            items: vec![item],
            joiner: String::new(),
        }
    }
    fn push(&mut self, item: Item) {
        self.items.push(item);
    }
    fn render(&self) -> String {
        let parts: Vec<&str> = self
            .items
            .iter()
            .map(Item::text)
            .filter(|t| !t.is_empty())
            .collect();
        parts.join(&self.joiner)
    }
}

fn render_line(sectors: &[Sector], sep: &str) -> String {
    let parts: Vec<String> = sectors
        .iter()
        .map(Sector::render)
        .filter(|s| !s.is_empty())
        .collect();
    parts.join(sep)
}

/// Shed the lowest-ranked pieces until the line fits `width` columns (None =
/// unlimited). When nothing is left to shed, the shortest form is returned as is.
fn fit_line(mut sectors: Vec<Sector>, width: Option<usize>, sep: &str) -> String {
    loop {
        let line = render_line(&sectors, sep);
        let w = match width {
            Some(w) => w,
            None => return line,
        };
        if display_width(&line) <= w {
            return line;
        }
        let mut best: Option<(u8, usize, usize)> = None;
        for (si, s) in sectors.iter().enumerate() {
            for (ii, it) in s.items.iter().enumerate() {
                if let Some(r) = it.next_rank() {
                    if best.map_or(true, |b| r < b.0) {
                        best = Some((r, si, ii));
                    }
                }
            }
        }
        match best {
            Some((_, si, ii)) => sectors[si].items[ii].idx += 1,
            None => return line,
        }
    }
}

/// Columns a terminal uses for `s`: ANSI escape sequences take none, East Asian
/// wide characters and emoji take two, everything else one. This mirrors the
/// tables modern terminals draw with (the ASCII mode exists for the ones that don't).
fn display_width(s: &str) -> usize {
    let mut w = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ params... final byte 0x40-0x7E (SGR colors end in 'm')
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        w += char_cols(c);
    }
    w
}

fn char_cols(c: char) -> usize {
    let u = c as u32;
    if u < 0x20 || (0x7F..0xA0).contains(&u) {
        return 0;
    }
    if matches!(u, 0x0300..=0x036F | 0x200B..=0x200F | 0x20D0..=0x20FF | 0xFE00..=0xFE0F) {
        return 0; // combining marks, zero-width, variation selectors
    }
    let wide = matches!(
        u,
        0x1100..=0x115F
            | 0x231A..=0x231B
            | 0x2329..=0x232A
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F004
            | 0x1F0CF
            | 0x1F18E
            | 0x1F191..=0x1F19A
            | 0x1F200..=0x1F251
            | 0x1F300..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F7E0..=0x1F7EB
            | 0x1F90C..=0x1F9FF
            | 0x1FA70..=0x1FAFF
            | 0x20000..=0x3FFFD
    );
    if wide {
        2
    } else {
        1
    }
}

/// Columns each line may use: `--width N` (0 = unlimited), else STATUSLINE_WIDTH,
/// else the COLUMNS Claude Code sets before running the script, less a 2-column
/// margin for its own framing. None when nothing is known: no fitting.
fn target_width<I: IntoIterator<Item = String>>(
    args: I,
    env_width: Option<&str>,
    columns: Option<&str>,
) -> Option<usize> {
    fn num(s: &str) -> Option<usize> {
        s.trim().parse::<usize>().ok()
    }
    let mut explicit: Option<usize> = None;
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let v = match a.as_str() {
            "--width" => args.next().as_deref().and_then(num),
            s => s.strip_prefix("--width=").and_then(num),
        };
        if v.is_some() {
            explicit = v;
        }
    }
    match explicit.or_else(|| env_width.and_then(num)) {
        Some(0) => None,
        Some(w) => Some(w),
        None => columns.and_then(num).map(|c| c.saturating_sub(2)),
    }
}

// ---------------- minimal JSON ----------------
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        if let Json::Obj(m) = self {
            m.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    }
    fn path(&self, keys: &[&str]) -> Option<&Json> {
        let mut cur = self;
        for k in keys {
            cur = cur.get(k)?;
        }
        Some(cur)
    }
    fn as_str(&self) -> Option<&str> {
        if let Json::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    fn as_f64(&self) -> Option<f64> {
        if let Json::Num(n) = self {
            Some(*n)
        } else {
            None
        }
    }
    fn as_bool(&self) -> Option<bool> {
        if let Json::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

fn parse(s: &str) -> Result<Json, ()> {
    let mut p = P {
        b: s.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(()); // trailing garbage
    }
    Ok(v)
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Result<Json, ()> {
        match self.peek() {
            Some(b'{') => self.obj(),
            Some(b'[') => self.arr(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit(b"true", Json::Bool(true)),
            Some(b'f') => self.lit(b"false", Json::Bool(false)),
            Some(b'n') => self.lit(b"null", Json::Null),
            Some(_) => self.num(),
            None => Err(()),
        }
    }
    fn lit(&mut self, w: &[u8], v: Json) -> Result<Json, ()> {
        if self.b.len() - self.i >= w.len() && &self.b[self.i..self.i + w.len()] == w {
            self.i += w.len();
            Ok(v)
        } else {
            Err(())
        }
    }
    fn obj(&mut self) -> Result<Json, ()> {
        self.i += 1; // '{'
        let mut m = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(m));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(());
            }
            let k = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(());
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            m.push((k, v));
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(m));
                }
                _ => return Err(()),
            }
        }
    }
    fn arr(&mut self) -> Result<Json, ()> {
        self.i += 1; // '['
        let mut a = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(a));
        }
        loop {
            self.ws();
            a.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(a));
                }
                _ => return Err(()),
            }
        }
    }
    fn string(&mut self) -> Result<String, ()> {
        self.i += 1; // opening quote
        let mut s = String::new();
        let mut start = self.i;
        loop {
            let c = self.peek().ok_or(())?;
            if c == b'"' {
                s.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|_| ())?);
                self.i += 1;
                return Ok(s);
            } else if c == b'\\' {
                s.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|_| ())?);
                self.i += 1;
                let e = self.peek().ok_or(())?;
                self.i += 1;
                match e {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'b' => s.push('\u{8}'),
                    b'f' => s.push('\u{c}'),
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    b'u' => {
                        let u1 = self.hex4()?;
                        let cp = if (0xD800..0xDC00).contains(&u1) {
                            // high surrogate: must be followed by \uDC00-\uDFFF
                            if self.peek() == Some(b'\\') && self.b.get(self.i + 1) == Some(&b'u')
                            {
                                self.i += 2;
                                let u2 = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&u2) {
                                    return Err(());
                                }
                                0x10000 + ((u1 - 0xD800) << 10) + (u2 - 0xDC00)
                            } else {
                                return Err(());
                            }
                        } else if (0xDC00..0xE000).contains(&u1) {
                            return Err(()); // lone low surrogate
                        } else {
                            u1
                        };
                        s.push(char::from_u32(cp).ok_or(())?);
                    }
                    _ => return Err(()),
                }
                start = self.i;
            } else {
                self.i += 1; // raw byte (multi-byte UTF-8 passes through untouched)
            }
        }
    }
    fn hex4(&mut self) -> Result<u32, ()> {
        let mut v = 0u32;
        for _ in 0..4 {
            let c = self.peek().ok_or(())?;
            self.i += 1;
            v = v * 16
                + match c {
                    b'0'..=b'9' => (c - b'0') as u32,
                    b'a'..=b'f' => (c - b'a' + 10) as u32,
                    b'A'..=b'F' => (c - b'A' + 10) as u32,
                    _ => return Err(()),
                };
        }
        Ok(v)
    }
    fn num(&mut self) -> Result<Json, ()> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        if self.i == start {
            return Err(());
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()
            .and_then(|t| t.parse::<f64>().ok())
            .map(Json::Num)
            .ok_or(())
    }
}

// ---------------- system stats: CPU busy%, RAM used%, swap used% ----------------

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Machine hostname, short form (domain stripped, e.g. "ip-10-0-1-23.ec2.internal"
/// -> "ip-10-0-1-23"). gethostname(2) on unix, COMPUTERNAME on Windows — both
/// zero-cost, no subprocess. Runs wherever the status line runs, so under SSH it
/// reports the *remote* box you're working on.
#[cfg(unix)]
fn hostname() -> Option<String> {
    extern "C" {
        fn gethostname(name: *mut u8, len: usize) -> i32;
    }
    let mut buf = [0u8; 256];
    if unsafe { gethostname(buf.as_mut_ptr(), buf.len()) } != 0 {
        return std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty());
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let full = String::from_utf8_lossy(&buf[..end]).into_owned();
    full.split('.')
        .next()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(windows)]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty())
}

#[cfg(not(any(unix, windows)))]
fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty())
}

// -- cumulative (busy, total) CPU time since boot, per OS --

#[cfg(target_os = "macos")]
mod plat {
    use super::clip;
    use std::ffi::c_void;

    extern "C" {
        fn mach_host_self() -> u32;
        fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
        fn host_statistics64(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
        fn host_page_size(host: u32, out: *mut usize) -> i32;
        fn sysctlbyname(
            name: *const u8,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize,
        ) -> i32;
    }

    const HOST_CPU_LOAD_INFO: i32 = 3;
    const HOST_VM_INFO64: i32 = 4;

    /// <mach/vm_statistics.h> vm_statistics64 — 152 bytes, count 38
    #[repr(C)]
    #[allow(dead_code)]
    struct VmStats64 {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }

    /// <sys/sysctl.h> xsw_usage — field order is total, AVAIL, used
    #[repr(C)]
    #[allow(dead_code)]
    struct XswUsage {
        total: u64,
        avail: u64,
        used: u64,
        pagesize: u32,
        encrypted: u32, // boolean_t
    }

    pub fn cpu_counters() -> Option<(u64, u64)> {
        unsafe {
            let mut ticks = [0u32; 4]; // user, system, idle, nice
            let mut count = 4u32;
            if host_statistics(
                mach_host_self(),
                HOST_CPU_LOAD_INFO,
                ticks.as_mut_ptr() as *mut i32,
                &mut count,
            ) != 0
            {
                return None;
            }
            let (u, s, i, n) = (
                ticks[0] as u64,
                ticks[1] as u64,
                ticks[2] as u64,
                ticks[3] as u64,
            );
            Some((u + s + n, u + s + n + i))
        }
    }

    pub fn mem() -> (Option<f64>, Option<f64>) {
        let mut ram = None;
        let mut swap = None;
        unsafe {
            let host = mach_host_self();
            // RAM: (active + wired + compressed) / total — Activity-Monitor-style "used"
            let mut page: usize = 0;
            if host_page_size(host, &mut page) != 0 || page == 0 {
                page = 4096;
            }
            let mut vs: VmStats64 = std::mem::zeroed();
            let mut count = (std::mem::size_of::<VmStats64>() / 4) as u32;
            if host_statistics64(
                host,
                HOST_VM_INFO64,
                &mut vs as *mut _ as *mut i32,
                &mut count,
            ) == 0
            {
                let used = (vs.active_count as u64
                    + vs.wire_count as u64
                    + vs.compressor_page_count as u64)
                    * page as u64;
                let mut total: u64 = 0;
                let mut len = std::mem::size_of::<u64>();
                if sysctlbyname(
                    b"hw.memsize\0".as_ptr(),
                    &mut total as *mut _ as *mut c_void,
                    &mut len,
                    std::ptr::null(),
                    0,
                ) == 0
                    && total > 0
                {
                    ram = Some(clip(used as f64 / total as f64 * 100.0));
                }
            }
            // swap ("VM"): macOS creates/deflates the swap file on demand, so
            // total == 0 is a common, legitimate "nothing swapped" state, not
            // a read failure — report 0% instead of hiding the segment.
            let mut xs: XswUsage = std::mem::zeroed();
            let mut len = std::mem::size_of::<XswUsage>();
            if sysctlbyname(
                b"vm.swapusage\0".as_ptr(),
                &mut xs as *mut _ as *mut c_void,
                &mut len,
                std::ptr::null(),
                0,
            ) == 0
            {
                swap = Some(if xs.total > 0 {
                    clip(xs.used as f64 / xs.total as f64 * 100.0)
                } else {
                    0.0
                });
            }
        }
        (ram, swap)
    }
}

#[cfg(target_os = "linux")]
mod plat {
    use super::clip;
    use std::fs;

    pub fn cpu_counters() -> Option<(u64, u64)> {
        let s = fs::read_to_string("/proc/stat").ok()?;
        let v: Vec<u64> = s
            .lines()
            .next()?
            .split_whitespace()
            .skip(1)
            .filter_map(|t| t.parse().ok())
            .collect();
        if v.len() < 4 {
            return None;
        }
        let idle = v[3] + v.get(4).copied().unwrap_or(0); // idle + iowait
        // sum only the 8 standard fields (user..steal); guest/guest_nice (fields
        // 9/10, kernel >= 2.6.24) are already counted inside user/nice, so
        // summing all fields would double-count them and understate CPU% on
        // hosts running VMs.
        let total: u64 = v.iter().take(8).sum();
        Some((total - idle, total))
    }

    pub fn mem() -> (Option<f64>, Option<f64>) {
        let s = match fs::read_to_string("/proc/meminfo") {
            Ok(s) => s,
            Err(_) => return (None, None),
        };
        let (mut total, mut free, mut buffers, mut cached, mut st, mut sf) =
            (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
        let mut avail: Option<f64> = None;
        for line in s.lines() {
            let mut it = line.splitn(2, ':');
            let k = it.next().unwrap_or("");
            let v: f64 = it
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0); // kB
            match k {
                "MemTotal" => total = v,
                "MemAvailable" => avail = Some(v),
                "MemFree" => free = v,
                "Buffers" => buffers = v,
                "Cached" => cached = v,
                "SwapTotal" => st = v,
                "SwapFree" => sf = v,
                _ => {}
            }
        }
        let avail = avail.unwrap_or(free + buffers + cached);
        let ram = if total > 0.0 {
            Some(clip((total - avail) / total * 100.0))
        } else {
            None
        };
        // SwapTotal == 0 means no swap configured — legitimately 0% used, not unknown.
        let swap = Some(if st > 0.0 {
            clip((st - sf) / st * 100.0)
        } else {
            0.0
        });
        (ram, swap)
    }
}

#[cfg(target_os = "windows")]
mod plat {
    use super::clip;

    #[repr(C)]
    struct Filetime {
        lo: u32,
        hi: u32,
    }
    #[repr(C)]
    #[allow(dead_code)]
    struct MemStatusEx {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virt: u64,
        avail_virt: u64,
        avail_ext: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemTimes(
            idle: *mut Filetime,
            kernel: *mut Filetime,
            user: *mut Filetime,
        ) -> i32;
        fn GlobalMemoryStatusEx(m: *mut MemStatusEx) -> i32;
    }

    fn ft(f: &Filetime) -> u64 {
        ((f.hi as u64) << 32) | f.lo as u64
    }

    pub fn cpu_counters() -> Option<(u64, u64)> {
        unsafe {
            let (mut i, mut k, mut u) = (
                Filetime { lo: 0, hi: 0 },
                Filetime { lo: 0, hi: 0 },
                Filetime { lo: 0, hi: 0 },
            );
            if GetSystemTimes(&mut i, &mut k, &mut u) == 0 {
                return None;
            }
            let (i, k, u) = (ft(&i), ft(&k), ft(&u));
            // kernel time already includes idle, so k+u >= i in principle;
            // saturate defensively since this path is untested in practice.
            Some(((k + u).saturating_sub(i), k + u))
        }
    }

    pub fn mem() -> (Option<f64>, Option<f64>) {
        unsafe {
            let mut m: MemStatusEx = std::mem::zeroed();
            m.length = std::mem::size_of::<MemStatusEx>() as u32;
            if GlobalMemoryStatusEx(&mut m) == 0 {
                return (None, None);
            }
            let ram = Some(clip(m.load as f64));
            // total_page == 0 means no page file configured — legitimately 0%, not unknown.
            let swap = Some(if m.total_page > 0 {
                clip((m.total_page - m.avail_page) as f64 / m.total_page as f64 * 100.0)
            } else {
                0.0
            });
            (ram, swap)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod plat {
    pub fn cpu_counters() -> Option<(u64, u64)> {
        None
    }
    pub fn mem() -> (Option<f64>, Option<f64>) {
        (None, None)
    }
}

/// Delta busy% between two (busy, total) tick snapshots, or None if the
/// tick counter hasn't advanced yet (see `cpu_percent` for why that happens).
fn delta_pct(p: (u64, u64), c: (u64, u64)) -> Option<f64> {
    let db = c.0.saturating_sub(p.0) as f64;
    let dt = c.1.saturating_sub(p.1) as f64;
    if dt > 0.0 {
        Some(clip(db / dt * 100.0))
    } else {
        None
    }
}

/// Busy% averaged since the previous run, via counters cached in the state file.
///
/// Always returns Some — never blanks the CPU segment. On some platforms
/// (observed on Apple Silicon) the underlying tick counter only advances at
/// roughly 1 Hz, coarser than this binary's own refresh cadence, so two
/// samples taken well under a second apart routinely read back identical
/// counters (a zero delta, not an error). Rather than surface that as
/// "unknown" and flicker the segment away, fall back to the last computed
/// percentage (cached alongside the counters), and only default to 0% if
/// there is truly no prior data (first run ever).
fn cpu_percent() -> Option<f64> {
    let state_path = home().join(".claude").join(".statusline-cpu.json");
    let cached = fs::read_to_string(&state_path)
        .ok()
        .and_then(|t| parse(t.trim()).ok());
    let last_pct = cached.as_ref().and_then(|st| st.get("pct")).and_then(Json::as_f64);

    let cur = match plat::cpu_counters() {
        Some(c) => c,
        None => return Some(last_pct.unwrap_or(0.0)),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    let mut prev: Option<(u64, u64)> = None;
    if let Some(st) = &cached {
        let t = st.get("t").and_then(Json::as_f64).unwrap_or(0.0);
        let busy = st.get("busy").and_then(Json::as_f64).unwrap_or(-1.0);
        let total = st.get("total").and_then(Json::as_f64).unwrap_or(-1.0);
        let dt = now - t;
        // reject too-close (parallel sessions), stale, or pre-reboot samples
        // (cumulative counters reset on boot, so total must not decrease)
        if busy >= 0.0 && total >= 0.0 && (0.1..=600.0).contains(&dt) && total as u64 <= cur.1 {
            prev = Some((busy as u64, total as u64));
        }
    }

    let pct = prev
        .and_then(|p| delta_pct(p, cur))
        .or_else(|| {
            std::thread::sleep(Duration::from_millis(100));
            plat::cpu_counters().and_then(|c2| delta_pct(cur, c2))
        })
        .or(last_pct)
        .unwrap_or(0.0);

    // save current sample + the pct we're reporting, for next run's fallback
    // (atomic replace; tmp name includes our pid so concurrent Claude Code
    // sessions/tabs writing at the same instant don't tear each other's write)
    let tmp = PathBuf::from(format!("{}.{}.tmp", state_path.display(), std::process::id()));
    if fs::write(
        &tmp,
        format!(
            "{{\"t\": {:.6}, \"busy\": {}, \"total\": {}, \"pct\": {:.4}}}",
            now, cur.0, cur.1, pct
        ),
    )
    .is_ok()
    {
        let _ = fs::rename(&tmp, &state_path);
    }

    Some(pct)
}

// ---------------- formatting ----------------

/// Fixed 6 characters, right-aligned: "   999", " 12.3K", "136.8K", " 1.02M".
fn fmt_tokens(n: u64) -> String {
    let s = if n >= 999_950 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    };
    format!("{:>6}", s)
}

/// "4h 31m", "0h 05m": minutes zero-padded, so the field is 6 characters until
/// the hours reach two digits.
fn fmt_duration(ms: f64) -> Option<String> {
    if ms <= 0.0 {
        return None;
    }
    let s = (ms / 1000.0) as u64;
    Some(format!("{}h {:02}m", s / 3600, (s % 3600) / 60))
}

/// Percentages right-aligned to a fixed 3-char field ("  4", " 14", "100") so a
/// value crossing a digit boundary doesn't shift everything after it on the line.
fn fmt_pct(v: f64) -> String {
    format!("{:>3.0}", v)
}

/// e.g. "5h  23% ⏳4h 12m" — reset countdown omitted when resets_at/now is
/// unavailable or already in the past (clock skew / stale sample). `pct` is
/// clipped to 0-100 defensively since it comes straight from the harness JSON,
/// not a locally-computed value. On a narrow terminal the countdown is shed
/// long before the percentage.
fn rate_limit_sector(
    label: &str,
    pct: f64,
    resets_at: Option<f64>,
    now: Option<f64>,
    g: &Glyphs,
) -> Sector {
    let pct = clip(pct);
    let mut s = Sector::new(" ");
    s.push(Item::optional(
        format!("{}{} {}{}%{}", C_DIM, label, color_for_pct(pct), fmt_pct(pct), RESET),
        R_USED_5H,
    ));
    if let (Some(r), Some(n)) = (resets_at, now) {
        if r > n {
            if let Some(rem) = fmt_duration((r - n) * 1000.0) {
                // ⏳ marks the reset countdown, matching the 7d segment
                s.push(Item::optional(
                    format!("{}{}{}{}", C_OFF, g.hourglass(), rem, RESET),
                    R_RESET_5H,
                ));
            }
        }
    }
    s
}

#[cfg(test)]
fn rate_limit_segment(
    label: &str,
    pct: f64,
    resets_at: Option<f64>,
    now: Option<f64>,
    g: &Glyphs,
) -> String {
    rate_limit_sector(label, pct, resets_at, now, g).render()
}

// ---- weekly usage pace (Claude Max weekly quota) ----
// The weekly quota resets every Saturday 04:00 KST (= Friday 19:00 UTC). "wk N%"
// is how far through that week we are — a linear baseline: at 10% of the week
// elapsed, ~10% used is "on pace"; using less leaves comfortable headroom, more
// means throttle. When the harness reports the real seven_day window we derive
// the position from its resets_at (authoritative, and keeps the number in step
// with the countdown); otherwise we fall back to a fixed KST anchor so the
// segment still renders without any rate-limit data.
const WEEK_SECS: f64 = 604_800.0; // 7 * 24 * 3600
const WEEK_ANCHOR: f64 = 1_704_481_200.0; // Sat 2024-01-06 04:00 KST, unix seconds

/// Seconds elapsed in the current weekly window. Prefers the harness-reported
/// next-reset instant (the window is exactly one week, so elapsed = week -
/// remaining); falls back to the fixed Saturday-04:00-KST grid when resets_at is
/// absent or out of range (already past / >1 week away from clock skew).
fn week_elapsed_secs(resets_at: Option<f64>, now: f64) -> f64 {
    week_elapsed_secs_at(resets_at, now, week_anchor())
}

/// Same, against an explicit fallback grid: any instant at which the week resets.
fn week_elapsed_secs_at(resets_at: Option<f64>, now: f64, anchor: f64) -> f64 {
    match resets_at {
        Some(r) if r > now && r - now <= WEEK_SECS => WEEK_SECS - (r - now),
        _ => (now - anchor).rem_euclid(WEEK_SECS),
    }
}

/// The fallback reset grid: STATUSLINE_WEEK_ANCHOR (unix seconds of any reset
/// instant of your plan) when set and sane, else the built-in Saturday 04:00 KST.
/// Only consulted when the harness sends no 7-day resets_at (API-key sessions).
fn week_anchor() -> f64 {
    std::env::var("STATUSLINE_WEEK_ANCHOR")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|a| a.is_finite() && *a > 0.0)
        .unwrap_or(WEEK_ANCHOR)
}

/// "3d 14h", "0d 05h" — fixed 6-character span for the weekly reset countdown
/// (hour resolution: it counts down a week).
fn fmt_span(secs: f64) -> Option<String> {
    if secs <= 0.0 {
        return None;
    }
    let s = secs as u64;
    Some(format!("{}d {:02}h", s / 86_400, (s % 86_400) / 3600))
}

/// Compact span for inline annotations, fixed 6 characters: " 8h24m", " 2d16h".
/// No inner space, so the value doesn't visually split from the token before it.
fn fmt_span_compact(secs: f64) -> Option<String> {
    if secs <= 0.0 {
        return None;
    }
    let s = secs as u64;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    Some(if d > 0 {
        format!("{:>2}d{:02}h", d, h)
    } else {
        format!("{:>2}h{:02}m", h, m)
    })
}

/// Color for the pace delta = used - elapsed (percentage points). Uses the rounded
/// delta for the under/on boundary so the color always agrees with the words.
/// Under pace (headroom) steps warm→cool as the cushion grows — yellow-green →
/// green → cyan → blue — so bluer == more room to spare. On-pace is yellow, and
/// over pace burns ahead of the clock through yellow→orange→red — 여유에 따라 색깔 다르게.
fn pace_color(delta: f64) -> String {
    fg(if delta.round() < 0.0 {
        // under pace: stepped by how much headroom (-delta), warm → cool
        let headroom = -delta;
        if headroom < 5.0 {
            148 // yellow-green — 연두: only a little ahead
        } else if headroom < 15.0 {
            42 // green — 초록: comfortably ahead
        } else if headroom < 30.0 {
            45 // cyan — 하늘색: lots of room
        } else {
            33 // blue — 파랑: way ahead, tons of headroom
        }
    } else if delta < 5.0 {
        220 // yellow — on pace
    } else if delta < 15.0 {
        214 // orange
    } else if delta < 30.0 {
        202 // red-orange
    } else {
        196 // red — throttle
    })
}

/// Time-equivalent of a pace delta (percentage points). Usage maps linearly to
/// the week, so 1pp == WEEK_SECS/100 of schedule time: at +5pp you've consumed
/// what an on-pace user reaches 5% of a week — ~8h24m — later, i.e. you're that
/// far *ahead* of the clock (over budget); a negative delta is that much slack.
/// Computed from the rounded delta so the shown "+5%" and its time always agree.
fn pace_time(delta: f64) -> Option<String> {
    let r = delta.round();
    if r == 0.0 {
        return None; // on pace to the nearest %: no meaningful time offset
    }
    fmt_span_compact(r.abs() / 100.0 * WEEK_SECS)
}

/// Line-1 weekly segment, written in plain language so it reads without a legend.
/// With usage: "N% used", then the pace as words ("N% over pace" burning fast /
/// "N% under pace" headroom / "on pace"), annotated with that same gap re-expressed
/// as time in parens, colored by headroom (used - elapsed); then the reset countdown
/// behind an ⏳. Without rate-limit data, just the week time-progress.
///   with usage: "7d  39% used · 18% over pace ( 1d06h) · ⏳5d 12h"
///   time only:  "wk  21% · ⏳5d 12h"
/// Every number is fixed-width so the segment only changes length when the pace
/// flips between over/under/on.
fn weekly_sector(used: Option<f64>, resets_at: Option<f64>, now: f64, g: &Glyphs) -> Sector {
    let elapsed = week_elapsed_secs(resets_at, now);
    let elapsed_pct = clip(elapsed / WEEK_SECS * 100.0);
    let countdown = fmt_span(WEEK_SECS - elapsed);

    let mut s = Sector::new(&g.dot());
    if let Some(u) = used {
        let u = clip(u);
        let delta = u - elapsed_pct;
        let col = pace_color(delta);
        let d = delta.round() as i64;
        s.push(Item::optional(
            format!("{}7d {}% used{}", C_DIM, fmt_pct(u), RESET),
            R_USED_7D,
        ));
        let words = if d > 0 {
            format!("{:>2}% over pace", d)
        } else if d < 0 {
            format!("{:>2}% under pace", -d)
        } else {
            "on pace".to_string()
        };
        // the same gap re-expressed as time, in parens right after the words; on a
        // narrow terminal the parens go first and the words much later
        let short = format!("{}{}{}", col, words, RESET);
        s.push(match pace_time(delta) {
            Some(t) => Item::steps(
                vec![format!("{}{} ({}){}", col, words, t, RESET), short, String::new()],
                vec![R_PACE_TIME, R_PACE_WORDS],
            ),
            None => Item::optional(short, R_PACE_WORDS),
        });
    } else {
        s.push(Item::optional(
            format!("{}wk {}%{}", C_OFF, fmt_pct(elapsed_pct), RESET),
            R_USED_7D,
        ));
    }
    if let Some(c) = countdown {
        s.push(Item::optional(
            format!("{}{}{}{}", C_OFF, g.hourglass(), c, RESET),
            R_RESET_7D,
        ));
    }
    s
}

#[cfg(test)]
fn weekly_segment(used: Option<f64>, resets_at: Option<f64>, now: f64, g: &Glyphs) -> String {
    weekly_sector(used, resets_at, now, g).render()
}

fn bar(pct: f64, width: usize, g: &Glyphs) -> String {
    let filled = ((pct / 100.0 * width as f64).round() as i64).clamp(0, width as i64) as usize;
    let (full, empty) = g.bar_cells();
    let (open, close) = g.bar_frame();
    format!(
        "{}{}{}{}{}{}{}{}{}",
        C_SEP,
        open,
        color_for_pct(pct),
        full.repeat(filled),
        C_OFF,
        empty.repeat(width - filled),
        C_SEP,
        close,
        RESET
    )
}

// ---------------- main ----------------

fn main() {
    let ancestors = ancestor_comms();
    let host = hostname();
    let g = resolve_glyphs(
        glyph_mode_from(
            std::env::args().skip(1),
            std::env::var("STATUSLINE_GLYPHS").ok().as_deref(),
        ),
        &ancestors,
        host.as_deref(),
    );
    let width = target_width(
        std::env::args().skip(1),
        std::env::var("STATUSLINE_WIDTH").ok().as_deref(),
        std::env::var("COLUMNS").ok().as_deref(),
    );

    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    let d = match parse(input.trim()) {
        Ok(v) => v,
        Err(_) => {
            print!("{}statusline parse error{}", C_ERR, RESET);
            return;
        }
    };

    let sid = d.get("session_id").and_then(Json::as_str).unwrap_or("");
    let model = d
        .path(&["model", "display_name"])
        .and_then(Json::as_str)
        .unwrap_or("");
    let cwd = d
        .get("cwd")
        .and_then(Json::as_str)
        .map(String::from)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_default();
    let base = Path::new(&cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let transcript = d
        .get("transcript_path")
        .and_then(Json::as_str)
        .unwrap_or("");

    // ---- context window ----
    let cw = d.get("context_window");
    let cw_size = cw
        .and_then(|c| c.get("context_window_size"))
        .and_then(Json::as_f64);
    let max_ctx: u64 = match cw_size {
        Some(s) if s > 0.0 => s as u64,
        _ => {
            if model.contains("1M") {
                1_000_000
            } else {
                200_000
            }
        }
    };
    let max_label = if max_ctx >= 1_000_000 {
        format!("{}M", max_ctx / 1_000_000)
    } else if max_ctx >= 1000 {
        format!("{}K", max_ctx / 1000)
    } else {
        max_ctx.to_string()
    };

    // primary: harness-provided token count — no file I/O, always current. Accepted
    // only when it fits the window (this field can occasionally report a bogus,
    // out-of-range value, which is why it stays guarded). The percent is still
    // derived from this raw count below, so decimal precision is preserved (we do
    // NOT use the pre-rounded integer `used_percentage`).
    let mut ctx_tokens: u64 = 0;
    if let Some(v) = cw
        .and_then(|c| c.get("total_input_tokens"))
        .and_then(Json::as_f64)
    {
        if v > 0.0 && (v as u64) <= max_ctx {
            ctx_tokens = v as u64;
        }
    }
    // fallback: last message's usage from the transcript tail — covers older Claude
    // Code builds that don't send context_window, or a rejected value above. Only
    // runs when the payload count is absent, so the normal path does no file I/O.
    if ctx_tokens == 0 && !transcript.is_empty() {
        if let Ok(mut f) = fs::File::open(transcript) {
            if let Ok(len) = f.seek(SeekFrom::End(0)) {
                let start = len.saturating_sub(256 * 1024);
                if f.seek(SeekFrom::Start(start)).is_ok() {
                    let mut buf = Vec::new();
                    if f.read_to_end(&mut buf).is_ok() {
                        let tail = String::from_utf8_lossy(&buf);
                        for ln in tail.lines().rev() {
                            let ln = ln.trim();
                            if ln.is_empty() {
                                continue;
                            }
                            if let Ok(obj) = parse(ln) {
                                if let Some(u) = obj.path(&["message", "usage"]) {
                                    let g = |k: &str| {
                                        u.get(k).and_then(Json::as_f64).unwrap_or(0.0)
                                    };
                                    ctx_tokens = (g("input_tokens")
                                        + g("cache_read_input_tokens")
                                        + g("cache_creation_input_tokens"))
                                        as u64;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let cost_usd = d
        .path(&["cost", "total_cost_usd"])
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    let dur_str = d
        .path(&["cost", "total_duration_ms"])
        .and_then(Json::as_f64)
        .and_then(fmt_duration);
    let effort = d
        .path(&["effort", "level"])
        .and_then(Json::as_str)
        .unwrap_or("");
    let thinking = d
        .path(&["thinking", "enabled"])
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let exceeds_200k = d
        .get("exceeds_200k_tokens")
        .and_then(Json::as_bool)
        .unwrap_or(false);

    let sep = format!(" {}|{} ", C_SEP, RESET); // between sectors
    let dot = g.dot(); // within a sector

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|t| t.as_secs_f64());

    // ---- Two lines split by domain, each fitted to the terminal width ----
    //   Claude (session): mode · context | 5h | 7d | elapsed · cost
    //   Local  (machine): CPU · RAM · VM · host | dir | version | resume
    // Every piece carries a drop rank (R_*): on a narrow terminal the lowest go first.
    let mut claude: Vec<Sector> = Vec::new();

    // mode sector: model · effort · think
    let mut mode = Sector::new(&dot);
    // drop the verbose " context" suffix (e.g. "Opus 4.8 (1M context)" -> "(1M)")
    mode.push(Item::fixed(format!(
        "{}{}{}{}",
        BOLD,
        C_MODEL,
        model.replace(" context", ""),
        RESET
    )));
    if !effort.is_empty() {
        mode.push(Item::optional(
            format!("{}{}{}", color_for_effort(effort), g.effort(effort), RESET),
            R_EFFORT,
        ));
    }
    mode.push(Item::optional(
        if thinking {
            format!("{}think on{}", C_ON, RESET)
        } else {
            format!("{}think off{}", C_OFF, RESET)
        },
        R_THINK,
    ));
    claude.push(mode);

    // context sector: the most-actionable live metric, placed right after mode.
    if ctx_tokens > 0 {
        // % is derived from the same number as the displayed X/Y — they always agree.
        let pct = if max_ctx > 0 {
            clip(ctx_tokens as f64 / max_ctx as f64 * 100.0)
        } else {
            0.0
        };
        let col = color_for_pct(pct);
        let marker = g.ctx_marker(pct, exceeds_200k && max_ctx <= 200_000);
        let mut ctx = Sector::new(" ");
        ctx.push(Item::optional(
            format!("{}{}", marker, bar(pct, 10, &g)),
            R_GAUGE,
        ));
        // the count shrinks to the bare percentage, late, and the percentage stays
        ctx.push(Item::steps(
            vec![
                format!(
                    "{}{}/{} ({:>5.1}%){}",
                    col,
                    fmt_tokens(ctx_tokens),
                    max_label,
                    pct,
                    RESET
                ),
                format!("{}{:>5.1}%{}", col, pct, RESET),
            ],
            vec![R_TOKENS],
        ));
        claude.push(ctx);
    }

    // limits — the real constraints (colored, high value). Pro/Max only, appears
    // after the first API response; each window may be independently absent.
    let mut seven_used = None;
    let mut seven_reset = None;
    if let Some(rl) = d.get("rate_limits") {
        if let Some(pct) = rl.path(&["five_hour", "used_percentage"]).and_then(Json::as_f64) {
            let resets_at = rl.path(&["five_hour", "resets_at"]).and_then(Json::as_f64);
            claude.push(rate_limit_sector("5h", pct, resets_at, now, &g));
        }
        seven_used = rl.path(&["seven_day", "used_percentage"]).and_then(Json::as_f64);
        seven_reset = rl.path(&["seven_day", "resets_at"]).and_then(Json::as_f64);
    }
    if let Some(n) = now {
        claude.push(weekly_sector(seven_used, seven_reset, n, &g));
    }

    // lower priority: elapsed · cost — demoted to the right, plain dim (cost is an
    // API-equivalent figure, not a subscription charge). Both right-aligned to 7
    // characters, so they only widen past 99 hours / $999.
    let mut spend = Sector::new(&dot);
    if let Some(ds) = &dur_str {
        spend.push(Item::optional(
            format!("{}{:>7}{}", C_DIM, ds, RESET),
            R_ELAPSED,
        ));
    }
    if cost_usd > 0.0 {
        spend.push(Item::optional(
            format!("{}{:>7}{}", C_DIM, format!("${:.2}", cost_usd), RESET),
            R_COST,
        ));
    }
    claude.push(spend);

    // ---- Local (machine) line: CPU · RAM · VM · host | dir | version | resume ----
    let mut local: Vec<Sector> = Vec::new();

    // machine sector first, live meters leading: CPU · RAM · VM at the very left
    // edge (first-read anchor), then host (🏠 local / 🌐 SSH remote)
    let mut machine = Sector::new(&dot);
    let cpu = cpu_percent();
    let (ram, vm) = plat::mem();
    for (label, val, rank) in [("CPU", cpu, R_CPU), ("RAM", ram, R_RAM), ("VM", vm, R_VM)] {
        if let Some(v) = val {
            machine.push(Item::optional(
                format!("{} {}{}%{}", label, color_for_pct(v), fmt_pct(v), RESET),
                rank,
            ));
        }
    }
    if let Some(h) = &host {
        let remote = is_remote(&ancestors, Some(h.as_str()));
        let hc = if remote { fg(214) } else { C_HOST.to_string() };
        machine.push(Item::optional(
            format!("{}{}{}", hc, g.host(h, remote), RESET),
            R_HOST,
        ));
    }
    local.push(machine);

    // dir sector
    local.push(Sector::one(Item::optional(
        format!("{}{}{}", C_DIR, base, RESET),
        R_DIR,
    )));

    // version: build tag for cross-machine update checks
    local.push(Sector::one(Item::optional(
        format!("{}v{}{}", C_DIM, VERSION, RESET),
        R_VERSION,
    )));

    // resume: full command at the far right of the machine line (dim reference;
    // the first thing to go on a narrow terminal, not the stats you watch).
    if !sid.is_empty() {
        local.push(Sector::one(Item::optional(
            format!("{}claude --resume {}{}", C_DIM, sid, RESET),
            R_RESUME,
        )));
    }

    let claude_line = fit_line(claude, width, &sep);
    let local_line = fit_line(local, width, &sep);

    if claude_line.is_empty() {
        print!("{}", local_line);
    } else {
        // Claude session on top, this machine below (nearest the prompt)
        print!("{}\n{}", claude_line, local_line);
    }
}

// ---------------- tests: rustc --edition 2021 --test statusline.rs ----------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let j = parse(r#"{"a": 1.5, "b": [true, null, "x\ny"], "c": {"d": -2e3}}"#).unwrap();
        assert_eq!(j.path(&["a"]).unwrap().as_f64(), Some(1.5));
        assert_eq!(j.path(&["c", "d"]).unwrap().as_f64(), Some(-2000.0));
        match j.get("b").unwrap() {
            Json::Arr(a) => {
                assert_eq!(a.len(), 3);
                assert_eq!(a[0].as_bool(), Some(true));
                assert_eq!(a[1], Json::Null);
                assert_eq!(a[2].as_str(), Some("x\ny"));
            }
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn parse_unicode_and_escapes() {
        let j = parse(r#"{"s": "😀 café \"q\" \\ /"}"#).unwrap();
        assert_eq!(j.get("s").unwrap().as_str(), Some("😀 café \"q\" \\ /"));
        // raw multi-byte UTF-8 passes through
        let j2 = parse("{\"k\": \"한글 🟢\"}").unwrap();
        assert_eq!(j2.get("k").unwrap().as_str(), Some("한글 🟢"));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse("").is_err());
        assert!(parse("{oops}").is_err());
        assert!(parse("{\"a\":1}trailing").is_err());
        assert!(parse(r#"{"s": "\ud800"}"#).is_err()); // lone surrogate
    }

    #[test]
    fn transcript_usage_extraction() {
        let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":117727,"cache_creation_input_tokens":8781,"output_tokens":1845}}}"#;
        let obj = parse(line).unwrap();
        let u = obj.path(&["message", "usage"]).unwrap();
        let g = |k: &str| u.get(k).and_then(Json::as_f64).unwrap_or(0.0);
        let ctx = (g("input_tokens") + g("cache_read_input_tokens")
            + g("cache_creation_input_tokens")) as u64;
        assert_eq!(ctx, 126_510);
    }

    #[test]
    fn token_units() {
        assert_eq!(fmt_tokens(999), "   999");
        assert_eq!(fmt_tokens(126_510), "126.5K");
        assert_eq!(fmt_tokens(999_949), "999.9K");
        assert_eq!(fmt_tokens(999_950), " 1.00M"); // never "1000.0K"
        assert_eq!(fmt_tokens(4_457_100), " 4.46M");
    }

    #[test]
    fn duration_units() {
        assert_eq!(fmt_duration(16_260_000.0), Some("4h 31m".to_string()));
        assert_eq!(fmt_duration(60_000.0), Some("0h 01m".to_string()));
        assert_eq!(fmt_duration(0.0), None);
        assert_eq!(fmt_duration(-5.0), None);
    }

    #[test]
    fn bar_always_ten_cells() {
        for pct in [0.0, 12.7, 45.0, 99.9, 100.0, 250.0] {
            let b = bar(pct, 10, &Glyphs::UNICODE);
            assert_eq!(
                b.matches('█').count() + b.matches('░').count(),
                10,
                "pct={}",
                pct
            );
            let a = visible(&bar(pct, 10, &Glyphs::ASCII));
            assert_eq!(a.matches('#').count() + a.matches('.').count(), 10, "pct={}", pct);
            assert!(a.starts_with('[') && a.ends_with(']'), "{}", a);
        }
    }

    #[test]
    fn pct_color_thresholds() {
        assert_eq!(color_for_pct(29.9), fg(42));
        assert_eq!(color_for_pct(30.0), fg(148));
        assert_eq!(color_for_pct(90.0), fg(196));
    }

    #[test]
    fn delta_pct_zero_tick_delta_is_none() {
        // same (busy, total) twice = no new tick data, not "0% busy"
        assert_eq!(delta_pct((10, 100), (10, 100)), None);
    }

    #[test]
    fn delta_pct_normal_case() {
        assert_eq!(delta_pct((10, 100), (15, 200)), Some(5.0));
    }

    #[test]
    fn fmt_pct_pads_single_digit_to_match_double_digit_width() {
        assert_eq!(fmt_pct(4.0), "  4");
        assert_eq!(fmt_pct(14.0), " 14");
        assert_eq!(fmt_pct(100.0), "100"); // reserved: 100% never widens the field
    }

    #[test]
    fn rate_limit_segment_with_reset() {
        let g = Glyphs::UNICODE;
        let s = rate_limit_segment("5h", 23.0, Some(2000.0), Some(1000.0), &g);
        assert!(s.contains("5h"));
        assert!(s.contains("23%"));
        assert!(s.contains("⏳0h 16m")); // (2000-1000)s = 1000s = 16m
        let a = rate_limit_segment("5h", 23.0, Some(2000.0), Some(1000.0), &Glyphs::ASCII);
        assert!(a.contains("~0h 16m"));
    }

    #[test]
    fn rate_limit_segment_no_reset_info() {
        let s = rate_limit_segment("7d", 41.0, None, None, &Glyphs::UNICODE);
        assert!(s.contains("7d"));
        assert!(s.contains("41%"));
        assert!(!s.contains('⏳')); // no countdown when resets_at/now missing
    }

    #[test]
    fn rate_limit_segment_clips_out_of_range_pct() {
        // defensive: this comes straight from harness JSON, not a locally
        // computed/bounded value, so malformed input shouldn't render garbage
        let g = Glyphs::UNICODE;
        assert!(rate_limit_segment("5h", 150.0, None, None, &g).contains("100%"));
        assert!(rate_limit_segment("5h", -20.0, None, None, &g).contains("  0%"));
    }

    #[test]
    fn rate_limit_segment_past_reset_omitted() {
        // resets_at already elapsed (stale/clock skew) — no countdown, no panic
        let s = rate_limit_segment("5h", 90.0, Some(500.0), Some(1000.0), &Glyphs::UNICODE);
        assert!(!s.contains('⏳'));
    }

    #[test]
    fn cpu_counters_sane() {
        // on supported platforms busy <= total and both nonzero after boot
        if let Some((busy, total)) = plat::cpu_counters() {
            assert!(busy <= total);
            assert!(total > 0);
        }
    }

    #[test]
    fn week_anchor_is_saturday_0400_kst() {
        // the anchor itself is a reset instant → 0s elapsed
        assert_eq!(week_elapsed_secs(None, WEEK_ANCHOR), 0.0);
        // +3.5 days → exactly half the week
        assert_eq!(week_elapsed_secs(None, WEEK_ANCHOR + 302_400.0), 302_400.0);
        // a much later Saturday 04:00 KST (anchor + 130 weeks) also lands on 0
        assert_eq!(week_elapsed_secs(None, WEEK_ANCHOR + 130.0 * WEEK_SECS), 0.0);
    }

    #[test]
    fn week_elapsed_from_real_timestamp() {
        // 2026-07-04 06:45:00 UTC == Sat 15:45 KST → 11h45m into the week
        let now = 1_783_147_500.0;
        assert_eq!(week_elapsed_secs(None, now), 42_300.0); // 11h45m
        let pct = week_elapsed_secs(None, now) / WEEK_SECS * 100.0;
        assert!((pct - 6.994).abs() < 0.01, "pct={}", pct);
    }

    #[test]
    fn week_prefers_resets_at_when_present() {
        // resets in 1 day → 6 days already elapsed
        assert_eq!(
            week_elapsed_secs(Some(1000.0 + 86_400.0), 1000.0),
            WEEK_SECS - 86_400.0
        );
        // out-of-range resets_at (already past) falls back to the KST anchor
        assert_eq!(
            week_elapsed_secs(Some(500.0), 1000.0),
            (1000.0 - WEEK_ANCHOR).rem_euclid(WEEK_SECS)
        );
    }

    #[test]
    fn pace_color_tracks_headroom() {
        // under pace: stepped warm -> cool as headroom grows
        assert_eq!(pace_color(-3.0), fg(148)); // slight headroom -> yellow-green
        assert_eq!(pace_color(-0.5), fg(148)); // rounds to -1% under pace -> yellow-green
        assert_eq!(pace_color(-10.0), fg(42)); // comfortable -> green
        assert_eq!(pace_color(-20.0), fg(45)); // lots of room -> cyan
        assert_eq!(pace_color(-30.0), fg(33)); // way ahead -> blue
        // on pace and over pace
        assert_eq!(pace_color(-0.3), fg(220)); // rounds to on pace -> yellow
        assert_eq!(pace_color(0.0), fg(220)); // on pace -> yellow
        assert_eq!(pace_color(10.0), fg(214)); // over -> orange
        assert_eq!(pace_color(34.0), fg(196)); // way over -> red
        assert_ne!(pace_color(-30.0), pace_color(30.0));
    }

    #[test]
    fn fmt_span_units() {
        assert_eq!(fmt_span(0.0), None);
        assert_eq!(fmt_span(90_000.0), Some("1d 01h".to_string()));
        assert_eq!(fmt_span(3_660.0), Some("0d 01h".to_string()));
        assert_eq!(fmt_span(120.0), Some("0d 00h".to_string()));
    }

    #[test]
    fn version_is_set() {
        // guards against an empty/placeholder version reaching a machine, which
        // would defeat the cross-machine update check
        assert!(!VERSION.is_empty());
        assert!(VERSION.chars().next().unwrap().is_ascii_digit());
    }

    #[test]
    fn fmt_span_compact_units() {
        assert_eq!(fmt_span_compact(0.0), None);
        assert_eq!(fmt_span_compact(90_000.0), Some(" 1d01h".to_string()));
        assert_eq!(fmt_span_compact(3_660.0), Some(" 1h01m".to_string()));
        assert_eq!(fmt_span_compact(120.0), Some(" 0h02m".to_string()));
        assert_eq!(fmt_span_compact(30_240.0), Some(" 8h24m".to_string()));
    }

    #[test]
    fn pace_time_converts_delta_to_span() {
        // 1pp == 1% of a week; +5pp -> 5% of 604800s == 8h24m, sign-independent
        assert_eq!(pace_time(5.0), Some(" 8h24m".to_string()));
        assert_eq!(pace_time(-5.0), Some(" 8h24m".to_string()));
        // multi-day offset for a large delta
        assert_eq!(pace_time(40.0), Some(" 2d19h".to_string()));
        // rounds to 0% -> on pace -> no time shown
        assert_eq!(pace_time(0.4), None);
        assert_eq!(pace_time(-0.3), None);
    }

    #[test]
    fn weekly_segment_with_and_without_usage() {
        // used 12%, resets in 6 days -> ~1 day (~14%) of the week elapsed ->
        // "12% used", 2% under pace, that gap re-expressed as ~3h21m of headroom
        let g = Glyphs::UNICODE;
        let s = weekly_segment(Some(12.0), Some(1000.0 + 6.0 * 86_400.0), 1000.0, &g);
        assert!(s.contains("7d"));
        assert!(s.contains(" 12% used"));
        assert!(s.contains(" 2% under pace"));
        assert!(s.contains("( 3h21m)")); // gap re-expressed as time
        assert!(s.contains("⏳6d 00h")); // reset countdown marker
        assert!(!s.contains("wk")); // usage mode has no wk label
        let t = weekly_segment(None, None, WEEK_ANCHOR + 86_400.0, &g);
        assert!(t.contains("wk"));
        assert!(t.contains('⏳'));
        assert!(!t.contains("7d"));
    }

    /// The text a terminal shows: the same string with the ANSI color codes removed.
    fn visible(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn glyph_mode_parsing() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(glyph_mode_from(a(&[]), None), GlyphMode::Auto);
        assert_eq!(glyph_mode_from(a(&["--ascii"]), None), GlyphMode::Ascii);
        assert_eq!(glyph_mode_from(a(&["--glyphs", "ascii"]), None), GlyphMode::Ascii);
        // a flag beats the environment, the environment beats the default
        assert_eq!(glyph_mode_from(a(&["--glyphs=unicode"]), Some("ascii")), GlyphMode::Unicode);
        assert_eq!(glyph_mode_from(a(&["--bogus"]), Some("ASCII")), GlyphMode::Ascii);
        assert_eq!(glyph_mode_from(a(&[]), Some("nonsense")), GlyphMode::Auto);
    }

    #[test]
    fn auto_ascii_rules() {
        let good = TermEnv {
            windows: false,
            lang: Some("en_US.UTF-8".to_string()),
            term: "xterm-256color".to_string(),
            in_mux: false,
            remote: true,
            libc_ok: Some(true),
            win_modern: false,
        };
        assert!(!auto_ascii(&good));
        assert!(auto_ascii(&TermEnv { lang: Some("C".to_string()), ..good.clone() }));
        assert!(auto_ascii(&TermEnv { lang: Some("ko_KR.EUC-KR".to_string()), ..good.clone() }));
        assert!(!auto_ascii(&TermEnv { lang: Some("ko_KR.utf8".to_string()), ..good.clone() }));
        assert!(auto_ascii(&TermEnv { term: "linux".to_string(), ..good.clone() }));
        // old libc tables matter only when something on this box re-measures the line
        assert!(!auto_ascii(&TermEnv { libc_ok: Some(false), ..good.clone() })); // plain SSH
        assert!(auto_ascii(&TermEnv { libc_ok: Some(false), in_mux: true, ..good.clone() })); // tmux here
        assert!(auto_ascii(&TermEnv { libc_ok: Some(false), remote: false, ..good.clone() })); // local terminal
        assert!(!auto_ascii(&TermEnv { libc_ok: None, in_mux: true, ..good.clone() })); // nothing to go on
        assert!(auto_ascii(&TermEnv { lang: None, in_mux: true, ..good.clone() }));
        assert!(!auto_ascii(&TermEnv { lang: None, ..good.clone() }));
        let win = TermEnv { windows: true, win_modern: true, ..good.clone() };
        assert!(!auto_ascii(&win));
        assert!(!auto_ascii(&TermEnv { lang: None, ..win.clone() })); // LANG is rarely set on Windows
        assert!(auto_ascii(&TermEnv { lang: Some("C".to_string()), ..win.clone() })); // but an explicit non-UTF-8 one counts
        assert!(auto_ascii(&TermEnv { win_modern: false, ..win }));
    }

    #[test]
    fn remote_detection_without_ssh_variables() {
        assert!(is_ec2_hostname("ip-172-31-16-160"));
        assert!(!is_ec2_hostname("ip-172-31-16"));
        assert!(!is_ec2_hostname("my-mac"));
        assert!(!is_ec2_hostname("ip-1234-1-1-1"));
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // ancestry is consulted even with no SSH_* in the environment of a test run
        assert!(is_remote(&v(&["statusline-rs", "bash", "node", "bash", "sshd-session"]), Some("box")));
        assert!(is_remote(&v(&["statusline-rs", "bash", "node", "zsh", "mosh-server"]), Some("box")));
        assert!(in_mux(&v(&["statusline-rs", "bash", "node", "bash", "tmux: server"])));
        assert!(in_mux(&v(&["statusline-rs", "sh", "SCREEN"])));
        assert!(!in_mux(&v(&["statusline-rs", "bash", "node", "bash", "sshd-session"])) || env_set("TMUX") || env_set("STY"));
    }

    #[test]
    fn ascii_mode_prints_only_ascii() {
        let g = Glyphs::ASCII;
        let mut all = String::new();
        all.push_str(&rate_limit_segment("5h", 23.0, Some(2000.0), Some(1000.0), &g));
        all.push_str(&weekly_segment(Some(12.0), Some(1000.0 + 6.0 * 86_400.0), 1000.0, &g));
        all.push_str(&weekly_segment(Some(73.0), Some(1000.0 + 6.0 * 86_400.0), 1000.0, &g));
        all.push_str(&weekly_segment(None, None, WEEK_ANCHOR + 86_400.0, &g));
        all.push_str(&bar(37.0, 10, &g));
        all.push_str(&g.ctx_marker(95.0, true));
        all.push_str(&g.ctx_marker(10.0, false));
        all.push_str(&g.effort("max"));
        all.push_str(&g.host("ip-172-31-16-160", true));
        all.push_str(&g.host("mac", false));
        all.push_str(&g.dot());
        all.push_str(g.hourglass());
        assert!(all.is_ascii(), "{:?}", all);
        // and the unicode set still carries the glyphs
        assert!(bar(37.0, 10, &Glyphs::UNICODE).contains('█'));
        assert!(Glyphs::UNICODE.host("x", true).contains('🌐'));
        assert!(Glyphs::UNICODE.ctx_marker(10.0, false).contains('🟢'));
    }

    #[test]
    fn changing_fields_keep_their_width() {
        // every value that moves between refreshes renders at one width, so nothing
        // after it on the line shifts, and a cell-diffing layer repaints only digits
        let w = |s: String| visible(&s).chars().count();
        for v in [0.0, 4.0, 14.0, 99.0, 100.0] {
            assert_eq!(fmt_pct(v).len(), 3, "pct {}", v);
        }
        for n in [0, 999, 1_000, 12_345, 136_779, 999_949, 1_020_000, 9_999_999] {
            assert_eq!(fmt_tokens(n).len(), 6, "tokens {}", n);
        }
        for ms in [60_000.0, 2_520_000.0, 16_260_000.0, 5.0 * 3_600_000.0] {
            assert_eq!(fmt_duration(ms).unwrap().len(), 6, "duration {}", ms);
        }
        for s in [120.0, 3_660.0, 90_000.0, 6.9 * 86_400.0] {
            assert_eq!(fmt_span(s).unwrap().len(), 6, "span {}", s);
            assert_eq!(fmt_span_compact(s).unwrap().len(), 6, "compact {}", s);
        }
        for g in [Glyphs::UNICODE, Glyphs::ASCII] {
            // 16 minutes vs 4h12m left, 4% vs 44% used
            let a = w(rate_limit_segment("5h", 4.0, Some(1960.0), Some(1000.0), &g));
            let b = w(rate_limit_segment("5h", 44.0, Some(16_120.0), Some(1000.0), &g));
            assert_eq!(a, b);
            // both under pace: 12% used after 1 day vs 3% used after half a day
            let c = w(weekly_segment(Some(12.0), Some(1000.0 + 6.0 * 86_400.0), 1000.0, &g));
            let d = w(weekly_segment(Some(3.0), Some(1000.0 + 6.5 * 86_400.0), 1000.0, &g));
            assert_eq!(c, d);
            assert_eq!(w(bar(0.0, 10, &g)), w(bar(100.0, 10, &g)));
        }
    }

    #[test]
    fn week_anchor_override_shifts_the_fallback_grid() {
        // with resets_at the anchor is irrelevant; without it, elapsed is measured
        // from the given grid
        let now = WEEK_ANCHOR + 86_400.0;
        assert_eq!(week_elapsed_secs_at(Some(now + 3600.0), now, 0.0), WEEK_SECS - 3600.0);
        assert_eq!(week_elapsed_secs_at(None, now, WEEK_ANCHOR), 86_400.0);
        assert_eq!(week_elapsed_secs_at(None, now, WEEK_ANCHOR + 3600.0), 86_400.0 - 3600.0);
        assert_eq!(week_elapsed_secs_at(None, now, now), 0.0);
    }

    #[test]
    fn display_width_counts_terminal_columns() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("🟢 █░ ⏳3d"), 2 + 1 + 1 + 1 + 1 + 2 + 2);
        assert_eq!(display_width("한글 host"), 4 + 5);
        assert_eq!(display_width(&format!("{}x{}", C_DIM, RESET)), 1);
        assert_eq!(display_width(&bar(50.0, 10, &Glyphs::UNICODE)), 10);
        assert_eq!(display_width(&bar(50.0, 10, &Glyphs::ASCII)), 12);
        assert_eq!(display_width(" · "), 3);
    }

    #[test]
    fn target_width_precedence() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(target_width(a(&[]), None, None), None);
        assert_eq!(target_width(a(&[]), None, Some("120")), Some(118)); // COLUMNS minus the margin
        assert_eq!(target_width(a(&[]), Some("100"), Some("120")), Some(100));
        assert_eq!(target_width(a(&["--width", "90"]), Some("100"), Some("120")), Some(90));
        assert_eq!(target_width(a(&["--width=80"]), None, None), Some(80));
        assert_eq!(target_width(a(&["--width", "0"]), None, Some("120")), None); // 0 = never shorten
        assert_eq!(target_width(a(&["--ascii"]), None, Some("abc")), None);
    }

    #[test]
    fn fit_line_sheds_lowest_rank_first() {
        let sep = " | ";
        let build = || {
            let mut a = Sector::new(" . ");
            a.push(Item::fixed("model".to_string()));
            a.push(Item::optional("think on".to_string(), 30));
            let mut b = Sector::new(" . ");
            b.push(Item::fixed("7d 33% used".to_string()));
            b.push(Item::steps(
                vec![
                    "16% under pace ( 1d02h)".to_string(),
                    "16% under pace".to_string(),
                    String::new(),
                ],
                vec![10, 35],
            ));
            let c = Sector::one(Item::optional("claude --resume abc".to_string(), 5));
            vec![a, b, c]
        };
        let full = "model . think on | 7d 33% used . 16% under pace ( 1d02h) | claude --resume abc";
        assert_eq!(fit_line(build(), None, sep), full);
        assert_eq!(fit_line(build(), Some(200), sep), full);
        // 1st: the resume command goes, separator included
        assert_eq!(
            fit_line(build(), Some(full.len() - 1), sep),
            "model . think on | 7d 33% used . 16% under pace ( 1d02h)"
        );
        // 2nd: the pace parenthetical
        assert_eq!(
            fit_line(build(), Some(50), sep),
            "model . think on | 7d 33% used . 16% under pace"
        );
        // 3rd: think; 4th: the pace words
        assert_eq!(fit_line(build(), Some(40), sep), "model | 7d 33% used . 16% under pace");
        assert_eq!(fit_line(build(), Some(20), sep), "model | 7d 33% used");
        // nothing left to shed: the shortest form comes back even if it still overflows
        assert_eq!(fit_line(build(), Some(5), sep), "model | 7d 33% used");
    }

    #[test]
    fn narrow_terminal_keeps_the_budget_numbers() {
        // a realistic ~150-column line 1 squeezed into narrower terminals
        let g = Glyphs::UNICODE;
        let dot = g.dot();
        let sep = format!(" {}|{} ", C_SEP, RESET);
        let line = |width: Option<usize>| {
            let mut mode = Sector::new(&dot);
            mode.push(Item::fixed(format!("{}{}Fable 5.1{}", BOLD, C_MODEL, RESET)));
            mode.push(Item::optional(
                format!("{}{}{}", color_for_effort("max"), g.effort("max"), RESET),
                R_EFFORT,
            ));
            mode.push(Item::optional(format!("{}think on{}", C_ON, RESET), R_THINK));
            let mut ctx = Sector::new(" ");
            ctx.push(Item::optional(
                format!("{}{}", g.ctx_marker(13.7, false), bar(13.7, 10, &g)),
                R_GAUGE,
            ));
            ctx.push(Item::steps(
                vec![
                    format!("{}/1M ({:>5.1}%)", fmt_tokens(136_779), 13.7),
                    format!("{:>5.1}%", 13.7),
                ],
                vec![R_TOKENS],
            ));
            let five = rate_limit_sector("5h", 11.0, Some(3520.0), Some(1000.0), &g);
            let seven = weekly_sector(
                Some(33.0),
                Some(1000.0 + 3.0 * 86_400.0 + 13.0 * 3600.0),
                1000.0,
                &g,
            );
            let mut spend = Sector::new(&dot);
            spend.push(Item::optional(format!("{:>7}", "23h 13m"), R_ELAPSED));
            spend.push(Item::optional(format!("{:>7}", "$539.75"), R_COST));
            fit_line(vec![mode, ctx, five, seven, spend], width, &sep)
        };
        let full = line(None);
        assert!(display_width(&full) > 150, "{}", visible(&full));
        for w in [118usize, 100, 80, 60] {
            let l = line(Some(w));
            let v = visible(&l);
            assert!(display_width(&l) <= w, "width {}: {} cols: {}", w, display_width(&l), v);
            assert!(v.contains("5h  11%") && v.contains("7d  33% used"), "width {}: {}", w, v);
        }
        let at_118 = visible(&line(Some(118)));
        assert!(at_118.contains("% under pace"), "pace words outlive think: {}", at_118);
        assert!(!at_118.contains("think on"), "{}", at_118);
    }
}
