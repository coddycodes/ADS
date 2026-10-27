import { describe, expect, it } from "vite-plus/test";
import {
  DESKTOP_LOCAL_CONNECTION_ID_PREFIX,
  isDesktopLocalConnectionId,
} from "@coddy/client-runtime/connection";

import {
  ADD_SERVER_FAILURE_REASONS,
  countRunningThreadsForEnvironment,
  describeAddServerFailure,
  describeRenameServerFailure,
  describeSshEnvironmentAddedToast,
  resolvePairingAddFailureDetail,
  describeCompatBadge,
  formatServerVersionLabel,
  isLoopbackAcknowledgementRequired,
  normalizePairingCodeInput,
  resolvePairingAddFailureReason,
  resolveTransportBadge,
} from "./connectPresentation";

describe("formatServerVersionLabel", () => {
  it("renders the D16 version string and hides unknown versions", () => {
    expect(formatServerVersionLabel("1.4.2")).toBe("Coddy Studio v1.4.2");
    expect(formatServerVersionLabel("  ")).toBeNull();
    expect(formatServerVersionLabel(null)).toBeNull();
    expect(formatServerVersionLabel(undefined)).toBeNull();
  });
});

describe("describeCompatBadge", () => {
  it("maps every verdict kind to the pinned copy", () => {
    expect(describeCompatBadge(null)).toBeNull();
    expect(describeCompatBadge({ kind: "compatible" })).toBeNull();
    expect(describeCompatBadge({ kind: "legacy" })).toEqual({
      tone: "warning",
      label: "Limited compatibility",
    });
    expect(
      describeCompatBadge({ kind: "server-too-old", serverVersion: 0, minSupported: 1 }),
    ).toEqual({ tone: "destructive", label: "Server update required" });
    expect(
      describeCompatBadge({ kind: "client-too-old", serverMinCompatible: 2, clientVersion: 1 }),
    ).toEqual({ tone: "destructive", label: "App update required" });
  });
});

describe("resolveTransportBadge", () => {
  const bearer = (profile: { readonly _tag: string; readonly hostKey?: string | null } | null) => ({
    relayManaged: false,
    entry: {
      target: { _tag: "BearerConnectionTarget" as const, connectionId: "bearer:x" },
      profile:
        profile === null
          ? ({ _tag: "None" } as const)
          : ({ _tag: "Some", value: profile } as const),
    },
  });

  it("labels relay, ssh, e2ee, and legacy-unencrypted saved servers", () => {
    expect(
      resolveTransportBadge({
        relayManaged: true,
        entry: { target: { _tag: "RelayConnectionTarget" }, profile: { _tag: "None" } },
      }),
    ).toEqual({ kind: "relay", label: "Coddy Connect" });
    expect(
      resolveTransportBadge({
        relayManaged: false,
        entry: { target: { _tag: "SshConnectionTarget" }, profile: { _tag: "None" } },
      }),
    ).toEqual({ kind: "ssh", label: "SSH tunnel" });
    expect(
      resolveTransportBadge(
        bearer({
          _tag: "BearerConnectionProfile",
          hostKey: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        }),
      ),
    ).toEqual({ kind: "e2ee", label: "End-to-end encrypted" });
    expect(
      resolveTransportBadge(bearer({ _tag: "BearerConnectionProfile", hostKey: null })),
    ).toEqual({
      kind: "unencrypted",
      label: "Unencrypted",
      guidance: "Re-pair with a new pairing code to secure this connection.",
    });
  });

  it("shows no transport badge for the desktop-managed local (WSL) environment", () => {
    const connectionId = `${DESKTOP_LOCAL_CONNECTION_ID_PREFIX}wsl`;
    expect(isDesktopLocalConnectionId(connectionId)).toBe(true);
    expect(
      resolveTransportBadge({
        relayManaged: false,
        entry: {
          target: { _tag: "BearerConnectionTarget", connectionId },
          profile: { _tag: "None" },
        },
      }),
    ).toBeNull();
  });
});

