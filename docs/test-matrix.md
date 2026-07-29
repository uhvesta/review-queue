# Product test matrix

| Flow | Status | Evidence |
| --- | --- | --- |
| Local multi-repository commit-on-submit | passing | Rust integration tests |
| Queue rank, supersession, and lifecycle | passing | Rust unit tests |
| Socket allowlist / token-free CLI boundary | passing | Rust unit tests |
| Purge preserves Git source state | passing | Rust unit tests |
| Keychain signed-app integration | not started | requires macOS app shell |
| Copilot streaming and no-replay | blocked | requires SDK adapter |
| GitHub mirror/publish | not started | requires desktop OAuth adapter |
| ACP delivery | not started | requires Phase 3 ADR |

Each eventual row records setup, exact action, expected/actual outcome,
restart behavior, recovery behavior, and retained screenshot/log/request ID.
