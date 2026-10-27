import type { VcsRepositoryUnavailableReason, VcsStatusResult } from "@coddy/contracts";

export type GitManagerRepositoryUnavailableReason = VcsRepositoryUnavailableReason | "unknown";

/** null means loading or readable; unknown means unavailable without a known reason. */
export type RepositoryUnavailable = GitManagerRepositoryUnavailableReason | null;

export function resolveGitManagerRepositoryUnavailable(
  status: Pick<VcsStatusResult, "isRepo" | "repositoryUnavailableReason"> | null,
): RepositoryUnavailable {
  return status?.isRepo === false ? (status.repositoryUnavailableReason ?? "unknown") : null;
}

function copy(
  shortLabel: string,
  beforeCommand: string,
  command: string | null = null,
  afterCommand = "",
) {
  return {
    shortLabel,
    beforeCommand,
    command,
    afterCommand,
    message: `${beforeCommand}${command ?? ""}${afterCommand}`,
  };
}

function quoteTrustPath(cwd: string): string {
  // The checkout belongs to the server, which can use a different OS from the client.
  const isUncPath = cwd.startsWith("\\\\");
  if (/^[A-Za-z]:[\\/]/.test(cwd) || isUncPath) {
    const normalizedPath = cwd.replace(/\\/g, "/");
    // Git for Windows interpolates %(prefix)/ when matching a UNC safe.directory.
    const path = isUncPath ? `%(prefix)/${normalizedPath}` : normalizedPath;
    return /^[A-Za-z0-9_.:/-]+$/.test(path)
      ? path
      : `'${path.replace(/['\u2018\u2019\u201a\u201b]/g, "$&$&")}'`;
  }

  return /^[A-Za-z0-9_@%+=:,./-]+$/.test(cwd) ? cwd : `'${cwd.replace(/'/g, "'\\''")}'`;
}

/** One wording for both the formatted panel and plain-text disabled reasons. */
export function gitManagerRepositoryUnavailableCopy(
  reason: GitManagerRepositoryUnavailableReason,
  cwd: string,
) {
  switch (reason) {
    case "absent":
      return copy(
        "Not a Git repository",
        "This folder isn't a Git repository. Run ",
        "git init",
        " to create one.",
      );
    case "unreadable":
      return copy(
        "Repository unreadable",
        "Git can't read this repository. Check its .git folder, for example a damaged HEAD or config file.",
      );
    case "untrusted":
      return copy(
        "Repository not trusted",
        "Git doesn't trust this repository because another user owns it. Run ",
        `git config --global --add safe.directory ${quoteTrustPath(cwd)}`,
        " to trust it.",
      );
    case "unknown":
      return copy(
        "Repository unavailable",
        "Git can't read this folder as a repository. Run ",
        "git init",
        " to create one, or check its .git folder if it already is one.",
      );
  }
}