describe("add-server failure copy", () => {
  it("has copy for every spec-pinned failure reason", () => {
    expect(ADD_SERVER_FAILURE_REASONS).toEqual([
      "unreachable",
      "host-identity-mismatch",
      "pairing-rejected",
      "incompatible",
      "duplicate-storage-identity",
      "local-persistence-failed",
    ]);
    for (const reason of ADD_SERVER_FAILURE_REASONS) {
      const described = describeAddServerFailure(reason);
      expect(described.title.length).toBeGreaterThan(0);
      expect(described.detail.length).toBeGreaterThan(0);
    }
    expect(describeAddServerFailure("pairing-rejected").title).toBe("Pairing rejected");
    expect(describeAddServerFailure("local-persistence-failed").detail).toContain(
      "revoke the incomplete attempt",
    );
    expect(describeAddServerFailure("host-identity-mismatch").detail).toContain(
      "revoke the incomplete attempt",
    );
  });

  it("names the saved entry a duplicate pairing collided with", () => {
    expect(
      describeAddServerFailure("duplicate-storage-identity", "ai-server is already saved.").detail,
    ).toBe(
      "ai-server is already saved. Reconnect or adopt the existing entry instead of adding a duplicate.",
    );
    expect(describeAddServerFailure("duplicate-storage-identity").detail).toContain(
      "already uses this server's storage identity",
    );
  });

  it("reads the detail off a PairingAddError and rejects everything else", () => {
    expect(
      resolvePairingAddFailureDetail({
        _tag: "PairingAddError",
        reason: "duplicate-storage-identity",
        detail: "  ai-server is already saved.  ",
      }),
    ).toBe("ai-server is already saved.");
    expect(
      resolvePairingAddFailureDetail({
        _tag: "PairingAddError",
        reason: "duplicate-storage-identity",
        detail: "   ",
      }),
    ).toBeNull();
    expect(resolvePairingAddFailureDetail({ _tag: "SomethingElse", detail: "nope" })).toBeNull();
    expect(resolvePairingAddFailureDetail(null)).toBeNull();
  });

  it("reads the reason off a PairingAddError and rejects everything else", () => {
    expect(
      resolvePairingAddFailureReason({
        _tag: "PairingAddError",
        reason: "host-identity-mismatch",
        detail: "pinned key changed",
      }),
    ).toBe("host-identity-mismatch");
    expect(resolvePairingAddFailureReason(new Error("boom"))).toBeNull();
    expect(
      resolvePairingAddFailureReason({ _tag: "PairingAddError", reason: "something-else" }),
    ).toBeNull();
    expect(resolvePairingAddFailureReason({ kind: "pairing-rejected" })).toBeNull();
  });

  it("detects the loopback-acknowledgement error by tag", () => {
    expect(
      isLoopbackAcknowledgementRequired({
        _tag: "PairingLoopbackAcknowledgementRequiredError",
        endpoint: "http://127.0.0.1:3773",
      }),
    ).toBe(true);
    expect(
      isLoopbackAcknowledgementRequired({ _tag: "PairingAddError", reason: "unreachable" }),
    ).toBe(false);
    expect(isLoopbackAcknowledgementRequired(new Error("boom"))).toBe(false);
  });
});

describe("normalizePairingCodeInput", () => {
  it("accepts raw codes, deep links, and web pair URLs", () => {
    expect(normalizePairingCodeInput("  abc123-_  ")).toBe("abc123-_");
    expect(normalizePairingCodeInput("coddy-studio://pair?code=abc123-_")).toBe("abc123-_");
    expect(normalizePairingCodeInput("http://192.168.1.20:3773/pair?code=abc123-_")).toBe(
      "abc123-_",
    );
    expect(normalizePairingCodeInput("")).toBeNull();
    expect(normalizePairingCodeInput("coddy-studio://pair")).toBeNull();
    expect(normalizePairingCodeInput("http://example.com/other?code=x")).toBe("x");
  });
});

describe("countRunningThreadsForEnvironment", () => {
  it("counts only running sessions belonging to the environment", () => {
    const shells = [
      { environmentId: "env-1", session: { status: "running" } },
      { environmentId: "env-1", session: { status: "idle" } },
      { environmentId: "env-1", session: null },
      { environmentId: "env-2", session: { status: "running" } },
    ];
    expect(countRunningThreadsForEnvironment(shells, "env-1")).toBe(1);
    expect(countRunningThreadsForEnvironment(shells, "env-3")).toBe(0);
  });
});

describe("describeRenameServerFailure", () => {
  it("explains a server removed elsewhere, keeps validation copy, and hides storage internals", () => {
    expect(
      describeRenameServerFailure({
        _tag: "EnvironmentNotRegisteredError",
        environmentId: "env-1",
      }),
    ).toBe("This server is no longer saved on this device.");
    expect(
      describeRenameServerFailure({
        _tag: "ConnectionBlockedError",
        reason: "configuration",
        message: "Enter a name for this server.",
      }),
    ).toBe("Enter a name for this server.");
    expect(
      describeRenameServerFailure({
        _tag: "ConnectionPersistenceError",
        operation: "rename-connection",
        message: "Could not rename connection: IndexedDB aborted",
      }),
    ).toBe("Couldn't save the name on this device. Try again.");
    expect(describeRenameServerFailure(null)).toBe(
      "Couldn't save the name on this device. Try again.",
    );
  });
});

describe("describeSshEnvironmentAddedToast", () => {
  it("says the environment was added and is still connecting", () => {
    expect(describeSshEnvironmentAddedToast({ label: "devbox", updated: false })).toEqual({
      title: "Environment added",
      description: "devbox is connecting over SSH.",
    });
  });

  it("says a saved alias was updated", () => {
    expect(describeSshEnvironmentAddedToast({ label: "GPU box", updated: true })).toEqual({
      title: "Environment updated",
      description: "GPU box is connecting over SSH.",
    });
  });
});
