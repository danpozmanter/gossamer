# Try Gossamer - playground component

A self-contained, dependency-light "Try Gossamer" web component: a CodeMirror 6
editor with Gossamer syntax highlighting, Run / Reset controls, and an output
pane that talks to a lazily-loaded runtime. It ships with a stub runtime so the
whole thing works in a browser today, and is designed to switch to the real
WebAssembly VM by changing a single option.

## Files

| File | Purpose |
| --- | --- |
| `playground.js` | The component. Exports `mountPlayground(el, opts)`. |
| `playground.css` | Refined-dark theme, scoped under `.gp`. |
| `gossamer-lang.js` | CodeMirror 6 `StreamLanguage` tokenizer + highlight style. |
| `runtime-stub.js` | Drop-in stub implementing the runtime contract. |
| `index.html` | Standalone demo / test page with three mounted examples. |
| `gossamer_playground.js` | (Not yet present.) The real wasm runtime, same exports. |

## Quick start

The component is an ES module and pulls CodeMirror from
[esm.sh](https://esm.sh), so it must be served over `http(s)` - a `file://`
open will not load the editor.

```sh
cd landing/playground
python3 -m http.server 8000
# open http://localhost:8000/
```

Embed it anywhere:

```html
<link rel="stylesheet" href="./playground/playground.css" />
<div id="demo"></div>
<script type="module">
  import { mountPlayground } from "./playground/playground.js";

  mountPlayground(document.getElementById("demo"), {
    source: 'fn main() {\n    println("hello")\n}\n',
    autorun: true,
    height: "240px",
  });
</script>
```

The component renders entirely inside the target element; only the
`playground.css` stylesheet is required alongside it. Provide the **Inter** and
**JetBrains Mono** web fonts on the host page for a pixel-faithful match (the CSS
falls back to system sans / mono otherwise).

## API: `mountPlayground(el, opts)`

Renders a playground into `el` (its contents are replaced) and returns a control
handle. Throws synchronously only if `el` is falsy.

### `opts`

| Option | Type | Default | Meaning |
| --- | --- | --- | --- |
| `source` | `string` | `""` | Initial Gossamer source shown in the editor and used by Reset. |
| `autorun` | `boolean` | `false` | Run once immediately after mount. |
| `height` | `string` | auto | Fixed editor height, e.g. `"320px"`. Omit to auto-size (capped, then scrolls). |
| `editable` | `boolean` | `true` | When `false`, the editor is read-only. |
| `runtimeUrl` | `string` | `"./runtime-stub.js"` | URL of the runtime module to lazy-load. Resolved relative to `playground.js`. |

### Return value (control handle)

| Member | Type | Description |
| --- | --- | --- |
| `run()` | `() => Promise<void>` | Trigger a run programmatically (same path as the Run button). |
| `reset()` | `() => void` | Restore the editor to `opts.source` and clear output. |
| `getSource()` | `() => string` | Current editor contents. |
| `destroy()` | `() => void` | Tear down the CodeMirror view and empty `el`. |
| `view` | `EditorView` | The underlying CodeMirror 6 view, for advanced use. |

### Behaviour notes

- **Lazy runtime load.** The runtime module is imported on the first Run (or on
  mount when `autorun` is set), not when the component mounts.
- **Per-page cache.** A successfully loaded runtime is cached per `runtimeUrl`
  for the page session, so multiple playgrounds and repeated runs share one
  instance. A failed load is evicted so a later Run can retry.
- **Errors are rendered, not thrown.** If the runtime module fails to import or
  initialise, the output pane shows `Failed to load runtime: ...`. If `run()`
  itself throws, the message is rendered as a runtime error. The component never
  rejects out of an event handler.
- **Keyboard.** Ctrl+Enter / Cmd+Enter runs. Tab indents inside the editor.
- **Output colours.** stdout is mono/default, stderr is amber (`#fbbf24`),
  errors are red, and an empty result shows `(no output)`.
- **Diagnostics.** If the runtime exposes `check()`, its diagnostics are listed
  under the output after each run. Diagnostics are best-effort and never fail a
  run.

## Runtime API contract

The component is decoupled from any particular runtime through a small ES-module
contract. Both `runtime-stub.js` and the future `gossamer_playground.js`
implement it identically.

```ts
// Default export: initialise the runtime (instantiate wasm, etc.).
// Called once per page session, awaited before the first run.
export default function init(wasmUrl?: string): Promise<void>;

// Execute Gossamer source. `fuel` optionally bounds execution.
export function run(
  source: string,
  fuel?: number,
): {
  stdout: string;      // program stdout
  stderr: string;      // program stderr
  error: string | null; // fatal/compile error message, or null
  fuel_used: number;   // execution budget consumed
};

// Optional: type-check without running. Drives the diagnostics list.
export function check(source: string): {
  diagnostics: Array<{
    severity: "error" | "warning";
    message: string;
    line?: number;
    col?: number;
    code?: string;
  }>;
};
```

Contract requirements:

- The **default export** is awaited once. It may be `async`; a synchronous
  function or a missing default is tolerated (the component only awaits it when
  it is a function).
- `run` is **required** and must be synchronous (return the result object
  directly, not a promise). A module without a `run` export is treated as a load
  failure.
- `check` is **optional**. When absent, the diagnostics list stays hidden.

## Swapping the stub for the real wasm module

When the real runtime is built, drop it next to these files as
`gossamer_playground.js` with the same exports (`init` default, `run`, `check`).
No code change is needed - just point `runtimeUrl` at it:

```js
mountPlayground(el, {
  source,
  runtimeUrl: "./gossamer_playground.js", // was the stub
});
```

`runtimeUrl` is resolved relative to `playground.js`, so a sibling filename like
`"./gossamer_playground.js"` works regardless of where the host page lives.

A typical `wasm-bindgen` / wasm build wires the contract as a thin wrapper:

```js
// gossamer_playground.js
import initWasm, { run as wasmRun, check as wasmCheck } from "./pkg/gossamer_wasm.js";

export default async function init(wasmUrl) {
  await initWasm(wasmUrl); // instantiate the module
}

export function run(source, fuel) {
  // Return { stdout, stderr, error, fuel_used } from the VM.
  return wasmRun(source, fuel ?? 0);
}

export function check(source) {
  return wasmCheck(source); // { diagnostics: [...] }
}
```

As long as the returned shapes match the contract above, the editor, controls,
output rendering, and diagnostics all keep working unchanged.

## Dependencies

CodeMirror 6, imported as ESM from esm.sh at load time (no build step, no
bundler):

- `codemirror` - `EditorView`, `basicSetup`
- `@codemirror/state` - `EditorState`
- `@codemirror/view` - `keymap`
- `@codemirror/commands` - `indentWithTab`
- `@codemirror/language` - `StreamLanguage`, `LanguageSupport`,
  `HighlightStyle`, `syntaxHighlighting`
- `@lezer/highlight` - `tags`

The Gossamer grammar in `gossamer-lang.js` is a pragmatic stream tokenizer
(comments, `#!` comments, keywords, types, strings, numbers, the `|>` pipe,
the `_` placeholder, and format macros), not a full parser - enough for
faithful highlighting in the editor.
