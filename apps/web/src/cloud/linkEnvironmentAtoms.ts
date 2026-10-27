import {
  createAtomCommandScheduler,
  createRuntimeCommand,
} from "@coddy/client-runtime/state/runtime";

import { connectionAtomRuntime } from "../connection/runtime";
import {
  linkPrimaryEnvironmentToCloud,
  type CloudLinkTarget,
  relinkPrimaryManagedEndpoint,
  unlinkPrimaryEnvironmentFromCloud,
} from "./linkEnvironment";

const cloudLinkScheduler = createAtomCommandScheduler();
const cloudLinkConcurrency = {
  mode: "serial" as const,
  key: (input: { readonly target: CloudLinkTarget }) => input.target.environmentId,
};

export const linkPrimaryEnvironment = createRuntimeCommand(connectionAtomRuntime, {
  label: "web:cloud:link-primary-environment",
  scheduler: cloudLinkScheduler,
  concurrency: cloudLinkConcurrency,
  execute: (input: {
    readonly target: CloudLinkTarget;
    readonly clerkToken: string;
    readonly installRelayClient?: boolean;
  }) => linkPrimaryEnvironmentToCloud(input),
});

/** Shares the link scheduler, so it queues behind (and re-checks after) a manual link or unlink. */
export const relinkPrimaryManagedEndpointCommand = createRuntimeCommand(connectionAtomRuntime, {
  label: "web:cloud:relink-primary-environment-for-preview-hosts",
  scheduler: cloudLinkScheduler,
  concurrency: cloudLinkConcurrency,
  execute: (input: {
    readonly target: CloudLinkTarget;
    readonly clerkToken: string;
    readonly accountId: string;
  }) => relinkPrimaryManagedEndpoint(input),
});

export const unlinkPrimaryEnvironment = createRuntimeCommand(connectionAtomRuntime, {
  label: "web:cloud:unlink-primary-environment",
  scheduler: cloudLinkScheduler,
  concurrency: cloudLinkConcurrency,
  execute: (input: { readonly target: CloudLinkTarget; readonly clerkToken: string | null }) =>
    unlinkPrimaryEnvironmentFromCloud(input),
});
