import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import type { WslEnvironmentCandidate } from "~/wslPaths";

import {
  canUseNativeHostFolderPicker,
  pickHostFolder,
  type HostFolderPickerTarget,
  type PickHostFolderInput,
} from "./hostFolderPicker";

const ENV_PRIMARY = EnvironmentId.make("primary");
const ENV_WSL = EnvironmentId.make("wsl");

const primaryHost: HostFolderPickerTarget = {
  environmentId: ENV_PRIMARY,
  platform: "MacIntel",
  isPrimary: true,
  desktopInstanceId: null,
  nativePickerAvailable: true,
};

const wslHost: HostFolderPickerTarget = {
  environmentId: ENV_WSL,
  platform: "Linux",
  isPrimary: false,
  desktopInstanceId: "wsl:Ubuntu",
  nativePickerAvailable: true,
};

function makeInput(
  options: {
    readonly host?: HostFolderPickerTarget;
    readonly pickedPath?: string | null;
    readonly wslCandidates?: ReadonlyArray<WslEnvironmentCandidate<EnvironmentId>>;
  } = {},
): PickHostFolderInput & {
  readonly dialogs: { readonly pickFolder: ReturnType<typeof vi.fn> };
} {
  const pickFolder = vi.fn(async () => options.pickedPath ?? null);
  return {
    host: options.host ?? primaryHost,
    primaryEnvironmentId: ENV_PRIMARY,
    initialPath: "~/",
    dialogs: { pickFolder },
    getWslState: async () => ({
      enabled: true,
      wslOnly: false,
      distro: null,
      available: true,
      distros: [],
      preflightError: null,
    }),
    primaryRunningDistro: null,
    wslCandidates: options.wslCandidates ?? [],
  };
}

describe("pickHostFolder", () => {
  it("uses the native picker only for routable hosts", () => {
    expect(
      canUseNativeHostFolderPicker({
        environmentId: EnvironmentId.make("primary"),
        platform: "Win32",
        isPrimary: true,
        desktopInstanceId: null,
        nativePickerAvailable: true,
      }),
    ).toBe(true);

    expect(
      canUseNativeHostFolderPicker({
        environmentId: EnvironmentId.make("remote"),
        platform: "Linux",
        isPrimary: false,
        desktopInstanceId: null,
        nativePickerAvailable: true,
      }),
    ).toBe(false);
  });

  it("returns host-path guidance without opening a picker for an unroutable host", async () => {
    const input = makeInput({
      host: {
        ...primaryHost,
        environmentId: EnvironmentId.make("remote"),
        isPrimary: false,
        nativePickerAvailable: false,
      },
    });

    await expect(pickHostFolder(input)).resolves.toEqual({
      _tag: "Failure",
      message: "This host does not support folder picking. Enter its project path manually.",
    });
    expect(input.dialogs.pickFolder).not.toHaveBeenCalled();
  });

  it("returns cancellation without an error", async () => {
    const result = await pickHostFolder(makeInput({ pickedPath: null }));
    expect(result).toEqual({ _tag: "Cancelled" });
  });

  it("returns a primary local selection", async () => {
    const result = await pickHostFolder(makeInput({ pickedPath: "/Users/me/code" }));
    expect(result).toEqual({
      _tag: "Selected",
      environmentId: EnvironmentId.make("primary"),
      path: "/Users/me/code",
    });
  });

  it("targets a mapped WSL backend and returns its Linux path", async () => {
    const input = makeInput({
      host: wslHost,
      pickedPath: "\\\\wsl.localhost\\Ubuntu\\home\\me\\code",
      wslCandidates: [
        {
          environmentId: EnvironmentId.make("wsl"),
          backendId: "wsl:Ubuntu",
          runningDistro: "Ubuntu",
        },
      ],
    });
    const result = await pickHostFolder(input);
    expect(input.dialogs.pickFolder).toHaveBeenCalledWith({
      initialPath: "~/",
      targetEnvironmentId: "wsl:Ubuntu",
    });
    expect(result).toEqual({
      _tag: "Selected",
      environmentId: EnvironmentId.make("wsl"),
      path: "/home/me/code",
    });
  });

  it("rejects an unmatched WSL selection", async () => {
    const result = await pickHostFolder(
      makeInput({
        pickedPath: "\\\\wsl.localhost\\Fedora\\srv\\code",
        wslCandidates: [],
      }),
    );
    expect(result).toEqual({
      _tag: "Failure",
      message: "Start the matching WSL backend, then choose the folder again.",
    });
  });
});
