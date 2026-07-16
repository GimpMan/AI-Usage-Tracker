<p align="center"><img src="assets/ai-usage-tracker-banner.png?v=62fa9ec" width="740" alt="AI Usage Tracker"></p>

<h1 align="center">AI Usage Tracker</h1>

<p align="center">
  <strong>Always-on Windows overlay for AI rate limits, quotas, and prepaid credits.</strong><br>
  See remaining allowance for Z.ai, MiniMax, Codex, Claude, Grok, Kimi Code, and OpenRouter — without jumping between dashboards.
</p>

<p align="center">
  <a href="#download">Download</a>
  ·
  <a href="#features">Features</a>
  ·
  <a href="#supported-providers">Providers</a>
  ·
  <a href="#getting-started">Getting started</a>
  ·
  <a href="PRIVACY.md">Privacy</a>
  ·
  <a href="SUPPORT.md">Support</a>
</p>

<p align="center">
  <a href="https://github.com/GimpMan/AI-Usage-Tracker/releases/latest">
    <img src="https://img.shields.io/github/v/release/GimpMan/AI-Usage-Tracker?style=flat-square&label=latest" alt="Latest release">
  </a>
  <a href="https://github.com/GimpMan/AI-Usage-Tracker/releases/latest">
    <img src="https://img.shields.io/badge/platform-Windows%2010%20%2F%2011%20x64-0078D4?style=flat-square" alt="Windows 10/11 x64">
  </a>
  <a href="PRIVACY.md">
    <img src="https://img.shields.io/badge/telemetry-none-2ea44f?style=flat-square" alt="No telemetry">
  </a>
  <a href="NOTICE.txt">
    <img src="https://img.shields.io/badge/license-proprietary-lightgrey?style=flat-square" alt="Proprietary">
  </a>
</p>

<p align="center">
  <img src="assets/demo-hq.gif" width="720" alt="AI Usage Tracker demo">
</p>

---

## Features

- **Always visible** — a slim overlay bar stays on top while you work
- **Multi-provider** — Z.ai, MiniMax, Codex, Claude, Grok, Kimi Code, and OpenRouter in one place
- **Click for details** — windows, reset times, balances, and local usage totals in a popup
- **Reorder & drag** — rearrange segments; drag the bar where you want it
- **Pace-aware** — weekly and window remaining percentages at a glance
- **Local-first** — keys and sessions stay on your machine; no developer backend
- **Auto-updates** — daily check with signed packages; install when you choose

---

## Download

| Requirement | Detail |
| --- | --- |
| OS | **64-bit Windows 10 and Windows 11** |
| Package | **NSIS setup EXE** (current user; no admin required) |
| Runtime | Microsoft Edge WebView2 (installer can bootstrap it if missing) |

**[Download the latest Windows setup EXE →](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest)**

> [!NOTE]
> Windows SmartScreen may warn on first install because the setup EXE is **not Authenticode-signed**. Confirm you downloaded from this repository’s [Releases](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest) page before continuing.

This public repository ships **installers and documentation only**. It is not the application source repository. Nothing is pre-authenticated: API keys, OAuth sessions, and usage history are not included in the installer.

---

## Supported providers

| Provider | Authentication | Information shown |
| --- | --- | --- |
| **Z.ai Coding Plan** | API key from **z.ai → Manage API Key** | 5-hour and weekly coding windows; monthly Web Search / Reader / Zread tool quota in the popup |
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
2. If SmartScreen appears, verify the file came from this repository, then continue.
3. Open the **gear** on the overlay and enable a provider.
4. Sign in, paste an API key, or pick the MiniMax region as shown in Settings.
5. Click a segment for details. Use the tray icon to open, hide, or quit.

The tracker talks to enabled providers **directly** and keeps the last successful reading visible through short network blips. It does not increase quotas — it helps you see and pace the allowance you already have.

---

## Settings

| Setting | Description |
| --- | --- |
| **Refresh interval** | 30 seconds to 5 minutes (default **60s**) |
| **Provider visibility** | Hide a configured provider without deleting credentials |
| **Autostart** | Start with Windows sign-in |
| **Provider order** | Drag segments; order is saved locally |
| **Update channel** | Stable releases or prerelease builds |

Updates check once a day (and on demand). Packages download from GitHub and are verified with the app’s **signed updater** package before install; installation is always user-initiated. That signature is separate from Windows Authenticode, so SmartScreen may still warn on a fresh installer.

---

## Privacy

AI Usage Tracker has **no telemetry, advertising, analytics, crash-reporting service, or developer server**.

| Data | Where it lives |
| --- | --- |
| API keys | **Windows Credential Manager** |
| OAuth sessions | Local official CLI credential files (Kimi Code uses the same format as its CLI) |
| Preferences & snapshots | Windows user profile |
| Network | Enabled providers for usage; GitHub for updates only |

Usage data is not routed through a server operated by the AI Usage Tracker developer.

Full details: [Privacy Policy](PRIVACY.md) · [Security Policy](SECURITY.md) · [Support](SUPPORT.md)

> Never post API keys, OAuth tokens, credential files, or unredacted account details in an issue.

---

## Support

Bugs and help requests → [GitHub Issues](https://github.com/GimpMan/AI-Usage-Tracker/issues)

Include Windows version, AI Usage Tracker version, provider, exact error text, and safe reproduction steps. Search existing issues first.

Common fixes (WebView2, auth, storage, updates, uninstall) are documented in [SUPPORT.md](SUPPORT.md).

---

## License

AI Usage Tracker is **proprietary, closed-source** software distributed in binary form. This documentation repository does not contain the application source code and does not grant an open-source license.

See [NOTICE.txt](NOTICE.txt).
