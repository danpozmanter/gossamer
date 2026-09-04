// Runs Gossamer programs through the browser playground's wasm build and
// reports any that end the module rather than answering.
//
// wasm32-unknown-unknown has no unwinder, so a Rust panic anywhere under
// `run` aborts the module: the call throws `RuntimeError: unreachable`
// and the program gets no output at all. Every other outcome - a
// front-end rejection, a runtime error, an exit status - comes back
// inside the result, so a throw is the whole failure condition here.
//
// Usage: node scripts/playground_smoke.js <bindgen-dir> <file.gos>...
"use strict";

const fs = require("fs");
const path = require("path");

const [bindgenDir, ...files] = process.argv.slice(2);
if (!bindgenDir || files.length === 0) {
  console.error("usage: node playground_smoke.js <bindgen-dir> <file.gos>...");
  process.exit(2);
}

const pg = require(path.resolve(bindgenDir, "gossamer_playground.js"));

let trapped = 0;
for (const file of files) {
  const source = fs.readFileSync(file, "utf8");
  try {
    pg.run(source, undefined);
    console.log("ok   " + file);
  } catch (err) {
    trapped += 1;
    const panic = typeof pg.last_panic === "function" ? pg.last_panic() : "";
    const reason = panic || (err && err.message) || String(err);
    console.log("TRAP " + file + ": " + reason.split("\n")[0]);
  }
}
console.log(`${files.length} program(s), ${trapped} trapped`);
process.exit(trapped === 0 ? 0 : 1);
