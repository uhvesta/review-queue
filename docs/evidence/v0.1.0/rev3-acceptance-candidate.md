# Rev3 signed acceptance candidate

Source commit `04ad09cade3f5097ef66c0e7f689cf84af8556c9` was built on
2026-07-29 as the universal `0.1.0` candidate used to close the final Rev3
acceptance gaps.

- The app and DMG were signed with the Developer ID Application identity for
  team `7H66Q22DJD`.
- Apple accepted both notarization submissions; both staples validated.
- Strict deep code-signature verification and Gatekeeper assessment passed for
  the app and DMG.
- The executable contains both `arm64` and `x86_64`.
- All seven checksum entries passed.
- The updater archive signature verified against the committed updater public
  key.
- The hardened release scan passed against the mounted/extracted app, DMG,
  app ZIP, updater archive, signature, updater manifest, SBOM, and
  notarization record.

The release pipeline re-ran:

- 42/42 frontend tests and the production TypeScript/Vite build;
- 10 CLI unit tests, two daemon integrations (including real OpenSSH), four
  CLI parity integrations, 96 core tests, and seven adversarial no-side-effect
  tests;
- 53 desktop tests, with the two disposable Keychain tests also run
  explicitly outside the default suite;
- workspace and desktop formatting plus strict Clippy with warnings denied.

This candidate is retained locally for acceptance only and is not a published
release or updater feed.
