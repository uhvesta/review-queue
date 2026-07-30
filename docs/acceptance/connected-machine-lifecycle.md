# Connected-machine and lifecycle acceptance

This procedure covers the release-spec requirements for a real system
OpenSSH tunnel, the shipped token-free daemon, and explicit local lifecycle
operations. The automated layers are deterministic and disposable. The final
UI pass must use the production app bundle because unit or browser-fixture
results alone are not release evidence.

## Deterministic real SSH tunnel

Run the macOS integration test:

```bash
cargo test -p review-queue --test machine_daemon \
  system_openssh_agent_tunnel_reaches_the_shipped_daemon_without_storing_credentials
```

The test creates an ephemeral source repository, standalone daemon database,
owner-only Unix sockets, SSH host/user keys, foreground `ssh-agent`, and
unprivileged `sshd`. It then:

1. starts the built `review-queue daemon` executable;
2. loads the disposable user key into the agent;
3. resolves a host alias, port, user, and `IdentityAgent` through an isolated
   OpenSSH config;
4. starts an actual `/usr/bin/ssh -N -L local.sock:remote.sock` process;
5. fetches daemon health, index, detail, and immutable snapshot through the
   tunnel;
6. verifies the desktop-form machine database contains the alias and remote
   socket but neither private-key bytes nor key/agent paths; and
7. verifies the remote source HEAD and complete worktree status are unchanged.

The desktop-side unit contract separately asserts that production tunnel
launch arguments contain only `-N`, `ExitOnForwardFailure`, the Unix-socket
forward, and the validated config host. Identity files, passphrases, agent
paths, proxy commands, and arbitrary SSH option strings never enter app
configuration or process arguments.

## Restart-separated packaged lifecycle

Run this against the exact production app candidate. The optional evidence
path must not already exist.

```bash
scripts/run-packaged-acceptance-smoke.sh \
  --app "/Applications/Review Queue.app" \
  --lifecycle \
  --lifecycle-evidence \
  docs/evidence/v0.1.0/packaged-lifecycle.jsonl
```

The first invocation of the packaged executable creates two disposable Git
repositories, submits a multi-repository snapshot, resubmits a newer snapshot,
and exercises Request changes, Complete, Requeue, rank movement, and rejected
delete/approval confirmations. The second executable process reopens the same
SQLite database, verifies lifecycle/rank/supersession persistence, confirms
Delete and local Approve purges, and checks their retained lifecycle audit
events. Both phases compare every source repository's HEAD, tree, and complete
porcelain status. No production app data or Keychain item is used.

The two retained JSON lines must show:

- `requestCompleteRequeueEvents` exactly
  `["request_changes","complete","requeue"]`;
- both cancellation codes as `confirmation_required` in phase one;
- `databaseReopened: true`, `deletedRoundPurged: true`, and
  `approvedRoundPurged: true` in phase two;
- `supersededRoundRetained: true`; and
- `sourceCommitsAndFilesUnchanged: true` in both phases.

The frontend fixture tests also click both confirmation dialogs and prove
Cancel performs no API call while the destructive button sends the exact
delete or local-approval operation.

## Required production UI evidence

The automation above does not replace the final native UI observation:

1. On a disposable remote account, start the same built standalone daemon on
   an owner-only absolute Unix socket.
2. Put host, port, user, jump-host/key policy, and agent selection in the
   user's ordinary SSH config. Add only the host alias and remote socket to
   Review Queue. Do not paste or copy a credential into the app.
3. In the production app, explicitly Connect, fetch health, refresh the lazy
   index, open one item, and cache its immutable snapshot. Capture the config
   summary, connected health, and materialized review without credential
   values.
4. Disconnect and stop the daemon. Reopen the cached round, preview
   reproduction, then confirm a new detached reproduction. Verify the recorded
   source commits and files are unchanged.
5. Retry once with the daemon or tunnel unavailable. Retain the actionable
   failure showing retry/inspect-target and disconnect/delete recovery, then
   reconnect or remove the stale source.
6. In a disposable local multi-repository round, visually exercise Request
   changes, Complete, Show old, Requeue, rank movement, Delete → Cancel,
   Delete → confirm, Approve → Cancel, and Approve → confirm. Restart between
   rank/requeue and terminal actions. Retain before/after screenshots plus Git
   HEAD/tree/status output.

Record the exact app/daemon commit, app signature identity, daemon command
path, SSH host alias (not credential material), machine ID, round IDs,
snapshot version, restart boundary, screenshots, and the JSONL lifecycle log.
Run the release token scan over all retained artifacts before marking the
acceptance rows passing.
