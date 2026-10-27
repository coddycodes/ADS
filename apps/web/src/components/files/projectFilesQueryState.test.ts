import type { ProjectReadFileResult } from "@coddy/contracts";
import { EnvironmentId } from "@coddy/contracts";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  clearProjectFileQueryData,
  confirmProjectFileQueryData,
  getProjectEntriesQueryAtom,
  getProjectFileQueryAtom,
  getOptimisticProjectFileQueryData,
  resolveProjectFileQueryData,
  setProjectFileQueryData,
} from "./projectFilesQueryState";

const environmentId = EnvironmentId.make("environment-project-files-query-test");

describe("project files queries", () => {
  afterEach(() => {
    clearProjectFileQueryData(environmentId, "/repo", "convex.json");
    vi.unstubAllGlobals();
  });

  it("keeps the latest optimistic draft when an older write finishes", () => {
    vi.stubGlobal("window", {});
    const initial = {
      relativePath: "convex.json",
      contents: '{"nodeVersion":"20"}',
      byteLength: 20,
      truncated: false,
    } satisfies ProjectReadFileResult;
    setProjectFileQueryData(environmentId, "/repo", "convex.json", '{"nodeVersion":"220"}');
    setProjectFileQueryData(environmentId, "/repo", "convex.json", '{"nodeVersion":"22"}');

    expect(getOptimisticProjectFileQueryData(environmentId, "/repo", "convex.json")?.contents).toBe(
      '{"nodeVersion":"22"}',
    );

    expect(
      confirmProjectFileQueryData(environmentId, "/repo", "convex.json", '{"nodeVersion":"220"}'),
    ).toBe(false);

    expect(resolveProjectFileQueryData(environmentId, "/repo", "convex.json", initial)).toEqual({
      relativePath: "convex.json",
      contents: '{"nodeVersion":"22"}',
      byteLength: 20,
      truncated: false,
    });

    expect(
      confirmProjectFileQueryData(environmentId, "/repo", "convex.json", '{"nodeVersion":"22"}'),
    ).toBe(true);
  });

  it("builds entry and file queries and resolves missing paths", () => {
    expect(getProjectEntriesQueryAtom(environmentId, "/repo")).toBeDefined();
    expect(getProjectFileQueryAtom(environmentId, "/repo", "convex.json")).toBeDefined();
    expect(getProjectFileQueryAtom(environmentId, "/repo", null)).toBeDefined();
    expect(getOptimisticProjectFileQueryData(environmentId, "/repo", "convex.json")).toBeNull();

    const data = {
      relativePath: "convex.json",
      contents: "{}",
      byteLength: 2,
      truncated: false,
    } satisfies ProjectReadFileResult;
    expect(resolveProjectFileQueryData(environmentId, "/repo", null, data)).toBe(data);
    expect(resolveProjectFileQueryData(environmentId, "/repo", "convex.json", data)).toBe(data);
  });
});
