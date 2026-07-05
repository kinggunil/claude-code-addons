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

**요구 사항:** `curl`과 Rust 컴파일용 C 링커 — `cc`/`clang`/`gcc`(macOS는 Xcode Command Line Tools: `xcode-select --install`, Linux는 `build-essential` 또는 `gcc`). Windows는 [Rust](https://rustup.rs) + 링커(VS Build Tools의 "C++를 사용한 데스크톱 개발")와, `settings.json` 자동 패치를 위한 Python(없으면 수동 추가할 JSON을 안내).

---

## 1. 상태줄(status line)

프롬프트 아래 2줄:

```
claude --resume <sid> | 4h 31m | $2.74 | 5h  9% (3h 54m) | 7d 23/18% (+5% 8h24m) (5d 17h)
Opus 4.8 (1M) | ⚡xhigh | think on | mydir | 🟢 █░░░░░░░░░ 126.5K/1M (12.7%) | CPU 10% | RAM 56% | VM 62% | 🏠 my-mac | v26.07.05
```

SSH 세션에서는 호스트 세그먼트가 주황색 🌐로 바뀌어, 지금 세션이 실제로 어느 머신에서 도는지 항상 알 수 있습니다:

```
… | CPU 10% | RAM 56% | VM  0% | 🌐 ip-10-0-1-23 | v26.07.05
```

- **1줄**: resume 명령, 경과 시간, 비용, Claude.ai 사용량 한도(Pro/Max). 7일 창은 **주간 페이스** 게이지입니다: `사용%/주 경과%` 와 함께, 선형 "정상 페이스" 대비 얼마나 앞서/뒤처졌는지를 여유에 따라 색으로 표시하고, 토요일 04:00 KST 리셋까지 카운트다운을 보여줍니다.
- **2줄**: 모델, effort, thinking 토글, 디렉터리, 컨텍스트 게이지, CPU/RAM/VM, 머신 **호스트**(🏠 회색=로컬, 🌐 주황=SSH 원격 — 호스트명은 상태줄이 실행되는 머신에서 오므로 SSH에선 원격 서버 이름을 표시), 그리고 `vYY.MM.DD` 빌드 태그(저장소 버전과 비교해 오래된 머신 파악).

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
- **`remote`** — 어디서 재생할지(아래 참고). `""`면 이 머신에서 재생.

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

**1. 로컬 머신(스피커 있는 쪽)** — 데몬을 상시 실행:

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

**3. 서버** — 서버에도 claude-code-addons를 설치한 뒤, 서버의 `~/.claude/.claude-notify.json`에서 `remote`를 지정:

```json
{ "vol": 2.0, "completed": "claude", "question": "claude", "remote": "127.0.0.1:47291" }
```

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
