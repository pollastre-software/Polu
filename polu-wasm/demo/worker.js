// Loads, solves and inspects a Model off the page's thread, so the page stays responsive while
// HiGHS works. main.js sends `{ id, call, args }` and gets back `{ id, result }` or `{ id, error }`;
// during a solve it also gets `{ id, progress }` and `{ id, log }` (HiGHS's log, line by line).
// A solve can't be interrupted, so the page cancels one by terminating this worker.
import init, { Model } from "./pkg/polu_wasm.js";
import highsLoader from "./vendor/highs.mjs";

// HiGHS's MIP presolve finds nothing to remove in a transport problem but takes most of the
// solve: on a generated 700 x 600 network, 57 s with it and 17 s without, for the same plan.
const HIGHS_OPTIONS = { output_flag: true, presolve: "off" };

let model = null;
let log = () => {}; // where HiGHS's log lines go

const ready = (async () => {
  await init();
  return highsLoader({ locateFile: (file) => `./vendor/${file}`, print: (line) => log(line) });
})();

// Each call gets `send`, which posts to the page under the call's id.
const calls = {
  /** Loads and solves `spec`, and returns everything the page draws as one snapshot. */
  async solve(send, spec, corner) {
    const highs = await ready;
    model?.free();
    model = null;
    send({ progress: { stage: "loading" } });
    const t0 = performance.now();
    model = new Model(spec, (lp) => {
      log = (line) => send({ log: line });
      try {
        return highs.solve(lp, HIGHS_OPTIONS);
      } finally {
        log = () => {};
        send({ progress: { stage: "reading" } });
      }
    });
    const routes = model.routes();
    send({ progress: { stage: "solving", routes: routes.length } });
    const t1 = performance.now();
    let error = null;
    try {
      model.solve();
    } catch (e) {
      error = message(e);
    }
    const timings = { load: t1 - t0, solve: performance.now() - t1 };
    return { view: view(routes, corner), timings, error };
  },

  trace(send, source, sink) {
    return model.trace(source, sink);
  },

  /** The LP text, cut to `limit` characters: all of it can run to tens of MB. */
  lp(send, limit) {
    const text = model.lp();
    return { text: text.slice(0, limit), length: text.length };
  },

  export(send, what, format) {
    return what === "flows" ? model.exportFlows(format) : model.exportSlack(format);
  },
};

/**
 * What the page draws: the tables and layers, the solution and unused capacity (null before a
 * solve), and the costs of the top-left `corner` x `corner` pairs for the cost map.
 */
function view(routes, corner) {
  const sources = model.sources();
  const sinks = model.sinks();
  const costs = Array.from({ length: Math.min(sources.rows.length, corner) }, () =>
    Array(Math.min(sinks.rows.length, corner)).fill(null));
  for (const r of routes) {
    if (r.source < costs.length && r.sink < corner) costs[r.source][r.sink] = r.cost;
  }
  const solution = model.solution() ?? null;
  return {
    sources,
    sinks,
    layers: model.layers(),
    routes: routes.length,
    costs,
    solution,
    slack: solution ? model.slack() : null,
  };
}

const message = (e) => e?.message ?? String(e);

onmessage = async ({ data: { id, call, args } }) => {
  const send = (reply) => postMessage({ id, ...reply });
  try {
    const result = await calls[call](send, ...args);
    // hand file bytes over instead of copying them
    postMessage({ id, result }, result instanceof Uint8Array ? [result.buffer] : []);
  } catch (e) {
    postMessage({ id, error: message(e) });
  }
};
