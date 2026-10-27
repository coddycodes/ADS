import type { FileDiffMetadata, SelectedLineRange } from "@pierre/diffs";
import type { ScopedThreadRef } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  stateValues: [null, null] as unknown[],
  stateIndex: 0,
  setters: [] as Array<ReturnType<typeof vi.fn>>,
  codeViewProps: null as Record<string, unknown> | null,
  localComments: [] as Array<Record<string, unknown>>,
  addReviewComment: vi.fn(),
  removeReviewComment: vi.fn(),
  reviewComments: [] as Array<Record<string, unknown>>,
  buildDiffReviewComment: vi.fn(),
  restoreDiffReviewCommentRange: vi.fn(),
  nextId: vi.fn(() => "draft-1"),
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useState: (initial: unknown) => {
      const index = harness.stateIndex++;
      const value = harness.stateValues[index] ?? initial;
      const setter = vi.fn();
      harness.setters[index] = setter;
      return [value, setter];
    },
  };
});

vi.mock("@pierre/diffs/react", () => ({
  CodeView: (props: Record<string, unknown>) => {
    harness.codeViewProps = props;
    return <div data-code-view />;
  },
}));

vi.mock("~/composerDraftStore", () => ({
  useComposerDraftStore: (selector: (store: Record<string, unknown>) => unknown) =>
    selector({
      addReviewComment: harness.addReviewComment,
      removeReviewComment: harness.removeReviewComment,
      getComposerDraft: () => ({ reviewComments: harness.reviewComments }),
    }),
}));

vi.mock("~/reviewCommentContext", () => ({
  buildDiffReviewComment: harness.buildDiffReviewComment,
  restoreDiffReviewCommentRange: harness.restoreDiffReviewCommentRange,
}));

vi.mock("../files/fileCommentAnnotations", () => ({
  nextFileCommentId: harness.nextId,
}));

vi.mock("../files/LocalCommentAnnotation", () => ({
  LocalCommentAnnotation: (props: Record<string, unknown>) => {
    harness.localComments.push(props);
    return <div data-local-comment={props.kind as string}>{props.text as string}</div>;
  },
}));

import { AnnotatableCodeView } from "./AnnotatableCodeView";

const threadRef = {
  environmentId: "env-1",
  threadId: "thread-1",
} as ScopedThreadRef;
const fileDiff = { name: "src/app.ts" } as unknown as FileDiffMetadata;
const files = [
  {
    fileDiff,
    filePath: "src/app.ts",
    fileKey: "file-1",
    collapsed: false,
  },
];

function defaultComment(input: {
  id: string;
  range: SelectedLineRange;
  text: string;
  filePath: string;
}) {
  return {
    id: input.id,
    sectionId: "turn-1",
    sectionTitle: "Turn 1",
    filePath: input.filePath,
    rangeLabel: `L${input.range.end}`,
    text: input.text,
    fenceLanguage: "diff",
  };
}

function renderView(overrides: Partial<React.ComponentProps<typeof AnnotatableCodeView>> = {}) {
  renderToStaticMarkup(
    <AnnotatableCodeView
      files={files}
      sectionId="turn-1"
      sectionTitle="Turn 1"
      composerDraftTarget={threadRef}
      options={{ theme: "dark" } as never}
      renderHeaderPrefix={vi.fn(() => (
        <span>header</span>
      ))}
      {...overrides}
    />,
  );
  if (!harness.codeViewProps) throw new Error("CodeView was not rendered");
  return harness.codeViewProps;
}

beforeEach(() => {
  harness.stateValues = [null, null];
  harness.stateIndex = 0;
  harness.setters.length = 0;
  harness.codeViewProps = null;
  harness.localComments.length = 0;
  harness.reviewComments.length = 0;
  harness.addReviewComment.mockReset();
  harness.removeReviewComment.mockReset();
  harness.buildDiffReviewComment.mockReset();
  harness.buildDiffReviewComment.mockImplementation(defaultComment);
  harness.restoreDiffReviewCommentRange.mockReset();
  harness.nextId.mockClear();
});

