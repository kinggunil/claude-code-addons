// claude-notify — cross-platform, dependency-free completion/question chime for
// Claude Code hooks. Single static binary, no subprocess (afplay/paplay/PS) on
// the play path, no external sound file: the WAV is embedded at compile time and
// pushed as raw PCM straight to the OS audio stack.
//
//   macOS   -> AudioToolbox AudioQueue (framework, always present)
//   Windows -> winmm waveOut            (dll, always present)
//   Linux   -> ALSA libasound via dlopen (graceful no-op if absent)
//
// Remote (SSH) use: when Claude Code runs on a headless box you SSH into, set
// "remote":"127.0.0.1:<port>" in ~/.claude/.claude-notify.json there. The play
// hook then hands the request to a `claude-notify listen` daemon on the machine
// with speakers, reached through an SSH RemoteForward tunnel (rides the existing
// SSH connection — no extra ports, firewall-immune). If the tunnel/daemon isn't
// reachable it falls back to a terminal BEL/OSC over the TTY, which works
// whenever you have an SSH session. "remote":"bell" forces the bell-only path;
// "" (default) plays locally.
//
// Build: rustc --edition 2021 -O claude-notify.rs -o "$HOME/.claude/claude-notify"
// Test:  rustc --edition 2021 --test claude-notify.rs -o /tmp/cn-test && /tmp/cn-test
//
// Modes (first arg):
//   mark                    record this session's task-start time  (UserPromptSubmit hook)
//   play                    play now (local, remote, or bell per config)
//   play --threshold 60     play only if >=60s since this session's mark (Stop hook)
//   play --event question   pick the question sound instead of completed
//   listen [--port 47291]   run the local playback daemon (machine with speakers)
//   install-daemon [--port] keep `listen` running (macOS launchd / Linux systemd)
// Options: --vol <f> volume, --sound <name|path>, --remote <host:port|bell|"">.
// Session id is read from the hook JSON on stdin so parallel sessions don't
// clobber each other's start marker.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const VERSION: &str = "26.07.05.5";

// The only embedded sound: the default voice saying "claude", pre-rendered TTS
// baked to 16-bit PCM WAV, so no speech engine is needed at runtime and it
// sounds identical on every platform.
const CLAUDE_WAV: &[u8] = include_bytes!("claude.wav");

/// Resolve a sound name to WAV bytes: the built-in "claude", else a path to a
/// WAV file on disk (so you can point config at your own without recompiling).
fn load_sound(name: &str) -> Option<Vec<u8>> {
    match name {
        "claude" => Some(CLAUDE_WAV.to_vec()),
        path => fs::read(path).ok(),
    }
}

/// Decoded interleaved 16-bit PCM.
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------- WAV (PCM16) decode ----------------

/// Parse a canonical RIFF/WAVE PCM-16 file. Iterates chunks so filler chunks
/// (e.g. afconvert's "FLLR") between "fmt " and "data" are skipped. Returns None
/// for anything that isn't 16-bit integer PCM.
fn parse_wav(b: &[u8]) -> Option<Pcm> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let (mut channels, mut rate, mut bits) = (0u16, 0u32, 0u16);
    let mut fmt_pcm = false;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        let body_start = pos + 8;
        let body_end = body_start.checked_add(size)?;
        if body_end > b.len() {
            break;
        }
        let body = &b[body_start..body_end];
        if id == b"fmt " && body.len() >= 16 {
            fmt_pcm = u16::from_le_bytes([body[0], body[1]]) == 1;
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            data = Some(body);
        }
        pos = body_end + (size & 1); // chunks are word-aligned
    }
    if !fmt_pcm || bits != 16 || channels == 0 || rate == 0 {
        return None;
    }
    let d = data?;
    let mut samples = Vec::with_capacity(d.len() / 2);
    let mut i = 0;
    while i + 1 < d.len() {
        samples.push(i16::from_le_bytes([d[i], d[i + 1]]));
        i += 2;
    }
    Some(Pcm {
        sample_rate: rate,
        channels,
        samples,
    })
}

