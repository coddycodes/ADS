import { useAtomValue } from "@effect/atom-react";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@coddy/client-runtime/state/runtime";
import { useCallback, useState } from "react";

import { resolveAndPersistPreferredEditor } from "../../editorPreferences";
import { usePrimaryEnvironment } from "../../state/environments";
import { primaryServerAvailableEditorsAtom } from "../../state/server";
import { shellEnvironment } from "../../state/shell";
import { useAtomCommand } from "../../state/use-atom-command";

/**
 * Opens a directory of the primary environment's machine in the preferred editor.
 * `failureMessage` is shown when the failure carries no message of its own.
 */
export function useOpenDirectoryInEditor(failureMessage: string) {
  const availableEditors = useAtomValue(primaryServerAvailableEditorsAtom);
  const environmentId = usePrimaryEnvironment()?.environmentId ?? null;
  const openInEditor = useAtomCommand(shellEnvironment.openInEditor, { reportFailure: false });
  const [isOpening, setIsOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const open = useCallback(
    (directoryPath: string) => {
      const editor = resolveAndPersistPreferredEditor(availableEditors ?? []);
      if (!editor) {
        setError("No available editors found.");
        return;
      }
      if (environmentId === null) {
        setError("No environment is selected.");
        return;
      }

      setIsOpening(true);
      setError(null);
      void (async () => {
        const result = await openInEditor({
          environmentId,
          input: { cwd: directoryPath, editor },
        });
        setIsOpening(false);
        if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
          const failure = squashAtomCommandFailure(result);
          setError(failure instanceof Error ? failure.message : failureMessage);
        }
      })();
    },
    [availableEditors, environmentId, failureMessage, openInEditor],
  );

  return { isOpening, error, open };
}
