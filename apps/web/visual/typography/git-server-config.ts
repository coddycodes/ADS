import { DEFAULT_SERVER_SETTINGS, type EnvironmentId, type ServerConfig } from "@bibcode/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import { environmentId } from "./git-data.ts";

// Complete shape follows packages/client-runtime/src/rpc/session.test.ts SERVER_CONFIG.
export function makeGitFixtureServerConfig(
  targetEnvironmentId: EnvironmentId = environmentId,
): ServerConfig {
  return {
    environment: {
      environmentId: targetEnvironmentId,
      label: "Owned Git typography fixture",
      platform: { os: "linux", arch: "x64" },
      serverVersion: "0.0.0-component-fixture",
      storageInstanceId: null,
      bootId: null,
      remoteUpdateSupport: null,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: makeTestExecutionEnvironmentCapabilities({
        repositoryIdentity: true,
        gitPullRequestBranchSelection: true,
        pullRequestCreateOptions: true,
      }),
    },
    auth: {
      policy: "loopback-browser",
      bootstrapMethods: ["one-time-token"],
      sessionMethods: ["browser-session-cookie", "bearer-access-token"],
      sessionCookieName: "bibcode_session",
    },
    cwd: "/component-fixtures/issue27",
    keybindingsConfigPath: "/component-fixtures/issue27/keybindings.json",
    keybindings: [],
    issues: [],
    providers: [],
    availableEditors: [],
    observability: {
      logsDirectoryPath: "/component-fixtures/issue27/logs",
      localTracingEnabled: false,
      otlpTracesEnabled: false,
      otlpMetricsEnabled: false,
    },
    settings: DEFAULT_SERVER_SETTINGS,
  };
}
export const gitServerConfigs: ReadonlyMap<EnvironmentId, ServerConfig> = new Map([
  [environmentId, makeGitFixtureServerConfig()],
]);
