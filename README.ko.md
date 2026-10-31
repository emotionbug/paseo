# Paseo Sessions

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

Paseo Sessions는 코딩 에이전트 작업을 실행하고 검토하는 네이티브 환경입니다. **Linux, macOS, Windows, Android, iOS** 클라이언트는 Flutter, 데몬과 `paseo` CLI는 Rust로 구현됩니다. 데스크톱은 로컬 데몬을 관리할 수 있으며, 모바일은 다른 컴퓨터의 데몬에 원격으로 연결하는 전용 클라이언트입니다.

현재 체크아웃 버전은 **0.13.0**이며 네이티브 전환 버전은 **아직 출시되지 않았습니다**. [업스트림 다운로드 페이지](https://paseo.sh/download)와 [기존 GitHub releases](https://github.com/getpaseo/paseo/releases)는 이번 전환의 빌드 산출물이 아닙니다. 이 체크아웃의 소스로 빌드하고, 바이너리를 배포하기 전에 [출시 요구 사항](docs/release.md)을 확인하세요.

## 여러 호스트에서 작업하기와 기능 범위

사이드바는 **호스트 → 디렉터리 → 세션** 구조입니다. 호스트가 하나여도 별도 행으로 표시하며 빈 디렉터리도 유지합니다. 세션은 최근 활동순으로 정렬됩니다. 호스트와 디렉터리는 각각 접을 수 있고 디렉터리의 접힘 상태는 저장됩니다. 서로 다른 호스트의 같은 경로는 별개로 취급합니다.

검색은 호스트, 디렉터리 이름과 경로, 세션 제목, 제공자를 대상으로 하며, 저장된 접힘 설정을 덮어쓰지 않고 일치 항목의 상위 노드를 펼칩니다. 디렉터리의 추가 버튼은 해당 호스트의 체크아웃에서 세션을 만듭니다. 프로젝트 추가에서는 호스트 측 폴더 탐색기를 사용하며 원격 경로를 로컬 OS 파일 대화 상자로 선택하지 않습니다. 워크스페이스는 오케스트레이션, worktree, 스크립트, Command Center 탐색에 계속 사용되지만 사이드바 그룹화 컨트롤은 아닙니다. 기존 세션 ID, 기록, 파일, 설정과 저장 데이터는 유지됩니다. 채팅 탭을 닫아도 세션이 보관 처리되지는 않습니다.

- 에이전트 채팅, 권한, 제공자/모델 선택, 프로필, 세션 가져오기와 기록.
- Markdown 표시, 파일 탐색과 소스 편집. Git diff는 요약부터 보여 주고 패치 본문은 필요할 때 불러옵니다.
- 워크스페이스 설정 및 서비스 스크립트는 데몬이 소유한 PTY에서 실행합니다. 앱은 **읽기 전용 로그**만 표시하며 대화형 터미널 에뮬레이터를 제공하지 않습니다.
- 예약 작업, MCP 도구와 서버, 음성, 연결된 호스트, 원격 디렉터리 선택.
- 네이티브 데스크톱 통합과 로컬 데몬 수명 주기 관리. 모바일은 원격 연결만 지원합니다.

기본 설치 런타임에 **Node, Electron, Chromium을 포함하지 않습니다**. 제품 웹 클라이언트, 내장 브라우저, 브라우저 자동화 또는 실행 가능한 React UI 플러그인 호환성을 제공하지 않습니다. 외부 링크는 앱 밖에서 열립니다. 웹사이트는 별도의 문서/마케팅 프로젝트입니다. 에이전트 CLI는 외부 프로그램이므로 자체 런타임 요구 사항이 있을 수 있습니다.

내장 제공자 및 사용량 통합은 Rust에서 실행됩니다. 선택적 JavaScript **백엔드** 플러그인은 별도로 설치한 Node 20+가 필요하며 `PASEO_PLUGIN_NODE`로 지정할 수 있습니다. 컴파일러/SDK 자산이 있다고 Node 데몬을 번들로 제공하는 것은 아닙니다. [플러그인 문서](docs/plugins.md)를 참고하세요.

## 소스에서 빌드하고 실행하기

Rust/Cargo, Flutter SDK, 대상 플랫폼의 네이티브 도구 체인과 빌드 도구용 **Node 24 및 npm**이 필요합니다. 에이전트 CLI도 하나 이상 설치하고 인증 정보를 설정하세요: [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex), [Copilot](https://github.com/features/copilot/cli/), [OpenCode](https://github.com/anomalyco/opencode), [Pi](https://pi.dev). 자세한 준비 사항은 [개발 가이드](docs/development.md)에 있습니다.

```sh
npm ci
npm run build:daemon
npm run dev:desktop
```

개발 데몬과 네이티브 클라이언트를 따로 실행하려면:

```sh
npm run dev:daemon
# 다른 터미널에서:
npm run dev:app
```

개발 환경은 체크아웃의 `.dev/paseo-home`과 포트 `6768`을 사용하며, 일반 실행의 `~/.paseo` 및 포트 `6767`과 분리합니다. 빌드 래퍼는 Node 도구로 Rust를 실행하며 Node 서버가 아닙니다.

```sh
npm run build:flutter -- linux
npm run build:flutter -- macos
npm run build:flutter -- windows
npm run build:apk
npm run build:apk:arm64
npm run build:ios
```

데스크톱은 대상 OS에서 빌드해야 하며 iOS는 macOS/Xcode가 필요합니다. 산출물과 빌드 설명은 `artifacts/flutter/`에 저장됩니다. Android/iOS에는 데몬이 없습니다. Android 앱 이름은 **Paseo Sessions**, 패키지는 `sh.paseo.sessions`입니다. 디버그 서명 APK나 서명되지 않은 iOS 앱 아카이브는 스토어 출시용 산출물이 아닙니다. [Android](docs/android.md)와 [출시 가이드](docs/release.md)를 참고하세요.

헤드리스 호스트에서는 생성된 `target/debug/paseo`(Windows에서는 `paseo.exe`)를 사용합니다. `CARGO_TARGET_DIR`를 설정했다면 해당 디렉터리를 사용하고, 생성된 런타임 자산을 실행 파일 옆에 유지하세요. npm 전역 CLI 설치는 필요하지 않습니다.

```sh
./target/debug/paseo onboard
./target/debug/paseo daemon start
./target/debug/paseo daemon status
```

`build:daemon`은 디버그 빌드와 자산을 생성합니다. `npm run build:daemon:release`는 `target/release/`에 최적화된 **로컬 빌드**와 자산을 생성할 뿐 출시 검증을 의미하지 않습니다. `paseo onboard`는 명시적인 초기 설정 명령입니다. 인수 없이 `paseo`를 실행하면 CLI 채팅이 열리며 설치나 초기 설정을 수행하지 않습니다.

일반 데이터 디렉터리는 `~/.paseo`이며 `PASEO_HOME` 또는 `--home`으로 변경할 수 있습니다. `config.json`, 에이전트 기록, 프로젝트/워크스페이스 메타데이터와 연결 자격 증명은 기존 영속성 규약을 유지합니다. 원격 데몬에는 `--host`로 연결하고 필요한 경우 `PASEO_PASSWORD`를 설정합니다. SSH 전송은 이미 있는 데몬에 연결할 뿐 설치하지 않습니다. [Docker 가이드](docs/docker.md)는 네이티브 데몬 전용 컨테이너를 빌드하며 HTTP 루트에서 앱을 제공하지 않습니다.

## CLI와 SDK

호스트 설정의 에이전트 프로필에서 **Use for CLI**를 선택하면 기본 제공자, 모델, 모드와 사고 수준을 설정할 수 있습니다. 명시적인 CLI 옵션이 우선하며, 충돌하는 제공자/모델 인수는 연결 전에 오류를 반환합니다.

```sh
paseo
paseo --resume <session-id>
paseo run --provider codex "review this checkout"
paseo ls
paseo attach <session-id>
paseo send <session-id> "also add tests"
paseo import --provider pi <native-session-id>
paseo run --host workstation.local:6767 --cwd /workspace --provider codex "review the diff"
```

외부 통합용 TypeScript SDK `@getpaseo/client`는 `packages/client`, 공유 프로토콜 정의는 `packages/protocol`에 있습니다. Rust 데몬과 Dart 클라이언트는 동일한 통신 규약을 구현합니다. SDK 사용은 제품 런타임이 Node에 의존한다는 뜻이 아닙니다.

## 소스 구성과 검증 범위

| 영역                                                            | 경로                                                    |
| --------------------------------------------------------------- | ------------------------------------------------------- |
| Rust 데몬, CLI, 제공자, 저장소, supervisor, 데스크톱 호스트 IPC | `packages/daemon`                                       |
| 5개 플랫폼용 Flutter 클라이언트                                 | `packages/flutter_app`                                  |
| 데스크톱 패키징 및 런타임 자산                                  | `packages/flutter_desktop_host`                         |
| 네이티브 오디오 통합                                            | `packages/flutter_audio`                                |
| TypeScript SDK 및 프로토콜                                      | `packages/client`, `packages/protocol`                  |
| 선택적 확장 SDK / 비공개 백엔드 worker                          | `packages/plugin` / `packages/daemon/plugin-worker/src` |
| 문서 웹사이트                                                   | `packages/website`                                      |

런타임 경계를 변경하기 전에 [아키텍처](docs/architecture.md), [개발](docs/development.md), [테스트](docs/testing.md), [출시](docs/release.md) 문서를 읽으세요. Linux에는 네이티브 데몬/클라이언트 및 데스크톱 호스트 수명 주기의 실제 실행 증거가 있습니다. Android ARM64 release 모드 빌드는 통과했지만 디버그 서명을 사용했으며 설치나 기기 실행은 하지 않았습니다. 이번 전환에서 Windows, macOS, iOS의 실제 실행은 없습니다. 플랫폼별 CI 정의는 실행 증거가 아닙니다. 데스크톱 추가 검증을 포함한 최신 증거와 제한은 테스트 문서에서 관리합니다.

## 라이선스와 배포

Paseo 소스는 Apache-2.0이며 의존성에는 각각의 라이선스가 적용됩니다. 패키징 과정에서 Cargo, 선택적 worker, 음성 의존성의 고지문을 생성하지만 이는 **준수 인증이나 대응 소스 제공 제안이 아닙니다**. 공식 Sherpa 1.12.28 TTS 활성화 사전 빌드에는 GPL espeak-ng가 정적으로 포함됩니다. 소스 전달/제공 제안, 결합 저작물, MPL/LGPL, CRT 및 Rust 도구 체인 관련 의무는 아직 해결되지 않은 출시 배포 요구 사항입니다. 음성 모델 가중치는 번들에 포함하지 않고 필요할 때 다운로드합니다. [배포 요구 사항](docs/release.md#distribution-requirements)을 참고하세요.

## 커뮤니티와 관련 프로젝트

[Discord](https://discord.gg/jz8T2uahpH) · [Reddit](https://www.reddit.com/r/PaseoAI/) · [GitHub Sponsors](https://github.com/sponsors/boudra) · [후원](https://paseo.sh/sponsor)

- [getpaseo/paseo-relay](https://github.com/getpaseo/paseo-relay): 독립적인 Elixir 분산 릴레이 프로젝트.
- [paseo-vscode](https://marketplace.visualstudio.com/items?itemName=hinnes.paseo-vscode): VS Code 확장 프로그램. 이번 네이티브 전환에서의 호환성 검증을 의미하지 않습니다.
- [paseo-skins](https://github.com/huangguang1999/paseo-skins): 기존 커뮤니티 테마 프로젝트 링크입니다. 현재 Flutter 클라이언트와의 호환성이나 실행 가능한 UI 플러그인 지원을 의미하지 않습니다.
