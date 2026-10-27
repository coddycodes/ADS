import { definePlugin } from "@oxlint/plugins";

import namespaceNodeImports from "./rules/namespace-node-imports.ts";
import noEffectEventInMemoOrForwardRef from "./rules/no-effect-event-in-memo-or-forward-ref.ts";
import noGlobalProcessRuntime from "./rules/no-global-process-runtime.ts";
import noInlineSchemaCompile from "./rules/no-inline-schema-compile.ts";
import noManualEffectRuntimeInTests from "./rules/no-manual-effect-runtime-in-tests.ts";

export default definePlugin({
  meta: {
    name: "coddy",
  },
  rules: {
    "namespace-node-imports": namespaceNodeImports,
    "no-effect-event-in-memo-or-forward-ref": noEffectEventInMemoOrForwardRef,
    "no-global-process-runtime": noGlobalProcessRuntime,
    "no-inline-schema-compile": noInlineSchemaCompile,
    "no-manual-effect-runtime-in-tests": noManualEffectRuntimeInTests,
  },
});
