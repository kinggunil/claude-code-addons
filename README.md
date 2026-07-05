# claude-code-addons

**English** · [한국어](README.ko.md)

Two zero-dependency **Rust** tools for [Claude Code](https://claude.com/claude-code),
installed together by one command:

1. **Status line** — a two-line CPU/RAM/VM + cost + weekly-quota-pace bar.
2. **claude-notify** — a cross-platform sound when a **long task finishes** or Claude
   **waits for your answer**. By default it speaks the word *"claude"*. Works over
   SSH too (plays on your local machine, not the remote box).

Both are single static binaries — no interpreter, no runtime dependencies, and
for `claude-notify` no `afplay`/`paplay`/PowerShell subprocess and no external
sound file (the voice is embedded at compile time).

---

## Install

### macOS / Linux

```sh
curl -sSfL https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.ps1 | iex
```

Then **restart Claude Code** (or start a new session).

The installer downloads the sources + the embedded voice, installs the Rust
toolchain via rustup if `rustc` is missing (macOS/Linux; on Windows install Rust
yourself first), compiles both binaries into `~/.claude`, writes a default
`~/.claude/.claude-notify.json`, and patches `~/.claude/settings.json` — adding
the `statusLine` block **and** three `claude-notify` hooks. Existing settings are
preserved and backed up to `settings.json.bak`; re-running is idempotent.

**Requirements:** `curl` and a C linker for the Rust compile — `cc`/`clang`/`gcc`
(Xcode Command Line Tools on macOS: `xcode-select --install`; `build-essential`
or `gcc` on Linux). On Windows: [Rust](https://rustup.rs) + a linker (VS Build
Tools "Desktop development with C++"), and Python for the automatic
`settings.json` patch (otherwise the installer prints the JSON to add by hand).

---

## 1. Status line

Two lines under the prompt, split by domain — your **Claude session** on top,
**this machine** below:

```
Opus 4.8 (1M) · ⚡xhigh · think on | 🟢 █░░░░░░░░░ 126.5K/1M (12.7%) | 5h  9% ⏳3h 54m | 7d 23% used · 5% over pace (8h24m) · ⏳5d 17h | 4h 31m · $2.74
CPU 10% · RAM 56% · VM 62% · 🏠 my-mac | mydir | v26.07.05 | claude --resume <sid>
```

On an SSH session the host segment turns orange and switches icon, so the whole
bottom line reads as "the box this session is really on":

```
CPU 10% · RAM 56% · VM  0% · 🌐 ip-10-0-1-23 | mydir | v26.07.05 | claude --resume <sid>
```

- **Line 1 — Claude session**: model, effort, thinking toggle, context gauge,
  then Claude.ai rate-limit usage (Pro/Max) — each window as `used%` plus a reset
  countdown behind an ⏳. The 7-day window adds a **weekly pace** in plain words:
  `N% over pace` (burning faster than the even Saturday-04:00-KST reset line) or
  `N% under pace` (headroom), colored by how much slack you have and annotated
  with that gap re-expressed as time; then elapsed time and cost.
- **Line 2 — this machine**: CPU/RAM/VM first (the only live-changing values, so
  they get the leftmost first-read spot), then the machine **host** (🏠 cyan =
  local, 🌐 orange = SSH remote — the hostname comes from wherever the status line
  runs, so under SSH it names the remote box), the working dir, a `vYY.MM.DD`
  build tag (compare it against the repo to spot stale machines), and the full
  `claude --resume` command at the far right.

All stats come from direct syscalls (macOS `mach`/`sysctl`, Linux `/proc`,
Windows `kernel32`) — no subprocesses, ~2–5 ms per refresh.

---

## 2. claude-notify

### When it makes a sound

| Event | Hook | Behaviour |
|-------|------|-----------|
| **Task completed** | `Stop` | plays only if the task took **≥ 60 s** (short tasks stay silent) |
| **Waiting for your answer** | `PreToolUse` / `AskUserQuestion` | plays **immediately** |
| (prompt submitted) | `UserPromptSubmit` | records the start time for the 60 s threshold |

### Configure — `~/.claude/.claude-notify.json`

No recompile, no hook edits — just change this file:

```json
{
  "vol": 2.0,
  "completed": "claude",
  "question": "claude",
  "remote": ""
}
```

- **`vol`** — linear volume. The embedded voice peaks at ~56 % full-scale, so
  ~`1.8` is the loudest clean value; higher gets louder with mild clipping.
  Default `2.0`.
- **`completed` / `question`** — the sound for each event: the built-in
  `claude`, or an **absolute path to your own 16-bit PCM `.wav`**. You can set
  them differently.
- **`remote`** — where to play (see below). `""` = play on this machine.

### CLI

```
claude-notify mark                                   # record task start
claude-notify play --event completed --threshold 60  # play if >=60s since mark
claude-notify play --event question                  # play now
claude-notify play --sound /path/to.wav --vol 1.5    # explicit overrides
claude-notify listen [--port 47291]                  # local playback daemon (see SSH)
claude-notify install-daemon [--port 47291]          # keep the daemon running
claude-notify --version
```

---

## Using it over SSH (e.g. an EC2 box)

If you **SSH into a server and run Claude Code there**, the hooks fire on the
server — which is headless and has no speakers, and SSH doesn't forward audio.
So the sound has to be sent back to your local machine. Two options, and they
combine:

### The signal rides your existing SSH connection (no firewall changes)

`claude-notify` on the server can hand the play request to a small **daemon on
your local machine** through an **SSH `RemoteForward` tunnel**. That tunnel
travels *inside* the SSH connection you already have — only port 22 crosses the
network, both ends are `127.0.0.1`, so **no security-group / firewall / NAT
changes are needed**. If the tunnel or daemon isn't reachable, it falls back to
a **terminal bell / desktop-notification** over the TTY, which works whenever you
have an SSH session at all.

### Setup

**1. On your local machine (with speakers)** — run the daemon and keep it up:

```sh
claude-notify install-daemon          # macOS launchd / Linux systemd --user
# or just run it in a terminal:  claude-notify listen --port 47291
```

**2. In your `~/.ssh/config`** — tunnel the port back to your machine for that host:

```
Host my-ec2
  HostName 1.2.3.4
  User ubuntu
  RemoteForward 47291 127.0.0.1:47291
```

**3. On the server** — install claude-code-addons there too, then set `remote`
in its `~/.claude/.claude-notify.json`:

```json
{ "vol": 2.0, "completed": "claude", "question": "claude", "remote": "127.0.0.1:47291" }
```

Now when a long task finishes on the server, your **local machine speaks
"claude"**. If the daemon is down or forwarding is disabled, you still get a
terminal bell.

**Bell only** (no daemon, zero setup): set `"remote": "bell"` on the server —
it just rings your terminal / posts a desktop notification via the TTY.

**Prerequisites** (not firewalls): the server's `sshd` must allow TCP forwarding
(`AllowTcpForwarding yes`, the default), and the daemon must be running on your
local machine (`install-daemon` keeps it up across reboots/logins).

---

## Platform support

| | Status line | claude-notify playback | daemon auto-start |
|---|---|---|---|
| **macOS** | `mach`/`sysctl` | AudioToolbox AudioQueue | launchd |
| **Linux / Ubuntu** | `/proc` | ALSA (`libasound`, via `dlopen`) | systemd `--user` |
| **Windows** | `kernel32` | winmm `waveOut` | manual (Startup shortcut) |

The status line and `claude-notify` are developed and end-to-end tested on
macOS; the Windows and Linux code paths compile and follow each OS's native
audio API but are less battle-tested — please report issues. On a headless Linux
server without ALSA, local playback is a silent no-op (that's the SSH-remote case
above — use the tunnel/bell).

---

## Uninstall

Remove the `statusLine` block and the three `claude-notify` hooks from
`~/.claude/settings.json` (or restore `settings.json.bak`), then:

```sh
rm -f ~/.claude/claude-notify ~/.claude/claude-notify.rs ~/.claude/claude.wav \
      ~/.claude/statusline-rs ~/.claude/statusline.rs ~/.claude/.claude-notify.json \
      ~/.claude/.statusline-cpu.json
rm -rf ~/.claude/.claude-notify
# if you installed the daemon:
launchctl unload ~/Library/LaunchAgents/com.kinggunil.claude-notify.plist  # macOS
systemctl --user disable --now claude-notify.service                       # Linux
```

## Notes

- The `claude` voice is macOS text-to-speech, bundled as convenience audio for
  personal use.
- Only `~/.claude/settings.json` (statusLine + hooks) has to exist on each
  machine; the installer handles it. Everything else is machine-local.
- Bump `VERSION` in each `.rs` on changes so the status line's on-screen tag
  reveals which machines are stale.
