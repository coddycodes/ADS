export type DesktopUiKeyboardKey = "ArrowDown" | "ArrowUp" | "Enter" | "Escape" | "Tab";

export interface DesktopUiFocusedElement {
  readonly activityRow: string | null;
  readonly activitySection: string | null;
  readonly ariaLabel: string | null;
  readonly tagName: string | null;
  readonly testId: string | null;
}

interface DesktopUiDriverKeyEvidence {
  readonly click: number;
  readonly focusChanged: boolean;
  readonly keydown: number;
  readonly keyup: number;
}

interface DesktopUiSyntheticKeyEvidence {
  readonly click: number;
  readonly defaultPrevented: boolean;
  readonly keydown: number;
  readonly keyup: number;
}

export interface DesktopUiKeyboardResult {
  readonly transport: "webdriver" | "synthetic";
  readonly before: DesktopUiFocusedElement;
  readonly after: DesktopUiFocusedElement;
  readonly driver: DesktopUiDriverKeyEvidence;
  readonly synthetic: DesktopUiSyntheticKeyEvidence | null;
  readonly tabOrder?: {
    readonly candidateCount: number;
    readonly currentIndex: number;
    readonly nextAriaLabel: string | null;
    readonly nextTagName: string | null;
  };
}

interface DesktopUiKeyCapture {
  click: number;
  initialTarget: HTMLElement;
  listenerTarget: EventTarget;
  key: string;
  keydown: number;
  keyup: number;
  onClick: (event: Event) => void;
  onKeyDown: (event: Event) => void;
  onKeyUp: (event: Event) => void;
}

interface DesktopUiKeyboardOwnership {
  readonly id: string;
  readonly target: HTMLElement;
  restoreFocus: boolean;
  probe: HTMLElement | null;
  capture: DesktopUiKeyCapture | null;
  synthetic: {
    readonly target: HTMLElement;
    readonly onClick: () => void;
  } | null;
}

declare global {
  interface Window {
    __coddyStudioDesktopUiKeyboardOwnership?: DesktopUiKeyboardOwnership;
  }
}

let desktopUiKeyboardOwnershipSequence = 0;

async function cleanupDesktopUiKeyboardOwnership(
  ownershipId: string,
  operationSucceeded: boolean,
): Promise<void> {
  await browser.execute(
    (expectedOwnershipId: string, completed: boolean) => {
      const ownership = window.__coddyStudioDesktopUiKeyboardOwnership;
      if (!ownership || ownership.id !== expectedOwnershipId) return;
      if (completed) {
        ownership.restoreFocus = false;
      }
      const capture = ownership.capture;
      if (capture) {
        for (const [type, listener] of [
          ["keydown", capture.onKeyDown],
          ["keyup", capture.onKeyUp],
          ["click", capture.onClick],
        ] as const) {
          try {
            capture.listenerTarget.removeEventListener(type, listener, true);
          } catch {
            // Cleanup is best-effort per listener so one hostile target cannot strand the rest.
          }
        }
      }
      const synthetic = ownership.synthetic;
      if (synthetic) {
        try {
          synthetic.target.removeEventListener("click", synthetic.onClick);
        } catch {
          // The ownership record still must be released if a hostile target rejects removal.
        }
      }
      try {
        ownership.probe?.remove();
      } catch {
        // Detached/hostile probe nodes must not prevent global ownership release.
      }
      if (ownership.restoreFocus && ownership.target.isConnected) {
        try {
          ownership.target.focus();
        } catch {
          // Focus restoration failure must not strand temporary ownership globals.
        }
      }
      delete window.__coddyStudioDesktopUiKeyboardOwnership;
    },
    ownershipId,
    operationSucceeded,
  );
}

/** Returns the stable subset of active-element state used by desktop UI acceptance tests. */
export async function describeDesktopUiFocus(): Promise<DesktopUiFocusedElement> {
  return browser.execute(() => {
    const active = document.activeElement;
    if (!(active instanceof HTMLElement)) {
      return {
        activityRow: null,
        activitySection: null,
        ariaLabel: null,
        tagName: null,
        testId: null,
      };
    }
    return {
      activityRow: active.dataset.activityRow ?? null,
      activitySection: active.dataset.activitySection ?? null,
      ariaLabel: active.getAttribute("aria-label"),
      tagName: active.tagName,
      testId: active.dataset.testid ?? null,
    };
  });
}

