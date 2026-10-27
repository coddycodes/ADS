import { createPullRequestsEnvironmentAtoms } from "@coddy/client-runtime/state/pull-requests";
import { connectionAtomRuntime } from "../connection/runtime";
export const pullRequestsEnvironment = createPullRequestsEnvironmentAtoms(connectionAtomRuntime);
