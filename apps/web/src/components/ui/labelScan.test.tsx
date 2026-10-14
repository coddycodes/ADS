// @vitest-environment happy-dom

import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { Checkbox } from "./checkbox";
import { Radio, RadioGroup } from "./radio-group";
import { Switch } from "./switch";

/**
 * Counts reads of `labels` on inputs. WebKit rebuilds that live list by walking the whole
 * document whenever the DOM changed, and Base UI's label fallback read it after every commit
 * of every switch, checkbox and radio: with the status bar committing every 2 s and a long
 * thread loaded, the macOS UI spent most of its time there (#106).
 */
function countLabelReads(): { reads: () => number; restore: () => void } {
  const descriptor = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "labels");
  let reads = 0;
  Object.defineProperty(HTMLInputElement.prototype, "labels", {
    configurable: true,
    get(this: HTMLInputElement) {
      reads += 1;
      return descriptor?.get?.call(this) ?? null;
    },
  });
  return {
    reads: () => reads,
    restore: () => {
      if (descriptor) Object.defineProperty(HTMLInputElement.prototype, "labels", descriptor);
      else Reflect.deleteProperty(HTMLInputElement.prototype, "labels");
    },
  };
}

let cleanup: (() => void) | null = null;
afterEach(() => {
  cleanup?.();
  cleanup = null;
});

function Controls({ tick }: { tick: number }) {
  return (
    <div data-tick={tick}>
      <Switch aria-label="Profiler" />
      <Checkbox aria-label="Include archived" />
      <RadioGroup aria-label="Theme" defaultValue="dark">
        <Radio aria-label="Dark" value="dark" />
        <Radio aria-label="Light" value="light" />
      </RadioGroup>
      {/* Labelled by a wrapping <label>, as most settings rows are. */}
      <label>
        <Switch />
        Create more
      </label>
      <label>
        <Checkbox />
        Signed-off-by
      </label>
    </div>
  );
}

describe("labelled form controls", () => {
  it("do not scan for an associated label on every commit", async () => {
    const counter = countLabelReads();
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    cleanup = () => {
      act(() => root.unmount());
      container.remove();
      counter.restore();
    };
    let rerender: (() => void) | null = null;
    function Harness() {
      const [tick, setTick] = useState(0);
      rerender = () => setTick((value) => value + 1);
      return <Controls tick={tick} />;
    }
    await act(async () => root.render(<Harness />));
    const afterMount = counter.reads();
    // Like status-bar polls committing over a mounted settings panel.
    for (let commit = 0; commit < 20; commit += 1) {
      await act(async () => rerender?.());
    }
    expect(counter.reads() - afterMount).toBe(0);
  });
});
