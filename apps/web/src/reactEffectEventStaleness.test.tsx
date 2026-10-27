// @vitest-environment happy-dom

import { act, forwardRef, memo, useEffect, useEffectEvent, useRef } from "react";
import { version } from "react-dom";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vite-plus/test";

interface ProbeProps {
  readonly value: string;
  readonly subscribe: (event: () => string) => void;
}

function PlainProbe({ value, subscribe }: ProbeProps) {
  const subscribeOnMount = useRef(subscribe);
  const readValue = useEffectEvent(() => value);
  useEffect(() => {
    subscribeOnMount.current(readValue);
  }, []);
  return <output>{value}</output>;
}

const compare = (previous: ProbeProps, next: ProbeProps) =>
  previous.value === next.value && previous.subscribe === next.subscribe;

const ComparedMemoProbe = memo(function ComparedMemoProbe({ value, subscribe }: ProbeProps) {
  const subscribeOnMount = useRef(subscribe);
  const readValue = useEffectEvent(() => value);
  useEffect(() => {
    subscribeOnMount.current(readValue);
  }, []);
  return <output>{value}</output>;
}, compare);

const MemoProbe = memo(function MemoProbe({ value, subscribe }: ProbeProps) {
  const subscribeOnMount = useRef(subscribe);
  // oxlint-disable-next-line coddy/no-effect-event-in-memo-or-forward-ref -- Pins the SimpleMemoComponent defect.
  const readValue = useEffectEvent(() => value);
  useEffect(() => {
    subscribeOnMount.current(readValue);
  }, []);
  return <output>{value}</output>;
});

const ForwardRefProbe = forwardRef<HTMLOutputElement, ProbeProps>(function ForwardRefProbe(
  { value, subscribe },
  ref,
) {
  const subscribeOnMount = useRef(subscribe);
  // oxlint-disable-next-line coddy/no-effect-event-in-memo-or-forward-ref -- Pins the ForwardRef defect.
  const readValue = useEffectEvent(() => value);
  useEffect(() => {
    subscribeOnMount.current(readValue);
  }, []);
  return <output ref={ref}>{value}</output>;
});

const ComparedForwardRefProbe = memo(
  forwardRef<HTMLOutputElement, ProbeProps>(function ComparedForwardRefProbe(
    { value, subscribe },
    ref,
  ) {
    const subscribeOnMount = useRef(subscribe);
    // oxlint-disable-next-line coddy/no-effect-event-in-memo-or-forward-ref -- A comparator does not repair ForwardRef.
    const readValue = useEffectEvent(() => value);
    useEffect(() => {
      subscribeOnMount.current(readValue);
    }, []);
    return <output ref={ref}>{value}</output>;
  }),
  compare,
);

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe(`Effect Event subscriptions on react-dom ${version}`, () => {
  it.each([
    { form: "plain function", Component: PlainProbe, retainedValue: "second" },
    { form: "memo(C, compare)", Component: ComparedMemoProbe, retainedValue: "second" },
    { form: "memo(C)", Component: MemoProbe, retainedValue: "first" },
    { form: "forwardRef(C)", Component: ForwardRefProbe, retainedValue: "first" },
    {
      form: "memo(forwardRef(C), compare)",
      Component: ComparedForwardRefProbe,
      retainedValue: "first",
    },
  ])(
    "$form retains $retainedValue after a committed update",
    async ({ form, Component, retainedValue }) => {
      const subscriptions: Array<() => string> = [];
      const subscribe = (event: () => string) => {
        subscriptions.push(event);
      };

      await act(async () => root.render(<Component value="first" subscribe={subscribe} />));
      expect(container.textContent).toBe("first");
      expect(subscriptions).toHaveLength(1);
      const originalEvent = subscriptions[0]!;
      expect(originalEvent()).toBe("first");

      await act(async () => root.render(<Component value="second" subscribe={subscribe} />));
      expect(container.textContent, `${form} must commit the changed value`).toBe("second");
      expect(
        subscriptions,
        "The mount-only effect must retain the original subscription",
      ).toHaveLength(1);
      expect(subscriptions[0]).toBe(originalEvent);
      expect(
        originalEvent(),
        `react-dom ${version} still keeps stale Effect Events in memo(C), forwardRef(C), and ` +
          `memo(forwardRef(C), compare); ${form} should return ${retainedValue}. ` +
          "When a prohibited form returns the new value after an upgrade, re-check every form " +
          "and remove coddy/no-effect-event-in-memo-or-forward-ref only once all of them recover.",
      ).toBe(retainedValue);
    },
  );
});
