import { useAtomValue } from "@effect/atom-react";
import type { AtomCommand } from "@bibcode/client-runtime/state/runtime";
import { DEFAULT_SERVER_SETTINGS } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/unstable/reactivity";

import type * as ServerPorts from "../../src/state/server";
import type * as AssetPorts from "../../src/state/assets";
import type * as PreviewPorts from "../../src/state/preview";
import type * as ProjectPorts from "../../src/state/projects";
import type * as ShellPorts from "../../src/state/shell";
import type * as SessionPorts from "../../src/state/session";
import type * as EnvironmentPorts from "../../src/state/environments";
export { useActiveEnvironmentId } from "../../src/state/activeEnvironment";

function refused<W, A, E>(label: string): AtomCommand<W, A, E> {
  return {
    label,
    run: async () =>
      AsyncResult.failure(Cause.die(new Error(`Component fixture forbids ${label}.`))),
  };
}
export const primaryServerSettingsAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerSettingsAtom>>(DEFAULT_SERVER_SETTINGS);
const config =
  Atom.make<Atom.Type<ReturnType<typeof ServerPorts.serverEnvironment.configValueAtom>>>(null);
export const serverEnvironment: Pick<
  typeof ServerPorts.serverEnvironment,
  "configValueAtom" | "settingsValueAtom" | "updateSettings"
> = {
  configValueAtom: () => config,
  settingsValueAtom: () => primaryServerSettingsAtom,
  updateSettings: refused("server settings writes"),
};
type AssetState = Atom.Type<ReturnType<typeof AssetPorts.assetEnvironment.createUrl>>;
const assetUrl = Atom.make<AssetState>(
  AsyncResult.failure<AsyncResult.AsyncResult.Success<AssetState>, never>(
    Cause.die(new Error("Component fixture forbids asset URL requests.")),
  ),
);
export const assetEnvironment: Pick<typeof AssetPorts.assetEnvironment, "createUrl"> = {
  createUrl: () => assetUrl,
};
export const previewEnvironment: Pick<typeof PreviewPorts.previewEnvironment, "open"> = {
  open: refused("preview opening"),
};
export const projectEnvironment: Pick<typeof ProjectPorts.projectEnvironment, "writeFile"> = {
  writeFile: refused("workspace file writes"),
};
type Projects = typeof ProjectPorts.environmentProjects;
export const environmentProjects: Projects = {
  environmentProjectsAtom: Atom.family((_id: Parameters<Projects["environmentProjectsAtom"]>[0]) =>
    Atom.make<Atom.Type<ReturnType<Projects["environmentProjectsAtom"]>>>([]),
  ),
  environmentProjectIndexAtom: Atom.family(
    (_id: Parameters<Projects["environmentProjectIndexAtom"]>[0]) =>
      Atom.make<Atom.Type<ReturnType<Projects["environmentProjectIndexAtom"]>>>(new Map()),
  ),
  projectRefsAtom: Atom.make<Atom.Type<Projects["projectRefsAtom"]>>([]),
  projectsAtom: Atom.make<Atom.Type<Projects["projectsAtom"]>>([]),
  environmentProjectRefsAtom: Atom.family(
    (_id: Parameters<Projects["environmentProjectRefsAtom"]>[0]) =>
      Atom.make<Atom.Type<ReturnType<Projects["environmentProjectRefsAtom"]>>>([]),
  ),
  projectAtom: Atom.family((_ref: Parameters<Projects["projectAtom"]>[0]) =>
    Atom.make<Atom.Type<ReturnType<Projects["projectAtom"]>>>(null),
  ),
};
export const shellEnvironment: Pick<typeof ShellPorts.shellEnvironment, "openInEditor"> = {
  openInEditor: refused("editor launch"),
};
const preparedConnection = Atom.make<ReturnType<typeof SessionPorts.usePreparedConnection>>(
  Option.none(),
);
export const usePreparedConnection: typeof SessionPorts.usePreparedConnection = () =>
  useAtomValue(preparedConnection);
export const readPreparedConnection: typeof SessionPorts.readPreparedConnection = () => null;
export const usePrimaryEnvironment: typeof EnvironmentPorts.usePrimaryEnvironment = () => null;
