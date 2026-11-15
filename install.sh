#!/usr/bin/env sh
# Install both Claude Code add-ons into ~/.claude:
#   1. status line   (statusline.rs   -> statusline-rs)
#   2. claude-notify  (claude-notify.rs -> claude-notify)  completion/question chime
# Fetches sources + the embedded voice, installs the Rust toolchain if missing,
# compiles both, writes a default config, and patches ~/.claude/settings.json
# (statusLine block + four hooks; existing settings preserved, idempotent, a
# backup written only when the file actually changes). Requires: curl.
# macOS / Linux. On Windows use install.ps1.
#
# Knobs (environment variables), mainly for CI and offline installs:
#   CCA_LOCAL_SRC=<dir>          copy statusline.rs / claude-notify.rs / claude.wav
#                                from this directory instead of downloading them
#   CCA_NO_DAEMON=1              don't start the playback daemon on a speaker machine
#   CCA_JSON_TOOL=python3|node   force the tool that patches settings.json
#   REPO_RAW=<url>               download from another branch or fork
#
# The whole script is one function called on the last line, so a download that
# is cut off halfway can't run half a script.
set -e

main() {
  REPO_RAW="${REPO_RAW:-https://raw.githubusercontent.com/kinggunil/claude-code-addons/main}"
  CLAUDE_DIR="$HOME/.claude"
  SL_SRC="$CLAUDE_DIR/statusline.rs"
  SL_BIN="$CLAUDE_DIR/statusline-rs"
  CN_SRC="$CLAUDE_DIR/claude-notify.rs"
  CN_BIN="$CLAUDE_DIR/claude-notify"
  WAV="$CLAUDE_DIR/claude.wav"
  SETTINGS="$CLAUDE_DIR/settings.json"
  CFG="$CLAUDE_DIR/.claude-notify.json"

  mkdir -p "$CLAUDE_DIR"

  # ---- 0. preflight: a C linker (rustc needs cc/clang/gcc to link) ----
  if ! command -v cc >/dev/null 2>&1 \
     && ! command -v clang >/dev/null 2>&1 \
     && ! command -v gcc >/dev/null 2>&1; then
    echo "error: no C linker found — rustc needs one to build the binaries." >&2
    case "$(uname -s)" in
      Darwin) echo "  fix: xcode-select --install" >&2 ;;
      Linux)  echo "  fix (Debian/Ubuntu): sudo apt-get install -y build-essential" >&2
              echo "  fix (Fedora/RHEL):   sudo dnf install -y gcc" >&2
              echo "  fix (Alpine):        sudo apk add build-base" >&2 ;;
      *)      echo "  install a C toolchain (cc/clang/gcc), then re-run." >&2 ;;
    esac
    exit 1
  fi

  # ---- 1. fetch sources + the embedded voice ----
  fetch() { # $1 file name in the repo, $2 destination
    if [ -n "${CCA_LOCAL_SRC:-}" ]; then
      cp "$CCA_LOCAL_SRC/$1" "$2"
    else
      curl --proto '=https' --tlsv1.2 -sSfL "$REPO_RAW/$1" -o "$2"
    fi
  }
  echo "==> fetching sources"
  fetch statusline.rs    "$SL_SRC"
  fetch claude-notify.rs "$CN_SRC"
  fetch claude.wav       "$WAV"

  # ---- 2. rust toolchain (install via rustup if missing) ----
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! command -v rustc >/dev/null 2>&1; then
    echo "==> rustc not found — installing Rust (rustup, minimal, ~1-2 min)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
    . "$HOME/.cargo/env"
  fi
  RUSTC_VER="$(rustc --version)"   # fails fast here if rustup has no default toolchain
  echo "==> $RUSTC_VER"

  # ---- 3. compile both ----
  # Build into a temp file beside the target, then rename it into place. The copy
  # that may be running right now (Claude Code starts the status line every second;
  # a `listen` daemon never exits) keeps its old inode, and nothing can ever exec a
  # half-written binary. install.ps1 does the same on Windows.
  build() { # $1 source, $2 binary
    tmp="$2.$$.tmp"
    rustc --edition 2021 -O "$1" -o "$tmp"
    mv -f "$tmp" "$2"
  }
  echo "==> compiling status line  -> $SL_BIN"
  build "$SL_SRC" "$SL_BIN"
  echo "==> compiling claude-notify -> $CN_BIN"
  build "$CN_SRC" "$CN_BIN"

  # ---- 4. default config (never clobber an existing one) ----
  # Auto-pick "remote": a headless / SSH box forwards its chime to a speaker
  # machine's daemon over the SSH RemoteForward tunnel; a machine with its own
  # speakers plays locally (""). Headless when we're inside an SSH session
  # ($SSH_CONNECTION) or Linux reports no ALSA sound card.
  is_headless() {
    [ -n "$SSH_CONNECTION" ] && return 0             # installing over SSH -> remote box
    [ "$(uname -s)" = "Linux" ] || return 1          # local macOS/other -> has speakers
    { [ -r /proc/asound/cards ] && grep -q '\[' /proc/asound/cards 2>/dev/null; } && return 1
    return 0                                          # Linux, no sound card -> headless
  }
  if is_headless; then REMOTE_DEFAULT="127.0.0.1:47291"; else REMOTE_DEFAULT=""; fi

  if [ ! -f "$CFG" ]; then
    echo "==> writing default config -> $CFG (remote=\"$REMOTE_DEFAULT\")"
    printf '{\n  "vol": 2.0,\n  "completed": "claude",\n  "question": "claude",\n  "remote": "%s"\n}\n' "$REMOTE_DEFAULT" > "$CFG"
  else
    echo "==> keeping existing $CFG"
  fi

  # ---- 4b. speaker machine: start the playback daemon automatically ----
  # (a headless box has nothing to play through — it forwards instead, so skip.)
  if [ -z "$REMOTE_DEFAULT" ] && [ -z "${CCA_NO_DAEMON:-}" ]; then
    echo "==> starting playback daemon (this machine has speakers)"
    "$CN_BIN" install-daemon || echo "    (daemon not started; run \"$CN_BIN install-daemon\" manually)"
  fi

  # ---- 5. patch settings.json: statusLine key + claude-notify hooks ----
  # Idempotent: a hook is added only if the event has no claude-notify hook yet,
  # and the file is rewritten (and backed up to settings.json.bak) only when
  # something actually changed — so a re-run never turns the backup into a copy
  # of the already-patched file, and never reformats a file it didn't touch.
  echo "==> updating settings.json"
  JSON_TOOL="${CCA_JSON_TOOL:-}"
  if [ -z "$JSON_TOOL" ]; then
    if command -v python3 >/dev/null 2>&1; then JSON_TOOL=python3
    elif command -v node >/dev/null 2>&1; then JSON_TOOL=node
    fi
  fi
  case "$JSON_TOOL" in
    python3)
      python3 - "$SETTINGS" "$SL_BIN" "$CN_BIN" <<'PY'
