import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  RepositoryFileTree,
  buildRepositoryFileTree,
  filterRepositoryFileTree,
  repositoryFileKey,
} from "../src/RepositoryFileTree";
import type { DiffFile, RepositoryDiff } from "../src/types";

function fixtureFile(repositoryId: string, path: string, additions = 1, deletions = 0): DiffFile {
  return {
    repository_id: repositoryId,
    old_path: path,
    new_path: path,
    old_blob_sha: `${repositoryId}-old-${path}`,
    new_blob_sha: `${repositoryId}-new-${path}`,
    status: "modified",
    is_binary: false,
    patch: "",
    hunks: [{
      repository_id: repositoryId,
      old_start: 1,
      old_lines: additions + deletions,
      new_start: 1,
      new_lines: additions + deletions,
      header: "",
      lines: [
        ...Array.from({ length: deletions }, (_, index) => ({ type: "deletion" as const, content: `removed ${index}` })),
        ...Array.from({ length: additions }, (_, index) => ({ type: "addition" as const, content: `added ${index}` })),
      ],
    }],
  };
}

const repositories: RepositoryDiff[] = [
  {
    repository_id: "repo-web",
    root: "web",
    base_sha: "web-base",
    head_sha: "web-head",
    files: [
      fixtureFile("repo-web", "src/hooks/usePagination.ts", 3, 1),
      fixtureFile("repo-web", "src/App.tsx", 1),
    ],
  },
  {
    repository_id: "repo-api",
    root: "api",
    base_sha: "api-base",
    head_sha: "api-head",
    files: [fixtureFile("repo-api", "src/hooks/usePagination.ts", 2)],
  },
];

describe("RepositoryFileTree", () => {
  it("keeps same-named paths repository-qualified while sorting roots and collapsing unambiguous folders", () => {
    const tree = buildRepositoryFileTree(repositories);

    expect(tree.map((repository) => repository.repositoryRoot)).toEqual(["api", "web"]);
    expect(tree[0]?.children[0]).toMatchObject({
      kind: "directory",
      name: "src/hooks",
      key: "repo-api\u0000src/hooks",
    });
    expect(repositoryFileKey("repo-api", "src/hooks/usePagination.ts")).not.toBe(
      repositoryFileKey("repo-web", "src/hooks/usePagination.ts"),
    );
  });

  it("filters only matching branches and renders an explicit empty result", () => {
    const tree = buildRepositoryFileTree(repositories);
    expect(filterRepositoryFileTree(tree, "web/src/hooks/usepagination")).toHaveLength(1);
    expect(filterRepositoryFileTree(tree, "does-not-exist")).toHaveLength(0);

    render(
      <RepositoryFileTree
        repositories={repositories}
        viewedKeys={new Set()}
        onSelect={vi.fn()}
        onToggleViewed={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByRole("searchbox", { name: "Filter changed files" }), {
      target: { value: "does-not-exist" },
    });

    expect(screen.getByRole("status")).toHaveTextContent("No changed files match");
  });

  it("selects a tree leaf and uses the existing viewed callback with its qualified identity", () => {
    const onSelect = vi.fn();
    const onToggleViewed = vi.fn();
    const webPath = "src/hooks/usePagination.ts";
    const webKey = repositoryFileKey("repo-web", webPath);
    render(
      <RepositoryFileTree
        repositories={repositories}
        selectedKey={webKey}
        viewedKeys={new Set([webKey])}
        onSelect={onSelect}
        onToggleViewed={onToggleViewed}
      />,
    );

    fireEvent.click(screen.getAllByRole("button", { name: "usePagination.ts" })[1]!);
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ key: webKey, repositoryId: "repo-web", path: webPath }));

    fireEvent.click(screen.getByRole("button", { name: `Mark ${webPath} not viewed` }));
    expect(onToggleViewed).toHaveBeenCalledWith(
      expect.objectContaining({ key: webKey, repositoryId: "repo-web", path: webPath }),
      false,
    );
  });
});
