# Mcli

**English** | [简体中文](README.md)

Mcli is a native Windows desktop multi-Agent CLI terminal workbench built with Rust and Rune.

Run multiple independent PowerShell sessions in a single window, automatically grouped by working directory and Git branch on the left while interacting with a GPU-rendered native ConPTY terminal on the right. Mcli never rewrites your CLI arguments or requires project hooks -- just type `codex`, `agy`, `kimi`, or `grok` to get live status indicators, background reply badges, recent tasks, and searchable prompt footprints.

---

## Key Features

- **Native Multi-Session Terminal Workbench**
  - Groups sessions automatically by the shell's reported filesystem directory, showing session counts and active Git branches (or detached HEAD short SHAs). Right-click any project header for a dark styled context menu to open the directory in Explorer, launch a new terminal, copy path, or expand/collapse all project groups.
  - Enforces a single instance per Windows user session, automatically focusing and restoring the existing window (including from the system tray) on relaunch.
  - Preserves each session's unsubmitted draft and up to 3,000 lines of scrollback when switching tabs; background sessions keep running uninterrupted.
  - Supports 16-color, 256-color, and 24-bit truecolor rendering, CJK IME composition, text selection, and clipboard paste for text, screenshots, and files (`Ctrl+V` / `Shift+Insert`).
- **Zero-Rewrite Support for 4 Agent CLIs**
  - Automatically detects **OpenAI Codex CLI (`codex`)**, **Antigravity CLI (`agy`)**, **Kimi Code CLI (`kimi`)**, and **Grok Build CLI (`grok`)** without modifying user command lines.
  - Tracks live states (`Running`, `Waiting for User`, `Ready`, `Error`) via native `OSC 7501` (including Kimi subagent aggregation) and Grok `OSC 0` titles, lighting an unread badge when a background agent finishes replying.
- **Recent Tasks, Prompt Footprints & Auto-Resume**
  - Read-only discovery of official conversation logs under the current project and its subdirectories, with a floating right-hand drawer to browse, search, and copy full multi-line prompts without resizing the terminal or clearing selection.
  - Saves window layout and per-tab proxy overrides on exit and automatically resumes verified conversation IDs on reopen (with automatic retry up to 3 times on transient launch failure).
- **Multi-Proxy Profiles & Quick Switch (Top Bar + Shell `mp` + Per-Agent Auto Proxy)**
  - Tracks per-terminal changes to `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY` using Windows DPAPI user-scoped encryption.
  - Manage reusable proxy presets in **Settings -> Proxy Profiles** (port-only shorthand like `10808` expands automatically), set a global default for new shells, or configure per-Agent defaults (`Codex`, `agy`, `Kimi`, `Grok`) that apply right before CLI launch without overwriting manual overrides.
  - Switch proxies from the styled top-bar pill (idle Agent tabs seamlessly reopen and resume their official conversation ID) or inside PowerShell via `mp <name|index|off|sys>`.
- **Optional WeChat ClawBot Remote Control (Off by Default)**
  - Pair your WeChat account via QR code in **Settings** with DPAPI-encrypted local credentials and strict single-user whitelist enforcement (zero async runtime dependencies, pure WinHTTP long-polling).
  - Supports `/list`, `/use`, `/status`, `/proxy`, `/new`, `/reopen`, `/screen`, `/stop`, `/approve`, `/deny`, `/key`, `/more`, `/ping`, opt-in `/sh`, 3h/2h/1h proactive keepalive reminders, and offline stale-message protection on restart.
- **Completion Sound Alerts & Customizable Settings**
  - Plays Windows system notification sounds when an Agent finishes or waits for approval (defaults to background / unfocused / non-selected tabs only, with 1.5s debounce, 3 selectable system sounds + live preview, dynamic DLL loading with zero static `winmm.dll` dependency, and silent fallback on stripped systems).
  - Configure **Minimize to System Tray on Close**, toggle the **Top System Metrics & Proxy Bar**, and **rebind all global keyboard shortcuts** in Settings.

---

## System Requirements

