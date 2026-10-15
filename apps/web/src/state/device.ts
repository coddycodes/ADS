import { createDeviceEnvironmentAtoms } from "@bibcode/client-runtime/state/device";

import { connectionAtomRuntime } from "../connection/runtime";

export const deviceEnvironment = createDeviceEnvironmentAtoms(connectionAtomRuntime);
