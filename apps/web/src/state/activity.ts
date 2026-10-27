import { createEnvironmentActivityAtoms } from "@coddy/client-runtime/state/activity";

import { connectionAtomRuntime } from "../connection/runtime";

export const environmentActivity = createEnvironmentActivityAtoms(connectionAtomRuntime);