describe("AnnotatableCodeView", () => {
  it("builds and groups persisted annotations for matching diff comments", () => {
    harness.reviewComments.push(
      {
        id: "comment-1",
        sectionId: "turn-1",
        filePath: "src/app.ts",
        fenceLanguage: undefined,
        rangeLabel: "-2",
        text: "first",
      },
      {
        id: "comment-2",
        sectionId: "turn-1",
        filePath: "src/app.ts",
        fenceLanguage: "diff",
        rangeLabel: "-2 again",
        text: "second",
      },
      { id: "wrong-section", sectionId: "other", filePath: "src/app.ts" },
      { id: "wrong-file", sectionId: "turn-1", filePath: "src/other.ts" },
      {
        id: "wrong-language",
        sectionId: "turn-1",
        filePath: "src/app.ts",
        fenceLanguage: "ts",
      },
      {
        id: "unrestorable",
        sectionId: "turn-1",
        filePath: "src/app.ts",
        fenceLanguage: "diff",
      },
    );
    harness.restoreDiffReviewCommentRange
      .mockReturnValueOnce({ start: 2, end: 2, side: "deletions" })
      .mockReturnValueOnce({ start: 2, end: 2, side: "deletions" })
      .mockReturnValueOnce(null);

    const props = renderView({ viewerRef: { current: null }, className: "custom" });
    const items = props.items as Array<Record<string, unknown>>;
    const annotations = items[0]!.annotations as Array<Record<string, unknown>>;

    expect(props.ref).toEqual({ current: null });
    expect(props.className).toBe("custom");
    expect(annotations).toHaveLength(1);
    expect(annotations[0]).toMatchObject({ side: "deletions", lineNumber: 2 });
    expect(
      (annotations[0]!.metadata as { entries: Array<{ id: string }> }).entries.map(
        (entry) => entry.id,
      ),
    ).toEqual(["comment-1", "comment-2"]);
    expect(items[0]).toMatchObject({ id: "file-1", type: "diff", collapsed: false });
    expect(typeof items[0]!.version).toBe("number");
  });

  it("renders headers and annotation actions while interactions are enabled", () => {
    harness.reviewComments.push({
      id: "comment-1",
      sectionId: "turn-1",
      filePath: "src/app.ts",
      fenceLanguage: "diff",
      rangeLabel: "+3",
      text: "note",
    });
    harness.restoreDiffReviewCommentRange.mockReturnValue({ start: 3, end: 3, side: "additions" });
    const renderHeaderPrefix = vi.fn(() => <span>header</span>);
    const props = renderView({ renderHeaderPrefix });
    const options = props.options as Record<string, unknown>;

    expect(options).toMatchObject({
      theme: "dark",
      enableGutterUtility: true,
      enableLineSelection: true,
    });
    const header = props.renderHeaderPrefix as (item: Record<string, unknown>) => React.ReactNode;
    expect(renderToStaticMarkup(header((props.items as Array<Record<string, unknown>>)[0]!))).toBe(
      "<span>header</span>",
    );
    expect(header({ type: "file" })).toBeNull();
    expect(renderHeaderPrefix).toHaveBeenCalledWith(fileDiff, "file-1", false);

    const annotation = (
      (props.items as Array<Record<string, unknown>>)[0]!.annotations as Array<
        Record<string, unknown>
      >
    )[0]!;
    const renderAnnotation = props.renderAnnotation as (
      value: Record<string, unknown>,
    ) => React.ReactNode;
    expect(renderToStaticMarkup(renderAnnotation(annotation))).toContain(
      'data-local-comment="comment"',
    );
    const localComment = harness.localComments[0]!;
    (localComment.onCancel as () => void)();
    (localComment.onDelete as () => void)();
    expect(harness.removeReviewComment).toHaveBeenCalledTimes(2);
    expect(harness.removeReviewComment).toHaveBeenCalledWith(threadRef, "comment-1");
    expect(harness.setters[0]).toHaveBeenCalledWith(null);
  });

  it("forwards a controlled non-comment selection mode", () => {
    const selectedLines = {
      id: "file-1",
      range: { start: 1, end: 3, side: "additions" },
    } as const;
    const onSelectedLinesChange = vi.fn();
    const onLineSelectionEnd = vi.fn();
    const props = renderView({
      enableGutterUtility: false,
      enableLineSelection: true,
      selectedLines,
      onSelectedLinesChange,
      onLineSelectionEnd,
    });

    expect(props.selectedLines).toBe(selectedLines);
    expect(props.onSelectedLinesChange).toBe(onSelectedLinesChange);
    expect(props.options).toMatchObject({
      enableGutterUtility: false,
      enableLineSelection: true,
      onLineSelectionEnd,
    });
  });

  it("ignores invalid selections and starts a draft for a diff selection", () => {
    const props = renderView();
    const beginComment = (props.options as Record<string, unknown>).onLineSelectionEnd as (
      range: SelectedLineRange | null,
      context: { item: Record<string, unknown> },
    ) => void;
    const range = { start: 4, end: 5, side: "deletions" } as SelectedLineRange;

    beginComment(null, { item: { type: "diff", id: "file-1" } });
    beginComment(range, { item: { type: "file", id: "file-1" } });
    beginComment(range, { item: { type: "diff", id: "missing" } });
    expect(harness.buildDiffReviewComment).not.toHaveBeenCalled();

    harness.buildDiffReviewComment.mockReturnValueOnce(null);
    beginComment(range, { item: { type: "diff", id: "file-1" } });
    expect(harness.setters[1]).not.toHaveBeenCalled();

    harness.buildDiffReviewComment.mockImplementation(defaultComment);
    beginComment(range, { item: { type: "diff", id: "file-1" } });
    expect(harness.nextId).toHaveBeenCalled();
    expect(harness.setters[1]).toHaveBeenCalledWith({
      fileKey: "file-1",
      annotation: {
        side: "deletions",
        lineNumber: 5,
        metadata: {
          entries: [
            {
              id: "draft-1",
              kind: "draft",
              range,
              rangeLabel: "L5",
              text: "",
            },
          ],
        },
      },
    });
  });

  it("appends a draft, disables selection, and submits it", () => {
    const range = {
      start: 7,
      end: 8,
      side: "additions",
      endSide: "additions",
    } as SelectedLineRange;
    harness.reviewComments.push({
      id: "comment-1",
      sectionId: "turn-1",
      filePath: "src/app.ts",
      fenceLanguage: "diff",
      rangeLabel: "+8",
      text: "persisted",
    });
    harness.restoreDiffReviewCommentRange.mockReturnValue(range);
    harness.stateValues = [
      { id: "selection", range },
      {
        fileKey: "file-1",
        annotation: {
          side: "additions",
          lineNumber: 8,
          metadata: {
            entries: [{ id: "draft-1", kind: "draft", range, rangeLabel: "L8", text: "" }],
          },
        },
      },
    ];
    const props = renderView();
    const options = props.options as Record<string, unknown>;
    const item = (props.items as Array<Record<string, unknown>>)[0]!;

    expect(options).toMatchObject({ enableGutterUtility: false, enableLineSelection: false });
    expect(props.selectedLines).toEqual({ id: "selection", range });
    expect((item.annotations as unknown[]).length).toBe(2);
    (props.onSelectedLinesChange as (value: unknown) => void)({ id: "next", range });
    expect(harness.setters[0]).toHaveBeenCalledWith({ id: "next", range });

    const draftAnnotation = (item.annotations as Array<Record<string, unknown>>)[1]!;
    renderToStaticMarkup(
      (props.renderAnnotation as (value: Record<string, unknown>) => React.ReactNode)(
        draftAnnotation,
      ),
    );
    (harness.localComments[0]!.onComment as (text: string) => void)("ship it");
    expect(harness.addReviewComment).toHaveBeenCalledWith(
      threadRef,
      expect.objectContaining({ id: "draft-1", text: "ship it", filePath: "src/app.ts" }),
    );
    expect(harness.setters[0]).toHaveBeenCalledWith(null);
    expect(harness.setters[1]).toHaveBeenCalledWith(null);
  });

  it("clears a cancelled draft without touching persisted comments", () => {
    const range = { start: 1, end: 1, side: "additions" } as SelectedLineRange;
    harness.stateValues = [
      null,
      {
        fileKey: "file-1",
        annotation: {
          side: "additions",
          lineNumber: 1,
          metadata: {
            entries: [{ id: "draft-1", kind: "draft", range, rangeLabel: "+1", text: "" }],
          },
        },
      },
    ];
    const props = renderView();
    const draftAnnotation = (
      (props.items as Array<Record<string, unknown>>)[0]!.annotations as Array<
        Record<string, unknown>
      >
    )[0]!;
    renderToStaticMarkup(
      (props.renderAnnotation as (value: Record<string, unknown>) => React.ReactNode)(
        draftAnnotation,
      ),
    );
    (harness.localComments[0]!.onCancel as () => void)();

    expect(harness.setters[1]).toHaveBeenCalledWith(null);
    expect(harness.removeReviewComment).not.toHaveBeenCalled();
  });

  it("ignores draft submissions when the entry, file, or comment cannot be built", () => {
    const range = { start: 1, end: 1, side: "additions" } as SelectedLineRange;
    const draft = {
      fileKey: "missing",
      annotation: {
        side: "additions",
        lineNumber: 1,
        metadata: {
          entries: [{ id: "draft-1", kind: "draft", range, rangeLabel: "+1", text: "" }],
        },
      },
    };
    harness.stateValues = [null, draft];
    let props = renderView();
    const renderAnnotation = props.renderAnnotation as (
      value: Record<string, unknown>,
    ) => React.ReactNode;
    renderToStaticMarkup(renderAnnotation(draft.annotation));
    (harness.localComments[0]!.onComment as (text: string) => void)("missing file");
    expect(harness.addReviewComment).not.toHaveBeenCalled();

    harness.stateIndex = 0;
    harness.codeViewProps = null;
    harness.localComments.length = 0;
    harness.stateValues = [null, { ...draft, fileKey: "file-1" }];
    props = renderView();
    const annotation = (
      (props.items as Array<Record<string, unknown>>)[0]!.annotations as Array<
        Record<string, unknown>
      >
    )[0]!;
    renderToStaticMarkup(
      (props.renderAnnotation as (value: Record<string, unknown>) => React.ReactNode)({
        ...annotation,
        metadata: {
          entries: [{ id: "ghost", kind: "draft", range, rangeLabel: "+1", text: "" }],
        },
      }),
    );
    (harness.localComments[0]!.onComment as (text: string) => void)("missing entry");
    expect(harness.addReviewComment).not.toHaveBeenCalled();

    harness.localComments.length = 0;
    renderToStaticMarkup(
      (props.renderAnnotation as (value: Record<string, unknown>) => React.ReactNode)(annotation),
    );
    harness.buildDiffReviewComment.mockReturnValueOnce(null);
    (harness.localComments[0]!.onComment as (text: string) => void)("invalid comment");
    expect(harness.addReviewComment).not.toHaveBeenCalled();
    expect(harness.setters[0]).toHaveBeenCalledWith(null);
    expect(harness.setters[1]).toHaveBeenCalledWith(null);
  });
});
