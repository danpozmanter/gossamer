// Stub Gossamer runtime - implements the same ES-module contract the
// real WebAssembly VM will expose, so the playground component is fully
// testable in a browser without the wasm build.
//
// Contract (the real module at ./gossamer_playground.js must match):
//   default export  init(wasmUrl?) -> Promise<void>
//   run(source, fuel?) -> { stdout, stderr, error, fuel_used }
//   check(source)      -> { diagnostics: [{ severity, message, line, col, code }] }

/// Initialize the runtime. The stub has nothing to load, so this
/// resolves immediately; the real module loads/instantiates the wasm.
export default async function init(_wasmUrl) {
  return undefined;
}

/// Execute Gossamer source. The stub returns a clearly-marked
/// placeholder rather than evaluating anything.
export function run(_source, _fuel) {
  return {
    stdout: "(stub runtime - real WebAssembly VM not yet wired)\n",
    stderr: "",
    error: null,
    fuel_used: 0,
  };
}

/// Type-check Gossamer source and return diagnostics. The stub never
/// reports any.
export function check(_source) {
  return { diagnostics: [] };
}
