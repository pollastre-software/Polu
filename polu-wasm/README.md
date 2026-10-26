# polu-wasm

polu-core for the browser. It builds polu-core without HiGHS (`default-features = false`, plus
`csv`, `json` and `xlsx`) and solves with a JS LP solver instead: `JsSolver` hands the problem to a
JS function as CPLEX LP text (`Problem::to_lp`) and reads the result back. The demo passes
[highs-js](https://github.com/lovasoa/highs-js) (MIT), which compiles the official HiGHS to
WebAssembly; HiGHS has no JS build of its own.

`demo/` is a static site, plain HTML, CSS and ES modules with no bundler and no Node.

## Setup

One time:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack      # or: brew install wasm-pack
```

## Build and run the demo

```sh
sh polu-wasm/demo/build.sh                        # into target/demo-site
python3 -m http.server -d target/demo-site 8000   # or any static file server
```

Then open http://localhost:8000. `build.sh` copies the page, builds polu-wasm into `pkg/`, puts
highs-js in `vendor/` (downloaded once from npm with curl, checksum-pinned) and copies every
scenario from `demo-scenarios/` into `scenarios/`. Rerun it after any change and reload; Rust
rebuilds are incremental. Browsers won't load `.wasm` or ES modules from `file://`, so the folder
has to be served, by anything.

## Scenarios

`demo-scenarios/` at the repository root holds one folder per scenario: its sources, source caps,
sinks and costmap layers, plus one `*.polu.json` scenario file in the TUI's format (paths relative to it), so
the TUI opens the same scenarios. The page lists every folder that has a scenario file; to add one,
add a folder. `fruit` is the apples example.

## GitHub Pages

`.github/workflows/pages.yml` publishes the demo as the repository's github.io page on every push
to `main` that touches the core, polu-wasm or `demo-scenarios/` (or by hand from the Actions tab).
It runs the polu-core tests that don't need HiGHS, then `build.sh`, and deploys its output.

One-time setup: in the repository's Settings > Pages, set Source to "GitHub Actions". The site is
then at `https://<user>.github.io/<repo>/`; every path in the page is relative, so the subpath works.

Any other static host works the same way: copy `target/demo-site/` and serve `.wasm` as
`application/wasm`.

## Using it from JS

```js
import init, { Model, columnNames, sheetNames } from "./pkg/polu_wasm.js"; // paths as build.sh lays them out
import highsLoader from "./vendor/highs.mjs";

await init();
const highs = await highsLoader({ locateFile: (file) => `./vendor/${file}` });

const model = new Model(
  {
    sources: { name: "sources.csv", text: sourcesCsv, quantity: "capacity" },
    caps: { name: "caps.csv", text: "business,cap\napple inc,5", quantity: "cap" }, // optional
    sinks: { name: "sinks.csv", text: sinksCsv, quantity: "demand" },
    base: { name: "base.xlsx", bytes: workbookBytes, sheet: "base" }, // optional
    layers: [{ name: "costs-set.csv", text: setCsv, op: "set" }],
  },
  (lp) => highs.solve(lp, { output_flag: false }),
);
const { objective, flows } = model.solve(); // flows index into model.sources().rows / model.sinks().rows
model.slack(); // unused capacity per source row
model.trace(source, sink); // how one pair's cost was reached, layer by layer
model.exportFlows("xlsx"); // Uint8Array; also "csv", "json", and exportSlack(...)
model.free(); // release the Rust side when done
```

The inputs take the same file layouts as the CLI and TUI (see `polu_core::load`); `format` comes
from the name's extension unless given. The TypeScript types are in `pkg/polu_wasm.d.ts`.

The solve callback can be any solver that returns highs-js's result shape,
`{ Status: "Optimal", Columns: { x0: { Primal: 3 }, ... } }`, where column `xi` is route `i`.

## Notes

- `highs.solve` runs on the main thread and blocks it while solving. That is fine for demo sizes
  (a 150 x 200 generated network solves in about 1.5 s); a Web Worker is the next step for big ones.
- The `.wasm` is about 2.4 MB before `wasm-opt` (wasm-pack runs it on release builds), 0.85 MB
  gzipped. highs.wasm adds 3.5 MB (1.2 MB gzipped).
- The page follows the system light/dark setting (dark is Catppuccin Mocha); the header button
  switches it and remembers the choice.
