import type {
  DeviceInputEvent,
  DeviceJpegFrame,
  DeviceSnapshot,
  EnvironmentId,
} from "@bibcode/contracts";
import { useAtomRefresh, useAtomSubscribe } from "@effect/atom-react";
import { AsyncResult } from "effect/unstable/reactivity";
import {
  type ClipboardEvent as ReactClipboardEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { deviceEnvironment } from "../../state/device";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { clampToBox, displaySize, letterbox, toNormalized } from "./deviceGeometry";
import { keyboardEventToDeviceInput, splitDeviceText } from "./deviceKeyMap";
import { JpegRenderer } from "./jpegRenderer";
import { useDeviceVideo, type VideoMode } from "./useDeviceVideo";

interface Size {
  readonly width: number;
  readonly height: number;
}

/** A typical phone shape, until the device reports its own. */
const PLACEHOLDER_SIZE: Size = { width: 1080, height: 2340 };

const sameSize = (left: Size | null, right: Size | null) =>
  left === right ||
  (left !== null && right !== null && left.width === right.width && left.height === right.height);

/** The lowest touch id not in use, so the device sees small, reused finger ids. */
function freeTouchId(used: ReadonlyMap<number, number>): number {
  const taken = new Set(used.values());
  let id = 0;
  while (taken.has(id)) id += 1;
  return id;
}

const waitingForScreen = (
  <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
    <p className="text-xs text-muted-foreground">Waiting for the screen…</p>
  </div>
);

/** Why the screen shows still images instead of video, for the badge's tooltip. */
const JPEG_REASONS: Record<Extract<VideoMode, { kind: "jpeg" }>["reason"], string> = {
  unsupported:
    "Video isn't available in this browser or on this connection, so the screen updates as still images. The desktop app or an HTTPS connection usually shows video.",
  noFirstFrame:
    "Video did not start, so the screen updates as still images. Reopen the device panel to try video again.",
  decoderError:
    "Video could not be decoded, so the screen updates as still images. Reopen the device panel to try video again.",
  rejected:
    "The video connection kept failing, so the screen updates as still images. Reopen the device panel to try video again.",
};

/**
 * JPEG frames for the screen, mounted only in JPEG mode: the server captures them only while
 * someone subscribes. Each frame goes to the renderer outside React state.
 */
function JpegFeed({
  environmentId,
  deviceId,
  rendererRef,
  waiting,
  onFrameSize,
  onFirstFrame,
}: {
  readonly environmentId: EnvironmentId;
  readonly deviceId: string;
  readonly rendererRef: { readonly current: JpegRenderer | null };
  /** No frame has arrived yet. */
  readonly waiting: boolean;
  readonly onFrameSize: (size: Size) => void;
  readonly onFirstFrame: () => void;
}) {
  const [streamFailed, setStreamFailed] = useState(false);
  const streamFailedRef = useRef(false);
  const hasFrameRef = useRef(false);
  const framesAtom = deviceEnvironment.frames({ environmentId, input: { deviceId } });
  const refreshFrames = useAtomRefresh(framesAtom);
  const lastSeqRef = useRef<number | null>(null);
  const frameSizeRef = useRef<Size | null>(null);
  const onFrame = useCallback(
    (result: AsyncResult.AsyncResult<DeviceJpegFrame, unknown>) => {
      if (result._tag === "Failure") {
        streamFailedRef.current = true;
        setStreamFailed(true);
        return;
      }
      if (!AsyncResult.isSuccess(result)) return;
      const frame = result.value;
      if (frame.seq === lastSeqRef.current) return;
      lastSeqRef.current = frame.seq;
      // Only a new size or orientation re-renders; every other frame goes straight to the canvas.
      const size = displaySize(frame.width, frame.height, frame.orientation);
      if (!sameSize(frameSizeRef.current, size)) {
        frameSizeRef.current = size;
        onFrameSize(size);
      }
      if (!hasFrameRef.current) {
        hasFrameRef.current = true;
        onFirstFrame();
      }
      if (streamFailedRef.current) {
        streamFailedRef.current = false;
        setStreamFailed(false);
      }
      rendererRef.current?.push(frame.jpegBase64);
    },
    [onFirstFrame, onFrameSize, rendererRef],
  );
  useAtomSubscribe(framesAtom, onFrame, { immediate: true });

  const retryFrames = () => {
    streamFailedRef.current = false;
    setStreamFailed(false);
    refreshFrames();
  };

  if (!streamFailed) return waiting ? waitingForScreen : null;
  return (
    <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-4 text-center">
      <p className="text-xs text-foreground">The screen stopped updating.</p>
      <Button size="xs" variant="outline" onClick={retryFrames}>
        Reconnect screen
      </Button>
    </div>
  );
}

/**
 * The running device's screen, letterboxed on one canvas: H.264 video from a WebCodecs worker
 * where the client can decode it, else JPEG frames, drawn outside React state either way.
 * Pointer and keyboard input go to `onInput`, which queues it for the device. Held keys are
 * released when focus leaves the screen or the window, and everything held when it unmounts.
 */
export function DeviceScreen({
  environmentId,
  device,
  onInput,
  onRelease,
}: {
  readonly environmentId: EnvironmentId;
  readonly device: DeviceSnapshot;
  readonly onInput: (event: DeviceInputEvent) => void;
  /** Lifts held keys (`keys`), or held keys and touches (`all`), on the device. */
  readonly onRelease: (scope: "keys" | "all") => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // Also in state, so the video hook starts once the canvas exists.
  const [canvas, setCanvas] = useState<HTMLCanvasElement | null>(null);
  const attachCanvas = useCallback((node: HTMLCanvasElement | null) => {
    canvasRef.current = node;
    setCanvas(node);
  }, []);
  const [containerSize, setContainerSize] = useState<Size>({ width: 0, height: 0 });
  const [frameSize, setFrameSize] = useState<Size | null>(null);
  const [videoSize, setVideoSize] = useState<Size | null>(null);
  const [hasJpegFrame, setHasJpegFrame] = useState(false);
  // Browser pointer id → the touch id the device sees, for pointers held down on the screen.
  const touchesRef = useRef(new Map<number, number>());

  // Created in an effect, not during render, so a remount never draws with a disposed one.
  const rendererRef = useRef<JpegRenderer | null>(null);
  useEffect(() => {
    const renderer = new JpegRenderer((frame) => {
      const canvas = canvasRef.current;
      const context = canvas?.getContext("2d");
      if (!canvas || !context) return;
      context.drawImage(frame, 0, 0, canvas.width, canvas.height);
    });
    rendererRef.current = renderer;
    return () => {
      renderer.dispose();
      rendererRef.current = null;
    };
  }, []);

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (container === null) return;
    const measure = () => {
      const next = { width: container.clientWidth, height: container.clientHeight };
      setContainerSize((current) => (sameSize(current, next) ? current : next));
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const onVideoSize = useCallback((width: number, height: number) => {
    const next = { width, height };
    setVideoSize((current) => (sameSize(current, next) ? current : next));
  }, []);
  const video = useDeviceVideo({
    environmentId,
    deviceId: device.deviceId,
    canvas,
    onFrameSize: onVideoSize,
  });
  const jpeg = video.kind === "jpeg";
  const onJpegFrameSize = useCallback((size: Size) => setFrameSize(size), []);
  const onFirstJpegFrame = useCallback(() => setHasJpegFrame(true), []);

  const reported =
    device.width !== undefined && device.height !== undefined
      ? displaySize(device.width, device.height, device.orientation)
      : null;
  // Video and JPEG sizes both arrive oriented; neither is swapped again.
  const deviceSize = (jpeg ? frameSize : videoSize) ?? reported ?? PLACEHOLDER_SIZE;
  const box = useMemo(
    () => letterbox(containerSize.width, containerSize.height, deviceSize.width, deviceSize.height),
    [containerSize.width, containerSize.height, deviceSize.width, deviceSize.height],
  );

  // In JPEG mode the canvas backing store follows the box at the screen's pixel density;
  // resizing clears it, so the last frame is drawn again. Video sizes it to the picture.
  useLayoutEffect(() => {
    const canvas = canvasRef.current;
    if (canvas === null || !jpeg) return;
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.round(box.width * ratio));
    canvas.height = Math.max(1, Math.round(box.height * ratio));
    rendererRef.current?.redraw();
  }, [box.width, box.height, jpeg]);

  // A key released while another window has focus never reaches the canvas; lift it on blur.
  useEffect(() => {
    const releaseKeys = () => onRelease("keys");
    // A hidden page also ends drags: its pointer events may never arrive.
    const releaseWhenHidden = () => {
      if (!document.hidden) return;
      touchesRef.current.clear();
      onRelease("all");
    };
    window.addEventListener("blur", releaseKeys);
    document.addEventListener("visibilitychange", releaseWhenHidden);
    return () => {
      window.removeEventListener("blur", releaseKeys);
      document.removeEventListener("visibilitychange", releaseWhenHidden);
    };
  }, [onRelease]);
  // Unmounting mid-drag or with a key down must not leave it pressed on the device.
  useEffect(() => {
    const touches = touchesRef.current;
    return () => {
      touches.clear();
      onRelease("all");
    };
  }, [onRelease]);

  const queueTouch = useCallback(
    (action: "down" | "move" | "up" | "cancel", touchId: number, point: { x: number; y: number }) =>
      onInput({ type: "touch", action, pointerId: touchId, x: point.x, y: point.y }),
    [onInput],
  );
  const containerPoint = (event: ReactPointerEvent) => {
    const rect = containerRef.current?.getBoundingClientRect();
    return { x: event.clientX - (rect?.left ?? 0), y: event.clientY - (rect?.top ?? 0) };
  };

  const handlePointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    const local = containerPoint(event);
    const point = toNormalized(local.x, local.y, box);
    if (point === null) return;
    event.preventDefault();
    event.currentTarget.focus();
    event.currentTarget.setPointerCapture(event.pointerId);
    const touchId = freeTouchId(touchesRef.current);
    touchesRef.current.set(event.pointerId, touchId);
    queueTouch("down", touchId, point);
  };
  const handlePointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const touchId = touchesRef.current.get(event.pointerId);
    if (touchId === undefined) return;
    // A drag that leaves the screen stays on its edge, like a finger at the bezel.
    const local = containerPoint(event);
    queueTouch("move", touchId, clampToBox(local.x, local.y, box));
  };
  const endTouch = (event: ReactPointerEvent<HTMLCanvasElement>, action: "up" | "cancel") => {
    const touchId = touchesRef.current.get(event.pointerId);
    if (touchId === undefined) return;
    touchesRef.current.delete(event.pointerId);
    const local = containerPoint(event);
    queueTouch(action, touchId, clampToBox(local.x, local.y, box));
  };

  const handleKey = (event: ReactKeyboardEvent<HTMLCanvasElement>, action: "down" | "up") => {
    if (event.nativeEvent.isComposing) return;
    const input = keyboardEventToDeviceInput(event, action);
    if (input === null) return;
    event.preventDefault();
    onInput(input);
  };
  const handlePaste = (event: ReactClipboardEvent<HTMLCanvasElement>) => {
    const text = event.clipboardData.getData("text/plain");
    if (text.length === 0) return;
    event.preventDefault();
    for (const part of splitDeviceText(text)) onInput({ type: "text", text: part });
  };

  return (
    <div ref={containerRef} className="relative min-h-0 flex-1 overflow-hidden" data-device-screen>
      <canvas
        ref={attachCanvas}
        tabIndex={0}
        aria-label={`${device.avdName} screen. Click or drag to touch; type to send keys; Shift+Tab to leave.`}
        className="absolute touch-none rounded-2xl bg-black outline-none ring-4 ring-foreground/80 focus-visible:ring-primary"
        style={{ left: box.x, top: box.y, width: box.width, height: box.height }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={(event) => endTouch(event, "up")}
        onPointerCancel={(event) => endTouch(event, "cancel")}
        onLostPointerCapture={(event) => endTouch(event, "cancel")}
        onKeyDown={(event) => handleKey(event, "down")}
        onKeyUp={(event) => handleKey(event, "up")}
        onPaste={handlePaste}
        onBlur={() => onRelease("keys")}
        onContextMenu={(event) => event.preventDefault()}
      />
      {jpeg ? (
        <JpegFeed
          environmentId={environmentId}
          deviceId={device.deviceId}
          rendererRef={rendererRef}
          waiting={!hasJpegFrame}
          onFrameSize={onJpegFrameSize}
          onFirstFrame={onFirstJpegFrame}
        />
      ) : video.kind === "probing" ? (
        waitingForScreen
      ) : null}
      {video.kind === "jpeg" ? (
        <div className="absolute top-2 right-2">
          <Tooltip>
            <TooltipTrigger
              render={
                <Badge variant="outline" size="sm" tabIndex={0} data-video-fallback={video.reason}>
                  Low-bandwidth mode
                </Badge>
              }
            />
            <TooltipPopup>{JPEG_REASONS[video.reason]}</TooltipPopup>
          </Tooltip>
        </div>
      ) : null}
    </div>
  );
}
