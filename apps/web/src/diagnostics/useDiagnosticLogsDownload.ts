import { useCallback, useState } from "react";

import { toastManager } from "../components/ui/toast";
import { downloadDiagnosticLogs, type DiagnosticLogsOptions } from "./downloadDiagnosticLogs";
import { readFrontendLogSnapshot } from "./frontendLogCapture";

/**
 * Prepares the primary environment's diagnostic ZIP and reports the outcome in a toast.
 * One download runs at a time per caller.
 */
export function useDiagnosticLogsDownload() {
  const [status, setStatus] = useState<"idle" | "downloading">("idle");

  const download = useCallback(
    (options?: DiagnosticLogsOptions) => {
      if (status === "downloading") return;
      setStatus("downloading");
      void (async () => {
        try {
          const result = await downloadDiagnosticLogs(readFrontendLogSnapshot(), options);
          if (result.status === "saved") {
            toastManager.add({
              type: "success",
              title: "Diagnostic logs saved",
              description: result.path,
            });
          } else if (result.status === "downloaded") {
            toastManager.add({
              type: "success",
              title: "Diagnostic logs download started",
              description: result.filename,
            });
          }
        } catch (error) {
          toastManager.add({
            type: "error",
            title: "Could not download diagnostic logs",
            description:
              error instanceof Error ? error.message : "Unable to prepare the diagnostic archive.",
          });
        } finally {
          setStatus("idle");
        }
      })();
    },
    [status],
  );

  return { isDownloading: status === "downloading", download };
}