import json, os, sys, shutil
settings, sl_bin, cn_bin = sys.argv[1], sys.argv[2], sys.argv[3]
cfg, existed = {}, False
if os.path.exists(settings) and os.path.getsize(settings) > 0:
    try:
        with open(settings, encoding="utf-8") as f:
            cfg = json.load(f)
    except Exception as e:
        sys.exit("    existing settings.json is not valid JSON (%s); aborting so it isn't overwritten." % e)
    if not isinstance(cfg, dict):
        sys.exit("    existing settings.json is not a JSON object; aborting so it isn't overwritten.")
    existed = True
before = json.dumps(cfg, sort_keys=True)
cfg["statusLine"] = {"type": "command", "command": sl_bin, "refreshInterval": 1}
hooks = cfg.setdefault("hooks", {})
if not isinstance(hooks, dict):
    sys.exit("    'hooks' in settings.json is not a JSON object; aborting so it isn't overwritten.")
def add(event, command, matcher=None, uniq=None):
    arr = hooks.setdefault(event, [])
    uniq = uniq or command
    for grp in arr:
        for h in grp.get("hooks", []):
            if "claude-notify" in h.get("command", "") and uniq in h.get("command", ""):
                return
    entry = {"hooks": [{"type": "command", "command": command}]}
    if matcher:
        entry["matcher"] = matcher
    arr.append(entry)
