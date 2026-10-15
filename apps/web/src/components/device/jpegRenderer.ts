/** A decoded frame: an `ImageBitmap` in the browser. */
export interface DecodedFrame {
  readonly width: number;
  readonly height: number;
  close(): void;
}

export type JpegDecoder<F extends DecodedFrame> = (blob: Blob) => Promise<F>;

const decodeWithImageBitmap: JpegDecoder<ImageBitmap> = (blob) => createImageBitmap(blob);

function base64ToBytes(base64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

/**
 * Draws JPEG frames, latest only: while one frame decodes, at most one newer frame waits, and
 * each newer arrival replaces it. Every decoded frame is closed after drawing.
 */
export class JpegRenderer<F extends DecodedFrame = ImageBitmap> {
  private waiting: string | null = null;
  // The last frame drawn, kept so a resized (and so cleared) canvas can be painted again: the
  // server sends frames only when the screen changes.
  private lastDrawn: string | null = null;
  private decoding = false;
  private disposed = false;

  constructor(
    private readonly draw: (frame: F) => void,
    // The default decoder makes `ImageBitmap`s, the default frame type.
    private readonly decode: JpegDecoder<F> = decodeWithImageBitmap as unknown as JpegDecoder<F>,
  ) {}

  push(jpegBase64: string): void {
    if (this.disposed) return;
    if (this.decoding) {
      this.waiting = jpegBase64;
      return;
    }
    this.start(jpegBase64);
  }

  /** Draws the last drawn frame again, unless a newer one is already on its way. */
  redraw(): void {
    if (this.disposed || this.decoding || this.lastDrawn === null) return;
    this.start(this.lastDrawn);
  }

  dispose(): void {
    this.disposed = true;
    this.waiting = null;
    this.lastDrawn = null;
  }

  private start(jpegBase64: string): void {
    this.decoding = true;
    void this.render(jpegBase64).finally(() => {
      this.decoding = false;
      const next = this.waiting;
      this.waiting = null;
      if (next !== null && !this.disposed) this.start(next);
    });
  }

  private async render(jpegBase64: string): Promise<void> {
    let frame: F;
    try {
      frame = await this.decode(new Blob([base64ToBytes(jpegBase64)], { type: "image/jpeg" }));
    } catch {
      // A frame that does not decode is skipped; the next one replaces it.
      return;
    }
    try {
      if (!this.disposed) {
        this.draw(frame);
        this.lastDrawn = jpegBase64;
      }
    } finally {
      frame.close();
    }
  }
}
