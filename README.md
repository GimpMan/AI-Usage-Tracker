<p align="center"><img src="assets/ai-usage-tracker-banner.png?v=62fa9ec" width="740" alt="AI Usage Tracker"></p>

# AI Usage Tracker

**A compact quota overlay for Windows 10/11 x64.** See remaining AI coding-plan allowance at a glance, without repeatedly opening provider dashboards.

## Download

[**Open the latest release and download the Windows setup EXE**](https://github.com/GimpMan/AI-Usage-Tracker/releases/latest)

The setup EXE is the supported installer. AI Usage Tracker is currently available only for 64-bit Windows 10 and Windows 11.

## Supported providers

- Z.ai Coding Plan
- MiniMax Coding Plan
- Grok/SuperGrok
- OpenAI Codex
- Claude Code Pro/Max
- OpenRouter

## Install and first use

1. Download and run the setup EXE. The installer can add Microsoft Edge WebView2 if it is missing.
2. Windows SmartScreen may show an “unrecognized app” warning because the installer is not Authenticode-signed. Confirm that the file came from this repository’s Releases page, choose **More info**, then **Run anyway** if you want to continue.
3. Open the gear button on the overlay, enable a provider, and follow its authentication instructions.
4. The overlay refreshes periodically. Click it for details or use the tray icon to open, hide, or quit.

Updates are delivered through the app’s signed updater. When an update is available, the app asks before installing it; updater signatures protect the package even though the installer has no Authenticode certificate.

## Privacy and help

There is no telemetry. Credentials remain in Windows Credential Manager or the local files used by official provider CLIs, and usage requests go directly to providers. Read the full [Privacy Policy](PRIVACY.md).

For troubleshooting and issue reports, see [Support](SUPPORT.md). Security researchers should follow the [Security Policy](SECURITY.md).

## License status

AI Usage Tracker is proprietary, closed-source software distributed in binary form. This documentation repository does not contain the application source code and does not grant an open-source license. See [NOTICE.txt](NOTICE.txt).
