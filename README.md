# AI Usage Tracker

Windows overlay for viewing remaining rate-limit quota from Z.ai, MiniMax,
Codex, Claude Code, Grok, Kimi Code, and OpenRouter.

## Requirements

- Windows 10 or 11 x64
- Node.js 24 or newer
- Rust stable with the `x86_64-pc-windows-msvc` target
- Visual Studio Build Tools with MSVC and the Windows SDK
- Microsoft Edge WebView2

## Development

```powershell
npm ci
npm run dev
```

The frontend-only Vite server runs at `http://localhost:1420`. To run the full
desktop app:

```powershell
npm run tauri:dev
```

Provider credentials are entered in the app or read from official CLI files.
They are stored locally and are not included in this repository.

## Checks

```powershell
npm run build
Get-ChildItem scripts/test_*.mjs | ForEach-Object { node $_.FullName }
cargo check --manifest-path src-tauri/Cargo.toml --locked
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

GitHub Actions runs the same checks for pushes and pull requests to `main`.

## Releases

Set `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` as
GitHub Actions secrets. After updating the version in `package.json`,
`src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`, push a matching tag
such as `v0.7.32`. GitHub Actions builds the signed NSIS installer, updater
artifacts, and release notes.

## License

[MIT](LICENSE)