/// Scale amplitude in place; clamps so >1.0 gains don't wrap around.
fn scale(samples: &mut [i16], vol: f32) {
    if (vol - 1.0).abs() < 1e-3 {
        return;
    }
    for s in samples.iter_mut() {
        *s = (*s as f32 * vol).round().clamp(-32768.0, 32767.0) as i16;
    }
}

/// Play the named sound (built-in "claude" or a file path) at the given volume.
/// Falls back to the built-in voice if the name can't be loaded, so a bad config
/// never goes silent.
fn play_sound(name: &str, vol: f32) {
    let bytes = load_sound(name).or_else(|| load_sound("claude"));
    if let Some(b) = bytes {
        if let Some(mut pcm) = parse_wav(&b) {
            scale(&mut pcm.samples, vol);
            audio::play(&pcm);
        }
    }
}

// ---------------- user config (~/.claude/.claude-notify.json) ----------------

/// What to play for each event, how loud, and where. `remote`:
///   ""            play locally (default)
///   "host:port"   send to a `listen` daemon (usually 127.0.0.1:<port> via an
///                 SSH RemoteForward tunnel); bell fallback if unreachable
///   "bell"        terminal BEL/OSC only (no daemon needed)
struct Cfg {
    vol: f32,
    completed: String,
    question: String,
    remote: String,
}

/// Pull a bare number field ("vol": 1.5) out of the small config JSON.
fn extract_num(json: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{}\"", key);
    let idx = json.find(&pat)? + pat.len();
    let rest = &json[idx..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    num.parse().ok()
}

fn read_cfg() -> Cfg {
    let mut cfg = Cfg {
        vol: 2.0,
        completed: "claude".to_string(),
        question: "claude".to_string(),
        remote: String::new(),
    };
    if let Ok(s) = fs::read_to_string(home().join(".claude").join(".claude-notify.json")) {
        if let Some(v) = extract_num(&s, "vol") {
            cfg.vol = v as f32;
        }
        if let Some(c) = extract_str(&s, "completed") {
            cfg.completed = c;
        }
        if let Some(q) = extract_str(&s, "question") {
            cfg.question = q;
        }
        if let Some(r) = extract_str(&s, "remote") {
            cfg.remote = r;
        }
    }
    cfg
}

// ---------------- per-session start marker (long-task threshold) ----------------

fn marker_path(sid: &str) -> PathBuf {
    let safe: String = sid
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = if safe.is_empty() { "anon".to_string() } else { safe };
    home()
        .join(".claude")
        .join(".claude-notify")
        .join(format!("{}.json", name))
}

fn write_marker(sid: &str) {
    let p = marker_path(sid);
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&p, format!("{{\"t\":{:.3}}}", now_secs()));
}

fn read_marker(sid: &str) -> Option<f64> {
    let s = fs::read_to_string(marker_path(sid)).ok()?;
    let idx = s.find("\"t\"")? + 3;
    let rest = &s[idx..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    num.parse().ok()
}

/// Pull a string field out of the hook JSON without a full parser (values here
/// never contain escaped quotes).
fn extract_str(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\"", key);
    let idx = json.find(&pat)? + pat.len();
    let rest = &json[idx..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q = after.find('"')?;
    let s = &after[q + 1..];
    let end = s.find('"')?;
    Some(s[..end].to_string())
}

// ---------------- remote playback (SSH-tunnel daemon) + bell fallback ----------------

/// Hand the play request to a claude-notify daemon at `addr` (host:port), usually
/// 127.0.0.1:<port> reached through an SSH RemoteForward tunnel back to the
/// machine with speakers. Short timeouts so a missing daemon never hangs a hook.
/// Returns true only if the request was delivered.
fn send_remote(addr: &str, sound: &str, vol: f32) -> bool {
    use std::io::Write;
    use std::net::{TcpStream, ToSocketAddrs};
    let addrs = match addr.to_socket_addrs() {
        Ok(a) => a,
        Err(_) => return false,
    };
    for sa in addrs {
        if let Ok(mut s) = TcpStream::connect_timeout(&sa, Duration::from_millis(500)) {
            let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
            if s
                .write_all(format!("play\t{}\t{}\n", sound, vol).as_bytes())
                .is_ok()
            {
                let _ = s.flush();
                return true;
            }
        }
    }
    false
}

/// Ring the local terminal over the SSH TTY — works whenever you have an SSH
/// session, no ports, immune to firewall/forwarding config. BEL rings the bell;
/// OSC 9 adds a desktop notification on terminals that support it (e.g. iTerm2).
/// Written to the controlling terminal so Claude Code doesn't swallow it as
/// captured hook stdout.
fn bell() {
    use std::io::Write;
    let msg = "\x07\x1b]9;Claude Code \u{2014} done\x07";
    if let Ok(mut tty) = fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = tty.write_all(msg.as_bytes());
        let _ = tty.flush();
    } else {
        eprint!("{}", msg);
    }
}

