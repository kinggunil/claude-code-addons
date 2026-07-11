// Claude Code status line — zero-dependency single-file Rust implementation.
//
// Install: curl -sSfL https://gist.githubusercontent.com/kinggunil/0c34b865014ca16c446ba46bd9114cd9/raw/install.sh | sh
// Build:   rustc --edition 2021 -O statusline.rs -o "$HOME/.claude/statusline-rs"
// Test:    rustc --edition 2021 --test statusline.rs -o /tmp/sl-test && /tmp/sl-test
// ~/.claude/settings.json:
//   "statusLine": { "type": "command", "command": "$HOME/.claude/statusline-rs",
//                   "refreshInterval": 1 }
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
// on every gist push so each server/PC displays which revision it is running;
// comparing the on-screen version against the gist reveals stale machines.
// Format: YY.MM.DD for the first build of a day, then .2/.3/... appended for
// each further same-day revision (so a newer same-day build sorts after an
// older one). Date-based means a glance tells you how old a build is without
// looking anything up.
const VERSION: &str = "26.07.11";

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

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn fmt_duration(ms: f64) -> Option<String> {
    if ms <= 0.0 {
        return None;
    }
    let s = (ms / 1000.0) as u64;
    let (h, m) = (s / 3600, (s % 3600) / 60);
    Some(if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    })
}

/// Right-pads single-digit percentages to a fixed 2-char field (" 4" vs "14")
/// so a value crossing the 9%->10% boundary doesn't shift everything after it
/// on the line every refresh.
fn fmt_pct(v: f64) -> String {
    format!("{:>2.0}", v)
}

/// e.g. "5h 23%(4h12m)" — reset countdown omitted when resets_at/now is unavailable
/// or already in the past (clock skew / stale sample). `pct` is clipped to
/// 0-100 defensively since it comes straight from the harness JSON, not a
/// locally-computed value.
fn rate_limit_segment(label: &str, pct: f64, resets_at: Option<f64>, now: Option<f64>) -> String {
    let pct = clip(pct);
    let mut seg = format!("{} {}{}%{}", label, color_for_pct(pct), fmt_pct(pct), RESET);
    if let (Some(r), Some(n)) = (resets_at, now) {
        if r > n {
            if let Some(rem) = fmt_duration((r - n) * 1000.0) {
                // ⏳ marks the reset countdown, matching the 7d segment
                seg.push_str(&format!(" {}⏳{}{}", C_OFF, rem, RESET));
            }
        }
    }
    seg
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
    match resets_at {
        Some(r) if r > now && r - now <= WEEK_SECS => WEEK_SECS - (r - now),
        _ => (now - WEEK_ANCHOR).rem_euclid(WEEK_SECS),
    }
}

/// "Xd Yh" / "Xh Ym" / "Xm" — day-aware span for the weekly reset countdown.
fn fmt_span(secs: f64) -> Option<String> {
    if secs <= 0.0 {
        return None;
    }
    let s = secs as u64;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    Some(if d > 0 {
        format!("{}d {}h", d, h)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    })
}

/// Compact, space-free span ("8h24m", "2d16h", "45m") for inline annotations
/// where a space would visually split the value from the token before it.
fn fmt_span_compact(secs: f64) -> Option<String> {
    if secs <= 0.0 {
        return None;
    }
    let s = secs as u64;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    Some(if d > 0 {
        format!("{}d{}h", d, h)
    } else if h > 0 {
        format!("{}h{}m", h, m)
    } else {
        format!("{}m", m)
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
///   with usage: "7d 39% used · 18% over pace (1d6h) · ⏳5d12h"
///   time only:  "wk 21% · ⏳5d12h"
fn weekly_segment(used: Option<f64>, resets_at: Option<f64>, now: f64) -> String {
    let elapsed = week_elapsed_secs(resets_at, now);
    let elapsed_pct = clip(elapsed / WEEK_SECS * 100.0);
    let countdown = fmt_span(WEEK_SECS - elapsed);

    let mut seg = if let Some(u) = used {
        let u = clip(u);
        let delta = u - elapsed_pct;
        let col = pace_color(delta);
        let d = delta.round() as i64;
        // the same gap re-expressed as time, in parens right after the words
        let t = pace_time(delta)
            .map(|t| format!(" ({})", t))
            .unwrap_or_default();
        let pace = if d > 0 {
            format!("{}{}% over pace{}{}", col, d, t, RESET)
        } else if d < 0 {
            format!("{}{}% under pace{}{}", col, -d, t, RESET)
        } else {
            format!("{}on pace{}", col, RESET)
        };
        format!("7d {:.0}% used {}·{} {}", u, C_SEP, RESET, pace)
    } else {
        format!("{}wk {}%{}", C_OFF, fmt_pct(elapsed_pct), RESET)
    };
    if let Some(c) = countdown {
        seg.push_str(&format!(" {}·{} {}⏳{}{}", C_SEP, RESET, C_OFF, c, RESET));
    }
    seg
}

fn bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0 * width as f64).round() as i64).clamp(0, width as i64) as usize;
    format!(
        "{}{}{}{}{}",
        color_for_pct(pct),
        "█".repeat(filled),
        C_OFF,
        "░".repeat(width - filled),
        RESET
    )
}

