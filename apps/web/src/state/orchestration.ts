import { createOrchestrationEnvironmentAtoms } from "@coddy/client-runtime/state/orchestration";

import { connectionAtomRuntime } from "../connection/runtime";

export const orchestrationEnvironment = createOrchestrationEnvironmentAtoms(connectionAtomRuntime);