/// Daemon: accept one-line "play\t<sound>\t<vol>" requests on 127.0.0.1:port and
/// play them locally. Runs on the machine with speakers (see install-daemon).
/// Network-sourced requests only ever play the built-in voice, never a path.
fn listen(port: u16) {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    let l = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("claude-notify listen: bind 127.0.0.1:{} failed: {}", port, e);
            std::process::exit(1);
        }
    };
    eprintln!("claude-notify v{} listening on 127.0.0.1:{}", VERSION, port);
    for conn in l.incoming().flatten() {
        std::thread::spawn(move || {
            let mut stream = conn;
            let _ = stream.set_read_timeout(Some(Duration::from_millis(1000)));
            let mut line = String::new();
            if BufReader::new(&mut stream).read_line(&mut line).is_ok() {
                let p: Vec<&str> = line.trim().split('\t').collect();
                if p.len() >= 3 && p[0] == "play" {
                    let vol: f32 = p[2].parse::<f32>().unwrap_or(2.0).clamp(0.0, 4.0);
                    play_sound("claude", vol);
                }
            }
        });
    }
}

fn self_bin() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            home()
                .join(".claude")
                .join("claude-notify")
                .to_string_lossy()
                .into_owned()
        })
}

#[cfg(target_os = "macos")]
fn install_daemon(port: u16) {
    use std::process::Command;
    let bin = self_bin();
    let dir = home().join("Library").join("LaunchAgents");
    let _ = fs::create_dir_all(&dir);
    let plist = dir.join("com.kinggunil.claude-notify.plist");
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
\t<key>Label</key><string>com.kinggunil.claude-notify</string>\n\
\t<key>ProgramArguments</key><array><string>{}</string><string>listen</string><string>--port</string><string>{}</string></array>\n\
\t<key>RunAtLoad</key><true/>\n\
\t<key>KeepAlive</key><true/>\n\
</dict></plist>\n",
        bin, port
    );
    if fs::write(&plist, body).is_err() {
        eprintln!("failed to write {}", plist.display());
        return;
    }
    let ps = plist.to_string_lossy().into_owned();
    let _ = Command::new("launchctl").arg("unload").arg(&ps).status();
    match Command::new("launchctl").arg("load").arg("-w").arg(&ps).status() {
        Ok(s) if s.success() => {
            println!("claude-notify daemon installed + loaded on 127.0.0.1:{}", port)
        }
        _ => println!("plist written to {}\n  run: launchctl load -w \"{}\"", ps, ps),
    }
}

