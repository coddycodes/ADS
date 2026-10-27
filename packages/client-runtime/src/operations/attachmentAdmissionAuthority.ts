import { ORCHESTRATION_WS_METHODS } from "@coddy/contracts";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { currentSession, requestInSession } from "../rpc/client.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { AttachmentAdmissionAuthority } from "./attachmentAdmissionOwner.ts";
import type { StartThreadTurnInput } from "./commands.ts";

export class AttachmentAdmissionAuthorityError extends Schema.TaggedError<AttachmentAdmissionAuthorityError>()(
  "AttachmentAdmissionAuthorityError",
  { message: Schema.String },
) {}

const authorityForSession = Effect.fn("Attachments.authorityForSession")(function* (
  session: RpcSession,
) {
  const supervisor = yield* EnvironmentSupervisor;
  const registry = yield* EnvironmentRegistry;
  const config = yield* session.initialConfig;
  if (
    config.environment.storageInstanceId === null &&
    config.environment.capabilities.attachmentStaging !== true
  )
    return null;
  const prepared = yield* SubscriptionRef.get(supervisor.prepared);
  if (Option.isNone(prepared) || config.environment.storageInstanceId === null)
    return yield* new AttachmentAdmissionAuthorityError({
      message: "This server cannot identify its data store. Reconnect before sending attachments.",
    });
  const connection = prepared.value;
  let hostIdentity: string;
  if (connection.e2ee !== null) hostIdentity = `pinned:${connection.e2ee.hostKey}`;
  else if (supervisor.target._tag === "SshConnectionTarget") {
    const entries = yield* SubscriptionRef.get(registry.entries);
    const profile = entries.get(supervisor.target.environmentId)?.profile;
    if (!profile || Option.isNone(profile) || profile.value._tag !== "SshConnectionProfile")
      return yield* new AttachmentAdmissionAuthorityError({
        message:
          "The original SSH host cannot be identified. Reconnect before confirming this send.",
      });
    hostIdentity = `ssh:${encodeURIComponent(profile.value.target.hostname)}:${profile.value.target.port ?? 22}:${encodeURIComponent(profile.value.target.username ?? "")}`;
  } else hostIdentity = `${supervisor.target._tag}:${new URL(connection.httpBaseUrl).origin}`;
  return {
    storageInstanceId: config.environment.storageInstanceId,
    hostIdentity,
  } satisfies AttachmentAdmissionAuthority;
});

export const readAttachmentAdmissionAuthority = Effect.fn("Attachments.readAdmissionAuthority")(
  function* () {
    return yield* authorityForSession(yield* currentSession());
  },
);

/** Check authority and dispatch on one exact session, after the serial lane becomes available. */
export const admitStagedThreadTurn = Effect.fn("Attachments.admitThreadTurn")(function* (
  input: StartThreadTurnInput,
  expected: AttachmentAdmissionAuthority,
) {
  const supervisor = yield* EnvironmentSupervisor;
  const session = yield* currentSession();
  const actual = yield* authorityForSession(session);
  if (
    actual === null ||
    actual.storageInstanceId !== expected.storageInstanceId ||
    actual.hostIdentity !== expected.hostIdentity
  )
    return yield* new AttachmentAdmissionAuthorityError({
      message:
        "This send belongs to another host or data store. Reconnect to the original environment before confirming it.",
    });
  if (input.commandId === undefined || input.createdAt === undefined)
    return yield* new AttachmentAdmissionAuthorityError({
      message: "The original send cannot be identified safely. Your draft was kept.",
    });
  return yield* requestInSession(
    session,
    supervisor.target.environmentId,
    ORCHESTRATION_WS_METHODS.dispatchCommand,
    { ...input, commandId: input.commandId, createdAt: input.createdAt, type: "thread.turn.start" },
  );
});
