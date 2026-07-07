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

설치 스크립트는 소스 + 내장 음성을 내려받고, `rustc`가 없으면 rustup으로 Rust 툴체인을 설치(macOS/Linux; Windows는 Rust를 먼저 직접 설치)한 뒤 두 바이너리를 `~/.claude`에 컴파일하고, 기본 설정 `~/.claude/.claude-notify.json`을 작성하고, `~/.claude/settings.json`에 `statusLine` 블록과 `claude-notify` 훅 3개를 추가합니다. 기존 설정은 보존되고 `settings.json.bak`으로 백업되며, 재실행해도 중복되지 않습니다(idempotent).

**소리 전송 대상도 자동으로 설정합니다.** 설치 스크립트가 이 머신이 스피커 있는 기계인지 헤드리스/SSH 박스인지 감지해서 맞게 세팅하므로, 같은 한 줄 명령이 양쪽에서 알아서 올바르게 동작합니다:

- **스피커 있는 기계**(로컬 macOS/데스크톱, 또는 SSH 세션이 아닐 때) → `remote: ""`(로컬 재생)로 쓰고, **재생 데몬을 자동 실행**(`install-daemon`)해서 원격 박스들의 알림을 받을 준비를 합니다.
- **헤드리스/SSH 박스**(SSH 세션 안에서 설치했거나, 사운드카드 없는 Linux) → `remote: "127.0.0.1:47291"`로 써서, 알림을 SSH 터널로 스피커 있는 기계의 데몬에 전달합니다. 이 경우 로컬 데몬은 띄우지 않습니다.

기존 `.claude-notify.json`은 절대 덮어쓰지 않습니다. SSH 상황에서 수동으로 남는 건 로컬 `~/.ssh/config`의 `RemoteForward` 한 줄과 **SSH 세션 재접속**(실행 중인 세션엔 터널을 소급 적용할 수 없음)뿐입니다 — [SSH로 원격 작업할 때](#ssh로-원격-작업할-때-예-ec2) 참고.

**요구 사항:** `curl`과 Rust 컴파일용 C 링커 — `cc`/`clang`/`gcc`(macOS는 Xcode Command Line Tools: `xcode-select --install`, Linux는 `build-essential` 또는 `gcc`). Windows는 [Rust](https://rustup.rs) + 링커(VS Build Tools의 "C++를 사용한 데스크톱 개발")와, `settings.json` 자동 패치를 위한 Python(없으면 수동 추가할 JSON을 안내).

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
확장자 없음)입니다. Windows에서는 설치기가 `.exe` 절대경로로 씁니다(예:
`C:\Users\you\.claude\statusline-rs.exe`, `claude-notify.exe`). 어느 쪽이든 설치기가
그 머신에 맞는 경로를 알아서 채워 주며, 이 예시는 수동 설치용 참고일 뿐입니다.
(파일 안의 `_` 로 시작하는 키는 주석이며, Claude Code는 모르는 키를 무시합니다.)

---

## 1. 상태줄(status line)

프롬프트 아래 2줄, **도메인 기준으로 분리** — 위는 **내 Claude 세션**, 아래는 **이 머신**:

```
Opus 4.8 (1M) · ⚡xhigh · think on | 🟢 █░░░░░░░░░ 126.5K/1M (12.7%) | 5h  9% ⏳3h 54m | 7d 23% used · 5% over pace (8h24m) · ⏳5d 17h | 4h 31m · $2.74
CPU 10% · RAM 56% · VM 62% · 🏠 my-mac | mydir | v26.07.05 | claude --resume <sid>
```

SSH 세션에서는 호스트 세그먼트가 주황색 🌐로 바뀌어, 아랫줄 전체가 "이 세션이 실제로 도는 박스"처럼 읽힙니다:

```
CPU 10% · RAM 56% · VM  0% · 🌐 ip-10-0-1-23 | mydir | v26.07.05 | claude --resume <sid>
```

- **1줄 — Claude 세션**: 모델, effort, thinking 토글, 컨텍스트 게이지, 그다음 Claude.ai 사용량 한도(Pro/Max) — 각 창은 `사용%` 와 `⏳` 뒤의 리셋 카운트다운으로 표시. 7일 창은 여기에 **주간 페이스**를 쉬운 말로 덧붙입니다: `N% over pace`(균등 리셋 기준선보다 빠르게 소비) 또는 `N% under pace`(여유), 여유량에 따라 색이 바뀌고 그 차이를 시간으로도 함께 표기. 이어서 경과 시간과 비용.
- **2줄 — 이 머신**: CPU/RAM/VM을 맨 앞에(실시간으로 변하는 유일한 값이라 왼쪽 첫 자리에 배치), 그다음 머신 **호스트**(🏠 시안=로컬, 🌐 주황=SSH 원격 — 호스트명은 상태줄이 실행되는 머신에서 오므로 SSH에선 원격 서버 이름을 표시), 작업 디렉터리, `vYY.MM.DD` 빌드 태그(저장소 버전과 비교해 오래된 머신 파악), 그리고 맨 오른쪽에 전체 `claude --resume` 명령.

모든 수치는 직접 시스템콜로 수집합니다(macOS `mach`/`sysctl`, Linux `/proc`, Windows `kernel32`) — 서브프로세스 없이 갱신당 ~2–5ms.

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

## 참고

- `claude` 음성은 macOS 텍스트-투-스피치 결과물이며, 개인용 편의 오디오로 번들되어 있습니다.
- 각 머신에는 `~/.claude/settings.json`(statusLine + 훅)만 있으면 됩니다. 나머지는 모두 머신 로컬이며 설치 스크립트가 처리합니다.
- 변경 시 각 `.rs`의 `VERSION`을 올리면 상태줄의 화면 태그로 어떤 머신이 오래됐는지 알 수 있습니다.
