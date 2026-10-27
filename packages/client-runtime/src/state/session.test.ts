import { describe, expect, it } from "@effect/vitest";
import { EnvironmentId, type ExecutionEnvironmentDescriptor } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import { compatVerdictFromPrepared, initialConfigOption } from "./session.ts";

class TestConfigError extends Schema.TaggedError<TestConfigError>()("TestConfigError", {
  message: Schema.String,
}) {}

describe("environment session state", () => {
  it.effect("turns an initial config failure into an empty value", () =>
    Effect.gen(function* () {
      const result = yield* initialConfigOption(
        Effect.fail(new TestConfigError({ message: "temporary failure" })),
      );
      expect(Option.isNone(result)).toBe(true);
    }),
  );
});

const currentDescriptor: ExecutionEnvironmentDescriptor = {
  environmentId: EnvironmentId.make("env-current"),
  label: "Current",
  platform: { os: "linux", arch: "x64" },
  serverVersion: "0.0.0-test",
  storageInstanceId: null,
  bootId: null,
  remoteUpdateSupport: null,
  remoteProtocolVersion: 1,
  minCompatibleRemoteProtocol: 1,
  capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
};

describe("environment compatibility verdict selection", () => {
  it("yields no verdict before a prepared connection exists", () => {
    expect(compatVerdictFromPrepared(Option.none())).toBeNull();
  });

  it("derives the verdict from the prepared connection descriptor", () => {
    expect(compatVerdictFromPrepared(Option.some({ descriptor: currentDescriptor }))).toEqual({
      kind: "compatible",
    });
  });

  it("classifies a pre-window prepared descriptor as legacy", () => {
    expect(
      compatVerdictFromPrepared(
        Option.some({
          descriptor: {
            ...currentDescriptor,
            remoteProtocolVersion: 0,
            minCompatibleRemoteProtocol: 0,
          },
        }),
      ),
    ).toEqual({ kind: "legacy" });
  });
});