#[cfg(target_os = "linux")]
fn install_daemon(port: u16) {
    use std::process::Command;
    let bin = self_bin();
    let dir = home().join(".config").join("systemd").join("user");
    let _ = fs::create_dir_all(&dir);
    let unit = dir.join("claude-notify.service");
    let body = format!(
        "[Unit]\n\
Description=claude-notify local playback daemon\n\
After=default.target\n\n\
[Service]\n\
ExecStart={} listen --port {}\n\
Restart=always\n\
RestartSec=2\n\n\
[Install]\n\
WantedBy=default.target\n",
        bin, port
    );
    if fs::write(&unit, body).is_err() {
        eprintln!("failed to write {}", unit.display());
        return;
    }
    let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
    match Command::new("systemctl")
        .args(["--user", "enable", "--now", "claude-notify.service"])
        .status()
    {
        Ok(s) if s.success() => println!(
            "claude-notify daemon enabled + started on 127.0.0.1:{}\n  \
             (headless? enable lingering: loginctl enable-linger \"$USER\")",
            port
        ),
        _ => println!(
            "unit written to {}\n  run: systemctl --user enable --now claude-notify.service",
            unit.display()
        ),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_daemon(port: u16) {
    println!(
        "auto-start isn't wired for this OS. Run the daemon at login yourself:\n  \
         claude-notify listen --port {}\n  \
         (Windows: put a shortcut to that command in shell:startup)",
        port
    );
}

// ---------------- platform audio backends ----------------

#[cfg(target_os = "macos")]
mod audio {
    use super::Pcm;
    use std::ffi::c_void;
    use std::time::Duration;

    #[repr(C)]
    struct Asbd {
        sample_rate: f64,
        format_id: u32,
        format_flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels_per_frame: u32,
        bits_per_channel: u32,
        reserved: u32,
    }

    #[repr(C)]
    struct AudioQueueBuffer {
        capacity: u32,
        audio_data: *mut c_void,
        byte_size: u32,
        user_data: *mut c_void,
        pd_capacity: u32,
        packet_descriptions: *mut c_void,
        pd_count: u32,
    }

    type AudioQueueRef = *mut c_void;
    type AudioQueueBufferRef = *mut AudioQueueBuffer;
    type Callback = extern "C" fn(*mut c_void, AudioQueueRef, AudioQueueBufferRef);

    #[link(name = "AudioToolbox", kind = "framework")]
    extern "C" {
        fn AudioQueueNewOutput(
            fmt: *const Asbd,
            cb: Callback,
            user: *mut c_void,
            run_loop: *const c_void,
            run_loop_mode: *const c_void,
            flags: u32,
            out: *mut AudioQueueRef,
        ) -> i32;
        fn AudioQueueAllocateBuffer(
            aq: AudioQueueRef,
            size: u32,
            out: *mut AudioQueueBufferRef,
        ) -> i32;
        fn AudioQueueEnqueueBuffer(
            aq: AudioQueueRef,
            buf: AudioQueueBufferRef,
            n: u32,
            pd: *const c_void,
        ) -> i32;
        fn AudioQueueStart(aq: AudioQueueRef, start: *const c_void) -> i32;
        fn AudioQueueStop(aq: AudioQueueRef, immediate: u8) -> i32;
        fn AudioQueueDispose(aq: AudioQueueRef, immediate: u8) -> i32;
    }

    extern "C" fn noop(_u: *mut c_void, _aq: AudioQueueRef, _b: AudioQueueBufferRef) {}

    pub fn play(pcm: &Pcm) {
        if pcm.samples.is_empty() {
            return;
        }
        unsafe {
            let ch = pcm.channels.max(1) as u32;
            let asbd = Asbd {
                sample_rate: pcm.sample_rate as f64,
                format_id: u32::from_be_bytes(*b"lpcm"),
                format_flags: 0xC, // signed integer | packed
                bytes_per_packet: ch * 2,
                frames_per_packet: 1,
                bytes_per_frame: ch * 2,
                channels_per_frame: ch,
                bits_per_channel: 16,
                reserved: 0,
            };
            let mut aq: AudioQueueRef = std::ptr::null_mut();
            if AudioQueueNewOutput(
                &asbd,
                noop,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut aq,
            ) != 0
                || aq.is_null()
            {
                return;
            }
            let bytes = pcm.samples.len() * 2;
            let mut buf: AudioQueueBufferRef = std::ptr::null_mut();
            if AudioQueueAllocateBuffer(aq, bytes as u32, &mut buf) != 0 || buf.is_null() {
                AudioQueueDispose(aq, 1);
                return;
            }
            std::ptr::copy_nonoverlapping(
                pcm.samples.as_ptr() as *const u8,
                (*buf).audio_data as *mut u8,
                bytes,
            );
            (*buf).byte_size = bytes as u32;
            if AudioQueueEnqueueBuffer(aq, buf, 0, std::ptr::null()) != 0
                || AudioQueueStart(aq, std::ptr::null()) != 0
            {
                AudioQueueDispose(aq, 1);
                return;
            }
            let frames = pcm.samples.len() / ch as usize;
            let secs = frames as f64 / pcm.sample_rate.max(1) as f64;
            std::thread::sleep(Duration::from_millis((secs * 1000.0) as u64 + 300));
            AudioQueueStop(aq, 1);
            AudioQueueDispose(aq, 1);
        }
    }
}

#[cfg(target_os = "windows")]
mod audio {
    use super::Pcm;
    use std::ffi::c_void;
    use std::time::Duration;

    #[repr(C)]
    struct WaveFormatEx {
        format_tag: u16,
        channels: u16,
        samples_per_sec: u32,
        avg_bytes_per_sec: u32,
        block_align: u16,
        bits_per_sample: u16,
        cb_size: u16,
    }

    #[repr(C)]
    struct WaveHdr {
        data: *mut u8,
        buffer_length: u32,
        bytes_recorded: u32,
        user: usize,
        flags: u32,
        loops: u32,
        next: *mut c_void,
        reserved: usize,
    }

    type Hwaveout = *mut c_void;
    const WAVE_MAPPER: usize = 0xFFFF_FFFF; // (UINT)-1
    const WHDR_DONE: u32 = 0x0000_0001;
    const WAVE_FORMAT_PCM: u16 = 1;

    #[link(name = "winmm")]
    extern "system" {
        fn waveOutOpen(
            out: *mut Hwaveout,
            device: usize,
            fmt: *const WaveFormatEx,
            cb: usize,
            inst: usize,
            flags: u32,
        ) -> u32;
        fn waveOutPrepareHeader(h: Hwaveout, hdr: *mut WaveHdr, size: u32) -> u32;
        fn waveOutWrite(h: Hwaveout, hdr: *mut WaveHdr, size: u32) -> u32;
        fn waveOutUnprepareHeader(h: Hwaveout, hdr: *mut WaveHdr, size: u32) -> u32;
        fn waveOutClose(h: Hwaveout) -> u32;
    }

    pub fn play(pcm: &Pcm) {
        if pcm.samples.is_empty() {
            return;
        }
        unsafe {
            let ch = pcm.channels.max(1);
            let block_align = ch * 2;
            let fmt = WaveFormatEx {
                format_tag: WAVE_FORMAT_PCM,
                channels: ch,
                samples_per_sec: pcm.sample_rate,
                avg_bytes_per_sec: pcm.sample_rate * block_align as u32,
                block_align,
                bits_per_sample: 16,
                cb_size: 0,
            };
            let mut h: Hwaveout = std::ptr::null_mut();
            if waveOutOpen(&mut h, WAVE_MAPPER, &fmt, 0, 0, 0) != 0 || h.is_null() {
                return;
            }
            let mut bytes: Vec<u8> = Vec::with_capacity(pcm.samples.len() * 2);
            for s in &pcm.samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let mut hdr = WaveHdr {
                data: bytes.as_mut_ptr(),
                buffer_length: bytes.len() as u32,
                bytes_recorded: 0,
                user: 0,
                flags: 0,
                loops: 0,
                next: std::ptr::null_mut(),
                reserved: 0,
            };
            let sz = std::mem::size_of::<WaveHdr>() as u32;
            if waveOutPrepareHeader(h, &mut hdr, sz) != 0 {
                waveOutClose(h);
                return;
            }
            if waveOutWrite(h, &mut hdr, sz) != 0 {
                waveOutUnprepareHeader(h, &mut hdr, sz);
                waveOutClose(h);
                return;
            }
            let frames = pcm.samples.len() / ch as usize;
            let cap = (frames as f64 / pcm.sample_rate.max(1) as f64 * 1000.0) as u64 + 500;
            let mut waited = 0u64;
            while (hdr.flags & WHDR_DONE) == 0 && waited < cap {
                std::thread::sleep(Duration::from_millis(20));
                waited += 20;
            }
            waveOutUnprepareHeader(h, &mut hdr, sz);
            waveOutClose(h);
        }
    }
}

#[cfg(target_os = "linux")]
mod audio {
    use super::Pcm;
    use std::ffi::c_void;
    use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong};

    #[link(name = "dl")]
    extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }
    const RTLD_NOW: c_int = 2;
    const SND_PCM_STREAM_PLAYBACK: c_int = 0;
    const SND_PCM_FORMAT_S16_LE: c_int = 2;
    const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;

    type OpenFn = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
    type SetParamsFn =
        unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int;
    type WriteiFn = unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long;
    type SimpleFn = unsafe extern "C" fn(*mut c_void) -> c_int;

    unsafe fn sym<T>(lib: *mut c_void, name: &[u8]) -> Option<T> {
        let p = dlsym(lib, name.as_ptr() as *const c_char);
        if p.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy::<*mut c_void, T>(&p))
        }
    }

    pub fn play(pcm: &Pcm) {
        if pcm.samples.is_empty() {
            return;
        }
        unsafe {
            // try the versioned soname first, then the -dev symlink name
            let mut lib = dlopen(b"libasound.so.2\0".as_ptr() as *const c_char, RTLD_NOW);
            if lib.is_null() {
                lib = dlopen(b"libasound.so\0".as_ptr() as *const c_char, RTLD_NOW);
            }
            if lib.is_null() {
                return; // ALSA unavailable — silent no-op, never break the hook
            }
            let open: Option<OpenFn> = sym(lib, b"snd_pcm_open\0");
            let set_params: Option<SetParamsFn> = sym(lib, b"snd_pcm_set_params\0");
            let writei: Option<WriteiFn> = sym(lib, b"snd_pcm_writei\0");
            let drain: Option<SimpleFn> = sym(lib, b"snd_pcm_drain\0");
            let close: Option<SimpleFn> = sym(lib, b"snd_pcm_close\0");
            let (open, set_params, writei, drain, close) =
                match (open, set_params, writei, drain, close) {
                    (Some(a), Some(b), Some(c), Some(d), Some(e)) => (a, b, c, d, e),
                    _ => {
                        dlclose(lib);
                        return;
                    }
                };

            let ch = pcm.channels.max(1) as c_uint;
            let mut handle: *mut c_void = std::ptr::null_mut();
            if open(
                &mut handle,
                b"default\0".as_ptr() as *const c_char,
                SND_PCM_STREAM_PLAYBACK,
                0,
            ) < 0
                || handle.is_null()
            {
                dlclose(lib);
                return;
            }
            if set_params(
                handle,
                SND_PCM_FORMAT_S16_LE,
                SND_PCM_ACCESS_RW_INTERLEAVED,
                ch,
                pcm.sample_rate,
                1,
                500_000,
            ) >= 0
            {
                let frames = (pcm.samples.len() / ch as usize) as c_ulong;
                writei(handle, pcm.samples.as_ptr() as *const c_void, frames);
                drain(handle);
            }
            close(handle);
            dlclose(lib);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod audio {
    use super::Pcm;
    pub fn play(_pcm: &Pcm) {}
}

// ---------------- main ----------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");

    let mut threshold = 0f64;
    let mut vol_override: Option<f32> = None;
    let mut sound_override: Option<String> = None;
    let mut remote_override: Option<String> = None;
    let mut event = "completed".to_string();
    let mut port: u16 = 47291;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                threshold = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
                i += 2;
            }
            "--vol" => {
                vol_override = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--sound" => {
                sound_override = args.get(i + 1).cloned();
                i += 2;
            }
            "--remote" => {
                remote_override = args.get(i + 1).cloned();
                i += 2;
            }
            "--event" => {
                if let Some(e) = args.get(i + 1) {
                    event = e.clone();
                }
                i += 2;
            }
            "--port" => {
                port = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(47291);
                i += 2;
            }
            _ => i += 1,
        }
    }

    match mode {
        "listen" => {
            listen(port);
            return;
        }
        "install-daemon" => {
            install_daemon(port);
            return;
        }
        "--version" | "version" => {
            println!("claude-notify v{}", VERSION);
            return;
        }
        _ => {}
    }

    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let sid = extract_str(&input, "session_id").unwrap_or_else(|| "anon".to_string());

    match mode {
        "mark" => write_marker(&sid),
        "play" => {
            if threshold > 0.0 {
                // fail open: no marker (e.g. first task after install) -> play.
                if let Some(started) = read_marker(&sid) {
                    if now_secs() - started < threshold {
                        return;
                    }
                }
            }
            let cfg = read_cfg();
            let sound = sound_override.unwrap_or_else(|| match event.as_str() {
                "question" => cfg.question,
                _ => cfg.completed,
            });
            let vol = vol_override.unwrap_or(cfg.vol);
            let remote = remote_override
                .or_else(|| std::env::var("CLAUDE_NOTIFY_REMOTE").ok())
                .unwrap_or(cfg.remote);

            if remote.is_empty() {
                play_sound(&sound, vol); // local machine with speakers
            } else if remote == "bell" {
                bell(); // terminal signal only
            } else if !send_remote(&remote, &sound, vol) {
                bell(); // tunnel/daemon unreachable -> fall back to the bell
            }
        }
        _ => eprintln!(
            "usage: claude-notify <mark|play|listen|install-daemon> \
             [--event completed|question] [--threshold SECS] \
             [--sound NAME|PATH] [--vol F] [--remote host:port|bell] [--port N]"
        ),
    }
}

