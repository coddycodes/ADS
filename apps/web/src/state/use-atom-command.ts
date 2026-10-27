import { RegistryContext } from "@effect/atom-react";
import {
  type AtomCommand,
  type AtomCommandOptions,
  type AtomCommandResult,
  type AtomCommandRunOptions,
  runAtomCommand,
} from "@coddy/client-runtime/state/runtime";
import { useCallback, useContext } from "react";

export function useAtomCommand<A, E, W>(
  command: AtomCommand<W, A, E>,
  options?: string | AtomCommandOptions,
): (value: W, runOptions?: AtomCommandRunOptions) => Promise<AtomCommandResult<A, E>> {
  const registry = useContext(RegistryContext);
  const label = typeof options === "string" ? options : (options?.label ?? command.label);
  const reportFailure = typeof options === "string" ? true : (options?.reportFailure ?? true);
  const reportDefect = typeof options === "string" ? true : (options?.reportDefect ?? true);

  return useCallback(
    (value: W, runOptions?: AtomCommandRunOptions) =>
      runAtomCommand(registry, command, value, {
        label,
        reportFailure,
        reportDefect,
        signal: runOptions?.signal,
      }),
    [command, label, registry, reportDefect, reportFailure],
  );
}
