import { VcsCloneStoppedError } from "@coddy/client-runtime/state/vcs";
import { EnvironmentId, GitCloneOperationError } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  cloneCancelPendingNotice,
  cloneFinishedBeforeCancelNotice,
  cloneReconnectingNotice,
  defaultAddProjectParent,
  describeCloneOperationFailure,
  getEnvironmentBrowsePlatform,
  joinProjectPath,
  shouldUseNativePicker,
  validateAddProjectPath,
  validateGitCloneParentPath,
  validateGitCloneUrl,
  validateProjectName,
  type AddProjectHostOption,
} from "./AddProjectDialog.logic";

const localHost: AddProjectHostOption = {
  environmentId: EnvironmentId.make("local"),
  label: "Local Mac",
  platform: "MacIntel",
  baseDirectory: "~/",
  isPrimary: true,
  desktopInstanceId: null,
  nativePickerAvailable: true,
};

describe("Add Project rules", () => {
  it("resolves only authoritative server browse platforms", () => {
    expect(getEnvironmentBrowsePlatform("linux")).toBe("Linux");
    expect(getEnvironmentBrowsePlatform("windows")).toBe("Win32");
    expect(getEnvironmentBrowsePlatform("darwin")).toBe("MacIntel");
    expect(getEnvironmentBrowsePlatform(null)).toBeNull();
    expect(getEnvironmentBrowsePlatform(undefined)).toBeNull();
  });

  it("uses the environment base directory and falls back to home", () => {
    expect(defaultAddProjectParent(" ~/code ")).toBe("~/code/");
    expect(defaultAddProjectParent("")).toBe("~/");
  });

  it("accepts one directory name and rejects path-like names", () => {
    expect(validateProjectName(" demo ")).toBeNull();
    expect(validateProjectName("")).toBe("Enter a project name.");
    expect(validateProjectName(".")).toBe("Enter a project name other than . or ..");
    expect(validateProjectName("../demo")).toBe("Project names cannot contain path separators.");
    expect(validateProjectName("a\\b")).toBe("Project names cannot contain path separators.");
  });

  it("requires absolute or home-relative host paths", () => {
    expect(validateAddProjectPath("~/code", "Linux")).toBeNull();
    expect(validateAddProjectPath("/srv/code", "Linux")).toBeNull();
    expect(validateAddProjectPath("C:\\code", "Win32")).toBeNull();
    expect(validateAddProjectPath("./code", "Linux")).toBe(
      "Enter an absolute or home-relative path.",
    );
    expect(validateAddProjectPath("C:\\code", "Linux")).toBe(
      "Windows-style paths are only supported on Windows.",
    );
  });

  it("accepts common remote Git clone URL forms", () => {
    expect(validateGitCloneUrl("https://github.com/openai/codex.git")).toBeNull();
    expect(validateGitCloneUrl("http://git.example.com/team/project.git")).toBeNull();
    expect(validateGitCloneUrl("ssh://git@git.example.com/team/project.git")).toBeNull();
    expect(validateGitCloneUrl("git://git.example.com/team/project.git")).toBeNull();
    expect(validateGitCloneUrl("git@github.com:openai/codex.git")).toBeNull();
    expect(validateGitCloneUrl("  https://github.com/openai/codex.git  ")).toBeNull();
  });

  it("accepts a userless SCP-style domain remote", () => {
    expect(validateGitCloneUrl("github.com:openai/codex.git")).toBeNull();
  });

  it("accepts a userless SCP-style SSH alias remote", () => {
    expect(validateGitCloneUrl("work-git:team/repo.git")).toBeNull();
  });

  it("rejects Windows drive paths as clone URLs", () => {
    expect(validateGitCloneUrl("C:\\repo")).toBe("Enter a valid Git repository URL.");
    expect(validateGitCloneUrl("C:/repo")).toBe("Enter a valid Git repository URL.");
  });

  it("rejects whitespace, unsupported schemes, and arbitrary clone URL text", () => {
    expect(validateGitCloneUrl("")).toBe("Enter a Git URL.");
    expect(validateGitCloneUrl("not-a-url")).toBe("Enter a valid Git repository URL.");
    expect(validateGitCloneUrl("github.com/openai/codex")).toBe(
      "Enter a valid Git repository URL.",
    );
    expect(validateGitCloneUrl("https://github.com/openai/my repo.git")).toBe(
      "Enter a valid Git repository URL.",
    );
    expect(validateGitCloneUrl("file:///tmp/repository")).toBe("Enter a valid Git repository URL.");
  });

  it("accepts home-relative, POSIX, and Windows clone parent paths", () => {
    expect(validateGitCloneParentPath("~/projects", "Linux")).toBeNull();
    expect(validateGitCloneParentPath("/srv/projects", "Linux")).toBeNull();
    expect(validateGitCloneParentPath("C:\\projects", "Win32")).toBeNull();
    expect(validateGitCloneParentPath("\\\\server\\projects", "Win32")).toBeNull();
  });

  it("rejects clone parents that do not match the authoritative host platform", () => {
    expect(validateGitCloneParentPath("", "Linux")).toBe("Enter a clone parent folder.");
    expect(validateGitCloneParentPath("projects", "Linux")).toBe(
      "Enter an absolute or home-relative path.",
    );
    expect(validateGitCloneParentPath("./projects", "Linux")).toBe(
      "Enter an absolute or home-relative path.",
    );
    expect(validateGitCloneParentPath("C:\\projects", "Linux")).toBe(
      "Windows-style paths are only supported on Windows.",
    );
    expect(validateGitCloneParentPath("~/projects", null)).toBe(
      "Host platform information is still loading.",
    );
    expect(validateAddProjectPath("~/projects", null)).toBe(
      "Host platform information is still loading.",
    );
  });

  it("joins target paths with the selected host separator", () => {
    expect(joinProjectPath("~/code/", "demo", "Linux")).toBe("~/code/demo");
    expect(joinProjectPath("~/", "demo", "Win32")).toBe("~\\demo");
    expect(joinProjectPath("C:\\code\\", "demo", "Win32")).toBe("C:\\code\\demo");
  });

  it("only uses a native picker for routable desktop hosts", () => {
    expect(shouldUseNativePicker(localHost)).toBe(true);
    expect(
      shouldUseNativePicker({
        ...localHost,
        environmentId: EnvironmentId.make("remote"),
        isPrimary: false,
        desktopInstanceId: null,
      }),
    ).toBe(false);
    expect(
      shouldUseNativePicker({
        ...localHost,
        environmentId: EnvironmentId.make("wsl"),
        isPrimary: false,
        desktopInstanceId: "wsl:Ubuntu",
      }),
    ).toBe(true);
  });
});

