import { EnvironmentId, ProjectId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import type { SidebarProjectSnapshot } from "../../sidebarProjectGrouping";
import type { EnvironmentRailCandidate } from "./environmentRail.logic";
import { buildEnvironmentCardIdentities, groupProjectsByRepository } from "./repositoryView.logic";

const LOCAL = EnvironmentId.make("env-local");
const AI = EnvironmentId.make("env-ai");
const OFF = EnvironmentId.make("env-off");
const WSL = EnvironmentId.make("env-wsl");

function candidate(
  environmentId: EnvironmentId,
  label: string,
  overrides: Partial<EnvironmentRailCandidate> = {},
): EnvironmentRailCandidate {
  return {
    environmentId,
    label,
    isPrimary: false,
    isDesktopLocal: false,
    phase: "connected",
    compat: null,
    updateAvailable: false,
    ...overrides,
  };
}

const identities = buildEnvironmentCardIdentities([
  candidate(AI, "ai-server"),
  candidate(LOCAL, "This machine", { isPrimary: true }),
  candidate(OFF, "build box", { phase: "offline" }),
  candidate(WSL, "Alpine WSL", { isDesktopLocal: true }),
]);

function card(
  id: string,
  environmentId: EnvironmentId,
  workspaceRoot: string,
  canonicalKey: string | null,
  name = "repo",
): SidebarProjectSnapshot {
  return {
    id: ProjectId.make(id),
    environmentId,
    workspaceRoot,
    title: id,
    repositoryIdentity:
      canonicalKey === null
        ? null
        : {
            canonicalKey,
            locator: {
              source: "git-remote",
              remoteName: "origin",
              remoteUrl: `https://${canonicalKey}.git`,
            },
            name,
          },
    projectKey: `${environmentId}:${workspaceRoot}`,
  } as unknown as SidebarProjectSnapshot;
}

describe("buildEnvironmentCardIdentities", () => {
  it("presents local, remote and offline environments", () => {
    expect(identities.get(LOCAL)).toMatchObject({
      label: "Local",
      isLocal: true,
      statusLabel: "This device",
      available: true,
    });
    expect(identities.get(AI)).toMatchObject({
      label: "ai-server",
      isLocal: false,
      avatar: "AS",
      status: "connected",
      statusLabel: "Connected",
    });
    expect(identities.get(OFF)).toMatchObject({
      status: "disconnected",
      statusLabel: "Offline",
      available: false,
    });
  });
});

describe("groupProjectsByRepository", () => {
  it("groups by canonical key, keeps input order, and puts Local first", () => {
    const groups = groupProjectsByRepository({
      environments: identities,
      sortOrder: "updated_at",
      projects: [
        card("a-ai", AI, "/work/a", "gitlab.example/team/a", "a"),
        card("b", LOCAL, "/home/b", "github.com/acme/b", "b"),
        card("a-local", LOCAL, "/home/a", "gitlab.example/team/a", "a"),
      ],
    });
    expect(groups.map((group) => group.key)).toEqual([
      "gitlab.example/team/a",
      "github.com/acme/b",
    ]);
    expect(groups[0]!.cards.map((project) => project.id)).toEqual(["a-local", "a-ai"]);
    expect(groups[0]).toMatchObject({ title: "a", environmentCount: 2, showHost: false });
  });

  it("sorts repositories by name regardless of activity order", () => {
    const groups = groupProjectsByRepository({
      environments: identities,
      sortOrder: "name",
      projects: [
        card("z", AI, "/work/zeta", "github.com/acme/zeta", "zeta"),
        card("b2", AI, "/work/beta", "gitlab.example/other/Beta", "Beta"),
        card("a", LOCAL, "/home/alpha", "github.com/acme/alpha", "alpha"),
        card("b1", LOCAL, "/home/beta", "github.com/acme/beta", "beta"),
      ],
    });
    // Case-insensitive by title; equal titles fall back to the repository key.
    expect(groups.map((group) => group.key)).toEqual([
      "github.com/acme/alpha",
      "github.com/acme/beta",
      "gitlab.example/other/Beta",
      "github.com/acme/zeta",
    ]);
  });

  it("orders the primary Local first, then desktop-local environments, then remotes by label", () => {
    const [group] = groupProjectsByRepository({
      environments: identities,
      sortOrder: "name",
      projects: [
        card("remote", AI, "/work/a", "github.com/acme/a"),
        card("wsl", WSL, "/home/a", "github.com/acme/a"),
        card("offline", OFF, "/srv/a", "github.com/acme/a"),
        card("primary", LOCAL, "/Users/a", "github.com/acme/a"),
      ],
    });
    expect(group!.cards.map((project) => project.id)).toEqual([
      "primary",
      "wsl",
      "remote",
      "offline",
    ]);
  });

  it("keeps two checkouts in one environment as two cards", () => {
    const [group] = groupProjectsByRepository({
      environments: identities,
      sortOrder: "name",
      projects: [
        card("one", AI, "/work/a", "github.com/acme/a"),
        card("two", AI, "/work/a-copy", "github.com/acme/a"),
      ],
    });
    expect(group!.cards).toHaveLength(2);
    expect(group!.environmentCount).toBe(1);
  });

  it("titles identity-less projects by folder and shows hosts only on title clashes", () => {
    const groups = groupProjectsByRepository({
      environments: identities,
      sortOrder: "updated_at",
      projects: [
        card("plain", LOCAL, "C:\\work\\scratch", null),
        card("api-gh", LOCAL, "/a", "github.com/acme/api", "api"),
        card("api-gl", AI, "/b", "gitlab.com/acme/api", "api"),
      ],
    });
    expect(groups[0]).toMatchObject({ title: "scratch", host: null, showHost: false });
    expect(groups[1]).toMatchObject({ title: "api", host: "github.com", showHost: true });
    expect(groups[2]).toMatchObject({ title: "api", host: "gitlab.com", showHost: true });
  });
});
