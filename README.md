<p align="center"><img src="assets/ai-usage-tracker-banner.png" width="740" alt="AI Usage Tracker"></p>

<h1 align="center">AI Usage Tracker</h1>

<p align="center">
  <strong>Always-on Windows overlay for AI rate limits, quotas, and prepaid credits.</strong><br>
  See remaining allowance for Z.ai, MiniMax, Codex, Claude, Grok, Kimi Code, and OpenRouter without jumping between dashboards.
</p>

<p align="center">
  <a href="#download">Download</a>
  ·
  <a href="#features">Features</a>
  ·
  <a href="#supported-providers">Providers</a>
  ·
  <a href="#development">Development</a>
  ·
  <a href="#license">License</a>
</p>

<p align="center">
  <a href="https://github.com/GimpMan/AI-Usage-Tracker/releases/latest">
    <img src="https://img.shields.io/github/v/release/GimpMan/AI-Usage-Tracker?style=flat-square&label=latest" alt="Latest release">
  </a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%2F%2011%20x64-0078D4?style=flat-square" alt="Windows 10/11 x64">
  <img src="https://img.shields.io/badge/telemetry-none-2ea44f?style=flat-square" alt="No telemetry">
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License">
  </a>
</p>

<p align="center">
  <img src="assets/demo-hq.gif" width="720" alt="AI Usage Tracker demo">
</p>

---

## Features

- **Always visible** - a slim overlay bar stays on top while you work
- **Multi-provider** - Z.ai, MiniMax, Codex, Claude, Grok, Kimi Code, and OpenRouter in one place
- **Click for details** - windows, reset times, balances, and local usage totals in a popup
- **Reorder and drag** - rearrange segments and move the overlay where you want it
- **Pace-aware** - weekly and window remaining percentages at a glance
- **Local-first** - keys and sessions stay on your machine; there is no developer backend
- **Auto-updates** - daily checks with signed packages that install only when you choose

---

## Download

| Requirement | Detail |
| --- | --- |
| OS | **64-bit Windows 10 and Windows 11** |
| Package | **NSIS setup EXE** (current user; no administrator access required) |
| Runtime | Microsoft Edge WebView2 (the installer can bootstrap it if missing) |

**[Download the latest Windows setup EXE](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest)**

> [!NOTE]
> Windows SmartScreen may warn on first install because the setup EXE is not Authenticode-signed. Confirm that it came from this repository's [Releases](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest) page before continuing.

Nothing is pre-authenticated: API keys, OAuth sessions, and usage history are not included in the installer or source repository.

---

## Supported providers

| Provider | Authentication | Information shown |
| --- | --- | --- |
| **Z.ai Coding Plan** | API key from **z.ai -> Manage API Key** | 5-hour and weekly coding windows; monthly Web Search / Reader / Zread tool quota in the popup |
| **MiniMax Coding Plan** | Coding Plan or Token Plan API key; choose **Overseas** (`minimax.io`) or **China** (`minimaxi.com`) to match the key | 5-hour and weekly quota windows |
| **OpenAI Codex CLI** | Sign in with ChatGPT in Settings, or reuse the official CLI session at `~/.codex/auth.json` (no API key required) | Live primary and secondary Codex rate-limit windows, including reset times |
| **Claude Code** | Sign in in Settings or use the Claude CLI at `~/.claude`. Requires an active **Pro or Max** subscription | Recent local token totals from Claude project logs (no live rate-limit percentage) |
| **Grok (SuperGrok / Build)** | Sign in in Settings, or reuse `~/.grok/auth.json` | Weekly SuperGrok pool and, when available, credit details in the popup |
| **Moonshot Kimi Code** | Sign in with Kimi Code. Session defaults to `~/.kimi-code/credentials/kimi-code.json` (or `KIMI_CODE_HOME`) | Kimi Code 5-hour and 7-day plan quotas |
| **OpenRouter** | API key for per-key limits; optional **Management key** for account-wide balance | Daily, weekly, monthly, or lifetime key limits plus account balance when available |

Claude is registered only when an active Pro/Max subscription is detected. Providers without usable credentials or usage data stay hidden until configured.

---

## Getting started

1. Download and run the setup EXE from [Releases](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest).
2. If SmartScreen appears, verify that the file came from this repository, then continue.
3. Open the **gear** on the overlay and enable a provider.
4. Sign in, paste an API key, or pick the MiniMax region as shown in Settings.
5. Click a segment for details. Use the tray icon to open, hide, or quit.

The tracker talks to enabled providers directly and keeps the last successful reading visible through short network interruptions. It does not increase quotas; it helps you see and pace the allowance you already have.

---

## Settings

| Setting | Description |
| --- | --- |
| **Refresh interval** | 30 seconds to 5 minutes (default **60s**) |
| **Provider visibility** | Hide a configured provider without deleting credentials |
| **Autostart** | Start with Windows sign-in |
| **Provider order** | Drag segments; order is saved locally |
| **Update channel** | Stable releases or prerelease builds |

Updates check once a day and on demand. Packages download from GitHub and are verified with the app's signed updater package before installation. The updater signature is separate from Windows Authenticode, so SmartScreen may still warn on a fresh installer.

---

## Privacy

AI Usage Tracker has **no telemetry, advertising, analytics, crash-reporting service, or developer server**.

| Data | Where it lives |
| --- | --- |
| API keys | **Windows Credential Manager** |
| OAuth sessions | Local official CLI credential files (Kimi Code uses the same format as its CLI) |
| Preferences and snapshots | Windows user profile |
| Network | Enabled providers for usage; GitHub for updates only |

Usage data is not routed through a server operated by the AI Usage Tracker project. Never post API keys, OAuth tokens, credential files, or unredacted account details in an issue.

---

## Development

### Requirements

- Windows 10 or 11 x64
- Node.js 24 or newer
- Rust stable with the `x86_64-pc-windows-msvc` target
- Visual Studio Build Tools with MSVC and the Windows SDK
- Microsoft Edge WebView2

Install dependencies and run the frontend-only Vite server at `http://localhost:1420`:

```powershell
npm ci
npm run dev
```

Run the complete desktop application:

```powershell
npm run tauri:dev
```

Run the same checks used by GitHub Actions:

```powershell
npm run build
Get-ChildItem scripts/test_*.mjs | ForEach-Object { node $_.FullName }
cargo check --manifest-path src-tauri/Cargo.toml --locked
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Provider credentials are entered in the app or read from official CLI files. They remain local and are never part of the repository.

---

## Support

Report reproducible bugs through [GitHub Issues](https://github.com/GimpMan/AI-Usage-Tracker/issues). Include the Windows version, app version, provider, exact error text, and safe reproduction steps. Search existing issues first and never include credentials or account identifiers.

---

## License

AI Usage Tracker is open-source software licensed under the [MIT License](LICENSE).
