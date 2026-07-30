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

The payload-aware scanner then passed the notarized universal candidate built
from `04ad09cade3f5097ef66c0e7f689cf84af8556c9`: mounted app and DMG,
extracted app ZIP and updater archive, signature, updater manifest, SBOM, and
notarization record. It also passed the complete evidence directory after the
packaged ACP/lifecycle JSONL captures and the live application-support
directory.

Clear/leaking app-archive and DMG fixtures passed: clean payloads were
accepted, credential-shaped payloads were rejected, and no match was printed.
Compressed container bytes are no longer scanned as if they were file
contents, avoiding random credential-shaped false positives while scanning the
actual files users receive.

Successful scans print only `release secret scan passed`; failures report only
the affected path. No application log directory was present. The runtime IPC
endpoint was a Unix-domain socket rather than a persistent regular file, and
the socket allowlist/token-free boundary is covered by the workspace
integration tests.
