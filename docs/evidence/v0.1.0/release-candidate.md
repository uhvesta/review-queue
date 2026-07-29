# Signed release candidate evidence

Release candidate `v0.1.0-rc.1` was built from commit
`aeafea1421a35a1698dd85018743fd8821860ee9` on macOS and published as a
GitHub prerelease:

<https://github.com/uhvesta/review-queue/releases/tag/v0.1.0-rc.1>

The universal `arm64 x86_64` app and DMG passed strict code-signature
verification, stapler validation, Gatekeeper assessment with
`source=Notarized Developer ID`, archive extraction verification, DMG
verification, updater-signature verification, and the release artifact secret
scan. Apple accepted both submissions:

- App submission: `8c46d1c2-79ca-46f3-8ee1-7b2b8eba707b`
- DMG submission: `17305724-4130-4212-a640-2e49b615d8b1`

The published release contains the DMG, app zip, signed updater archive and
signature, update manifest, SHA-256 manifest, CycloneDX SBOM, and notarization
record. Retained SHA-256 values:

```text
259358682a96adcefd5ee0f55bd654ab91372c5ec932f0f46d1de40b7d5d4978  Review-Queue-0.1.0-rc.1-universal-candidate.dmg
6563a85f6e6dbcf21444362bc3ed3013e736d91ef0324088fefa5a8a96bded28  Review-Queue-0.1.0-rc.1-universal-candidate.app.zip
7ddfba6171a4efd49732e4b8fba49ffb4867244e8e49c725ab28005617bd4c6c  Review-Queue-0.1.0-rc.1-universal-candidate.app.tar.gz
2bcebaa46e4481c42a199013198372baf33b890fa566fb17497f33ff5dd80b2e  Review-Queue-0.1.0-rc.1-universal-candidate.app.tar.gz.sig
c566a7a7037dd2bb243806915f822dcc845a85ad1a887001f37ec50de53f27ff  latest.json
73528610c139f7b237e1692714c20d2ba209ff4b4ca89593c9691d682d1e1c65  Review-Queue-0.1.0-rc.1.cdx.json
ee22a67c1a38b47c6a05d09b7df953021949a52541706ec98119b818ce563525  Review-Queue-0.1.0-rc.1.notarization.json
```

The signed executable then passed the packaged launch smoke and two-phase
disposable Keychain restart/account-separation harness.