// ---------------- main ----------------

fn main() {
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
    let dot = format!(" {}·{} ", C_SEP, RESET); // within a sector (line 2)

    // ---- line 1 (dim): [spend: elapsed · cost] | [5h] | [7d] | [resume] ----
    // ordered by glance-frequency: budget first, the rarely-read resume id last.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|t| t.as_secs_f64());
    let mut meta: Vec<String> = Vec::new();

    // limits first — the real constraints (colored, high value). Pro/Max only,
    // appears after the first API response; each window may be independently absent.
    let mut seven_used = None;
    let mut seven_reset = None;
    if let Some(rl) = d.get("rate_limits") {
        if let Some(pct) = rl.path(&["five_hour", "used_percentage"]).and_then(Json::as_f64) {
            let resets_at = rl.path(&["five_hour", "resets_at"]).and_then(Json::as_f64);
            meta.push(rate_limit_segment("5h", pct, resets_at, now));
        }
        seven_used = rl.path(&["seven_day", "used_percentage"]).and_then(Json::as_f64);
        seven_reset = rl.path(&["seven_day", "resets_at"]).and_then(Json::as_f64);
    }
    if let Some(n) = now {
        meta.push(weekly_segment(seven_used, seven_reset, n));
    }

    // lower priority: elapsed · cost — demoted to the right, plain dim (cost is an
    // API-equivalent figure, not a subscription charge), just before the resume ref.
    let mut spend: Vec<String> = Vec::new();
    if let Some(ds) = &dur_str {
        spend.push(ds.clone());
    }
    if cost_usd > 0.0 {
        spend.push(format!("${:.2}", cost_usd));
    }
    if !spend.is_empty() {
        meta.push(spend.join(" · "));
    }


    // ---- Two lines split by domain ----
    //   Claude (session): mode · context | 5h | 7d | elapsed·cost
    //   Local  (machine): CPU · RAM · VM · host | dir | version | resume
    let mut claude: Vec<String> = Vec::new();

    // mode sector: model · effort · think
    let mut mode: Vec<String> = Vec::new();
    // drop the verbose " context" suffix (e.g. "Opus 4.8 (1M context)" -> "(1M)")
    mode.push(format!("{}{}{}{}", BOLD, C_MODEL, model.replace(" context", ""), RESET));
    if !effort.is_empty() {
        mode.push(format!("{}⚡{}{}", color_for_effort(effort), effort, RESET));
    }
    mode.push(if thinking {
        format!("{}think on{}", C_ON, RESET)
    } else {
        format!("{}think off{}", C_OFF, RESET)
    });
    claude.push(mode.join(&dot));

    // context sector: the most-actionable live metric, placed right after mode.
    if ctx_tokens > 0 {
        // % is derived from the same number as the displayed X/Y — they always agree.
        let pct = if max_ctx > 0 {
            clip(ctx_tokens as f64 / max_ctx as f64 * 100.0)
        } else {
            0.0
        };
        let col = color_for_pct(pct);
        let emo = if exceeds_200k && max_ctx <= 200_000 {
            "🚨"
        } else {
            ctx_emoji(pct)
        };
        claude.push(format!(
            "{} {} {}{}/{} ({:.1}%){}",
            emo,
            bar(pct, 10),
            col,
            fmt_tokens(ctx_tokens),
            max_label,
            pct,
            RESET
        ));
    }

    // remaining session refs (5h | 7d | elapsed·cost), dim on the right
    for m in &meta {
        claude.push(format!("{}{}{}", C_DIM, m, RESET));
    }
    let claude_line = claude.join(&sep);

    // ---- Local (machine) line: CPU · RAM · VM · host | dir | version | resume ----
    let mut local: Vec<String> = Vec::new();

    // machine sector first, live meters leading: CPU · RAM · VM at the very left
    // edge (first-read anchor), then host (🏠 local / 🌐 SSH remote)
    let mut machine: Vec<String> = Vec::new();
    let cpu = cpu_percent();
    let (ram, vm) = plat::mem();
    for (label, val) in [("CPU", cpu), ("RAM", ram), ("VM", vm)] {
        if let Some(v) = val {
            machine.push(format!("{} {}{}%{}", label, color_for_pct(v), fmt_pct(v), RESET));
        }
    }
    if let Some(h) = hostname() {
        let is_ssh = std::env::var_os("SSH_CONNECTION").is_some()
            || std::env::var_os("SSH_TTY").is_some();
        if is_ssh {
            machine.push(format!("{}🌐 {}{}", fg(214), h, RESET));
        } else {
            machine.push(format!("{}🏠 {}{}", C_HOST, h, RESET));
        }
    }
    if !machine.is_empty() {
        local.push(machine.join(&dot));
    }

    // dir sector
    local.push(format!("{}{}{}", C_DIR, base, RESET));

    // version: build tag for cross-machine update checks
    local.push(format!("{}v{}{}", C_DIM, VERSION, RESET));

    // resume: full command at the far right of the machine line (dim reference;
    // rightmost so a narrow terminal truncates it first, not the stats you watch).
    if !sid.is_empty() {
        local.push(format!("{}claude --resume {}{}", C_DIM, sid, RESET));
    }

    let local_line = local.join(&sep);

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
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(126_510), "126.5K");
        assert_eq!(fmt_tokens(4_457_100), "4.46M");
    }

    #[test]
    fn duration_units() {
        assert_eq!(fmt_duration(16_260_000.0), Some("4h 31m".to_string()));
        assert_eq!(fmt_duration(60_000.0), Some("1m".to_string()));
        assert_eq!(fmt_duration(0.0), None);
        assert_eq!(fmt_duration(-5.0), None);
    }

    #[test]
    fn bar_always_ten_cells() {
        for pct in [0.0, 12.7, 45.0, 99.9, 100.0, 250.0] {
            let b = bar(pct, 10);
            assert_eq!(
                b.matches('█').count() + b.matches('░').count(),
                10,
                "pct={}",
                pct
            );
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
        assert_eq!(fmt_pct(4.0), " 4");
        assert_eq!(fmt_pct(14.0), "14");
        assert_eq!(fmt_pct(100.0), "100"); // rare edge case, allowed to widen
    }

    #[test]
    fn rate_limit_segment_with_reset() {
        let s = rate_limit_segment("5h", 23.0, Some(2000.0), Some(1000.0));
        assert!(s.contains("5h"));
        assert!(s.contains("23%"));
        assert!(s.contains("16m")); // (2000-1000)s = 1000s = 16m
    }

    #[test]
    fn rate_limit_segment_no_reset_info() {
        let s = rate_limit_segment("7d", 41.0, None, None);
        assert!(s.contains("7d"));
        assert!(s.contains("41%"));
        assert!(!s.contains('⏳')); // no countdown when resets_at/now missing
    }

    #[test]
    fn rate_limit_segment_clips_out_of_range_pct() {
        // defensive: this comes straight from harness JSON, not a locally
        // computed/bounded value, so malformed input shouldn't render garbage
        assert!(rate_limit_segment("5h", 150.0, None, None).contains("100%"));
        assert!(rate_limit_segment("5h", -20.0, None, None).contains(" 0%"));
    }

    #[test]
    fn rate_limit_segment_past_reset_omitted() {
        // resets_at already elapsed (stale/clock skew) — no countdown, no panic
        let s = rate_limit_segment("5h", 90.0, Some(500.0), Some(1000.0));
        assert!(!s.contains('('));
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
        assert_eq!(fmt_span(90_000.0), Some("1d 1h".to_string()));
        assert_eq!(fmt_span(3_660.0), Some("1h 1m".to_string()));
        assert_eq!(fmt_span(120.0), Some("2m".to_string()));
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
        assert_eq!(fmt_span_compact(90_000.0), Some("1d1h".to_string()));
        assert_eq!(fmt_span_compact(3_660.0), Some("1h1m".to_string()));
        assert_eq!(fmt_span_compact(120.0), Some("2m".to_string()));
        assert_eq!(fmt_span_compact(30_240.0), Some("8h24m".to_string()));
    }

    #[test]
    fn pace_time_converts_delta_to_span() {
        // 1pp == 1% of a week; +5pp -> 5% of 604800s == 8h24m, sign-independent
        assert_eq!(pace_time(5.0), Some("8h24m".to_string()));
        assert_eq!(pace_time(-5.0), Some("8h24m".to_string()));
        // multi-day offset for a large delta
        assert_eq!(pace_time(40.0), Some("2d19h".to_string()));
        // rounds to 0% -> on pace -> no time shown
        assert_eq!(pace_time(0.4), None);
        assert_eq!(pace_time(-0.3), None);
    }

    #[test]
    fn weekly_segment_with_and_without_usage() {
        // used 12%, resets in 6 days -> ~1 day (~14%) of the week elapsed ->
        // "12% used", 2% under pace, that gap re-expressed as ~3h21m of headroom
        let s = weekly_segment(Some(12.0), Some(1000.0 + 6.0 * 86_400.0), 1000.0);
        assert!(s.contains("7d"));
        assert!(s.contains("12% used"));
        assert!(s.contains("2% under pace"));
        assert!(s.contains("3h21m")); // gap re-expressed as time
        assert!(s.contains('⏳')); // reset countdown marker
        assert!(!s.contains("wk")); // usage mode has no wk label
        let t = weekly_segment(None, None, WEEK_ANCHOR + 86_400.0);
        assert!(t.contains("wk"));
        assert!(t.contains('⏳'));
        assert!(!t.contains("7d"));
    }
}
