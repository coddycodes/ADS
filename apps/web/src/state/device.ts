import { createDeviceEnvironmentAtoms } from "@coddy/client-runtime/state/device";

import { connectionAtomRuntime } from "../connection/runtime";

export const deviceEnvironment = createDeviceEnvironmentAtoms(connectionAtomRuntime);
