# Real OpenSSH connected-machine acceptance

The current commit passed the macOS-only integration
`system_openssh_agent_tunnel_reaches_the_shipped_daemon_without_storing_credentials`.
It used an actual foreground `ssh-agent`, unprivileged `sshd`, isolated
OpenSSH host alias, and `/usr/bin/ssh -N -L` Unix-socket forward to the shipped
standalone `review-queue daemon`.

Through that real tunnel, the client fetched daemon health, the lazy item
index, full item detail, and the immutable snapshot. The desktop-form machine
database contained the validated host alias and remote socket but did not
contain private-key bytes, the key path, or the agent-socket path. The remote
fixture repository's HEAD and complete porcelain status were byte-for-byte
unchanged after the fetches.

The same test passed inside the signed acceptance-candidate release pipeline.
The production tunnel argument contract separately passed with only `-N`,
`ExitOnForwardFailure`, the Unix-socket forward, and the validated host alias;
credential, passphrase, agent-path, proxy-command, and arbitrary-option
arguments are rejected from app configuration.
