# Support

Use [GitHub Issues](https://github.com/GimpMan/AI-Usage-Tracker/issues) for reproducible bugs and support requests. Search existing issues first and include your Windows version, AI Usage Tracker version, provider, exact error text, and safe reproduction steps. Never post API keys, tokens, account identifiers, or credential files.

## Common recovery steps

- **Install or blank window:** Install pending Windows updates and repair or install Microsoft Edge WebView2 Runtime, then restart the app. Re-download setup only from Releases.
- **Authentication:** Reopen Settings from the overlay gear, verify the provider is enabled, and repeat its sign-in or API-key flow. Confirm the corresponding official CLI works where applicable.
- **Storage:** Secrets use Windows Credential Manager; configuration and state live in your Windows user profile. Do not attach these files to an issue without redacting them.
- **Update:** Quit and reopen the app, confirm GitHub is reachable, and retry. If necessary, download the newest setup EXE and install it over the existing version.
- **Uninstall:** Use Windows **Settings → Apps → Installed apps → AI Usage Tracker → Uninstall**. Provider CLI credentials remain managed by those CLIs. If you want a complete cleanup, remove the AI Usage Tracker entries from Windows Credential Manager and its local app-data folder after uninstalling.

Security-sensitive reports belong in GitHub private vulnerability reporting, as described in [SECURITY.md](SECURITY.md), not in an issue.
