# claude-code-addons

[English](README.md) · **한국어**

[Claude Code](https://claude.com/claude-code)용 의존성 없는 **Rust** 도구 2종. 명령 한 줄로 함께 설치됩니다.

1. **상태줄(status line)** — CPU/RAM/VM + 비용 + 주간 사용량 페이스를 보여주는 2줄 표시줄.
2. **claude-notify** — **긴 작업이 끝나거나** Claude가 **답변을 기다릴 때** 소리를 냅니다. 기본은 *"claude"* 라고 음성으로 말합니다. **SSH 원격에서도** 동작합니다(원격 서버가 아니라 내 로컬 머신에서 소리가 남).

둘 다 단일 정적 바이너리라 인터프리터·런타임 의존성이 없고, `claude-notify`는 재생 시 `afplay`/`paplay`/PowerShell 같은 서브프로세스도, 외부 사운드 파일도 필요 없습니다(음성이 컴파일 시 바이너리에 내장됨).

---

## 설치

### macOS / Linux

```sh
curl -sSfL https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/kinggunil/claude-code-addons/main/install.ps1 | iex
```

그다음 **Claude Code를 재시작**(또는 새 세션 시작)하세요.

설치 스크립트는 소스 + 내장 음성을 내려받고, `rustc`가 없으면 rustup으로 Rust 툴체인을 설치(macOS/Linux; Windows는 Rust를 먼저 직접 설치)한 뒤 두 바이너리를 `~/.claude`에 컴파일하고, 기본 설정 `~/.claude/.claude-notify.json`을 작성하고, `~/.claude/settings.json`에 `statusLine` 블록과 `claude-notify` 훅 3개를 추가합니다. 기존 설정은 보존되고 `settings.json.bak`으로 백업되며, 재실행해도 중복되지 않습니다(idempotent). Windows에서는 마지막에 각 명령을 Claude Code와 똑같이 Git Bash로 실행해 보므로, 경로가 잘못됐다면 상태줄이 조용히 안 보이는 대신 설치 출력에 빨간 줄로 바로 드러납니다.

**소리 전송 대상도 자동으로 설정합니다.** 설치 스크립트가 이 머신이 스피커 있는 기계인지 헤드리스/SSH 박스인지 감지해서 맞게 세팅하므로, 같은 한 줄 명령이 양쪽에서 알아서 올바르게 동작합니다:

- **스피커 있는 기계**(로컬 macOS/데스크톱, 또는 SSH 세션이 아닐 때) → `remote: ""`(로컬 재생)로 쓰고, **재생 데몬을 자동 실행**(`install-daemon`)해서 원격 박스들의 알림을 받을 준비를 합니다.
- **헤드리스/SSH 박스**(SSH 세션 안에서 설치했거나, 사운드카드 없는 Linux) → `remote: "127.0.0.1:47291"`로 써서, 알림을 SSH 터널로 스피커 있는 기계의 데몬에 전달합니다. 이 경우 로컬 데몬은 띄우지 않습니다.

기존 `.claude-notify.json`은 절대 덮어쓰지 않습니다. SSH 상황에서 수동으로 남는 건 로컬 `~/.ssh/config`의 `RemoteForward` 한 줄과 **SSH 세션 재접속**(실행 중인 세션엔 터널을 소급 적용할 수 없음)뿐입니다 — [SSH로 원격 작업할 때](#ssh로-원격-작업할-때-예-ec2) 참고.

**요구 사항:** `curl`과 Rust 컴파일용 C 링커 — `cc`/`clang`/`gcc`(macOS는 Xcode Command Line Tools: `xcode-select --install`, Linux는 `build-essential` 또는 `gcc`). Windows는 [Rust](https://rustup.rs) + 링커(VS Build Tools의 "C++를 사용한 데스크톱 개발")와 [Git for Windows](https://git-scm.com/downloads/win)(Claude Code 자체가 필요로 하며, 설치기는 그 Git Bash로 마지막 점검을 합니다). Windows PowerShell 5.1과 PowerShell 7 모두 되고, Python은 필요 없습니다.

### 설정 예시

[`settings.example.json`](settings.example.json)은 동작하는 `~/.claude/settings.json`
참고 예시이며, 두 그룹으로 나눠 표시해 뒀습니다:

- **(A) 애드온에 필요한 부분** — `statusLine` 블록과 `claude-notify` 훅 3개. 설치
  스크립트는 **이것만** 기존 설정에 병합합니다(직접 넣을 필요 없음).
- **(B) 작성자 개인 취향** (`model`, `effortLevel`, `enabledPlugins`,
  `permissions.defaultMode`) — 설치기가 건드리지 않으니 **원하는 것만 골라 복사**하세요.
  `"defaultMode": "bypassPermissions"`와 `"skipDangerousModePermissionPrompt": true`는
  **Claude Code의 권한 확인 프롬프트를 꺼 버리므로** 복사 전에 이해하고 쓰세요.

**경로는 OS마다 다릅니다.** 예시는 macOS/Linux 형태(`$HOME/.claude/statusline-rs`,
확장자 없음)입니다. Windows에서는 설치기가 **따옴표로 감싼 슬래시(`/`) 절대경로**로
씁니다(예: `"C:/Users/you/.claude/statusline-rs.exe"`). 이 형태가 중요합니다:
Windows의 Claude Code는 상태줄과 훅 명령을 Git Bash로 실행하는데, bash는 따옴표
밖의 역슬래시를 지워 버려서 `C:\Users\you\.claude\statusline-rs.exe`가
`C:Usersyou.claudestatusline-rs.exe`("command not found")가 되고, 상태줄은 아무 에러
없이 그냥 안 보입니다. Windows에서 `settings.json`을 손으로 고칠 때는 슬래시를 쓰거나
경로를 따옴표로 감싸세요. 어느 쪽이든 설치기가 그 머신에 맞는 경로를 알아서 채워 주며,
이 예시는 수동 설치용 참고일 뿐입니다. (파일 안의 `_` 로 시작하는 키는 주석이며,
Claude Code는 모르는 키를 무시합니다.)

---

## 1. 상태줄(status line)

프롬프트 아래 2줄, **도메인 기준으로 분리** — 위는 **내 Claude 세션**, 아래는 **이 머신**:

```
Opus 4.8 (1M) · ⚡xhigh · think on | 🟢 █░░░░░░░░░ 126.5K/1M ( 12.7%) | 5h   9% ⏳3h 54m | 7d  23% used ·  5% over pace ( 8h24m) · ⏳5d 17h | 4h 31m · $2.74
CPU  10% · RAM  56% · VM  62% · 🏠 my-mac | mydir | v26.10.20.3 | claude --resume <sid>
```

SSH 세션에서는 호스트 세그먼트가 주황색 🌐로 바뀌어, 아랫줄 전체가 "이 세션이 실제로 도는 박스"처럼 읽힙니다:

```
CPU  10% · RAM  56% · VM   0% · 🌐 ip-10-0-1-23 | mydir | v26.10.20.3 | claude --resume <sid>
```

- **1줄 — Claude 세션**: 모델, effort, thinking 토글, 컨텍스트 게이지, 그다음 Claude.ai 사용량 한도(Pro/Max) — 각 창은 `사용%` 와 `⏳` 뒤의 리셋 카운트다운으로 표시. 7일 창은 여기에 **주간 페이스**를 쉬운 말로 덧붙입니다: `N% over pace`(균등 리셋 기준선보다 빠르게 소비) 또는 `N% under pace`(여유), 여유량에 따라 색이 바뀌고 그 차이를 시간으로도 함께 표기. 이어서 경과 시간과 비용. 페이스의 기준은 Claude Code가 보내 주는 7일 창의 `resets_at`입니다. 사용량 한도 데이터가 없을 때(API 키 세션)는 토요일 04:00 KST에 리셋된다고 가정하는데 이는 작성자 플랜 기준이며, `STATUSLINE_WEEK_ANCHOR`에 내 리셋 시각 중 하나를 unix 초로 넣으면 바뀝니다.
- **2줄 — 이 머신**: CPU/RAM/VM을 맨 앞에(실시간으로 변하는 유일한 값이라 왼쪽 첫 자리에 배치), 그다음 머신 **호스트**(🏠 시안=로컬, 🌐 주황=SSH 원격 — 호스트명은 상태줄이 실행되는 머신에서 오므로 SSH에선 원격 서버 이름을 표시), 작업 디렉터리, `vYY.MM.DD` 빌드 태그(저장소 버전과 비교해 오래된 머신 파악), 그리고 맨 오른쪽에 전체 `claude --resume` 명령.

모든 수치는 직접 시스템콜로 수집합니다(macOS `mach`/`sysctl`, Linux `/proc`, Windows `kernel32`) — 서브프로세스 없이 갱신당 ~2–5ms.

호스트는 셸의 조상 프로세스에 `sshd`/`mosh-server`/SSM 세션이 있거나 그 머신이 EC2 인스턴스(DMI 제조사 또는 기본 호스트명 `ip-A-B-C-D`)이면 🌐로 표시됩니다. tmux 서버나 서비스 매니저가 `SSH_*` 변수를 떨어뜨려도 원격 박스가 로컬처럼 보이지 않습니다.

### 글자가 밀리거나 깨질 때 — 이모지 대신 ASCII

오래된 Ubuntu(18.04 이전: glibc가 유니코드 10 표를 씀)의 터미널이나 그 앞단의 tmux/screen/mosh는 🟢(유니코드 12, 2019) 같은 새 이모지의 폭을 모릅니다. Claude Code는 2칸으로 세는데 그 계층은 1칸으로 세니 그 뒤의 글자가 전부 한 칸씩 어긋나고, 매초 바뀐 숫자만 다시 그리기 때문에 옛 숫자가 남습니다. `11%`가 `114`로, `(13.6%)`가 `(13648)`로 보이는 현상이 그것입니다.

기본 글자 모드 `auto`는 이런 환경이 확인되면 ASCII로 바꿉니다: 로케일이 UTF-8이 아니거나, `TERM`이 `linux`/`dumb`이거나, Windows의 구식 콘솔이거나, Linux 박스의 libc `wcwidth()`가 이 이모지들을 2칸으로 답하지 않는데 그 박스의 로컬 터미널이나 tmux/screen/mosh가 줄을 다시 재는 경우. SSH 건너편의 터미널 프로그램은 볼 수 없으므로, 그래도 깨지면 `settings.json`의 명령 뒤에 `--ascii`를 붙여 강제하세요(또는 환경 변수 `STATUSLINE_GLYPHS=ascii`). 반대는 `--unicode`.

```json
"statusLine": { "type": "command", "command": "$HOME/.claude/statusline-rs --ascii", "refreshInterval": 1 }
```

Windows: `"command": "\"C:/Users/you/.claude/statusline-rs.exe\" --ascii"`.

| 유니코드 | ASCII |
|---|---|
| `🟢 █░░░░░░░░░` 컨텍스트 게이지(🟢 ⚡ 🔥 🚨는 사용률별) | `[#.........]`, 색은 동일. 경보 상태에서만 `!!` |
| `⚡max` effort | `eff max` |
| `⏳3d 14h` 리셋 카운트다운 | `~3d 14h` |
| `🏠 host` / `🌐 host` | `@host` / `ssh:host` |
| 구간 안 구분자 `·` | `,` |

```
Fable 5.1, eff max, think on | [#.........] 136.8K/1M ( 13.6%) | 5h  11% ~0h 42m | 7d  33% used, 15% under pace (23h31m), ~3d 14h | 23h 13m, $539.75
CPU  16%, RAM  54%, VM  29%, ssh:ip-172-31-16-160 | accounts | v26.10.20.3 | claude --resume <sid>
```

**갱신마다 바뀌는 값은 모두 고정 폭입니다**(두 모드 공통): 퍼센트는 세 자리 오른쪽 정렬(`  4%`, ` 14%`, `100%`), 토큰 수는 6자(` 12.3K`, `136.8K`, ` 1.02M`), 시간은 6자(`0h 42m`, `3d 14h`, `( 8h24m)`), 경과 시간과 비용은 7자(` 0h 42m`, `  $5.37`). 숫자가 자릿수를 넘어도 뒤가 밀리지 않고, 셀 단위로 다시 그리는 계층은 바뀐 숫자만 고칩니다.

### 좁은 터미널

Claude Code는 터미널 크기를 `COLUMNS`로 넘겨주며, 각 줄은 거기에 맞춰집니다(Claude Code 자체 여백용으로 2칸을 남깁니다). 맞을 때까지 정해진 순서로 조각을 뺍니다: resume 명령, 페이스의 괄호 시간, 비용, 버전, 경과 시간, `think`, 페이스 문구, VM, effort, 5h 카운트다운, 게이지, 7d 카운트다운, RAM, 디렉터리, 마지막으로 토큰 수(퍼센트는 남음). 줄이 꺾이는 일이 없고, 사용량 숫자가 가장 늦게 빠집니다. `--width N`(또는 `STATUSLINE_WIDTH`)으로 감지된 폭을 덮어쓸 수 있고, `--width 0`이면 줄이지 않습니다. 120칸 창에서는 위의 줄이 이렇게 됩니다:

```
Fable 5.1 · ⚡max | 🟢 █░░░░░░░░░ 136.8K/1M ( 13.7%) | 5h  11% ⏳0h 41m | 7d  33% used · 16% under pace · ⏳3d 13h
CPU  23% · RAM  78% · VM  77% · 🏠 my-mac | mydir | v26.10.20.3
```

---

## 2. claude-notify

### 언제 소리가 나나

| 이벤트 | 훅 | 동작 |
|-------|------|-----------|
| **작업 완료** | `Stop` | 작업이 **60초 이상** 걸렸을 때만 재생(짧은 작업은 무음) |
| **답변 대기** | `PreToolUse` / `AskUserQuestion` | **즉시** 재생 |
| (프롬프트 제출) | `UserPromptSubmit` | 60초 기준을 위해 시작 시각 기록 |

### 설정 — `~/.claude/.claude-notify.json`

재컴파일도, 훅 수정도 필요 없습니다. 이 파일만 바꾸세요:

```json
{
  "vol": 2.0,
  "completed": "claude",
  "question": "claude",
  "remote": ""
}
```

- **`vol`** — 선형 볼륨. 내장 음성 피크가 약 56% FS라 ~`1.8`이 클리핑 없는 최대치이고, 그보다 크면 약간의 클리핑과 함께 더 커집니다. 기본 `2.0`.
- **`completed` / `question`** — 각 이벤트의 소리: 내장 `claude`, 또는 **직접 만든 16-bit PCM `.wav`의 절대 경로**. 둘을 다르게 지정할 수 있습니다.
- **`remote`** — 어디서 재생할지(아래 참고). `""`면 이 머신에서 재생. 설치 스크립트가 자동으로 정합니다(스피커 있는 기계면 `""`, 헤드리스/SSH 박스면 `127.0.0.1:47291`). 바꾸려면 여기서 수정.

### CLI

```
claude-notify mark                                   # 작업 시작 기록
claude-notify play --event completed --threshold 60  # 마크 후 60초 이상이면 재생
claude-notify play --event question                  # 즉시 재생
claude-notify play --sound /path/to.wav --vol 1.5    # 명시적 오버라이드
claude-notify listen [--port 47291]                  # 로컬 재생 데몬(SSH 참고)
claude-notify install-daemon [--port 47291]          # 데몬 상시 실행 등록
claude-notify --version
```

---

## SSH로 원격 작업할 때 (예: EC2)

**서버에 SSH로 접속해 그 안에서 Claude Code를 실행**하면 훅은 서버에서 발동합니다 — 서버는 헤드리스라 스피커가 없고, SSH는 오디오를 전달하지 않습니다. 그래서 소리를 **내 로컬 머신으로 되돌려** 보내야 합니다. 방법은 두 가지이고, 함께 쓸 수 있습니다.

### 신호는 기존 SSH 연결을 타고 갑니다 (방화벽 변경 불필요)

서버의 `claude-notify`가 재생 요청을 **로컬 머신의 작은 데몬**에게 **SSH `RemoteForward` 터널**로 넘깁니다. 이 터널은 이미 연결된 SSH 안으로 흐르므로 — 네트워크를 실제로 건너는 건 22번 포트뿐이고 양 끝이 `127.0.0.1`이라 — **security group·방화벽·NAT 변경이 전혀 필요 없습니다.** 터널이나 데몬에 닿지 못하면 **터미널 벨/데스크톱 알림**(TTY 출력)으로 폴백하며, 이건 SSH 세션만 있으면 언제나 동작합니다.

### 설정

**1. 로컬 머신(스피커 있는 쪽)** — 데몬을 상시 실행. **스피커 있는 기계라면 설치 스크립트가 이미 해줍니다.** 건너뛰었거나 다시 걸고 싶을 때만 수동 실행:

```sh
claude-notify install-daemon          # macOS launchd / Linux systemd --user
# 또는 터미널에서 직접:  claude-notify listen --port 47291
```

**2. `~/.ssh/config`** — 해당 호스트에 포트 역터널 추가:

```
Host my-ec2
  HostName 1.2.3.4
  User ubuntu
  RemoteForward 47291 127.0.0.1:47291
```

**3. 서버** — 서버에도 claude-code-addons를 설치합니다. SSH 세션 안에서 설치하므로 설치 스크립트가 서버의 `~/.claude/.claude-notify.json`에서 `remote`를 `127.0.0.1:47291`로 **자동 설정**합니다(헤드리스 박스에는 로컬 데몬을 띄우지 않음). 수동으로 지정해야 할 때:

```json
{ "vol": 2.0, "completed": "claude", "question": "claude", "remote": "127.0.0.1:47291" }
```

그다음 **SSH 세션을 재접속**해야 해당 세션에 `RemoteForward` 터널이 붙습니다.

이제 서버에서 긴 작업이 끝나면 **내 로컬 머신이 "claude"라고 말합니다.** 데몬이 꺼져 있거나 포워딩이 막혀 있으면 터미널 벨이 대신 울립니다.

**벨만 쓰기**(데몬 없이, 무설정): 서버에서 `"remote": "bell"`로 설정하면 TTY로 터미널 벨/데스크톱 알림만 보냅니다.

**전제 조건**(방화벽이 아니라): 서버 `sshd`가 TCP 포워딩을 허용해야 하고(`AllowTcpForwarding yes`, 기본값), 로컬 머신에 데몬이 떠 있어야 합니다(`install-daemon`이 재부팅/로그인 후에도 유지).

---

## 플랫폼 지원

| | 상태줄 | claude-notify 재생 | 데몬 자동실행 |
|---|---|---|---|
| **macOS** | `mach`/`sysctl` | AudioToolbox AudioQueue | launchd |
| **Linux / Ubuntu** | `/proc` | ALSA(`libasound`, `dlopen`) | systemd `--user` |
| **Windows** | `kernel32` | winmm `waveOut` | 수동(시작프로그램 바로가기) |

상태줄과 `claude-notify`는 macOS에서 개발·엔드투엔드 테스트했습니다. Windows/Linux 경로는 각 OS의 네이티브 오디오 API를 따라 컴파일되지만 검증이 덜 되었습니다 — 문제가 있으면 알려주세요. ALSA가 없는 헤드리스 Linux 서버에서는 로컬 재생이 조용히 무시됩니다(그게 위의 SSH 원격 상황 — 터널/벨을 사용).

**Windows 참고.** Windows의 Claude Code는 상태줄과 훅을 Git Bash로 실행하므로
`settings.json`의 경로는 따옴표로 감싼 슬래시 형태여야 합니다(설치기가 그렇게 쓰고
점검까지 합니다 — [설정 예시](#설정-예시) 참고). 손으로 고친 뒤 상태줄이 안 보이면 이
경로 형태부터 확인하세요. `install-daemon`은 Windows에서 안내만 출력합니다: SSH 박스의
알림을 받으려면 `claude-notify.exe listen --port 47291` 바로가기를 `shell:startup`에
넣으세요.

---

## 제거

`~/.claude/settings.json`에서 `statusLine` 블록과 `claude-notify` 훅 3개를 지우고(또는 `settings.json.bak` 복원), 다음을 실행:

```sh
rm -f ~/.claude/claude-notify ~/.claude/claude-notify.rs ~/.claude/claude.wav \
      ~/.claude/statusline-rs ~/.claude/statusline.rs ~/.claude/.claude-notify.json \
      ~/.claude/.statusline-cpu.json
rm -rf ~/.claude/.claude-notify
# 데몬을 설치했다면:
launchctl unload ~/Library/LaunchAgents/com.kinggunil.claude-notify.plist  # macOS
systemctl --user disable --now claude-notify.service                       # Linux
```

Windows(PowerShell):

```powershell
Remove-Item -Force -ErrorAction SilentlyContinue ~\.claude\claude-notify.exe, ~\.claude\claude-notify.rs,
  ~\.claude\claude.wav, ~\.claude\statusline-rs.exe, ~\.claude\statusline.rs,
  ~\.claude\.claude-notify.json, ~\.claude\.statusline-cpu.json
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue ~\.claude\.claude-notify
```

## 참고

- `claude` 음성은 macOS 텍스트-투-스피치 결과물이며, 개인용 편의 오디오로 번들되어 있습니다.
- 각 머신에는 `~/.claude/settings.json`(statusLine + 훅)만 있으면 됩니다. 나머지는 모두 머신 로컬이며 설치 스크립트가 처리합니다.
- 변경 시 각 `.rs`의 `VERSION`을 올리면 상태줄의 화면 태그로 어떤 머신이 오래됐는지 알 수 있습니다.
