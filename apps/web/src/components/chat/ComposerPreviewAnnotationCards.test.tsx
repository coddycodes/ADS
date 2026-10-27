import type { PreviewAnnotationPayload } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

import type { ComposerImageAttachment } from "~/composerDraftStore";
import { ComposerPreviewAnnotationCards } from "./ComposerPreviewAnnotationCards";

const annotation: PreviewAnnotationPayload = {
  id: "annotation_1",
  pageUrl: "http://localhost:3000/welcome",
  pageTitle: "Welcome",
  comment: "Make this headline feel intentional.",
  elements: [],
  regions: [{ id: "region_1", rect: { x: 1, y: 2, width: 30, height: 20 } }],
  strokes: [],
  styleChanges: [
    {
      targetId: "element_1",
      selector: "h1",
      property: "font-size",
      previousValue: "32px",
      value: "40px",
    },
  ],
  screenshot: null,
  createdAt: "2026-06-13T00:00:00.000Z",
};

describe("ComposerPreviewAnnotationCards", () => {
  it("renders nothing without annotations", () => {
    expect(
      renderToStaticMarkup(
        <ComposerPreviewAnnotationCards
          annotations={[]}
          images={[]}
          onRemove={vi.fn()}
          onExpandImage={vi.fn()}
        />,
      ),
    ).toBe("");
  });

  it("presents the annotation as one contextual attachment", () => {
    const markup = renderToStaticMarkup(
      <ComposerPreviewAnnotationCards
        annotations={[annotation]}
        images={[]}
        onRemove={vi.fn()}
        onExpandImage={vi.fn()}
      />,
    );

    expect(markup).toContain("Make this headline feel intentional.");
    expect(markup).toContain('title="1 region"');
    expect(markup).toContain('title="1 style change"');
    expect(markup).not.toContain("Welcome");
    expect(markup).not.toContain("localhost:3000");
    expect(markup).not.toContain("Preview annotation");
  });

  it("renders image previews, normalized element labels, drawings, and plural counts", () => {
    const element = {
      id: "element_1",
      rect: { x: 1, y: 2, width: 3, height: 4 },
      element: {
        pageUrl: "http://localhost:3000/welcome",
        pageTitle: "Welcome",
        tagName: "BUTTON",
        selector: ".save",
        htmlPreview: "<button>Save</button>",
        componentName: "SaveButton",
        source: null,
        stack: [],
        styles: "color: red",
        pickedAt: "2026-06-13T00:00:00.000Z",
      },
    };
    const invalidElement = {
      ...element,
      id: "invalid",
      element: { ...element.element, pageUrl: " ", tagName: " " },
    };
    const richAnnotation: PreviewAnnotationPayload = {
      ...annotation,
      comment: " ",
      elements: [
        element,
        { ...element, id: "element_2", element: { ...element.element, componentName: null } },
        { ...element, id: "element_3", element: { ...element.element, componentName: "Card" } },
        invalidElement,
      ],
      regions: [],
      strokes: [
        {
          id: "stroke_1",
          color: "#000000",
          width: 2,
          points: [{ x: 1, y: 1 }],
          bounds: { x: 1, y: 1, width: 1, height: 1 },
        },
        {
          id: "stroke_2",
          color: "#ffffff",
          width: 3,
          points: [{ x: 2, y: 2 }],
          bounds: { x: 2, y: 2, width: 1, height: 1 },
        },
      ],
      styleChanges: [],
    };
    const image: ComposerImageAttachment = {
      type: "image",
      id: richAnnotation.id,
      name: "annotated.png",
      mimeType: "image/png",
      sizeBytes: 1,
      previewUrl: "data:image/png;base64,AA==",
      file: new File([new Uint8Array([0])], "annotated.png", { type: "image/png" }),
    };

    const markup = renderToStaticMarkup(
      <ComposerPreviewAnnotationCards
        annotations={[richAnnotation]}
        images={[image]}
        onRemove={vi.fn()}
        onExpandImage={vi.fn()}
        className="custom-class"
      />,
    );

    expect(markup).toContain("custom-class");
    expect(markup).toContain('aria-label="Preview annotated.png"');
    expect(markup).toContain('src="data:image/png;base64,AA=="');
    expect(markup).toContain("&lt;SaveButton&gt;");
    expect(markup).toContain("&lt;button&gt;");
    expect(markup).toContain("+1");
    expect(markup).toContain('title="4 elements"');
    expect(markup).toContain('title="2 drawings"');
    expect(markup).not.toContain("Make this headline feel intentional.");

    const twoLabelMarkup = renderToStaticMarkup(
      <ComposerPreviewAnnotationCards
        annotations={[{ ...richAnnotation, elements: richAnnotation.elements.slice(0, 2) }]}
        images={[]}
        onRemove={vi.fn()}
        onExpandImage={vi.fn()}
      />,
    );
    expect(twoLabelMarkup).not.toContain("+1");
  });
});
