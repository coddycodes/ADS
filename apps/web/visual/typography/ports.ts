import {
  serverEnvironment as settingsServer,
  previewEnvironment as settingsPreview,
} from "./settings-ports";
import {
  serverEnvironment as chatServer,
  previewEnvironment as chatPreview,
} from "./chat-context-ports";
import { vcsEnvironment as gitVcs } from "./git-ports";
import { remainingVcsStatus, remainingRefreshVcsStatus } from "./remaining-ports";
import { publishCwd, remainingCwd } from "./remaining-data";
export * from "./settings-ports";
export {
  assetEnvironment,
  projectEnvironment,
  environmentProjects,
  usePreparedConnection,
  readPreparedConnection,
} from "./chat-context-ports";
export * from "./git-ports";
export * from "./remaining-ports";
export const serverEnvironment = { ...settingsServer, ...chatServer };
export const previewEnvironment = { ...settingsPreview, ...chatPreview };
export const vcsEnvironment = {
  ...gitVcs,
  status: (target: Parameters<typeof gitVcs.status>[0]) =>
    target.input.cwd === publishCwd || target.input.cwd === remainingCwd
      ? remainingVcsStatus(target)
      : gitVcs.status(target),
  summary: (target: Parameters<typeof gitVcs.summary>[0]) =>
    target.input.cwd === publishCwd || target.input.cwd === remainingCwd
      ? remainingVcsStatus(target)
      : gitVcs.summary(target),
  refreshStatus: {
    label: "owned typography status refresh",
    run: (...args: Parameters<typeof gitVcs.refreshStatus.run>) =>
      args[1].input.cwd === publishCwd
        ? remainingRefreshVcsStatus.run(...args)
        : gitVcs.refreshStatus.run(...args),
  },
};
