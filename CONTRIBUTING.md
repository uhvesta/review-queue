# Contributing to Review Queue

Review Queue treats source repositories and review data as user work. Changes
must preserve immutable snapshots, make lifecycle mutations explicit, and
provide an actionable recovery path for every failure.

Before opening a change, add or update a focused acceptance test. Do not add a
token to source, fixtures, logs, URLs, diagnostics, process arguments, or the
token-free data plane. UI changes should retain a visible exit for every state
and must not cause a Copilot prompt, delivery, publish, or lifecycle mutation
on open or refresh.

Run `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, and
`cargo test --workspace` before review.
