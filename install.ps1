# claude-code-addons - Windows installer (Windows PowerShell 5.1 or PowerShell 7+).
# Run:  irm https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.ps1 | iex
# Installs the status line + claude-notify chime into %USERPROFILE%\.claude.
# Requires: Rust (rustc) + a linker (MSVC Build Tools or the GNU toolchain), and
#           Git for Windows (Claude Code on Windows needs it anyway). No Python.
#
# Why settings.json gets commands like "C:/Users/me/.claude/statusline-rs.exe":
# Claude Code on Windows runs statusLine and hook commands through Git Bash, and
# bash drops unquoted backslashes. C:\Users\me\.claude\statusline-rs.exe turns
# into C:Usersme.claudestatusline-rs.exe, "command not found", and the status
# line silently never appears. So every command is written as a quoted
# forward-slash path, and the installer ends by running each one through Git
# Bash exactly as Claude Code will.
#
# Everything runs inside one script block: nothing leaks into your session, and
# a failure stops with an error message instead of closing the window (a bare
# 'exit' under 'irm | iex' would end your whole PowerShell session).

& {
$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'   # the 5.1 progress bar slows downloads a lot

$repo     = 'https://raw.githubusercontent.com/kinggunil/claude-code-addons/main'
$dir      = Join-Path $env:USERPROFILE '.claude'
$slSrc    = Join-Path $dir 'statusline.rs'
$cnSrc    = Join-Path $dir 'claude-notify.rs'
$wav      = Join-Path $dir 'claude.wav'
$slBin    = Join-Path $dir 'statusline-rs.exe'
$cnBin    = Join-Path $dir 'claude-notify.exe'
$settings = Join-Path $dir 'settings.json'
$cfg      = Join-Path $dir '.claude-notify.json'
$utf8     = New-Object System.Text.UTF8Encoding($false)   # UTF-8 without BOM
$inv      = [Globalization.CultureInfo]::InvariantCulture
$build    = $null

# ---------------- helpers ----------------

# Run a native program. Its stderr (rustc warnings, say) is not an error; a
# non-zero exit code is, and then its output is shown.
function Invoke-Native([string]$What, [scriptblock]$Run) {
  $ErrorActionPreference = 'Continue'
  $out = @(& $Run 2>&1 | ForEach-Object { "$_" })
  if ($LASTEXITCODE -ne 0) {
    $out | ForEach-Object { Write-Host "    $_" }
    throw "$What failed (exit code $LASTEXITCODE)"
  }
  $out
}

# Compile into a temp dir, then swap into place. Renaming works even while the
# old .exe is running (Claude Code starts the status line every second, and a
# 'listen' daemon keeps claude-notify open), where overwriting it would fail.
function Install-Binary([string]$What, [string]$Src, [string]$Bin) {
  $tmp = Join-Path $build ([IO.Path]::GetFileName($Bin))
  $null = Invoke-Native "compiling $What" { rustc --edition 2021 -O $Src -o $tmp }
  $leaf = [IO.Path]::GetFileName($Bin)
  Get-ChildItem -LiteralPath $dir -Filter "$leaf.*.old" -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue
  $old = "$Bin." + [guid]::NewGuid().ToString('N').Substring(0, 8) + '.old'
  if (Test-Path -LiteralPath $Bin) { Move-Item -LiteralPath $Bin -Destination $old -Force }
  Move-Item -LiteralPath $tmp -Destination $Bin -Force
  Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue   # still running: removed next time
}

# A Windows path the way Git Bash needs it: forward slashes inside double
# quotes, with the two characters bash still expands there ($ and `) escaped.
function ConvertTo-BashPath([string]$Path) {
  '"' + (($Path -replace '\\', '/') -replace '([$`])', '\$1') + '"'
}

# Minimal JSON writer: 2-space indent, key order kept, UTF-8 text as is.
# ConvertTo-Json is avoided because Windows PowerShell 5.1 indents it oddly and
# escapes characters such as ' < > & into \u0027-style sequences.
function Format-JsonString([string]$s) {
  $sb = New-Object System.Text.StringBuilder
  [void]$sb.Append('"')
  foreach ($ch in $s.ToCharArray()) {
    $c = [int]$ch
    if     ($c -eq 34) { [void]$sb.Append('\"') }
    elseif ($c -eq 92) { [void]$sb.Append('\\') }
    elseif ($c -eq 10) { [void]$sb.Append('\n') }
    elseif ($c -eq 13) { [void]$sb.Append('\r') }
    elseif ($c -eq 9)  { [void]$sb.Append('\t') }
    elseif ($c -lt 32) { [void]$sb.Append(('\u{0:x4}' -f $c)) }
    else               { [void]$sb.Append($ch) }
  }
  [void]$sb.Append('"')
  $sb.ToString()
}

function ConvertTo-JsonText($Value, [int]$Depth = 0) {
  if ($null -eq $Value)        { return 'null' }
  if ($Value -is [string])     { return (Format-JsonString $Value) }
  if ($Value -is [bool])       { if ($Value) { return 'true' } else { return 'false' } }
  if ($Value -is [datetime])   { return (Format-JsonString $Value.ToString('o', $inv)) }
  if ($Value -is [double] -or $Value -is [single]) { return $Value.ToString('R', $inv) }
  if ($Value -is [ValueType])  { return [Convert]::ToString($Value, $inv) }
  $pad = '  ' * ($Depth + 1)
  $end = '  ' * $Depth
  if ($Value -is [System.Management.Automation.PSCustomObject]) {
    $items = foreach ($p in $Value.PSObject.Properties) {
      $pad + (Format-JsonString $p.Name) + ': ' + (ConvertTo-JsonText $p.Value ($Depth + 1))
    }
    if (-not $items) { return '{}' }
    return "{`n" + (@($items) -join ",`n") + "`n$end}"
  }
  if ($Value -is [System.Collections.IEnumerable]) {
    $items = foreach ($e in $Value) { $pad + (ConvertTo-JsonText $e ($Depth + 1)) }
    if (-not $items) { return '[]' }
    return "[`n" + (@($items) -join ",`n") + "`n$end]"
  }
  Format-JsonString ([string]$Value)
}

function Set-JsonProperty($Object, [string]$Name, $Value) {
  if ($Object.PSObject.Properties[$Name]) { $Object.$Name = $Value }
  else { $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value }
}

# Add a claude-notify hook unless this event already has one (re-runs stay
# idempotent, and hooks from earlier installs are kept as they are).
function Add-NotifyHook($Hooks, [string]$HookEvent, [string]$Command, [string]$Uniq, [string]$Matcher) {
  $groups = @()
  if ($Hooks.PSObject.Properties[$HookEvent] -and $null -ne $Hooks.$HookEvent) { $groups = @($Hooks.$HookEvent) }
  foreach ($g in $groups) {
    if ($null -eq $g -or -not $g.PSObject.Properties['hooks']) { continue }
    foreach ($h in @($g.hooks)) {
      if ($null -eq $h -or -not $h.PSObject.Properties['command']) { continue }
      $c = [string]$h.command
      if ($c.Contains('claude-notify') -and $c.Contains($Uniq)) { return }
    }
  }
  $entry = [pscustomobject]@{}
  if ($Matcher) { $entry | Add-Member -NotePropertyName matcher -NotePropertyValue $Matcher }
  $entry | Add-Member -NotePropertyName hooks -NotePropertyValue @([pscustomobject]@{ type = 'command'; command = $Command })
  Set-JsonProperty $Hooks $HookEvent (@($groups) + $entry)
}

# The bash Claude Code uses: CLAUDE_CODE_GIT_BASH_PATH, else Git for Windows'
# bin\bash.exe next to git.exe. Never C:\Windows\System32\bash.exe (that is WSL).
function Find-GitBash {
  $candidates = New-Object System.Collections.Generic.List[string]
  if ($env:CLAUDE_CODE_GIT_BASH_PATH) { $candidates.Add($env:CLAUDE_CODE_GIT_BASH_PATH) }
  foreach ($git in @(Get-Command git.exe -All -ErrorAction SilentlyContinue)) {
    $d = Split-Path -Parent $git.Source
    foreach ($rel in '..\bin\bash.exe', '..\..\bin\bash.exe', 'bash.exe') { $candidates.Add((Join-Path $d $rel)) }
  }
  foreach ($root in @($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:LOCALAPPDATA)) {
    if ($root) {
      $candidates.Add((Join-Path $root 'Git\bin\bash.exe'))
      $candidates.Add((Join-Path $root 'Programs\Git\bin\bash.exe'))
    }
  }
  foreach ($p in $candidates) {
    if (Test-Path -LiteralPath $p -PathType Leaf) { return [IO.Path]::GetFullPath($p) }
  }
  $null
}

# Run one settings.json command line through Git Bash with '{}' on stdin, the
# same way Claude Code does, and return its exit code and plain-text output.
# stdin comes from a heredoc inside the script, not a PowerShell pipe: Windows
# PowerShell prepends a UTF-8 BOM when piping to a native program on a UTF-8
# console, and the BOM would make the status line's JSON parser fail.
function Invoke-ViaBash([string]$Bash, [string]$CommandLine) {
  $script = Join-Path ([IO.Path]::GetTempPath()) ('claude-addons-check-' + [guid]::NewGuid().ToString('N') + '.sh')
  $body = "exec 0<<'CLAUDE_ADDONS_STDIN'`n{}`nCLAUDE_ADDONS_STDIN`n" + $CommandLine + "`n"
  [IO.File]::WriteAllText($script, $body, $utf8)
  $ErrorActionPreference = 'Continue'
  $prev = $null
  try { $prev = [Console]::OutputEncoding; [Console]::OutputEncoding = $utf8 } catch { }
  try {
    $out = @(& $Bash --noprofile --norc ($script -replace '\\', '/') 2>&1 | ForEach-Object { "$_" }) -join "`n"
    $code = $LASTEXITCODE
  } finally {
    if ($prev) { try { [Console]::OutputEncoding = $prev } catch { } }
    Remove-Item -LiteralPath $script -Force -ErrorAction SilentlyContinue
  }
  [pscustomobject]@{ Code = $code; Output = ($out -replace '\x1b\[[0-9;]*m', '') }
}

# ---------------- install ----------------

try {
  if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
    throw ("rustc not found. Install Rust (https://rustup.rs or 'winget install Rustlang.Rustup') plus a linker " +
           "(VS Build Tools 'Desktop development with C++'), then re-run.")
  }
  Write-Host ('==> ' + ((Invoke-Native 'rustc --version' { rustc --version }) -join ' '))

  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  Write-Host '==> fetching sources'
  Invoke-WebRequest -UseBasicParsing "$repo/statusline.rs"    -OutFile $slSrc
  Invoke-WebRequest -UseBasicParsing "$repo/claude-notify.rs" -OutFile $cnSrc
  Invoke-WebRequest -UseBasicParsing "$repo/claude.wav"       -OutFile $wav

  $build = Join-Path ([IO.Path]::GetTempPath()) ('claude-addons-build-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force -Path $build | Out-Null
  Write-Host "==> compiling status line  -> $slBin"
  Install-Binary 'the status line' $slSrc $slBin
  Write-Host "==> compiling claude-notify -> $cnBin"
  Install-Binary 'claude-notify' $cnSrc $cnBin

  # Auto-pick "remote": an SSH/headless box forwards its chime to a speaker
  # machine's daemon over the SSH RemoteForward tunnel; a local desktop plays
  # locally (""). On Windows, being inside an SSH session is the headless signal.
  $remote = if ($env:SSH_CONNECTION) { '127.0.0.1:47291' } else { '' }
  if (-not (Test-Path -LiteralPath $cfg)) {
    Write-Host "==> writing default config -> $cfg (remote=`"$remote`")"
    $text = "{`n  `"vol`": 2.0,`n  `"completed`": `"claude`",`n  `"question`": `"claude`",`n  `"remote`": `"$remote`"`n}`n"
    [IO.File]::WriteAllText($cfg, $text, $utf8)
  } else {
    Write-Host "==> keeping existing $cfg"
  }

  # speaker machine (local desktop): start the playback daemon automatically
  if (-not $remote) {
    Write-Host '==> starting playback daemon (this machine has speakers)'
    try { & $cnBin install-daemon } catch { Write-Host "    (run '$cnBin install-daemon' manually)" }
  }

  # ---- settings.json: statusLine block + three claude-notify hooks ----
  Write-Host '==> updating settings.json'
  $conf = [pscustomobject]@{}
  if (Test-Path -LiteralPath $settings -PathType Leaf) {
    $raw = [IO.File]::ReadAllText($settings)
    if ($raw.Trim()) {
      try {
        if ('System.Text.Json.JsonDocument' -as [type]) {
          # PowerShell 7's ConvertFrom-Json also takes comments and trailing
          # commas. Insist on strict JSON so such a file is never rewritten
          # without them (Windows PowerShell 5.1's parser already rejects both).
          ([System.Text.Json.JsonDocument]::Parse($raw)).Dispose()
        }
        if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) {
          $conf = ConvertFrom-Json -InputObject $raw -DateKind String   # PowerShell 7.5+: keep date-like text as text
        } else {
          $conf = ConvertFrom-Json -InputObject $raw
        }
      } catch {
        throw "existing settings.json is not valid JSON ($($_.Exception.Message)); left it untouched."
      }
      if ($conf -isnot [System.Management.Automation.PSCustomObject]) {
        throw 'existing settings.json is not a JSON object; left it untouched.'
      }
      Copy-Item -LiteralPath $settings -Destination "$settings.bak" -Force
      Write-Host "    backup: $settings.bak"
    }
  }
  if ($conf.PSObject.Properties['hooks'] -and $null -ne $conf.hooks -and
      $conf.hooks -isnot [System.Management.Automation.PSCustomObject]) {
    throw "'hooks' in settings.json is not a JSON object; left the file untouched."
  }
  $slCmd = ConvertTo-BashPath $slBin
  $cnCmd = ConvertTo-BashPath $cnBin
  Set-JsonProperty $conf 'statusLine' ([pscustomobject]@{ type = 'command'; command = $slCmd; refreshInterval = 1 })
  if ($null -eq $conf.hooks) { Set-JsonProperty $conf 'hooks' ([pscustomobject]@{}) }
  Add-NotifyHook $conf.hooks 'UserPromptSubmit' "$cnCmd mark" ' mark' ''
  Add-NotifyHook $conf.hooks 'Stop' "$cnCmd play --event completed --threshold 60" '--event completed' ''
  Add-NotifyHook $conf.hooks 'PreToolUse' "$cnCmd play --event question" '--event question' 'AskUserQuestion'
  [IO.File]::WriteAllText($settings, (ConvertTo-JsonText $conf) + "`n", $utf8)
  $conf = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($settings))   # what Claude Code will read
  Write-Host "    statusLine: $($conf.statusLine.command)"

  # ---- run the commands the way Claude Code will ----
  Write-Host '==> checking the commands through Git Bash, as Claude Code runs them'
  $problems = 0
  $bash = Find-GitBash
  if (-not $bash) {
    $problems++
    Write-Host '    Git Bash not found. Claude Code on Windows runs hooks and the status line through it:' -ForegroundColor Yellow
    Write-Host '    install Git for Windows (https://git-scm.com/downloads/win) or set CLAUDE_CODE_GIT_BASH_PATH.' -ForegroundColor Yellow
  } else {
    $r = Invoke-ViaBash $bash $conf.statusLine.command
    if ($r.Code -eq 0 -and $r.Output.Trim() -and $r.Output -notmatch 'parse error') {
      Write-Host '    status line OK:'
      foreach ($line in ($r.Output.Trim() -split "`r?`n")) { Write-Host "      $line" }
    } else {
      $problems++
      Write-Host "    status line FAILED (exit $($r.Code)): $($conf.statusLine.command)" -ForegroundColor Red
      foreach ($line in ($r.Output.Trim() -split "`r?`n")) { Write-Host "      $line" -ForegroundColor Red }
    }
    $programs = @()
    foreach ($ev in $conf.hooks.PSObject.Properties) {
      foreach ($g in @($ev.Value)) {
        foreach ($h in @($g.hooks)) {
          $c = [string]$h.command
          if ($c.Contains('claude-notify') -and $c -match '^\s*("[^"]*"|\S+)') { $programs += $Matches[1] }
        }
      }
    }
    foreach ($p in @($programs | Select-Object -Unique)) {
      $r = Invoke-ViaBash $bash "$p --version"
      if ($r.Code -eq 0 -and $r.Output -match 'claude-notify v') {
        Write-Host "    hooks OK: $($r.Output.Trim())"
      } else {
        $problems++
        Write-Host "    hook command FAILED (exit $($r.Code)): $p" -ForegroundColor Red
        foreach ($line in ($r.Output.Trim() -split "`r?`n")) { Write-Host "      $line" -ForegroundColor Red }
      }
    }
  }

  if ($remote) {
    Write-Host "==> remote/SSH box detected - chime is forwarded to $remote."
    Write-Host '    On your LOCAL machine (with speakers): install this repo there (it auto-starts'
    Write-Host "    the daemon), add 'RemoteForward 47291 127.0.0.1:47291' to its ~/.ssh/config for"
    Write-Host '    this host, then RECONNECT this SSH session so the tunnel is active.'
  }
  if ($problems) {
    Write-Host '==> installed, but the check above found problems. Fix them, then restart Claude Code.' -ForegroundColor Yellow
  } else {
    Write-Host '==> done. Restart Claude Code (or start a new session) to activate.'
  }
  Write-Host "==> test the chime:  '{}' | & '$cnBin' play"
} catch {
  Write-Host ('error: ' + $_.Exception.Message) -ForegroundColor Red
} finally {
  if ($build -and (Test-Path -LiteralPath $build)) { Remove-Item -LiteralPath $build -Recurse -Force -ErrorAction SilentlyContinue }
}
}
