# ADR 0002: local capture uses isolated commit preparation, then normalizes the index

Status: accepted

Submission must include staged, unstaged, deleted, binary, and untracked work
in a real per-repository commit. A capture failure must not leave a torn queue
round or a partially advanced branch.

Preflight lists every detected repository and requires an explicit non-empty
participation selection. Its token binds the canonical workspace, complete
review brief, topic, selected repository IDs, heads, and porcelain status.
Changing any form field, selection, ref, index, worktree, or untracked file
invalidates that token before mutation.

The implementation builds each selected commit through a temporary Git index.
The resulting refs remain guarded by `PendingCapture`, which retains the exact
original index bytes. The same desktop-owned operation then opens the SQLite
submission transaction, derives canonical topic identity/rank, synchronizes
real indexes, and commits SQLite. A database or index-finalization failure
drops the database transaction and restores every safely-owned ref plus the
exact original staging indexes.

The CLI performs no Git capture. It sends a token-free preflight request and
then the bound capture request to the already-running desktop socket, so there
is no post-capture handoff from CLI to desktop. For capture only, the socket
uses a two-phase acknowledgement: the desktop prepares Git/SQLite/index state,
the CLI acknowledges the prepared response, and only then does SQLite commit
and the desktop return final success. Disconnecting before acknowledgement
restores the database transaction, refs, and exact indexes. Disconnecting
after acknowledgement may make the final response ambiguous, but retry remains
idempotent because a successful snapshot commit and queue round are persisted
together.

This resolves a tension between preserving the physical staging file and
correctness of the newly created review commit. The spec prioritizes data
safety and no dead ends: source content is never deleted or rewritten, and a
successful submit must be repeatable without inventing a reverse commit.
