# Initial stable updater acceptance

The first stable release has one unavoidable ordering dependency: the
`Signed updater / relaunch` matrix row cannot pass until a signed `0.1.0`
feed exists, while the real `v0.1.0` release must not be published until that
row passes. Use one disposable, non-prerelease GitHub release to exercise the
candidate-to-stable transition. This is the only release procedure allowed to
invoke `--updater-acceptance-bootstrap`.

The bootstrap skips exactly the `Signed updater / relaunch` evidence row. It
still requires every other matrix row, a clean worktree, and a local
`updater-acceptance-v0.1.0` tag pointing to `HEAD`. A normal stable build
requires the exact `v0.1.0` tag, forces the bootstrap environment off, and
requires the complete matrix.

## 1. Freeze the prospective stable commit

Finish and commit every product, test, and non-updater acceptance change.
Confirm that the worktree is clean and that the release-readiness test passes
only when its updater exception is selected:

```bash
git status --short
bash scripts/test-check-release-readiness.sh
REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP=1 \
  scripts/check-release-readiness.sh
```

Create and push a disposable tag at that exact commit:

```bash
git tag -a updater-acceptance-v0.1.0 \
  -m "Disposable v0.1.0 updater acceptance feed"
git push origin refs/tags/updater-acceptance-v0.1.0
```

## 2. Build and publish the disposable feed

Load the updater signing key and password from their protected local files
without printing them. Build with the authorized notary profile:

```bash
scripts/release-macos.sh \
  --profile localreview-notary \
  --version 0.1.0 \
  --channel stable \
  --release-tag updater-acceptance-v0.1.0 \
  --output dist-updater-acceptance \
  --updater-acceptance-bootstrap
```

Verify that the pushed annotated tag peels to the exact local commit before
creating any release. Upload all eight retained artifacts to a draft first,
verify the draft, and only then atomically publish it as the latest
non-prerelease. This sequence works with the repository host's installed
GitHub CLI and prevents a partial upload from replacing the updater feed:

```bash
tag=updater-acceptance-v0.1.0
test "$(git rev-parse "$tag^{}")" = \
  "$(git ls-remote --tags origin "refs/tags/$tag^{}" | awk '{print $1}')"

gh release create "$tag" \
  dist-updater-acceptance/*.dmg \
  dist-updater-acceptance/*.app.zip \
  dist-updater-acceptance/*.app.tar.gz \
  dist-updater-acceptance/*.app.tar.gz.sig \
  dist-updater-acceptance/latest.json \
  dist-updater-acceptance/SHA256SUMS \
  dist-updater-acceptance/*.cdx.json \
  dist-updater-acceptance/*.notarization.json \
  --repo uhvesta/review-queue \
  --draft \
  --title "Review Queue updater acceptance (disposable)" \
  --notes "Temporary signed feed for candidate-to-v0.1.0 updater acceptance."

release_id="$(gh api \
  "repos/uhvesta/review-queue/releases/tags/$tag" --jq .id)"
gh api "repos/uhvesta/review-queue/releases/$release_id/assets" \
  --jq 'map(select(.state == "uploaded") | .name) | sort'
# Compare the output to the exact eight files in dist-updater-acceptance.
gh api -X PATCH \
  "repos/uhvesta/review-queue/releases/$release_id" \
  -F draft=false -F prerelease=false -F make_latest=true
```

Before opening the app, verify that
`https://github.com/uhvesta/review-queue/releases/latest/download/latest.json`
returns version `0.1.0`, references
`updater-acceptance-v0.1.0`, contains both macOS architectures, and carries
the same signature as the uploaded updater archive's `.sig` asset.

## 3. Exercise candidate to stable

Install the notarized `v0.1.0-rc.1` DMG from the existing prerelease. Verify
its checksum, Gatekeeper assessment, bundle version, and pre-update saved
state. Launch that candidate and retain evidence for each explicit step:

1. Verify the running version is `0.1.0-rc.1` and the persisted GitHub
   connections and one identifiable review/chat record are present.
2. Click **Check for updates**. It must report exactly `0.1.0`; checking must
   not install or restart anything.
3. Click **Confirm install 0.1.0**. The signed archive must download and
   install, while the app remains open and reports that relaunch is required.
4. Click **Confirm relaunch**. This must be the only relaunch trigger.
5. Verify the relaunched bundle and UI report `0.1.0`, Gatekeeper still
   accepts the installed app, and the identifiable review/chat record plus
   both Keychain-backed GitHub capabilities survived.
6. Check again. With the feed still at `0.1.0`, no newer update may be
   offered.

Retain before/check/install/relaunch/after screenshots, the disposable
release URL, the update archive request, installed bundle versions, and
Gatekeeper output. Never capture Keychain payloads or signing secrets.

If any step fails, leave the updater row non-passing, preserve the failure
evidence, delete the disposable GitHub release/tag as described below, and
manually reinstall the notarized RC DMG. Do not create `v0.1.0`.

## 4. Publish the real stable release

After a successful relaunch, update the updater matrix row and its retained
evidence, then commit those documentation-only changes. Create the real tag
at that evidence commit:

```bash
git tag -a v0.1.0 -m "Review Queue v0.1.0"
git push origin refs/tags/v0.1.0
```

Build again from the clean tagged commit, this time without the bootstrap
flag:

```bash
scripts/release-macos.sh \
  --profile localreview-notary \
  --version 0.1.0 \
  --channel stable \
  --release-tag v0.1.0 \
  --output dist-v0.1.0
```

Publish the real release with the same peeled-tag comparison, draft upload,
exact eight-asset verification, and final REST `PATCH` used for the
disposable feed. Verify that the latest release and latest updater manifest
both resolve to `v0.1.0`, and rerun release verification against the uploaded
artifacts.

## 5. Remove only the disposable GitHub state

Once the real stable release is verified as latest, remove the temporary
release and its remote and local tag:

```bash
gh release delete updater-acceptance-v0.1.0 \
  --repo uhvesta/review-queue \
  --yes
git push origin :refs/tags/updater-acceptance-v0.1.0
git tag -d updater-acceptance-v0.1.0
```

Finally confirm that the disposable release and tag are absent, `v0.1.0`
remains present, and `/releases/latest/download/latest.json` still serves the
real stable manifest. Do not delete the retained local acceptance evidence.
