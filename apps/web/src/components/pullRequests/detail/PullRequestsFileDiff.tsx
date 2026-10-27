import type { PullRequestsFile } from "@coddy/contracts";
import { FileDiff } from "@pierre/diffs/react";
import type { DiffLineAnnotation, FileDiffOptions } from "@pierre/diffs";
import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTheme } from "../../../hooks/useTheme";
import {
  buildFileDiffRenderKey,
  getRenderablePatch,
  resolveDiffThemeName,
} from "../../../lib/diffRendering";
import { classifyDiffPayload } from "../../gitManager/history/diffLadder";
import { Button } from "../../ui/button";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { patchWithoutWhitespaceChanges } from "./pullRequestsDetail.logic";
export interface PullRequestsFileDiffProps {
  file: PullRequestsFile;
  hostUrl: string;
  viewed: boolean;
  onToggleViewed: (path: string) => void;
  ignoreWhitespace: boolean;
  annotations?: DiffLineAnnotation<ReactNode>[] | undefined;
  onLineClick?: (path: string, line: number, side: "left" | "right") => void;
  onLineRangeSelect?: (
    path: string,
    startLine: number,
    line: number,
    side: "left" | "right",
  ) => void;
}
function AnnotationFallback({
  annotations,
}: {
  annotations?: DiffLineAnnotation<ReactNode>[] | undefined;
}) {
  return annotations?.length ? (
    <div className="space-y-3 border-t border-border p-3">
      {annotations.map((annotation) => (
        <div key={`${annotation.side}:${annotation.lineNumber}`}>
          <p className="text-xs text-muted-foreground">
            Review comment
            {annotation.lineNumber
              ? ` at line ${annotation.lineNumber} (${annotation.side === "deletions" ? "old" : "new"} version)`
              : ""}{" "}
            — line not shown in this diff
          </p>
          {annotation.metadata}
        </div>
      ))}
    </div>
  ) : null;
}
const renderAnnotation = (annotation: DiffLineAnnotation<ReactNode>) => annotation.metadata;
function DiffBody({
  file,
  hostUrl,
  ignoreWhitespace,
  onLineClick,
  onLineRangeSelect,
  annotations,
}: PullRequestsFileDiffProps) {
  const { resolvedTheme } = useTheme();
  const [allowedPatch, setAllowedPatch] = useState<string | null>(null);
  const patch = file.patch ?? "";
  const measurements = useMemo(() => {
    let longestLineLength = 0;
    let start = 0;
    for (let index = 0; index <= patch.length; index++)
      if (index === patch.length || patch.charCodeAt(index) === 10) {
        longestLineLength = Math.max(longestLineLength, index - start);
        start = index + 1;
      }
    return { byteLength: new TextEncoder().encode(patch).byteLength, longestLineLength };
  }, [patch]);
  const classification = classifyDiffPayload(measurements);
  const longLines = measurements.longestLineLength > 5_000;
  const gated = classification === "large-text" && !longLines && allowedPatch !== patch;
  const parsed = useMemo(
    () =>
      classification === "unrenderable" || longLines || gated
        ? null
        : getRenderablePatch(
            ignoreWhitespace ? patchWithoutWhitespaceChanges(patch) : patch,
            "pull-requests",
            { compactPartialHunkOffsets: true },
          ),
    [classification, gated, ignoreWhitespace, longLines, patch],
  );
  const options = useMemo<FileDiffOptions<ReactNode>>(
    () => ({
      diffStyle: "unified",
      theme: resolveDiffThemeName(resolvedTheme),
      disableFileHeader: true,
      tokenizeMaxLineLength: 1_000,
      tokenizeMaxLength: 100_000,
      enableLineSelection: onLineRangeSelect !== undefined,
      ...(onLineClick
        ? {
            onLineClick: (event) =>
              onLineClick(
                file.path,
                event.lineNumber,
                event.annotationSide === "deletions" ? "left" : "right",
              ),
          }
        : {}),
      ...(onLineRangeSelect
        ? {
            onLineSelectionEnd: (range) => {
              if (range && (range.endSide === undefined || range.endSide === range.side))
                onLineRangeSelect(
                  file.path,
                  Math.min(range.start, range.end),
                  Math.max(range.start, range.end),
                  range.side === "deletions" ? "left" : "right",
                );
            },
          }
        : {}),
    }),
    [file.path, onLineClick, onLineRangeSelect, resolvedTheme],
  );
  const anchored = (annotation: DiffLineAnnotation<ReactNode>) =>
    parsed?.kind === "files" &&
    parsed.files.some(
      (diff) =>
        annotation.lineNumber === 0 ||
        diff.hunks.some((hunk) => {
          const start = annotation.side === "deletions" ? hunk.deletionStart : hunk.additionStart;
          const count = annotation.side === "deletions" ? hunk.deletionCount : hunk.additionCount;
          return annotation.lineNumber >= start && annotation.lineNumber < start + count;
        }),
    );
  const visibleAnnotations = annotations?.filter(anchored);
  const unanchored = annotations?.filter((annotation) => !anchored(annotation));
  return (
    <>
      {classification === "unrenderable" ? (
        <p className="p-3 text-sm">
          <PullRequestsExternalLink href={hostUrl}>
            Diff too large to render — open on host
          </PullRequestsExternalLink>
        </p>
      ) : gated ? (
        <div className="space-y-2 p-3 text-sm">
          <p>This is a large text diff and may take time to render.</p>
          <Button variant="outline" size="sm" onClick={() => setAllowedPatch(patch)}>
            Show diff anyway
          </Button>
        </div>
      ) : longLines || parsed?.kind === "raw" ? (
        <div className="space-y-2 p-3">
          <p className="text-xs text-muted-foreground">
            {longLines
              ? "Long lines shown as plain text"
              : parsed?.kind === "raw"
                ? parsed.reason
                : ""}
          </p>
          <pre className="overflow-auto whitespace-pre rounded-md bg-muted/30 p-3 font-mono text-xs">
            {patch}
          </pre>
        </div>
      ) : parsed?.kind === "files" ? (
        <div>
          {parsed.files.map((diff) => (
            <FileDiff
              key={buildFileDiffRenderKey(diff)}
              fileDiff={diff}
              options={options}
              {...(visibleAnnotations ? { lineAnnotations: visibleAnnotations } : {})}
              renderAnnotation={renderAnnotation}
            />
          ))}
        </div>
      ) : (
        <p className="p-3 text-sm text-muted-foreground">No diff content.</p>
      )}
      <AnnotationFallback annotations={unanchored} />
    </>
  );
}
export const PullRequestsFileDiff = memo(function PullRequestsFileDiff(
  props: PullRequestsFileDiffProps,
) {
  const { file, hostUrl, viewed, onToggleViewed } = props;
  const container = useRef<HTMLElement | null>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const element = container.current;
    if (!element || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setVisible(true);
          observer.disconnect();
        }
      },
      { rootMargin: "600px" },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return (
    <section
      ref={container}
      id={`file=${file.path}`}
      data-text-surface="background"
      className="m-3 min-w-0 overflow-hidden rounded-lg border border-border"
    >
      <header className="flex flex-wrap items-center gap-3 border-b border-border bg-muted/30 p-3 text-xs">
        <span className="min-w-0 flex-1 break-all font-mono">
          {file.previousPath ? `${file.previousPath} → ` : ""}
          {file.path}
        </span>
        <span className="text-green-700 dark:text-green-400">+{file.additions}</span>
        <span className="text-red-700 dark:text-red-400">−{file.deletions}</span>
        <label className="flex cursor-pointer items-center gap-1.5">
          <input
            aria-label={`Viewed ${file.path}`}
            type="checkbox"
            checked={viewed}
            onChange={() => onToggleViewed(file.path)}
          />
          Viewed
        </label>
      </header>
      {file.changeType === "binary" ? (
        <p className="p-3 text-sm text-muted-foreground">Binary file</p>
      ) : file.tooLarge ? (
        <p className="p-3 text-sm">
          <PullRequestsExternalLink href={hostUrl}>
            Diff too large to render — open on host
          </PullRequestsExternalLink>
        </p>
      ) : file.patch === null ? (
        <p className="p-3 text-sm">
          <PullRequestsExternalLink href={hostUrl}>
            Diff unavailable — open on host
          </PullRequestsExternalLink>
        </p>
      ) : visible ? (
        <DiffBody {...props} />
      ) : (
        <div className="min-h-28 p-3">
          <Button size="sm" variant="outline" onClick={() => setVisible(true)}>
            Show diff
          </Button>
        </div>
      )}
      {file.changeType === "binary" || file.tooLarge || file.patch === null ? (
        <AnnotationFallback annotations={props.annotations} />
      ) : null}
    </section>
  );
});