describe("clone re-attach copy", () => {
  const error = (reason: GitCloneOperationError["reason"]) =>
    new GitCloneOperationError({ reason, destination: "/srv/code/demo", message: "server text" });

  it("names the host while reconnecting and while a cancel waits for it", () => {
    expect(cloneReconnectingNotice("Remote")).toBe(
      "Lost the connection to Remote. The clone continues on the server; reconnecting…",
    );
    expect(cloneCancelPendingNotice("Remote")).toBe("The clone stops when Remote reconnects.");
  });

  it("says where a clone that finished before its cancel was left", () => {
    expect(cloneFinishedBeforeCancelNotice("/srv/code/demo")).toBe(
      "The clone finished before it could be cancelled. It is in /srv/code/demo and was not added as a project. Press Clone to add it.",
    );
  });

  it("maps every clone runtime reason to the dialog copy", () => {
    expect(describeCloneOperationFailure(error("not-in-progress"), "Remote", false)).toEqual({
      kind: "error",
      text: "No clone is in progress for /srv/code/demo. Press Clone to start again.",
    });
    expect(describeCloneOperationFailure(error("cancelled"), "Remote", false)).toEqual({
      kind: "error",
      text: "The clone into /srv/code/demo was cancelled elsewhere. Press Clone to start again.",
    });
    expect(describeCloneOperationFailure(error("cancelled"), "Remote", true)).toEqual({
      kind: "notice",
      text: "Clone cancelled.",
    });
    expect(describeCloneOperationFailure(error("busy"), "Remote", false)).toEqual({
      kind: "error",
      text: "Another clone into /srv/code/demo is in progress. Wait for it to finish or choose another folder.",
    });
    expect(describeCloneOperationFailure(error("capacity"), "Remote", false)).toEqual({
      kind: "error",
      text: "Too many clones are running on Remote. Wait for one to finish and try again.",
    });
    expect(describeCloneOperationFailure(error("shutting-down"), "Remote", false)).toEqual({
      kind: "error",
      text: "Remote is shutting down. Press Clone again once it is back.",
    });
  });

  it("explains a clone the client stopped following", () => {
    const stopped = (reason: VcsCloneStoppedError["reason"]) =>
      new VcsCloneStoppedError({ environmentId: "remote", reason, message: "x" });
    expect(
      describeCloneOperationFailure(stopped("environment-unavailable"), "Remote", false),
    ).toEqual({
      kind: "error",
      text: "Can't reconnect to Remote. The clone continues there; clone the same URL into the same folder to finish.",
    });
    expect(describeCloneOperationFailure(stopped("reattach-unsupported"), "Remote", false)).toEqual(
      {
        kind: "error",
        text: "The clone stopped before it finished. Try again.",
      },
    );
  });

  it("leaves other failures to the generic Clone failed copy", () => {
    expect(describeCloneOperationFailure(new Error("boom"), "Remote", false)).toBeNull();
    expect(describeCloneOperationFailure(null, "Remote", false)).toBeNull();
  });
});
