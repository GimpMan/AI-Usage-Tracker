# Security Policy

## Supported version

Only the current version shown on the GitHub Releases page receives security fixes. Upgrade before reporting an issue that is already resolved in the current release.

Application updates use signed update packages. The `latest.json` metadata is delivered over GitHub HTTPS and supplies the package signature; the installed app verifies that signature before applying an update. The metadata file is not described as separately signed. The Windows setup EXE is **not Authenticode-signed**, so SmartScreen may warn on first installation; download only from this repository’s Releases page.

## Reporting a vulnerability

Please privately report a vulnerability through GitHub’s **Security** tab and **Report a vulnerability** form. Do not open a public issue for an unpatched vulnerability. Include the affected version, reproduction steps, impact, and any suggested mitigation. Please do not include live credentials or tokens.