/** Focuses a real production element without activating it, then describes activeElement. */
export async function focusDesktopUiElement(selector: string): Promise<DesktopUiFocusedElement> {
  await browser.execute((targetSelector: string) => {
    const target = document.querySelector<HTMLElement>(targetSelector);
    if (!target) throw new Error(`Cannot focus missing element: ${targetSelector}`);
    target.focus();
    if (document.activeElement !== target) {
      throw new Error(`The target did not receive focus: ${targetSelector}`);
    }
  }, selector);
  return describeDesktopUiFocus();
}

/**
 * Sends a key to the production element that already owns DOM focus.
 *
 * The helper records WebDriver key/click/focus evidence first. WebKit can emit
 * key events without performing Tab navigation or native Enter activation, so
 * the fallback runs only when the requested semantic result is absent. Enter's
 * native Button activation is emulated exactly once, and only when the
 * dispatched keydown was not prevented.
 */
export async function sendFocusedKeyboardKey(
  key: DesktopUiKeyboardKey,
  shift = false,
): Promise<DesktopUiKeyboardResult> {
  const before = await describeDesktopUiFocus();
  const ownershipId = `desktop-ui-keyboard-${desktopUiKeyboardOwnershipSequence++}`;
  let operationSucceeded = false;
  try {
    await browser.execute(
      (keyboardKey: DesktopUiKeyboardKey, nextOwnershipId: string) => {
        const target = document.activeElement;
        if (!(target instanceof HTMLElement)) {
          throw new Error("Desktop UI keyboard input requires a focused HTMLElement.");
        }
        const ownership: DesktopUiKeyboardOwnership = {
          id: nextOwnershipId,
          target,
          restoreFocus: true,
          probe: null,
          capture: null,
          synthetic: null,
        };
        window.__coddyStudioDesktopUiKeyboardOwnership = ownership;

        const probe = document.createElement("div");
        ownership.probe = probe;
        probe.dataset.coddyStudioKeyboardProbe = keyboardKey;
        probe.style.cssText =
          "position:fixed;left:-10000px;top:-10000px;width:1px;height:1px;overflow:hidden";
        const first = document.createElement("button");
        const second = document.createElement("button");
        first.type = "button";
        second.type = "button";
        probe.append(first, second);
        document.body.append(probe);

        const capture: DesktopUiKeyCapture = {
          click: 0,
          initialTarget: first,
          listenerTarget: window,
          key: keyboardKey,
          keydown: 0,
          keyup: 0,
          onClick: (event) => {
            if (event.target === first) {
              capture.click += 1;
              event.stopPropagation();
            }
          },
          onKeyDown: (event) => {
            if (
              event.target === first &&
              event instanceof KeyboardEvent &&
              event.key === capture.key
            ) {
              capture.keydown += 1;
              event.stopPropagation();
            }
          },
          onKeyUp: (event) => {
            if (
              event.target === first &&
              event instanceof KeyboardEvent &&
              event.key === capture.key
            ) {
              capture.keyup += 1;
              event.stopPropagation();
            }
          },
        };
        ownership.capture = capture;
        window.addEventListener("keydown", capture.onKeyDown, true);
        window.addEventListener("keyup", capture.onKeyUp, true);
        window.addEventListener("click", capture.onClick, true);
        first.focus();
      },
      key,
      ownershipId,
    );
    await browser.keys(shift ? ["Shift", key] : key);

    const probeSupported = await browser.execute(
      (keyboardKey: DesktopUiKeyboardKey, expectedOwnershipId: string) => {
        const ownership = window.__coddyStudioDesktopUiKeyboardOwnership;
        if (!ownership || ownership.id !== expectedOwnershipId || !ownership.capture) {
          throw new Error("Desktop UI keyboard capability probe lost its ownership.");
        }
        const capture = ownership.capture;
        const focusChanged = capture.initialTarget !== document.activeElement;
        const supported =
          keyboardKey === "Tab"
            ? focusChanged
            : keyboardKey === "Enter"
              ? capture.keydown === 1 && capture.keyup === 1 && capture.click === 1
              : capture.keydown === 1 && capture.keyup === 1;
        capture.listenerTarget.removeEventListener("keydown", capture.onKeyDown, true);
        capture.listenerTarget.removeEventListener("keyup", capture.onKeyUp, true);
        capture.listenerTarget.removeEventListener("click", capture.onClick, true);
        ownership.capture = null;
        ownership.probe?.remove();
        ownership.probe = null;
        ownership.target.focus();
        return supported;
      },
      key,
      ownershipId,
    );

    if (probeSupported) {
      await browser.execute(
        (keyboardKey: DesktopUiKeyboardKey, expectedOwnershipId: string) => {
          const ownership = window.__coddyStudioDesktopUiKeyboardOwnership;
          if (!ownership || ownership.id !== expectedOwnershipId) {
            throw new Error("Desktop UI keyboard target was lost after its capability probe.");
          }
          const capture: DesktopUiKeyCapture = {
            click: 0,
            initialTarget: ownership.target,
            listenerTarget: ownership.target,
            key: keyboardKey,
            keydown: 0,
            keyup: 0,
            onClick: () => {
              capture.click += 1;
            },
            onKeyDown: (event) => {
              if (event instanceof KeyboardEvent && event.key === keyboardKey) {
                capture.keydown += 1;
              }
            },
            onKeyUp: (event) => {
              if (event instanceof KeyboardEvent && event.key === keyboardKey) {
                capture.keyup += 1;
              }
            },
          };
          ownership.capture = capture;
          ownership.target.addEventListener("keydown", capture.onKeyDown, true);
          ownership.target.addEventListener("keyup", capture.onKeyUp, true);
          ownership.target.addEventListener("click", capture.onClick, true);
          ownership.target.focus();
        },
        key,
        ownershipId,
      );
      await browser.keys(shift ? ["Shift", key] : key);
      const driverEvidence = await browser.execute(
        (expectedOwnershipId: string): DesktopUiDriverKeyEvidence => {
          const ownership = window.__coddyStudioDesktopUiKeyboardOwnership;
          if (!ownership || ownership.id !== expectedOwnershipId || !ownership.capture) {
            throw new Error("Desktop UI keyboard evidence lost its ownership.");
          }
          return {
            click: ownership.capture.click,
            focusChanged: ownership.capture.initialTarget !== document.activeElement,
            keydown: ownership.capture.keydown,
            keyup: ownership.capture.keyup,
          };
        },
        ownershipId,
      );
      const result: DesktopUiKeyboardResult = {
        transport: "webdriver",
        before,
        after: await describeDesktopUiFocus(),
        driver: driverEvidence,
        synthetic: null,
      };
      operationSucceeded = true;
      return result;
    }

    const syntheticState = await browser.execute(
      (keyboardKey: DesktopUiKeyboardKey, shiftKey: boolean, expectedOwnershipId: string) => {
        const ownership = window.__coddyStudioDesktopUiKeyboardOwnership;
        if (!ownership || ownership.id !== expectedOwnershipId) {
          throw new Error("Desktop UI keyboard fallback lost its ownership.");
        }
        const initialTarget = ownership.target;
        let click = 0;
        const onClick = () => {
          click += 1;
        };
        ownership.synthetic = { target: initialTarget, onClick };
        initialTarget.addEventListener("click", onClick);
        const init: KeyboardEventInit = {
          bubbles: true,
          cancelable: true,
          key: keyboardKey,
          shiftKey,
        };
        const keyDown = new KeyboardEvent("keydown", init);
        initialTarget.dispatchEvent(keyDown);

        let tabOrder:
          | {
              readonly candidateCount: number;
              readonly currentIndex: number;
              readonly nextAriaLabel: string | null;
              readonly nextTagName: string | null;
            }
          | undefined;
        if (!keyDown.defaultPrevented && keyboardKey === "Tab") {
          const candidates = [
            ...document.querySelectorAll<HTMLElement>(
              'a[href], button, input, select, textarea, [tabindex], [contenteditable]:not([contenteditable="false"])',
            ),
          ].filter((candidate) => {
            const isImplicitContentEditable =
              candidate.matches('[contenteditable]:not([contenteditable="false"])') &&
              !candidate.hasAttribute("tabindex");
            if (
              (candidate.tabIndex < 0 && !isImplicitContentEditable) ||
              candidate.matches(":disabled") ||
              candidate.hidden ||
              candidate.closest("[inert], [aria-hidden='true']")
            ) {
              return false;
            }
            const style = getComputedStyle(candidate);
            const rectangle = candidate.getBoundingClientRect();
            return (
              rectangle.width > 0 &&
              rectangle.height > 0 &&
              style.display !== "none" &&
              style.visibility !== "hidden"
            );
          });
          const ordered = candidates
            .map((candidate, index) => ({
              candidate,
              index,
              tabIndex:
                candidate.matches('[contenteditable]:not([contenteditable="false"])') &&
                !candidate.hasAttribute("tabindex")
                  ? 0
                  : candidate.tabIndex,
            }))
            .sort((left, right) => {
              const leftOrder = left.tabIndex > 0 ? left.tabIndex : Number.MAX_SAFE_INTEGER;
              const rightOrder = right.tabIndex > 0 ? right.tabIndex : Number.MAX_SAFE_INTEGER;
              return leftOrder - rightOrder || left.index - right.index;
            })
            .map(({ candidate }) => candidate);
          const currentIndex = ordered.indexOf(initialTarget);
          const offset = shiftKey ? -1 : 1;
          const documentRelativeCandidate =
            currentIndex >= 0
              ? undefined
              : shiftKey
                ? candidates
                    .toReversed()
                    .find(
                      (candidate) =>
                        (candidate.compareDocumentPosition(initialTarget) &
                          Node.DOCUMENT_POSITION_FOLLOWING) !==
                        0,
                    )
                : candidates.find(
                    (candidate) =>
                      (initialTarget.compareDocumentPosition(candidate) &
                        Node.DOCUMENT_POSITION_FOLLOWING) !==
                      0,
                  );
          const nextIndex =
            currentIndex >= 0
              ? (currentIndex + offset + ordered.length) % ordered.length
              : documentRelativeCandidate
                ? ordered.indexOf(documentRelativeCandidate)
                : shiftKey
                  ? ordered.length - 1
                  : 0;
          const next = ordered[nextIndex];
          tabOrder = {
            candidateCount: ordered.length,
            currentIndex,
            nextAriaLabel: next?.getAttribute("aria-label") ?? null,
            nextTagName: next?.tagName ?? null,
          };
          next?.focus();
        }

        if (
          !keyDown.defaultPrevented &&
          keyboardKey === "Enter" &&
          initialTarget instanceof HTMLButtonElement &&
          !initialTarget.disabled
        ) {
          initialTarget.click();
        }
        const keyUp = new KeyboardEvent("keyup", init);
        initialTarget.dispatchEvent(keyUp);
        return {
          evidence: {
            click,
            defaultPrevented: keyDown.defaultPrevented,
            keydown: 1,
            keyup: 1,
          } satisfies DesktopUiSyntheticKeyEvidence,
          tabOrder,
        };
      },
      key,
      shift,
      ownershipId,
    );
    const after = await describeDesktopUiFocus();
    const result: DesktopUiKeyboardResult = {
      transport: "synthetic",
      before,
      after,
      driver: { click: 0, focusChanged: false, keydown: 0, keyup: 0 },
      synthetic: syntheticState.evidence,
      ...(syntheticState.tabOrder === undefined ? {} : { tabOrder: syntheticState.tabOrder }),
    };
    operationSucceeded = true;
    return result;
  } finally {
    await cleanupDesktopUiKeyboardOwnership(ownershipId, operationSucceeded);
  }
}
