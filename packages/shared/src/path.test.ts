import { describe, expect, it } from "vite-plus/test";
import {
  isExplicitRelativePath,
  isUncPath,
  isWindowsAbsolutePath,
  isWindowsDrivePath,
  joinHostPath,
} from "./path.ts";

describe("path helpers", () => {
  it("detects windows drive paths", () => {
    expect(isWindowsDrivePath("C:\\repo")).toBe(true);
    expect(isWindowsDrivePath("D:/repo")).toBe(true);
    expect(isWindowsDrivePath("/repo")).toBe(false);
  });

  it("detects UNC paths", () => {
    expect(isUncPath("\\\\server\\share\\repo")).toBe(true);
    expect(isUncPath("C:\\repo")).toBe(false);
  });

  it("detects windows absolute paths", () => {
    expect(isWindowsAbsolutePath("C:\\repo")).toBe(true);
    expect(isWindowsAbsolutePath("\\\\server\\share\\repo")).toBe(true);
    expect(isWindowsAbsolutePath("./repo")).toBe(false);
  });

  it("detects explicit relative paths", () => {
    expect(isExplicitRelativePath(".")).toBe(true);
    expect(isExplicitRelativePath("..")).toBe(true);
    expect(isExplicitRelativePath("./repo")).toBe(true);
    expect(isExplicitRelativePath("..\\repo")).toBe(true);
    expect(isExplicitRelativePath("~/repo")).toBe(false);
  });
});

describe("joinHostPath", () => {
  it("joins Windows drive paths with Windows separators", () => {
    expect(joinHostPath("X:\\Workspaces\\coddy-studio", "new-folder")).toBe(
      "X:\\Workspaces\\coddy-studio\\new-folder",
    );
    expect(joinHostPath("X:/Workspaces/coddy-studio/", "team/new-folder")).toBe(
      "X:\\Workspaces\\coddy-studio\\team\\new-folder",
    );
  });

  it("preserves Windows drive roots", () => {
    expect(joinHostPath("X:\\", "new-folder")).toBe("X:\\new-folder");
    expect(joinHostPath("X:/", "")).toBe("X:\\");
    expect(joinHostPath("X:\\\\", ".")).toBe("X:\\");
  });

  it("preserves UNC share roots", () => {
    expect(joinHostPath("\\\\server\\share\\", "team/new-folder")).toBe(
      "\\\\server\\share\\team\\new-folder",
    );
    expect(joinHostPath("\\\\server\\share", "")).toBe("\\\\server\\share\\");
    expect(joinHostPath("\\\\server\\share\\\\", ".")).toBe("\\\\server\\share\\");
  });

  it("normalizes non-root UNC paths without losing the share marker", () => {
    expect(joinHostPath("\\\\server\\share\\team\\\\", "new\\folder\\")).toBe(
      "\\\\server\\share\\team\\new\\folder",
    );
  });

  it("joins POSIX paths and preserves the POSIX root", () => {
    expect(joinHostPath("/srv/projects", "team\\new-folder")).toBe("/srv/projects/team/new-folder");
    expect(joinHostPath("/", "new-folder")).toBe("/new-folder");
    expect(joinHostPath("/", "")).toBe("/");
  });

  it("returns a normalized base for an empty or dot relative path", () => {
    expect(joinHostPath("/srv//projects///", "")).toBe("/srv/projects");
    expect(joinHostPath("X:\\Workspaces\\\\", ".")).toBe("X:\\Workspaces");
  });

  it("normalizes mixed and repeated relative separators", () => {
    expect(joinHostPath("/srv/projects/", "team//nested\\\\folder///")).toBe(
      "/srv/projects/team/nested/folder",
    );
    expect(joinHostPath("X:\\Workspaces\\", "team//nested\\\\folder///")).toBe(
      "X:\\Workspaces\\team\\nested\\folder",
    );
  });

  it("rejects parent segments anywhere in the relative path", () => {
    expect(joinHostPath("/srv/projects/", "../outside")).toBe("/srv/projects");
    expect(joinHostPath("X:\\Workspaces\\", "team\\..\\outside")).toBe("X:\\Workspaces");
    expect(joinHostPath("\\\\server\\share\\", "team/../outside")).toBe("\\\\server\\share\\");
  });

  it("rejects rooted or drive-prefixed relative inputs", () => {
    const windowsBase = "X:\\Workspaces";
    expect(joinHostPath("/srv/projects/", "/outside/folder")).toBe("/srv/projects");
    expect(joinHostPath(windowsBase, "\\outside\\folder")).toBe(windowsBase);
    expect(joinHostPath(windowsBase, "\\\\other\\share\\folder")).toBe(windowsBase);
    expect(joinHostPath(windowsBase, "C:")).toBe(windowsBase);
    expect(joinHostPath(windowsBase, "C:outside")).toBe(windowsBase);
    expect(joinHostPath(windowsBase, "C:\\outside\\folder")).toBe(windowsBase);
  });

  it("does not compose paths from non-canonical bases", () => {
    expect(joinHostPath("", "child")).toBe("");
    expect(joinHostPath("C:", "child")).toBe("C:");
    expect(joinHostPath("C:folder", "child")).toBe("C:folder");
    expect(joinHostPath("relative/path/", "child")).toBe("relative/path/");
    expect(joinHostPath("\\\\server", "child")).toBe("\\\\server");
  });
});
