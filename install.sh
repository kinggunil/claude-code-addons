#!/usr/bin/env sh
# Install both Claude Code add-ons into ~/.claude:
#   1. status line   (statusline.rs   -> statusline-rs)
#   2. claude-notify  (claude-notify.rs -> claude-notify)  completion/question chime
# Fetches sources + the embedded voice, installs the Rust toolchain if missing,
# compiles both, writes a default config, and patches ~/.claude/settings.json
# (statusLine block + three hooks; existing settings preserved, idempotent,
# backup written). Requires: curl. macOS / Linux. On Windows use install.ps1.
set -e

REPO_RAW="https://raw.githubusercontent.com/kinggunil/claude-code-addons/main"
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
echo "==> fetching sources"
curl --proto '=https' --tlsv1.2 -sSfL "$REPO_RAW/statusline.rs"    -o "$SL_SRC"
curl --proto '=https' --tlsv1.2 -sSfL "$REPO_RAW/claude-notify.rs" -o "$CN_SRC"
curl --proto '=https' --tlsv1.2 -sSfL "$REPO_RAW/claude.wav"       -o "$WAV"

# ---- 2. rust toolchain (install via rustup if missing) ----
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v rustc >/dev/null 2>&1; then
  echo "==> rustc not found — installing Rust (rustup, minimal, ~1-2 min)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
  . "$HOME/.cargo/env"
fi
echo "==> $(rustc --version)"

# ---- 3. compile both ----
echo "==> compiling status line  -> $SL_BIN"
rustc --edition 2021 -O "$SL_SRC" -o "$SL_BIN"
echo "==> compiling claude-notify -> $CN_BIN"
rustc --edition 2021 -O "$CN_SRC" -o "$CN_BIN"

# ---- 4. default config (never clobber an existing one) ----
if [ ! -f "$CFG" ]; then
  echo "==> writing default config -> $CFG"
  printf '{\n  "vol": 2.0,\n  "completed": "claude",\n  "question": "claude",\n  "remote": ""\n}\n' > "$CFG"
else
  echo "==> keeping existing $CFG"
fi

# ---- 5. patch settings.json: statusLine key + claude-notify hooks ----
echo "==> updating settings.json"
if command -v python3 >/dev/null 2>&1; then
  python3 - "$SETTINGS" "$SL_BIN" "$CN_BIN" <<'PY'
import json, os, sys, shutil
settings, sl_bin, cn_bin = sys.argv[1], sys.argv[2], sys.argv[3]
cfg = {}
if os.path.exists(settings) and os.path.getsize(settings) > 0:
    try:
        cfg = json.load(open(settings))
    except Exception as e:
        sys.exit("    existing settings.json is not valid JSON (%s); aborting so it isn't overwritten." % e)
    shutil.copy2(settings, settings + ".bak")
cfg["statusLine"] = {"type": "command", "command": sl_bin, "refreshInterval": 1}
hooks = cfg.setdefault("hooks", {})
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
json.dump(cfg, open(settings, "w"), indent=2)
open(settings, "a").write("\n")
print("    ok" + (" (backup: %s.bak)" % settings if os.path.exists(settings + ".bak") else " (created)"))
PY
elif command -v node >/dev/null 2>&1; then
  SETTINGS="$SETTINGS" SL_BIN="$SL_BIN" CN_BIN="$CN_BIN" node -e '
    const fs=require("fs"), p=process.env.SETTINGS, sl=process.env.SL_BIN, cn=process.env.CN_BIN;
    let cfg={};
    if (fs.existsSync(p) && fs.statSync(p).size>0){
      try{cfg=JSON.parse(fs.readFileSync(p,"utf8"));}
      catch(e){console.error("    settings.json invalid JSON; aborting.");process.exit(1);}
      fs.copyFileSync(p,p+".bak");
    }
    cfg.statusLine={type:"command",command:sl,refreshInterval:1};
    const hooks=cfg.hooks||(cfg.hooks={});
    const add=(ev,cmd,matcher,uniq)=>{
      const arr=hooks[ev]||(hooks[ev]=[]);
      for(const g of arr)for(const h of (g.hooks||[]))
        if((h.command||"").includes("claude-notify")&&(h.command||"").includes(uniq))return;
      const e={hooks:[{type:"command",command:cmd}]}; if(matcher)e.matcher=matcher; arr.push(e);
    };
    add("UserPromptSubmit",`${cn} mark`,null,"claude-notify mark");
    add("Stop",`${cn} play --event completed --threshold 60`,null,"--event completed");
    add("PreToolUse",`${cn} play --event question`,"AskUserQuestion","--event question");
    fs.writeFileSync(p,JSON.stringify(cfg,null,2)+"\n");
    console.log("    ok");
  '
else
  echo "    neither python3 nor node found — add the statusLine block and three claude-notify hooks to $SETTINGS manually (see README)."
fi

# ---- 6. verify (no sound: status line renders a mock, claude-notify prints version) ----
echo "==> status line:"
echo '{}' | "$SL_BIN"
echo
echo "==> claude-notify: $("$CN_BIN" --version)"
echo "==> test the chime:  printf '{}' | \"$CN_BIN\" play"
echo "==> done. Restart Claude Code (or start a new session) to activate everything."
