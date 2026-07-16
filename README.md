<p align="center"><img src="assets/ai-usage-tracker-banner.png?v=62fa9ec" width="740" alt="AI Usage Tracker"></p>

<h1 align="center">AI Usage Tracker</h1>

<p align="center">
  <strong>Always-on Windows overlay for AI rate limits, quotas, and prepaid credits.</strong><br>
  See remaining allowance for Z.ai, MiniMax, Codex, Claude, Grok, Kimi Code, and OpenRouter — without jumping between dashboards.
</p>

<p align="center">
  <a href="#download">Download</a>
  ·
  <a href="#what-it-tracks">What it tracks</a>
  ·
  <a href="PRIVACY.md">Privacy</a>
  ·
  <a href="SUPPORT.md">Support</a>
</p>

<p align="center">
  <img src="assets/demo.gif" width="720" alt="AI Usage Tracker demo">
</p>

## Download

[**Download the latest Windows setup EXE**](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest)

AI Usage Tracker is currently distributed for **64-bit Windows 10 and Windows 11**.

- The supported package is the **NSIS setup EXE**.
- It installs for the current Windows user and does not require administrator access.
- If Microsoft Edge WebView2 is missing, the installer can download its bootstrapper automatically.
- Nothing is pre-authenticated: API keys, OAuth sessions, and usage history are not included in the installer.

The public repository contains installers and documentation only; it is not the application source repository.

## What it tracks

The overlay summarizes enabled and authenticated providers in a single bar. Click a provider segment to see its available windows, reset times, balance details, or local usage totals. Drag segments to reorder them or drag the overlay to reposition it.

### Supported providers

| Provider | Authentication | Information shown |
| --- | --- | --- |
| **Z.ai Coding Plan** | API key from **z.ai → Manage API Key** | 5-hour and weekly coding windows; the monthly Web Search / Reader / Zread tool quota appears in the popup. |
| **MiniMax Coding Plan** | Coding Plan or Token Plan API key; choose **Overseas** (`minimax.io`) or **China** (`minimaxi.com`) to match the key. | 5-hour and weekly quota windows. |
| **OpenAI Codex CLI** | Sign in with ChatGPT in Settings, or reuse the official CLI session at `~/.codex/auth.json`. No API key is required. | Live primary and secondary Codex rate-limit windows, including reset times. |
| **Claude Code** | Sign in in Settings or use the Claude CLI at `~/.claude`. An active **Pro or Max** subscription is required. | Recent local token totals from Claude project logs. Claude does not provide a live rate-limit percentage here. |
| **Grok (SuperGrok / Build)** | Sign in in Settings, or reuse `~/.grok/auth.json`. | The weekly SuperGrok pool and, when available, credit details in the popup. |
| **Moonshot Kimi Code** | Sign in with Kimi Code. The shared session is stored at `~/.kimi-code/credentials/kimi-code.json` by default, or under the `KIMI_CODE_HOME` directory. No API key is required. | Kimi Code 5-hour and 7-day plan quotas. |
| **OpenRouter** | Normal API key for per-key limits. An optional **Management key** adds account-wide balance, top-up detection, and local balance rebasing. | Daily, weekly, monthly, or lifetime key limits plus account balance when available. |

Claude is only registered when the app detects an active Pro/Max subscription. Providers without usable credentials or usage data are automatically hidden from the overlay until they are configured.

## Install and first use

1. Download and run the setup EXE from [Releases](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest).
2. If Windows SmartScreen warns that the publisher is unrecognized, confirm that the installer came from this repository’s Releases page. The setup EXE is not Authenticode-signed.
3. Open the **gear** button on the overlay and enable a provider.
4. Sign in, paste an API key, or select the MiniMax region as indicated in Settings.
5. Click a segment for details. Use the tray icon to open, hide, or quit the app.

The tracker contacts enabled providers directly and keeps the last successful reading visible through transient network failures. It does not increase quotas; it helps you see and pace the allowance you already have.

## Settings and updates

- **Refresh interval:** 30 seconds to 5 minutes; the default is 60 seconds.
- **Provider visibility:** hide a configured provider without deleting its credentials.
- **Autostart:** optionally start AI Usage Tracker when you sign in to Windows.
- **Provider order:** drag segments into a preferred order; the order is saved locally.
- **Update channel:** choose stable releases or prerelease builds.

The app checks for updates automatically once a day and also supports a manual check. Updates are downloaded from GitHub and verified with the app’s signed updater package before installation; installation is always user-initiated. The updater signature is separate from Windows Authenticode signing, so SmartScreen may still warn on a fresh installer.

## Privacy and local data

AI Usage Tracker has **no telemetry, advertising, analytics, crash-reporting service, or developer server**.

- API keys are stored in **Windows Credential Manager**.
- OAuth sessions are stored locally in the provider files used by the relevant official CLIs; Kimi Code shares its official credential-file format.
- Preferences, provider visibility, overlay position, refresh settings, and usage snapshots stay in the Windows user profile.
- The app contacts enabled providers directly to authenticate and retrieve usage data. It also contacts GitHub for update checks and downloads.
- Usage data is not routed through a server operated by the AI Usage Tracker developer.

Read the full [Privacy Policy](PRIVACY.md), [Support guide](SUPPORT.md), and [Security Policy](SECURITY.md). Never post API keys, OAuth tokens, credential files, or unredacted account details in an issue.

## License status

AI Usage Tracker is proprietary, closed-source software distributed in binary form. This documentation repository does not contain the application source code and does not grant an open-source license. See [NOTICE.txt](NOTICE.txt).