- **OS**: Windows 10 1809 (Build 17763) 64-bit or later (Windows 10 22H2 or Windows 11 64-bit recommended).
- **Shell**: Uses PowerShell 7 (`pwsh.exe`) when available, and automatically falls back to built-in Windows PowerShell 5.1.
- **Agent CLIs (optional)**: Install whichever official CLIs (`codex`, `agy`, `kimi`, `grok`) you use and ensure they are on `PATH`.

---

## Quick Start (Pre-built Release)

1. Download the latest `mcli-v*-windows-x64.zip` from the [Releases page](https://github.com/ab0029/Mcli/releases).
2. Extract it to any folder, keeping `mcli.exe` and the `conpty/` directory side by side:
   ```text
   mcli/
   ├── mcli.exe
   ├── mcli-hook.exe          # Optional event hook relay; not needed for normal use
   └── conpty/                # Packaged Microsoft-signed ConPTY binaries
   ```
3. Launch `mcli.exe`, `cd` into your project directory, and run your CLI:
   ```powershell
   cd 'C:\path\to\your-project'
   codex    # or agy / kimi / grok
   ```
4. Type `/exit` to return to the underlying PowerShell; after `exit`, click `↻` to reopen a shell in the same directory.

---

## Default Keyboard Shortcuts

All global shortcuts can be rebound or reset to defaults in the **Settings** tab:

| Default Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+K` | Focus the project & session search box |
| `Ctrl+Shift+H` | Toggle the floating Prompt Footprints drawer |
| `Ctrl+Shift+L` | Toggle the All History browser |
| `Ctrl+Shift+T` | Open a new PowerShell session in the current directory |
| `Ctrl+PageUp` | Switch to the previous terminal session |
| `Ctrl+PageDown` | Switch to the next terminal session |

**Context-local shortcuts**:

- `Esc` in search or drawer: clear search / close drawer and return focus to the terminal.
- `Ctrl+F` in the Footprints drawer: focus full-text prompt search.
- `Ctrl+C` copies when text is selected in the terminal, otherwise sends interrupt; `Ctrl+V` / `Shift+Insert` pastes text, images, or files; hold `Shift` while dragging to select text when an app enables mouse reporting.

---

## Local Data & Privacy

All state stays strictly on your local machine:

- Window snapshots: `%LOCALAPPDATA%\Mcli\workspaces\`
- Local prompt index: `%LOCALAPPDATA%\Mcli\conversations.sqlite3`
- Preferences, proxy profiles & shortcuts: `%LOCALAPPDATA%\Mcli\preferences.json`, `%LOCALAPPDATA%\Mcli\proxy-profiles.json`
- WeChat ClawBot credentials (only when enabled): `%LOCALAPPDATA%\Mcli\clawbot-auth.bin` (Windows DPAPI encrypted)
- Official logs: read-only access to vendor directories (`~/.codex`, `~/.gemini/antigravity`, `~/.kimi-code`, `~/.grok`).

---

## Building from Source

### Prerequisites

- Windows x64 + Rust toolchain (Edition 2024, latest stable recommended)
- Linker toolchain:
  - **MSVC**: Visual Studio C++ Build Tools + Windows SDK (including `rc.exe`)
  - **GNU**: MinGW-w64 (`as`, `dlltool`, `windres` on `PATH`)

### Build & Prepare ConPTY

```powershell
# 1. Build the release binary
cargo build --release --locked

# 2. Download and verify the pinned Microsoft ConPTY x64 package into target/release/conpty/
.\scripts\prepare-conpty.ps1
```

Run `target\release\mcli.exe`. If `conpty/` is absent, Mcli automatically falls back to the built-in Windows ConPTY API.

### Formatting & Tests

```powershell
cargo fmt --check
cargo test --locked
```

> Note: `official_*` tests are marked `#[ignore]` for live testing against authenticated vendor CLIs and are skipped during normal `cargo test`.

---

## License

Licensed under the [MIT License](LICENSE).

Packaged Microsoft ConPTY binaries remain subject to their original license terms; see [Microsoft.Windows.Console.ConPTY.txt](doc/licenses/Microsoft.Windows.Console.ConPTY.txt).
