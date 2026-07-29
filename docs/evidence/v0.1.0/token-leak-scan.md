# Token-leak artifact scan evidence

On 2026-07-29, `scripts/scan-release-secrets.sh` completed successfully against:

- the complete `dist-candidate-rc1` release directory, including the signed
  application bundle, DMG, updater archive, manifests, SBOM, and notarization
  record; and
- the live `~/Library/Application Support/com.reviewqueue.desktop` directory,
  including the SQLite database and all regular runtime files.

After the current-head authentication-source acceptance run, the same scanner
also passed against the Developer-ID-signed `897aae3` debug application, the
complete retained `docs/evidence/v0.1.0` directory (including the source-choice
screenshots), and the live application-support directory.

The scan printed only `release secret scan passed`; it is deliberately designed
never to print matching credential material. No application log directory was
present. The runtime IPC endpoint was a Unix-domain socket rather than a
persistent regular file, and the socket allowlist/token-free boundary is
covered by the workspace integration tests.
