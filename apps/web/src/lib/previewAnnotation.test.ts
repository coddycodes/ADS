import type { PreviewAnnotationPayload } from "@coddy/contracts";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  appendPreviewAnnotationPrompt,
  buildPreviewAnnotationPrompt,
  extractTrailingPreviewAnnotation,
  previewAnnotationScreenshotFile,
} from "./previewAnnotation";

const annotation: PreviewAnnotationPayload = {
  id: "annotation_1",
  pageUrl: "http://localhost:3000",
  pageTitle: "Example",
  comment: "Make these cards feel related.",
  elements: [],
  regions: [{ id: "region_1", rect: { x: 10, y: 20, width: 100, height: 80 } }],
  strokes: [
    {
      id: "stroke_1",
      color: "#7c3aed",
      width: 4,
      points: [
        { x: 10, y: 10 },
        { x: 20, y: 20 },
      ],
      bounds: { x: 6, y: 6, width: 18, height: 18 },
    },
  ],
  styleChanges: [
    {
      targetId: "element_1",
      selector: ".card",
      property: "border-radius",
      previousValue: "4px",
      value: "16px",
    },
  ],
  screenshot: {
    dataUrl: "data:image/png;base64,AA==",
    width: 100,
    height: 80,
    cropRect: { x: 10, y: 20, width: 100, height: 80 },
  },
  createdAt: "2026-06-11T00:00:00.000Z",
};

