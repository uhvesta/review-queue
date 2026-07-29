import { useEffect, useMemo, useState, type ReactNode } from "react";

import type { DiffFile, DiffFileStatus, RepositoryDiff } from "./types";

/**
 * A file identity is always qualified by its source repository. Paths alone
 * are not unique in a review round containing more than one repository.
 */
export function repositoryFileKey(repositoryId: string, path: string): string {
  return `${repositoryId}\u0000${path}`;
}

export function diffLineCounts(file: DiffFile): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  for (const hunk of file.hunks) {
    for (const line of hunk.lines) {
      if (line.type === "addition") additions += 1;
      if (line.type === "deletion") deletions += 1;
    }
  }
  return { additions, deletions };
}

export interface RepositoryFileEntry {
  key: string;
  repositoryId: string;
  repositoryRoot: string;
  path: string;
  file: DiffFile;
  additions: number;
  deletions: number;
}

export interface FileTreeDirectory {
  kind: "directory";
  /** A repository-qualified, stable key suitable for React and expansion state. */
  key: string;
  name: string;
  path: string;
  children: FileTreeNode[];
}

export interface FileTreeLeaf {
  kind: "file";
  entry: RepositoryFileEntry;
}

export type FileTreeNode = FileTreeDirectory | FileTreeLeaf;

export interface RepositoryFileTree {
  repositoryId: string;
  repositoryRoot: string;
  children: FileTreeNode[];
}

type MutableDirectory = {
  name: string;
  path: string;
  children: Map<string, MutableDirectory | FileTreeLeaf>;
};

function compareNodes(left: FileTreeNode, right: FileTreeNode): number {
  if (left.kind !== right.kind) return left.kind === "directory" ? -1 : 1;
  const leftName = left.kind === "directory" ? left.name : left.entry.path.split("/").at(-1) ?? "";
  const rightName = right.kind === "directory" ? right.name : right.entry.path.split("/").at(-1) ?? "";
  return leftName.localeCompare(rightName, undefined, { numeric: true, sensitivity: "base" });
}

function materializeDirectory(
  directory: MutableDirectory,
  repositoryId: string,
  collapseSingleChildDirectories: boolean,
): FileTreeDirectory {
  const children = [...directory.children.values()]
    .map((child): FileTreeNode =>
      "children" in child
        ? materializeDirectory(child, repositoryId, true)
        : child,
    )
    .sort(compareNodes);

  if (
    collapseSingleChildDirectories &&
    children.length === 1 &&
    children[0]?.kind === "directory"
  ) {
    const child = children[0];
    return {
      ...child,
      name: `${directory.name}/${child.name}`,
    };
  }

  return {
    kind: "directory",
    key: `${repositoryId}\u0000${directory.path}`,
    name: directory.name,
    path: directory.path,
    children,
  };
}

/**
 * Builds a deterministic, repository-aware tree without mutating API data.
 * Repository roots stay separate; only directories below that root are
 * collapsed when they form an unambiguous single-child chain.
 */
export function buildRepositoryFileTree(repositories: RepositoryDiff[]): RepositoryFileTree[] {
  return repositories
    .map((repository) => {
      const root: MutableDirectory = { name: "", path: "", children: new Map() };

      for (const file of repository.files) {
        const path = file.new_path ?? file.old_path ?? "(unknown path)";
        const segments = path.split("/").filter(Boolean);
        let current = root;

        for (const [index, segment] of segments.entries()) {
          const isLeaf = index === segments.length - 1;
          if (isLeaf) {
            const counts = diffLineCounts(file);
            current.children.set(segment, {
              kind: "file",
              entry: {
                key: repositoryFileKey(file.repository_id, path),
                repositoryId: file.repository_id,
                repositoryRoot: repository.root,
                path,
                file,
                additions: counts.additions,
                deletions: counts.deletions,
              },
            });
            continue;
          }

          const pathSoFar = segments.slice(0, index + 1).join("/");
          const existing = current.children.get(segment);
          if (existing && "children" in existing) {
            current = existing;
            continue;
          }
          const next: MutableDirectory = { name: segment, path: pathSoFar, children: new Map() };
          current.children.set(segment, next);
          current = next;
        }
      }

      const materializedRoot = materializeDirectory(root, repository.repository_id, false);
      return {
        repositoryId: repository.repository_id,
        repositoryRoot: repository.root,
        children: materializedRoot.children,
      };
    })
    .sort((left, right) =>
      left.repositoryRoot.localeCompare(right.repositoryRoot, undefined, { numeric: true, sensitivity: "base" }),
    );
}

function filterNode(node: FileTreeNode, normalizedFilter: string): FileTreeNode | null {
  if (!normalizedFilter) return node;
  if (node.kind === "file") {
    const searchable = `${node.entry.repositoryRoot}/${node.entry.path}`.toLowerCase();
    return searchable.includes(normalizedFilter) ? node : null;
  }

  const children = node.children
    .map((child) => filterNode(child, normalizedFilter))
    .filter((child): child is FileTreeNode => child !== null);
  return children.length ? { ...node, children } : null;
}

/** Returns only the tree branches that contain a matching repository-qualified path. */
export function filterRepositoryFileTree(
  repositories: RepositoryFileTree[],
  filterText: string,
): RepositoryFileTree[] {
  const normalizedFilter = filterText.trim().toLowerCase();
  return repositories
    .map((repository) => ({
      ...repository,
      children: repository.children
        .map((child) => filterNode(child, normalizedFilter))
        .filter((child): child is FileTreeNode => child !== null),
    }))
    .filter((repository) => repository.children.length > 0);
}

