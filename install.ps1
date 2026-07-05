# claude-code-addons — Windows installer (PowerShell).
# Run:  irm https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.ps1 | iex
# Installs the status line + claude-notify chime into %USERPROFILE%\.claude.
# Requires: Rust (rustc) and a linker (MSVC Build Tools or the GNU toolchain).
$ErrorActionPreference = "Stop"
$repo = "https://raw.githubusercontent.com/kinggunil/claude-code-addons/main"
$dir  = Join-Path $env:USERPROFILE ".claude"
New-Item -ItemType Directory -Force -Path $dir | Out-Null

$slSrc = Join-Path $dir "statusline.rs"
$cnSrc = Join-Path $dir "claude-notify.rs"
$wav   = Join-Path $dir "claude.wav"
$slBin = Join-Path $dir "statusline-rs.exe"
$cnBin = Join-Path $dir "claude-notify.exe"
$settings = Join-Path $dir "settings.json"
$cfg = Join-Path $dir ".claude-notify.json"

Write-Host "==> fetching sources"
Invoke-WebRequest -UseBasicParsing "$repo/statusline.rs"    -OutFile $slSrc
Invoke-WebRequest -UseBasicParsing "$repo/claude-notify.rs" -OutFile $cnSrc
Invoke-WebRequest -UseBasicParsing "$repo/claude.wav"       -OutFile $wav

if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
  Write-Host "error: rustc not found. Install Rust (https://rustup.rs or 'winget install Rustlang.Rustup')"
  Write-Host "       plus a linker (VS Build Tools 'Desktop development with C++'), then re-run."
  exit 1
}
Write-Host ("==> " + (rustc --version))

Write-Host "==> compiling status line  -> $slBin"
rustc --edition 2021 -O $slSrc -o $slBin
Write-Host "==> compiling claude-notify -> $cnBin"
rustc --edition 2021 -O $cnSrc -o $cnBin

if (-not (Test-Path $cfg)) {
  Write-Host "==> writing default config -> $cfg"
@'
{
  "vol": 2.0,
  "completed": "claude",
  "question": "claude",
  "remote": ""
}
'@ | Set-Content -Path $cfg -Encoding utf8
}

# ---- patch settings.json — reuse the Python patcher when available (robust),
#      else fall back to printing manual instructions.
Write-Host "==> updating settings.json"
$py = Get-Command python -ErrorAction SilentlyContinue
if (-not $py) { $py = Get-Command python3 -ErrorAction SilentlyContinue }
if ($py) {
$pyCode = @'
import json, os, sys, shutil
settings, sl_bin, cn_bin = sys.argv[1], sys.argv[2], sys.argv[3]
cfg = {}
if os.path.exists(settings) and os.path.getsize(settings) > 0:
    try:
        cfg = json.load(open(settings))
    except Exception as e:
        sys.exit("settings.json invalid JSON (%s); aborting." % e)
    shutil.copy2(settings, settings + ".bak")
cfg["statusLine"] = {"type": "command", "command": sl_bin, "refreshInterval": 1}
hooks = cfg.setdefault("hooks", {})
def add(event, command, matcher=None, uniq=None):
    arr = hooks.setdefault(event, [])
    uniq = uniq or command
    for grp in arr:
        for h in grp.get("hooks", []):
            if "claude-notify" in h.get("command","") and uniq in h.get("command",""):
                return
    entry = {"hooks": [{"type": "command", "command": command}]}
    if matcher: entry["matcher"] = matcher
    arr.append(entry)
add("UserPromptSubmit", '"%s" mark' % cn_bin, uniq=" mark")
add("Stop", '"%s" play --event completed --threshold 60' % cn_bin, uniq="--event completed")
add("PreToolUse", '"%s" play --event question' % cn_bin, matcher="AskUserQuestion", uniq="--event question")
json.dump(cfg, open(settings, "w"), indent=2)
open(settings, "a").write("\n")
print("    ok")
'@
  $pyCode | & $py.Source - $settings $slBin $cnBin
} else {
  Write-Host "    Python not found. Add these to $settings manually (keep existing keys):"
  Write-Host "      statusLine.command = `"$slBin`"  (type command, refreshInterval 1)"
  Write-Host "      UserPromptSubmit hook: `"$cnBin`" mark"
  Write-Host "      Stop hook:             `"$cnBin`" play --event completed --threshold 60"
  Write-Host "      PreToolUse (matcher AskUserQuestion): `"$cnBin`" play --event question"
}

Write-Host ("==> claude-notify: " + (& $cnBin --version))
Write-Host "==> done. Restart Claude Code to activate."
Write-Host "==> test the chime:  '{}' | & '$cnBin' play"