add("UserPromptSubmit", "%s mark" % cn_bin, uniq="claude-notify mark")
add("Stop", "%s play --event completed --threshold 60" % cn_bin, uniq="--event completed")
add("PreToolUse", "%s play --event question" % cn_bin, matcher="AskUserQuestion", uniq="--event question")
add("Notification", "%s play --event question" % cn_bin, matcher="permission_prompt", uniq="--event question")
if existed and json.dumps(cfg, sort_keys=True) == before:
    print("    already up to date (left untouched)")
    sys.exit(0)
if existed:
    shutil.copy2(settings, settings + ".bak")
with open(settings, "w", encoding="utf-8") as f:
    json.dump(cfg, f, indent=2, ensure_ascii=False)   # keep non-ASCII text as typed
    f.write("\n")
print("    ok" + (" (backup: %s.bak)" % settings if existed else " (created)"))
PY
      ;;
    node)
      SETTINGS="$SETTINGS" SL_BIN="$SL_BIN" CN_BIN="$CN_BIN" node -e '
        const fs=require("fs"), p=process.env.SETTINGS, sl=process.env.SL_BIN, cn=process.env.CN_BIN;
        let cfg={}, existed=false;
        if (fs.existsSync(p) && fs.statSync(p).size>0){
          try{cfg=JSON.parse(fs.readFileSync(p,"utf8"));}
          catch(e){console.error("    existing settings.json is not valid JSON; aborting so it is not overwritten.");process.exit(1);}
          if(cfg===null||typeof cfg!=="object"||Array.isArray(cfg)){console.error("    existing settings.json is not a JSON object; aborting.");process.exit(1);}
          existed=true;
        }
        const stable=o=>JSON.stringify(o,(k,v)=>v&&typeof v==="object"&&!Array.isArray(v)?Object.keys(v).sort().reduce((a,c)=>(a[c]=v[c],a),{}):v);
        const before=stable(cfg);
        cfg.statusLine={type:"command",command:sl,refreshInterval:1};
        const hooks=cfg.hooks||(cfg.hooks={});
        if(typeof hooks!=="object"||Array.isArray(hooks)){console.error("    hooks in settings.json is not a JSON object; aborting.");process.exit(1);}
        const add=(ev,cmd,matcher,uniq)=>{
          const arr=hooks[ev]||(hooks[ev]=[]);
          for(const g of arr)for(const h of (g.hooks||[]))
            if((h.command||"").includes("claude-notify")&&(h.command||"").includes(uniq))return;
          const e={hooks:[{type:"command",command:cmd}]}; if(matcher)e.matcher=matcher; arr.push(e);
        };
        add("UserPromptSubmit",`${cn} mark`,null,"claude-notify mark");
        add("Stop",`${cn} play --event completed --threshold 60`,null,"--event completed");
        add("PreToolUse",`${cn} play --event question`,"AskUserQuestion","--event question");
        add("Notification",`${cn} play --event question`,"permission_prompt","--event question");
        if(existed&&stable(cfg)===before){console.log("    already up to date (left untouched)");process.exit(0);}
        if(existed)fs.copyFileSync(p,p+".bak");
        fs.writeFileSync(p,JSON.stringify(cfg,null,2)+"\n");
        console.log("    ok"+(existed?` (backup: ${p}.bak)`:" (created)"));
      '
      ;;
    *)
      echo "    neither python3 nor node found — add the statusLine block and the four claude-notify hooks to $SETTINGS manually (see README / settings.example.json)."
      ;;
  esac

  # ---- 6. verify (no sound: status line renders a mock, claude-notify prints version) ----
  echo "==> status line:"
  echo '{}' | "$SL_BIN"
  echo
  echo "==> claude-notify: $("$CN_BIN" --version)"
  echo "==> test the chime:  \"$CN_BIN\" play"
  if [ -n "$REMOTE_DEFAULT" ]; then
    echo "==> remote/headless box detected — chime is forwarded to $REMOTE_DEFAULT."
    echo "    On your LOCAL machine (with speakers): install this repo there (it auto-starts"
    echo "    the daemon), and add to that machine's ~/.ssh/config for this host:"
    echo "        RemoteForward 47291 127.0.0.1:47291"
    echo "    Then RECONNECT this SSH session so the tunnel is active."
  fi
  echo "==> done. Restart Claude Code (or start a new session) to activate everything."
}

main "$@"