function directoryKeys(nodes: FileTreeNode[]): string[] {
  return nodes.flatMap((node) =>
    node.kind === "directory" ? [node.key, ...directoryKeys(node.children)] : [],
  );
}

function statusGlyph(status: DiffFileStatus): string {
  if (status === "added") return "+";
  if (status === "deleted") return "−";
  return "●";
}

export interface RepositoryFileTreeProps {
  repositories: RepositoryDiff[];
  selectedKey?: string | null;
  viewedKeys: ReadonlySet<string>;
  viewedDisabled?: boolean;
  className?: string;
  filterPlaceholder?: string;
  onSelect: (file: RepositoryFileEntry) => void;
  onToggleViewed: (file: RepositoryFileEntry, viewed: boolean) => void;
}

/**
 * A stateful presentation component for a review-round file tree. It owns only
 * its filter and expanded-directory state; selection and viewed persistence
 * remain in the caller so they can continue through the existing API path.
 */
export function RepositoryFileTree({
  repositories,
  selectedKey = null,
  viewedKeys,
  viewedDisabled = false,
  className,
  filterPlaceholder = "Filter files…",
  onSelect,
  onToggleViewed,
}: RepositoryFileTreeProps) {
  const tree = useMemo(() => buildRepositoryFileTree(repositories), [repositories]);
  const allDirectoryKeys = useMemo(
    () => tree.flatMap((repository) => directoryKeys(repository.children)),
    [tree],
  );
  const [filterText, setFilterText] = useState("");
  const [expandedDirectories, setExpandedDirectories] = useState<Set<string>>(
    () => new Set(allDirectoryKeys),
  );

  useEffect(() => {
    setExpandedDirectories(new Set(allDirectoryKeys));
  }, [allDirectoryKeys]);

  const filteredTree = useMemo(
    () => filterRepositoryFileTree(tree, filterText),
    [filterText, tree],
  );
  const filtering = filterText.trim().length > 0;

  const toggleDirectory = (key: string) => {
    setExpandedDirectories((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const renderNode = (node: FileTreeNode, depth: number): ReactNode => {
    if (node.kind === "file") {
      const { entry } = node;
      const viewed = viewedKeys.has(entry.key);
      const selected = selectedKey === entry.key;
      return (
        <li key={entry.key} className="repository-file-tree-file" data-selected={selected || undefined}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, paddingLeft: depth * 16 }}>
            <button
              type="button"
              className="repository-file-tree-select"
              aria-current={selected ? "true" : undefined}
              onClick={() => onSelect(entry)}
              style={{ flex: 1, minWidth: 0, textAlign: "left" }}
            >
              <span aria-hidden="true" data-status={entry.file.status}>{statusGlyph(entry.file.status)}</span>{" "}
              <span>{entry.path.split("/").at(-1)}</span>
            </button>
            <span className="repository-file-tree-stats" aria-label={`${entry.additions} additions, ${entry.deletions} deletions`}>
              {entry.additions > 0 && <span data-kind="addition">+{entry.additions}</span>}
              {entry.deletions > 0 && <span data-kind="deletion">−{entry.deletions}</span>}
            </span>
            <button
              type="button"
              className="repository-file-tree-viewed"
              aria-pressed={viewed}
              aria-label={viewed ? `Mark ${entry.path} not viewed` : `Mark ${entry.path} viewed`}
              disabled={viewedDisabled}
              onClick={() => onToggleViewed(entry, !viewed)}
            >
              {viewed ? "Viewed" : "Mark viewed"}
            </button>
          </div>
        </li>
      );
    }

    const expanded = filtering || expandedDirectories.has(node.key);
    return (
      <li key={node.key} className="repository-file-tree-directory">
        <div style={{ display: "flex", alignItems: "center", paddingLeft: depth * 16 }}>
          <button
            type="button"
            className="repository-file-tree-directory-toggle"
            aria-expanded={expanded}
            aria-label={`${expanded ? "Collapse" : "Expand"} ${node.name}`}
            onClick={() => toggleDirectory(node.key)}
          >
            <span aria-hidden="true">{expanded ? "▾" : "▸"}</span> {node.name}
          </button>
        </div>
        {expanded && <ul>{node.children.map((child) => renderNode(child, depth + 1))}</ul>}
      </li>
    );
  };

  return (
    <section className={className ? `repository-file-tree ${className}` : "repository-file-tree"} aria-label="Changed files">
      <div className="repository-file-tree-filter">
        <label>
          <span>Filter changed files</span>
          <input
            type="search"
            value={filterText}
            placeholder={filterPlaceholder}
            onChange={(event) => setFilterText(event.target.value)}
          />
        </label>
        {allDirectoryKeys.length > 0 && (
          <button
            type="button"
            onClick={() => setExpandedDirectories((current) =>
              current.size === allDirectoryKeys.length ? new Set() : new Set(allDirectoryKeys),
            )}
          >
            {expandedDirectories.size === allDirectoryKeys.length ? "Collapse folders" : "Expand folders"}
          </button>
        )}
      </div>
      {filteredTree.length === 0 ? (
        <p className="repository-file-tree-empty" role="status">No changed files match “{filterText.trim()}”.</p>
      ) : (
        <ul className="repository-file-tree-repositories">
          {filteredTree.map((repository) => (
            <li key={repository.repositoryId} className="repository-file-tree-repository">
              <span className="repository-file-tree-repository-name">{repository.repositoryRoot}</span>
              <ul>{repository.children.map((node) => renderNode(node, 0))}</ul>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