// ---------------- tests: rustc --edition 2021 --test claude-notify.rs ----------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_wav(rate: u32, ch: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let block_align = ch * 2;
        let byte_rate = rate * block_align as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&ch.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&byte_rate.to_le_bytes());
        b.extend_from_slice(&block_align.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    #[test]
    fn parse_roundtrip() {
        let wav = make_wav(48000, 2, &[0, 1, -1, 32767, -32768]);
        let p = parse_wav(&wav).unwrap();
        assert_eq!(p.sample_rate, 48000);
        assert_eq!(p.channels, 2);
        assert_eq!(p.samples, vec![0, 1, -1, 32767, -32768]);
    }

    #[test]
    fn parse_rejects_non_pcm_and_garbage() {
        assert!(parse_wav(b"not a wav").is_none());
        assert!(parse_wav(&[]).is_none());
    }

    #[test]
    fn embedded_claude_is_valid_pcm16() {
        let bytes = load_sound("claude").expect("built-in claude must load");
        let p = parse_wav(&bytes).expect("embedded WAV must decode");
        assert!(p.sample_rate >= 8000);
        assert!(p.channels >= 1 && p.channels <= 2);
        assert!(!p.samples.is_empty());
    }

    #[test]
    fn unknown_sound_is_none_but_claude_loads() {
        assert!(load_sound("definitely-not-a-real-path.wav").is_none());
        assert!(load_sound("claude").is_some());
    }

    #[test]
    fn scale_clamps_and_no_ops_at_unity() {
        let mut s = vec![100i16, -100];
        scale(&mut s, 1.0);
        assert_eq!(s, vec![100, -100]);
        let mut s2 = vec![30000i16, -30000];
        scale(&mut s2, 2.0);
        assert_eq!(s2, vec![32767, -32768]);
    }

    #[test]
    fn extract_num_reads_vol() {
        assert_eq!(extract_num(r#"{"vol": 1.5}"#, "vol"), Some(1.5));
        assert_eq!(extract_num(r#"{"vol":2}"#, "vol"), Some(2.0));
        assert_eq!(extract_num(r#"{"x":1}"#, "vol"), None);
    }

    #[test]
    fn extract_reads_session_and_remote() {
        let j = r#"{"session_id":"abc-123","remote":"127.0.0.1:47291"}"#;
        assert_eq!(extract_str(j, "session_id").as_deref(), Some("abc-123"));
        assert_eq!(extract_str(j, "remote").as_deref(), Some("127.0.0.1:47291"));
        assert_eq!(extract_str(j, "missing"), None);
    }

    #[test]
    fn marker_path_sanitizes() {
        let p = marker_path("../../etc/passwd");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains('/'));
        assert!(name.ends_with(".json"));
    }

    #[test]
    fn send_remote_unreachable_is_false_fast() {
        assert!(!send_remote("127.0.0.1:1", "claude", 2.0));
    }
}
