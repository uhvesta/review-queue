import { describe, expect, it } from "vitest";

import {
  recoveryCopyContract,
  recoveryCopyFor,
  recoveryCopyProblems,
  type RecoveryCopy,
} from "../src/recoveryCopy";

const section8States = [
  "keychain_unavailable",
  "device_code_expired_or_cancelled",
  "wrong_github_account",
  "copilot_unavailable",
  "model_unavailable",
  "network_unavailable",
  "pr_stale",
  "snapshot_unavailable",
  "acp_busy_or_disconnected",
  "remote_tunnel_failed",
  "publish_rejected",
  "workspace_changed_during_capture",
  "submission_commit_failed",
  "publish_without_decision_unreachable",
  "upstream_comment_refresh_failed",
  "cli_app_or_data_plane_unreachable",
  "cli_required_capability_not_connected",
  "invalid_machine_config",
];

describe("Rev3 section 8 recovery-copy snapshot", () => {
  it("catalogs every specified state with complete, non-generic copy", () => {
    expect(recoveryCopyContract.schema_version).toBe(3);
    expect(recoveryCopyContract.contract).toBe("rev3_section_8_error_and_recovery");
    expect(recoveryCopyContract.entries.map((entry) => entry.state)).toEqual(section8States);
    expect(recoveryCopyContract.entries.flatMap(recoveryCopyProblems)).toEqual([]);
    expect(
      new Set(recoveryCopyContract.entries.flatMap((entry) => entry.codes)).size,
    ).toBe(recoveryCopyContract.entries.flatMap((entry) => entry.codes).length);
  });

  it("maps representative UI and CLI error codes to one exact recovery contract", () => {
    expect(recoveryCopyFor("keychain_unavailable")?.state).toBe("keychain_unavailable");
    expect(recoveryCopyFor("github_round_stale")?.state).toBe("pr_stale");
    expect(recoveryCopyFor("machine_tunnel_failed")?.state).toBe("remote_tunnel_failed");
    expect(recoveryCopyFor("github_publish_rejected")?.state).toBe("publish_rejected");
    expect(recoveryCopyFor("workspace_changed_during_capture")?.state).toBe(
      "workspace_changed_during_capture",
    );
    expect(recoveryCopyFor("submission_commit_failed")?.state).toBe(
      "submission_commit_failed",
    );
    expect(recoveryCopyFor("decision_required")?.state).toBe(
      "publish_without_decision_unreachable",
    );
    expect(recoveryCopyFor("github_graphql_request_failed")?.state).toBe(
      "upstream_comment_refresh_failed",
    );
    expect(recoveryCopyFor("app_unreachable")?.state).toBe(
      "cli_app_or_data_plane_unreachable",
    );
    expect(recoveryCopyFor("pr_read_capability_required")?.state).toBe(
      "cli_required_capability_not_connected",
    );
    expect(recoveryCopyFor("machine_ssh_target_invalid")?.state).toBe(
      "invalid_machine_config",
    );
  });

  it("rejects generic or incomplete recovery copy", () => {
    const generic = {
      state: "bad",
      codes: [],
      what_happened: "Something went wrong.",
      why_it_matters: "",
      data_safety: "",
      next_action: "Try again.",
      diagnostics_route: "",
      cancel_route: "",
    } satisfies RecoveryCopy;
    expect(recoveryCopyProblems(generic)).toEqual([
      "bad.why_it_matters is empty",
      "bad.data_safety is empty",
      "bad.diagnostics_route is empty",
      "bad.cancel_route is empty",
      "bad.codes is empty",
      "bad.what_happened is generic",
      "bad.next_action is generic",
    ]);
  });
});
