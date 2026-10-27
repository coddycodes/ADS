import { assert, describe } from "@effect/vitest";

import { createOxlintRuleHarness } from "../test/utils.ts";

const rule = createOxlintRuleHarness("coddy/no-effect-event-in-memo-or-forward-ref", {
  filename: "fixture.tsx",
});

const imports = 'import { memo, forwardRef, useEffectEvent } from "react";';
const component = "function Component({ value }) { useEffectEvent(() => value); return null; }";

describe("coddy/no-effect-event-in-memo-or-forward-ref", () => {
  for (const [name, source] of [
    ["an inline function", `export const View = memo(${component});`],
    [
      "an inline arrow",
      "export const View = memo(({ value }) => { useEffectEvent(() => value); return null; });",
    ],
    ["an expression-bodied arrow", "export const View = memo(() => useEffectEvent(() => 1));"],
    ["a function declaration", `${component} export const View = memo(Component);`],
    ["a hoisted function declaration", `export const View = memo(Component); ${component}`],
    [
      "a const arrow",
      "const Component = ({ value }) => { useEffectEvent(() => value); return null; }; export const View = memo(Component);",
    ],
    ["a const function", `const Render = ${component}; export const View = memo(Render);`],
    [
      "TypeScript wrappers and component aliases",
      `const Render = (${component} satisfies React.FC<Props>); const Alias = Render; export const View = memo((Alias as React.FC<Props>)!);`,
    ],
    ["undefined as comparator", `${component} export const View = memo(Component, undefined);`],
    ["null as comparator", `${component} export const View = memo(Component, null);`],
    ["void 0 as comparator", `${component} export const View = memo(Component, void 0);`],
    [
      "an undefined comparator alias",
      `${component} const absent = undefined; const compare = (absent as undefined); export const View = memo(Component, compare);`,
    ],
    [
      "a null comparator alias",
      `${component} const compare = null satisfies null; export const View = memo(Component, compare);`,
    ],
    [
      "a void comparator alias",
      `${component} const compare = void 0; export const View = memo(Component, compare);`,
    ],
    ["an inline forwardRef", `export const View = forwardRef(${component});`],
    ["a forwardRef declaration", `${component} export const View = forwardRef(Component);`],
    [
      "a forwardRef const arrow",
      "const Component = ({ value }) => { useEffectEvent(() => value); return null; }; export const View = forwardRef(Component);",
    ],
    [
      "memo around forwardRef with a comparator",
      `${component} export const View = memo(forwardRef(Component), () => false);`,
    ],
    [
      "memo around forwardRef without a comparator",
      `${component} export const View = memo(forwardRef(Component));`,
    ],
    [
      "an identifier-bound forwardRef with a comparator",
      `${component} const Forwarded = forwardRef(Component); export const View = memo(Forwarded, () => false);`,
    ],
  ] as const) {
    rule.invalid(`reports ${name}`, `${imports}\n${source}`, (output) => {
      assert.match(output, /keeps its first render's values on react-dom 19\.2/);
      assert.match(output, /read callbacks through a ref/);
      assert.equal(
        (output.match(/coddy-studio\(no-effect-event-in-memo-or-forward-ref\)/g) ?? []).length,
        1,
      );
    });
  }

  for (const [name, source] of [
    [
      "namespace memo",
      'import * as React from "react"; export const View = React.memo(() => { React.useEffectEvent(() => 1); return null; });',
    ],
    [
      "namespace forwardRef",
      'import * as React from "react"; export const View = React.forwardRef(() => { React.useEffectEvent(() => 1); return null; });',
    ],
    [
      "default memo",
      'import React from "react"; export const View = React.memo(() => { React.useEffectEvent(() => 1); return null; });',
    ],
    [
      "default forwardRef",
      'import React from "react"; export const View = React.forwardRef(() => { React.useEffectEvent(() => 1); return null; });',
    ],
    [
      "aliased exports",
      'import { memo as cache, forwardRef as forward, useEffectEvent as event } from "react"; export const View = cache(forward(() => { event(() => 1); return null; }), () => false);',
    ],
    [
      "destructured React exports",
      'import React from "react"; const { memo: cache, useEffectEvent: event } = React; export const View = cache(() => { event(() => 1); return null; });',
    ],
    [
      "destructured forwardRef",
      'import * as React from "react"; const { forwardRef: forward, useEffectEvent: event } = React; export const View = forward(() => { event(() => 1); return null; });',
    ],
    [
      "static members and aliases",
      'import React from "react"; const cache = React["memo"]; const event = React["useEffectEvent"]; export const View = cache(() => { event(() => 1); return null; });',
    ],
    [
      "CommonJS destructuring",
      'const { memo, useEffectEvent } = require("react"); export const View = memo(() => { useEffectEvent(() => 1); return null; });',
    ],
  ] as const) {
    rule.invalid(`resolves ${name}`, source);
  }

  rule.invalid(
    "reports a same-file hook at the component's call",
    `${imports}
      function useValue(value) { return useEffectEvent(() => value); }
      export const View = memo(({ value }) => { useValue(value); return null; });
    `,
    (output) => assert.match(output, /An Effect Event reached through useValue/),
  );

  rule.invalid(
    "follows two levels of hooks including const arrows and TS wrappers",
    `${imports}
      const useInner = ((value) => useEffectEvent(() => value)) as Hook;
      function useOuter(value) { return useInner(value); }
      export const View = forwardRef(({ value }) => { useOuter(value); return null; });
    `,
    (output) => assert.match(output, /An Effect Event reached through useOuter/),
  );

  rule.invalid(
    "finds Effect Events beyond a hook cycle from either entry point",
    `${imports}
      function useFirst() { useSecond(); useEffectEvent(() => 1); }
      function useSecond() { useFirst(); }
      export const First = memo(() => { useFirst(); return null; });
      export const Second = memo(() => { useSecond(); return null; });
    `,
    (output) => {
      assert.match(output, /An Effect Event reached through useFirst/);
      assert.match(output, /An Effect Event reached through useSecond/);
    },
  );

  rule.invalid(
    "reports the trimmed d932d861 pre-fix ComposerPendingUserInputCard",
    `import { memo, useEffect, useEffectEvent, useRef, useState } from "react";
      const ComposerPendingUserInputCard = memo(function ComposerPendingUserInputCard({
        prompt, answers, questionIndex, onToggleOption, onAdvance,
      }) {
        const progress = derivePendingUserInputProgress(prompt.questions, answers, questionIndex);
        const activeQuestion = progress.activeQuestion;
        const autoAdvanceTimerRef = useRef<number | null>(null);
        const onAdvanceRef = useRef(onAdvance);
        const [optimisticSingleSelect, setOptimisticSingleSelect] = useState(null);
        useEffect(() => { onAdvanceRef.current = onAdvance; }, [onAdvance]);
        const handleOptionSelection = useEffectEvent((questionId: string, optionLabel: string) => {
          if (activeQuestion?.multiSelect) {
            onToggleOption(questionId, optionLabel);
            return;
          }
          setOptimisticSingleSelect({ questionId, optionLabel });
          onToggleOption(questionId, optionLabel);
          if (autoAdvanceTimerRef.current !== null) {
            window.clearTimeout(autoAdvanceTimerRef.current);
          }
          autoAdvanceTimerRef.current = window.setTimeout(() => {
            autoAdvanceTimerRef.current = null;
            onAdvanceRef.current();
          }, 200);
        });
        return <button onClick={() => handleOptionSelection(activeQuestion.id, "A")}>A</button>;
      });
      export { ComposerPendingUserInputCard };
    `,
  );

  for (const [name, source] of [
    ["plain function components", `export ${component}`],
    [
      "a modified comparator binding",
      `${component} let compare = null; compare = (a, b) => a.value === b.value; export const View = memo(Component, compare);`,
    ],
    [
      "cyclic comparator aliases on a local component",
      `${component} const first = second; const second = first; export const View = memo(Component, first);`,
    ],
    [
      "memo with a comparator",
      `${component} export const View = memo(Component, (a, b) => a.value === b.value);`,
    ],
    [
      "a named comparator",
      `${component} function compare(a, b) { return a.value === b.value; } export const View = memo(Component, compare);`,
    ],
    ["any other comparator argument", `${component} export const View = memo(Component, false);`],
    [
      "shadowed undefined",
      `${component} function wrap(undefined) { return memo(Component, undefined); } export { wrap };`,
    ],
    [
      "a hook in a plain component",
      "function useValue() { return useEffectEvent(() => 1); } export function View() { useValue(); return null; }",
    ],
    [
      "a hook in memo with a comparator",
      "function useValue() { return useEffectEvent(() => 1); } export const View = memo(() => { useValue(); return null; }, () => false);",
    ],
    [
      "a nested JSX child with its own Effect Event",
      `export const View = memo(() => { ${component} return <Component value={1} />; });`,
    ],
    [
      "a separately declared plain JSX child",
      `${component} export const View = memo(() => <Component value={1} />);`,
    ],
    [
      "nested callbacks and functions",
      "export const View = memo(() => { const callback = () => useEffectEvent(() => 1); function nested() { useEffectEvent(() => 2); } return <button onClick={callback} onBlur={nested} />; });",
    ],
    [
      "a hook referenced only inside a callback",
      "function useValue() { return useEffectEvent(() => 1); } export const View = memo(() => { const callback = () => useValue(); return <button onClick={callback} />; });",
    ],
    [
      "a hook whose Effect Event is only in a nested function",
      "function useValue() { return () => useEffectEvent(() => 1); } export const View = memo(() => { useValue(); return null; });",
    ],
    [
      "a hook cycle with no Effect Event",
      "function useFirst() { useSecond(); } function useSecond() { useFirst(); } export const View = memo(() => { useFirst(); return null; });",
    ],
    [
      "shadowed local memo",
      "function wrap(memo) { return memo(() => { useEffectEvent(() => 1); return null; }); } export { wrap };",
    ],
    [
      "shadowed local forwardRef",
      "function wrap(forwardRef) { return forwardRef(() => { useEffectEvent(() => 1); return null; }); } export { wrap };",
    ],
    [
      "shadowed local useEffectEvent",
      "export const View = memo(({ useEffectEvent }) => { useEffectEvent(() => 1); return null; });",
    ],
    [
      "a shadowed same-file hook",
      "function useValue() { return useEffectEvent(() => 1); } export const View = memo(({ useValue }) => { useValue(); return null; });",
    ],
    [
      "imported components",
      'import Component from "./component"; export const View = memo(Component);',
    ],
    [
      "imported hooks",
      'import { useValue } from "./hooks"; export const View = memo(() => { useValue(); return null; });',
    ],
    [
      "lazy targets",
      'import { lazy } from "react"; export const View = memo(lazy(() => import("./component")));',
    ],
    [
      "cyclic component and comparator aliases",
      "const First = Second; const Second = First; export const View = memo(First, Second);",
    ],
  ] as const) {
    rule.valid(`allows ${name}`, `${imports}\n${source}`);
  }

  rule.valid(
    "does not confuse non-React exports with React",
    'import { memo, forwardRef, useEffectEvent } from "other"; export const View = memo(forwardRef(() => { useEffectEvent(() => 1); return null; }));',
  );
});