describe("preview annotations", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("describes regions, drawings, styles, and screenshot context", () => {
    const result = buildPreviewAnnotationPrompt(annotation);
    expect(result).toContain("Make these cards feel related.");
    expect(result).toContain("1 marked region");
    expect(result).toContain("1 drawing");
    expect(result).toContain("border-radius: 4px → 16px");
    expect(result).toContain("attached screenshot");
  });

  it("appends to an existing composer prompt", () => {
    expect(
      appendPreviewAnnotationPrompt("Fix this", annotation).startsWith(
        "Fix this\n\n<preview_annotation>",
      ),
    ).toBe(true);
  });

  it("extracts annotation presentation from a sent prompt", () => {
    const result = extractTrailingPreviewAnnotation(
      appendPreviewAnnotationPrompt("Fix this", annotation),
    );
    expect(result.promptText).toBe("Fix this");
    expect(result.annotation).toMatchObject({
      title: "Example",
      targetSummary: "1 marked region, 1 drawing.",
      hasScreenshot: true,
    });
  });

  it("extracts multiple trailing annotations one at a time", () => {
    const first = appendPreviewAnnotationPrompt("Fix this", annotation);
    const secondAnnotation = { ...annotation, id: "annotation_2", pageTitle: "Details" };
    const second = appendPreviewAnnotationPrompt(first, secondAnnotation);
    const extractedSecond = extractTrailingPreviewAnnotation(second);
    const extractedFirst = extractTrailingPreviewAnnotation(extractedSecond.promptText);
    expect(extractedSecond.annotation?.id).toBe("annotation_2");
    expect(extractedFirst.annotation?.id).toBe("annotation_1");
    expect(extractedFirst.promptText).toBe("Fix this");
  });

  it("covers empty, singular, and plural target descriptions", () => {
    const empty = {
      ...annotation,
      pageTitle: " ",
      comment: " ",
      regions: [],
      strokes: [],
      styleChanges: [],
      screenshot: null,
    };
    expect(buildPreviewAnnotationPrompt(empty)).toContain("Page: http://localhost:3000");
    expect(buildPreviewAnnotationPrompt(empty)).not.toContain("Targets:");
    expect(appendPreviewAnnotationPrompt("   ", empty)).toMatch(/^<preview_annotation>/);

    const element = {
      id: "element_1",
      rect: { x: 1, y: 2, width: 3, height: 4 },
      element: {
        pageUrl: "http://localhost:3000",
        pageTitle: "Example",
        tagName: "BUTTON",
        selector: ".save",
        htmlPreview: "<button>Save</button>",
        componentName: "SaveButton",
        source: null,
        stack: [],
        styles: "color: red",
        pickedAt: "2026-06-11T00:00:00.000Z",
      },
    };
    const plural = {
      ...empty,
      pageUrl: " ",
      elements: [element, { ...element, id: "element_2" }],
      regions: [annotation.regions[0]!, { ...annotation.regions[0]!, id: "region_2" }],
      strokes: [annotation.strokes[0]!, { ...annotation.strokes[0]!, id: "stroke_2" }],
      styleChanges: [{ ...annotation.styleChanges[0]!, previousValue: "" }],
    };
    const result = buildPreviewAnnotationPrompt(plural);
    expect(result).toContain("Page: Preview");
    expect(result).toContain("2 selected elements, 2 marked regions, 2 drawings");
    expect(result).toContain("border-radius: (unset) → 16px");
    expect(result).toContain("<element_context>");

    expect(buildPreviewAnnotationPrompt({ ...plural, elements: [element] })).toContain(
      "1 selected element",
    );
  });

  it("ignores invalid element context selections", () => {
    const invalidElement = {
      id: "invalid",
      rect: { x: 0, y: 0, width: 1, height: 1 },
      element: {
        pageUrl: " ",
        pageTitle: null,
        tagName: " ",
        selector: null,
        htmlPreview: "",
        componentName: null,
        source: null,
        stack: [],
        styles: "",
        pickedAt: "2026-06-11T00:00:00.000Z",
      },
    };
    const result = buildPreviewAnnotationPrompt({
      ...annotation,
      elements: [invalidElement],
      regions: [],
      strokes: [],
      styleChanges: [],
      screenshot: null,
    });
    expect(result).toContain("1 selected element");
    expect(result).not.toContain("<element_context>");
  });

  it("returns the original prompt when no trailing annotation exists", () => {
    expect(extractTrailingPreviewAnnotation("Fix this normally")).toEqual({
      promptText: "Fix this normally",
      annotation: null,
    });
  });

  it("supplies presentation defaults and extracts styles around element context", () => {
    const prompt = [
      "Before",
      "",
      "<preview_annotation>",
      "Preview annotation:",
      "Requested visual changes:",
      "- color: red → blue",
      "ignored line",
      "<element_context>",
      "- <button>",
      "</element_context>",
      "</preview_annotation>",
    ].join("\n");
    const extracted = extractTrailingPreviewAnnotation(prompt);
    expect(extracted).toEqual({
      promptText: "Before",
      annotation: {
        id: "6",
        title: "Preview annotation",
        comment: "",
        targetSummary: "",
        styleChanges: ["color: red → blue"],
        hasScreenshot: false,
      },
    });

    expect(
      extractTrailingPreviewAnnotation("<preview_annotation>\nId: only-id\n</preview_annotation>")
        .annotation?.styleChanges,
    ).toEqual([]);
  });

  it("creates screenshot files and preserves or defaults their media type", async () => {
    class TestFile extends Blob {
      readonly name: string;
      constructor(parts: BlobPart[], name: string, options?: FilePropertyBag) {
        super(parts, options);
        this.name = name;
      }
    }
    vi.stubGlobal("File", TestFile);
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(new Blob(["png"], { type: "image/custom" })))
      .mockResolvedValueOnce(new Response(new Blob(["png"])));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      previewAnnotationScreenshotFile({ ...annotation, screenshot: null }),
    ).resolves.toBeNull();
    const typed = await previewAnnotationScreenshotFile(annotation);
    const defaulted = await previewAnnotationScreenshotFile(annotation);

    expect(typed).toMatchObject({
      name: "preview-annotation-annotation_1.png",
      type: "image/custom",
    });
    expect(defaulted).toMatchObject({ type: "image/png" });
  });
});
