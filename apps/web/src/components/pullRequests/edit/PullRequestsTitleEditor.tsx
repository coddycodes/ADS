import type { PullRequestsProviderKind } from "@coddy/contracts";
import { formatChangeRequestNumber } from "@coddy/shared/sourceControl";
import { PullRequestsTextEditor, type PullRequestsEditorProps } from "./PullRequestsTextEditor";
export function PullRequestsTitleEditor({
  provider,
  ...props
}: PullRequestsEditorProps & { provider: PullRequestsProviderKind }) {
  return (
    <PullRequestsTextEditor {...props} field="title">
      <h1 className="min-w-0 break-words text-xl font-semibold">
        {props.detail.title}{" "}
        <span className="font-normal text-muted-foreground">
          {formatChangeRequestNumber(provider, props.detail.number)}
        </span>
      </h1>
    </PullRequestsTextEditor>
  );
}
