import type { GitManagerImageDiffSide } from "@coddy/contracts";

export type GitManagerImageDiffMode = "two-up" | "swipe" | "onion" | "difference";

export interface ImageLayerPresentation {
  readonly beforeOpacity: number;
  readonly afterOpacity: number;
  readonly afterClipPath: string | null;
  readonly afterMixBlendMode: "difference" | null;
}

const IMAGE_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "ico", "webp", "bmp", "avif"]);
const IMAGE_MIME_TYPE = /^image\/[a-z0-9.+-]+$/iu;
const BASE64_PAYLOAD = /^(?:[a-z0-9+/]{4})*(?:[a-z0-9+/]{2}==|[a-z0-9+/]{3}=)?$/iu;

export function isSupportedImageDiffPath(path: string): boolean {
  const separator = path.lastIndexOf(".");
  if (separator < 0 || separator === path.length - 1) return false;
  return IMAGE_EXTENSIONS.has(path.slice(separator + 1).toLowerCase());
}

export function buildImageDataUri(side: GitManagerImageDiffSide): string | null {
  if (
    side.contentBase64 === null ||
    side.mimeType === null ||
    !IMAGE_MIME_TYPE.test(side.mimeType) ||
    !BASE64_PAYLOAD.test(side.contentBase64)
  ) {
    return null;
  }
  return `data:${side.mimeType};base64,${side.contentBase64}`;
}

export function isRepositoryImageDataUri(value: string | null): value is string {
  if (value === null || !value.startsWith("data:image/")) return false;
  const separator = value.indexOf(";base64,");
  if (separator <= "data:image/".length) return false;
  const mimeType = value.slice("data:".length, separator);
  const payload = value.slice(separator + ";base64,".length);
  return IMAGE_MIME_TYPE.test(mimeType) && BASE64_PAYLOAD.test(payload);
}

export function resolveImageLayerPresentation(
  mode: GitManagerImageDiffMode,
  position: number,
): ImageLayerPresentation {
  const clamped = Math.min(100, Math.max(0, Number.isFinite(position) ? position : 50));
  if (mode === "swipe") {
    return {
      beforeOpacity: 1,
      afterOpacity: 1,
      afterClipPath: `inset(0 ${100 - clamped}% 0 0)`,
      afterMixBlendMode: null,
    };
  }
  if (mode === "onion") {
    return {
      beforeOpacity: 1,
      afterOpacity: clamped / 100,
      afterClipPath: null,
      afterMixBlendMode: null,
    };
  }
  if (mode === "difference") {
    return {
      beforeOpacity: 1,
      afterOpacity: 1,
      afterClipPath: null,
      afterMixBlendMode: "difference",
    };
  }
  return {
    beforeOpacity: 1,
    afterOpacity: 1,
    afterClipPath: null,
    afterMixBlendMode: null,
  };
}
