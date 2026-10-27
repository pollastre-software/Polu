// Plain ES modules, no bundler. build.sh puts polu-wasm in ./pkg, highs-js in ./vendor and the
// demo-scenarios in ./scenarios; serve its output over HTTP (browsers don't load .wasm from file://).
// Loading and solving run in worker.js; the page itself only reads column names and a workbook's
// sheets and tables.
import init, { columnNames, regions } from "./pkg/polu_wasm.js";

const OPS = ["set", "scale", "add", "block"];
const MATRIX_LIMIT = 60; // rows and columns drawn in the cost map
const LP_LIMIT = 200_000; // characters of LP text shown

const $ = (id) => document.getElementById(id);

/** Inputs as Model wants them: `{ name, format?, text | bytes, sheet? | table?, quantity | op }`. */
let spec = null;
/** The last solve's snapshot from the worker (see `view` in worker.js), or null. */
let view = null;
/** While a solve runs: `{ started, stage, routes?, line? }`. */
let solving = null;
let log = []; // HiGHS's log of the last solve
let tab = "flows";
let selected = null; // [source, sink] being traced
let trace = null; // the selected pair's trace

// ---- elements

function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else if (v !== false && v != null) el.setAttribute(k, v);
  }
  el.append(...children.flat().filter((c) => c != null && c !== false));
  return el;
}

const duration = (ms) => (ms < 1000 ? `${ms.toFixed(0)} ms` : `${(ms / 1000).toFixed(1)} s`);

const fmt = (n) =>
  n == null || Number.isNaN(n) ? "–" : Number.isInteger(n) ? n.toLocaleString() : n.toFixed(2);

// ---- scenarios

async function fetchOk(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status} ${response.statusText}`);
  return response;
}

/** The scenarios build.sh found in demo-scenarios/: `[{ name, scenario }]`. */
async function scenarios() {
  return (await fetchOk("./scenarios/index.json")).json();
}

/**
 * A scenario file, as the TUI saves it (`*.polu.json`, paths relative to it), with every file it
 * names fetched: text for CSV and JSON, bytes for Excel.
 */
async function loadScenario(path) {
  const dir = new URL(path, new URL("./scenarios/", location.href));
  const scenario = await (await fetchOk(dir)).json();
  const file = async ({ path, sheet, table }) => {
    const response = await fetchOk(new URL(path, dir));
    const name = path.split("/").pop();
    return /\.(csv|json)$/i.test(name)
      ? { name, text: await response.text() }
      : { name, sheet, table, bytes: new Uint8Array(await response.arrayBuffer()) };
  };
  const table = async (input) => input && { ...(await file(input)), quantity: input.quantity };
  return {
    sources: await table(scenario.sources),
    caps: await table(scenario.caps),
    sinks: await table(scenario.sinks),
    base: scenario.base ? await file(scenario.base) : null,
    layers: await Promise.all((scenario.layers ?? []).map(async (l) => ({ ...(await file(l)), op: l.op }))),
  };
}

/** The input fields that read `region`, one of `regions(bytes)`. */
const readFrom = (region) =>
  region.kind === "table" ? { sheet: undefined, table: region.name } : { sheet: region.name, table: undefined };

/**
 * Maps a workbook's sheets and named tables onto inputs by name, ignoring case: sources, caps,
 * sinks, base and costs-<op> (or costs_<op>, as table names can't have hyphens). A table wins over
 * a sheet of the same name.
 */
function fromWorkbook(name, bytes) {
  const found = regions(bytes);
  const key = (r) => r.name.toLowerCase().replace("_", "-");
  const named = (n) => {
    const all = found.filter((r) => key(r) === n);
    return all.find((r) => r.kind === "table") ?? all[0];
  };
  const input = (r, extra) => r && { name, bytes, ...readFrom(r), ...extra };
  const table = (n) => {
    const r = named(n);
    return input(r, { quantity: r?.columns[r.columns.length - 1] }) ?? null;
  };
  const [sources, sinks] = [table("sources"), table("sinks")];
  if (!sources || !sinks) {
    const names = found.map((r) => r.name).join(", ");
    throw new Error(`${name} needs sheets or tables named "sources" and "sinks" (it has ${names})`);
  }
  return {
    sources,
    caps: table("caps"),
    sinks,
    base: input(named("base")) ?? null,
    layers: found
      .filter((r) => /^costs-/.test(key(r)) && OPS.includes(key(r).slice(6)))
      .map((r) => input(r, { op: key(r).slice(6) })),
  };
}

/**
 * A synthetic network: businesses with sized sites, a cap per business, stores in states,
 * a distance base matrix, a small-site surcharge and a few blocked business/state pairs.
 */
function generated(nSources, nSinks) {
  let seed = 7;
  const rand = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
  const states = ["WA", "OR", "CA", "TX", "OH", "NY", "FL", "IL"];
  const sources = [];
  const caps = [];
  for (let b = 0; sources.length < nSources; b++) {
    const business = `business ${b + 1}`;
    const sizes = ["large", "small"].slice(0, Math.min(2, nSources - sources.length));
    const sites = sizes.map(() => 20 + Math.floor(rand() * 60));
    sizes.forEach((size, i) => sources.push({ business, size, cap: sites[i], x: rand(), y: rand() }));
    // the business cap bounds its sites together, below their sum
    caps.push({ business, cap: Math.floor(sites.reduce((a, c) => a + c, 0) * 0.8) });
  }
  const sinks = Array.from({ length: nSinks }, (_, k) => ({
    store: `store ${k + 1}`,
    state: states[k % states.length],
    demand: 5 + Math.floor(rand() * 25),
    x: rand(),
    y: rand(),
  }));
  // keep demand well under the business caps so the plan stays feasible
  const capacity = caps.reduce((a, c) => a + c.cap, 0) || 1;
  const demand = sinks.reduce((a, s) => a + s.demand, 0);
  const shrink = Math.min(1, (0.6 * capacity) / demand);
  sinks.forEach((s) => (s.demand = Math.max(1, Math.floor(s.demand * shrink))));

  const base = sources
    .map((s) => sinks.map((k) => (100 * Math.hypot(s.x - k.x, s.y - k.y)).toFixed(1)).join(","))
    .join("\n");
  const blocked = [];
  for (let b = 1; b <= 3; b++) blocked.push(`business ${b},${states[(b * 3) % states.length]}`);
  return {
    sources: {
      name: "sources.csv",
      text: "business,size,capacity\n" + sources.map((s) => `${s.business},${s.size},${s.cap}`).join("\n"),
      quantity: "capacity",
    },
    caps: {
      name: "caps.csv",
      text: "business,cap\n" + caps.map((c) => `${c.business},${c.cap}`).join("\n"),
      quantity: "cap",
    },
    sinks: {
      name: "sinks.csv",
      text: "store,state,demand\n" + sinks.map((s) => `${s.store},${s.state},${s.demand}`).join("\n"),
      quantity: "demand",
    },
    base: { name: "base.csv", text: base },
    layers: [
      { name: "small-sites.csv", text: "source.size,value\nsmall,1.5", op: "scale" },
      { name: "no-ship.csv", text: "source.business,sink.state\n" + blocked.join("\n"), op: "block" },
    ],
  };
}

/** `which` is a scenario file from the index, or "generated". */
async function loadExample(which) {
  $("generator").hidden = which !== "generated";
  try {
    spec = which === "generated"
      ? generated(+$("gen-sources").value, +$("gen-sinks").value)
      : await loadScenario(which);
  } catch (e) {
    $("error").textContent = e.message;
    $("error").hidden = false;
    return;
  }
  selected = null;
  renderCards();
  changed(true);
}

// ---- input cards

async function readFile(file) {
  const format = file.name.split(".").pop().toLowerCase();
  if (format === "csv" || format === "json") return { name: file.name, text: await file.text() };
  return { name: file.name, bytes: new Uint8Array(await file.arrayBuffer()) };
}

/** Drop and pick-a-file handling for one card; `replace` gets the file's input. */
function droppable(card, replace) {
  card.addEventListener("dragover", (e) => {
    e.preventDefault();
    e.stopPropagation();
    card.classList.add("over");
  });
  card.addEventListener("dragleave", () => card.classList.remove("over"));
  card.addEventListener("drop", async (e) => {
    e.preventDefault();
    e.stopPropagation();
    card.classList.remove("over");
    const file = e.dataTransfer.files[0];
    if (file) replace(await readFile(file));
  });
}

function card(title, input, { onReplace, onRemove, extra }) {
  const isText = input.text != null;
  const picker = h("input", {
    type: "file",
    accept: ".csv,.json,.xlsx,.xls,.ods",
    hidden: true,
    onchange: async (e) => e.target.files[0] && onReplace(await readFile(e.target.files[0])),
  });
  const body = isText
    ? h("textarea", {
        spellcheck: "false",
        rows: Math.min(10, Math.max(3, input.text.split("\n").length + 1)),
        oninput: (e) => {
          input.text = e.target.value;
          changed();
        },
      }, input.text)
    : h("div", { class: "workbook" }, regionPicker(input));
  const el = h("article", { class: "card" },
    h("div", { class: "card-head" },
      h("strong", {}, title),
      h("span", { class: "muted name" }, input.name),
      extra,
      h("button", { type: "button", class: "ghost small", onclick: () => picker.click() }, "Replace"),
      onRemove && h("button", { type: "button", class: "ghost small", onclick: onRemove, "aria-label": "Remove" }, "✕"),
    ),
    body,
    picker,
  );
  droppable(el, onReplace);
  return el;
}

/**
 * A workbook's sheets and named tables as a scrollable list, each with its columns, as in the TUI.
 * Picking one reads the input from it; an input with neither starts on the first sheet.
 */
function regionPicker(input) {
  let found = [];
  try {
    found = regions(input.bytes);
  } catch (e) {
    return h("span", { class: "error-text" }, e.message);
  }
  if (!found.length) return h("span", { class: "error-text" }, "The workbook has no sheets");
  if (input.sheet == null && input.table == null) Object.assign(input, readFrom(found[0]));
  const isPicked = (r) => (r.kind === "table" ? r.name === input.table : !input.table && r.name === input.sheet);
  const columns = ({ columns: c }) =>
    c.length ? `${c.length} column${c.length === 1 ? "" : "s"}: ${c.join(", ")}` : "empty";
  const list = h("div", {
    class: "regions",
    role: "listbox",
    "aria-label": "Sheet or table",
    // arrow keys move between entries, Enter or a click picks one
    onkeydown: (e) => {
      const next = { ArrowDown: "nextElementSibling", ArrowUp: "previousElementSibling" }[e.key];
      if (next && e.target[next]) {
        e.preventDefault();
        e.target[next].focus();
      }
    },
  }, found.map((r) => h("button", {
    type: "button",
    role: "option",
    class: "region",
    "aria-selected": String(isPicked(r)),
    onclick: () => { Object.assign(input, readFrom(r)); renderCards(); changed(); },
  },
    h("span", { class: "line" },
      h("span", { class: "muted kind" }, r.kind),
      h("strong", {}, r.name),
      r.kind === "table" && h("span", { class: "muted" }, ` on ${r.sheet}`)),
    h("span", { class: "line muted columns" }, columns(r)))));
  // the list is rebuilt on every pick, so bring the picked one back into view once it's on the page
  requestAnimationFrame(() => {
    const picked = list.querySelector("[aria-selected=true]");
    if (picked) list.scrollTop = picked.offsetTop - (list.clientHeight - picked.offsetHeight) / 2;
  });
  const at = found.findIndex(isPicked);
  return [
    h("div", { class: "muted regions-head" },
      "Sheet or table", h("span", {}, at < 0 ? `${found.length}` : `${at + 1} of ${found.length}`)),
    list,
  ];
}

function quantityPicker(input) {
  let columns = [];
  try {
    columns = columnNames(input);
  } catch {
    return null;
  }
  if (!columns.includes(input.quantity)) input.quantity = columns[columns.length - 1];
  return h("label", { class: "muted" }, "quantity ",
    h("select", { onchange: (e) => { input.quantity = e.target.value; changed(); } },
      columns.map((c) => h("option", { value: c, selected: c === input.quantity }, c))));
}

function opPicker(layer) {
  return h("select", { "aria-label": "Operation", onchange: (e) => { layer.op = e.target.value; changed(); } },
    OPS.map((op) => h("option", { value: op, selected: op === layer.op }, op)));
}

function renderCards() {
  const cards = [
    card("Sources", spec.sources, {
      extra: quantityPicker(spec.sources),
      onReplace: (i) => { spec.sources = { ...i, quantity: spec.sources.quantity }; renderCards(); changed(true); },
    }),
  ];
  if (spec.caps) {
    cards.push(card("Source caps", spec.caps, {
      extra: quantityPicker(spec.caps),
      onReplace: (i) => { spec.caps = { ...i, quantity: spec.caps.quantity }; renderCards(); changed(true); },
      onRemove: () => { spec.caps = null; renderCards(); changed(true); },
    }));
  } else {
    const add = h("button", { type: "button", class: "ghost dashed" }, "+ Source caps (optional, drop a file here)");
    // starts with the first source column as its header, so only rows are left to add
    add.onclick = () => {
      let column = "business";
      try {
        column = columnNames(spec.sources).find((c) => c !== spec.sources.quantity) ?? column;
      } catch {}
      spec.caps = { name: "caps.csv", text: `${column},cap\n`, quantity: "cap" };
      renderCards();
    };
    droppable(add, (i) => { spec.caps = { ...i, quantity: "" }; renderCards(); changed(true); });
    cards.push(add);
  }
  cards.push(
    card("Sinks", spec.sinks, {
      extra: quantityPicker(spec.sinks),
      onReplace: (i) => { spec.sinks = { ...i, quantity: spec.sinks.quantity }; renderCards(); changed(true); },
    }),
  );
  if (spec.base) {
    cards.push(card("Base costs", spec.base, {
      onReplace: (i) => { spec.base = i; renderCards(); changed(true); },
      onRemove: () => { spec.base = null; renderCards(); changed(true); },
    }));
  } else {
    const add = h("button", { type: "button", class: "ghost dashed" }, "+ Base cost matrix (optional, drop a file here)");
    add.onclick = () => { spec.base = { name: "base.csv", text: "" }; renderCards(); };
    droppable(add, (i) => { spec.base = i; renderCards(); changed(true); });
    cards.push(add);
  }
  spec.layers.forEach((layer, l) => {
    cards.push(card(`Layer ${l + 1}`, layer, {
      extra: opPicker(layer),
      onReplace: (i) => { spec.layers[l] = { ...i, op: layer.op }; renderCards(); changed(true); },
      onRemove: () => { spec.layers.splice(l, 1); renderCards(); changed(true); },
    }));
  });
  $("cards").replaceChildren(...cards);
}

// ---- the worker

/** Thrown at calls a cancel cut short. */
class Cancelled extends Error {}

let worker = null;
const calls = new Map(); // id -> { resolve, reject, onReply }
let nextId = 0;

/** Calls `name` in worker.js; `onReply` gets the call's progress and log messages. */
function call(name, args, onReply) {
  if (!worker) {
    worker = new Worker("./worker.js", { type: "module" });
    worker.onmessage = ({ data }) => {
      const pending = calls.get(data.id);
      if (!pending) return;
      if ("result" in data || "error" in data) calls.delete(data.id);
      if ("result" in data) pending.resolve(data.result);
      else if ("error" in data) pending.reject(new Error(data.error));
      else pending.onReply?.(data);
    };
    worker.onerror = (e) => stopWorker(new Error(e.message || "the solver stopped"));
  }
  const id = nextId++;
  return new Promise((resolve, reject) => {
    calls.set(id, { resolve, reject, onReply });
    worker.postMessage({ id, call: name, args });
  });
}

/** Ends the worker and whatever it's doing; its calls fail with `reason`. The next call starts a new one. */
function stopWorker(reason) {
  worker?.terminate();
  worker = null;
  for (const pending of calls.values()) pending.reject(reason);
  calls.clear();
}

// ---- solving

let timer = null;
function changed(now = false) {
  if (!$("auto").checked && !now) return;
  clearTimeout(timer);
  timer = setTimeout(solve, now ? 0 : 350);
}

let runs = 0; // which solve is the latest
async function solve() {
  if (!spec) return;
  // a solve can't be interrupted, so new inputs start over in a new worker
  if (solving) stopWorker(new Cancelled());
  const run = ++runs;
  $("error").hidden = true;
  solving = { started: performance.now(), stage: "loading" };
  log = [];
  const ticker = setInterval(showProgress, 200);
  showProgress();
  renderPanel();
  try {
    // an empty base card means no base
    const inputs = { ...spec, base: spec.base?.text === "" ? null : spec.base };
    const result = await call("solve", [inputs, MATRIX_LIMIT], (reply) => {
      if (reply.progress) Object.assign(solving, reply.progress);
      if (reply.log) log.push((solving.line = reply.log));
    });
    if (run !== runs) return;
    view = result.view;
    const { load, solve: solved } = result.timings;
    $("status").textContent = result.error
      ? "Loaded, but not solved"
      : `Loaded in ${duration(load)}, solved in ${duration(solved)}`;
    if (result.error) showError(result.error);
  } catch (e) {
    if (run !== runs) return;
    view = null;
    showError(e.message);
    $("status").textContent = "Not loaded";
  } finally {
    clearInterval(ticker);
    if (run === runs) {
      solving = null;
      showProgress();
      renderSummary();
      renderPanel();
      loadTrace();
    }
  }
}

function cancel() {
  if (!solving) return;
  runs++;
  solving = null;
  // the worker's model went with it
  view = null;
  stopWorker(new Cancelled());
  $("status").textContent = "Cancelled";
  showProgress();
  renderSummary();
  renderPanel();
}

const STAGES = {
  loading: () => "Loading the inputs",
  solving: (s) => `Solving ${s.routes == null ? "" : `${fmt(s.routes)} routes `}with HiGHS`,
  reading: () => "Reading the solution",
};

function showProgress() {
  $("cancel").hidden = !solving;
  $("busy").hidden = !solving;
  $("results").classList.toggle("solving", !!solving);
  if (!solving) return;
  const seconds = (performance.now() - solving.started) / 1000;
  $("status").textContent = `${STAGES[solving.stage](solving)}… ${seconds.toFixed(1)} s`;
  $("last-log").textContent = solving.line ?? "";
  if (tab === "log") $("log").textContent = log.join("\n");
}

function showError(message) {
  $("error").textContent = message;
  $("error").hidden = false;
}

function renderSummary() {
  // cap use downloads only make sense with caps
  for (const el of document.querySelectorAll("[data-caps]")) el.hidden = !view?.caps;
  if (!view?.solution) return $("summary").replaceChildren();
  const { solution, sources, sinks, routes } = view;
  const units = solution.flows.reduce((a, f) => a + f.units, 0);
  const stat = (label, value) => h("div", { class: "stat" }, h("span", {}, label), h("strong", {}, value));
  $("summary").replaceChildren(
    stat("Total cost", fmt(solution.objective)),
    stat("Units shipped", fmt(units)),
    stat("Flows", fmt(solution.flows.length)),
    stat("Usable pairs", `${fmt(routes)} of ${fmt(sources.rows.length * sinks.rows.length)}`),
  );
}

// ---- result panels

let panels = 0; // which panel render is the latest, for the LP text that arrives later
function renderPanel() {
  const shown = ++panels;
  for (const b of document.querySelectorAll("[role=tab]")) b.setAttribute("aria-selected", b.dataset.tab === tab);
  const panel = $("panel");
  if (tab === "log") {
    panel.replaceChildren(
      h("p", { class: "muted" }, "What HiGHS printed during the last solve."),
      h("pre", { class: "lp", id: "log" }, log.join("\n") || "Nothing yet."),
    );
    return renderTrace();
  }
  if (!view) {
    panel.replaceChildren();
    return renderTrace();
  }
  const solution = view.solution;
  if (tab === "flows") panel.replaceChildren(flowsTable(solution));
  if (tab === "costmap") panel.replaceChildren(costMatrix(solution));
  if (tab === "unused") panel.replaceChildren(solution ? unusedTable() : h("p", { class: "muted" }, "Solve first."));
  if (tab === "lp") {
    const about = h("p", { class: "muted" }, "The CPLEX LP text handed to HiGHS: column xi is route i, rows si, ci and dj are the supply, cap and demand limits.");
    const text = h("pre", { class: "lp" }, solving ? "Waiting for the solve…" : "Writing the LP…");
    panel.replaceChildren(about, text);
    if (!solving) {
      call("lp", [LP_LIMIT]).then(
        (lp) => shown === panels && (text.textContent = lp.length > LP_LIMIT ? lp.text + "\n…" : lp.text),
        (e) => shown === panels && !(e instanceof Cancelled) && (text.textContent = e.message),
      );
    }
  }
  renderTrace();
}

function flowsTable(solution) {
  if (!solution) return h("p", { class: "muted" }, "Solve first.");
  const { sources, sinks } = view;
  const rows = [...solution.flows].sort((a, b) => b.units * b.cost - a.units * a.cost);
  return h("table", {},
    h("thead", {}, h("tr", {}, ["Source", "Sink", "Units", "Unit cost", "Cost"].map((t) => h("th", {}, t)))),
    h("tbody", {}, rows.map((f) =>
      h("tr", { class: isSelected(f.source, f.sink) ? "selected clickable" : "clickable", onclick: () => select(f.source, f.sink) },
        h("td", {}, sources.rows[f.source].label),
        h("td", {}, sinks.rows[f.sink].label),
        h("td", { class: "num" }, fmt(f.units)),
        h("td", { class: "num" }, fmt(f.cost)),
        h("td", { class: "num" }, fmt(f.units * f.cost))))));
}

function costMatrix(solution) {
  const { sources, sinks, costs } = view;
  const units = new Map((solution?.flows ?? []).map((f) => [`${f.source},${f.sink}`, f.units]));
  const ns = costs.length;
  const nk = costs[0]?.length ?? 0;
  const note = ns < sources.rows.length || nk < sinks.rows.length
    ? h("p", { class: "muted" }, `Showing the first ${ns} sources and ${nk} sinks.`) : null;
  const head = h("tr", {}, h("th", {}), sinks.rows.slice(0, nk).map((k) => h("th", { class: "vertical" }, h("span", {}, k.label))));
  const body = sources.rows.slice(0, ns).map((s, i) =>
    h("tr", {}, h("th", {}, s.label), Array.from({ length: nk }, (_, j) => {
      const c = costs[i][j];
      const u = units.get(`${i},${j}`);
      const cls = ["cell", c == null ? "none" : "", u ? "used" : "", isSelected(i, j) ? "selected" : ""].join(" ");
      return h("td", { class: cls, title: u ? `${fmt(u)} units at ${fmt(c)}` : c == null ? "no route" : `cost ${fmt(c)}`, onclick: () => select(i, j) },
        c == null ? "" : fmt(c), u ? h("small", {}, `×${fmt(u)}`) : null);
    })));
  return h("div", {},
    h("p", { class: "muted" }, "Resolved cost of every source and sink pair; highlighted cells carry flow. Click a cell to see how its cost was reached."),
    note,
    h("div", { class: "scroll" }, h("table", { class: "matrix" }, h("thead", {}, head), h("tbody", {}, body))));
}

function unusedTable() {
  const { sources, slack, caps, capSlack } = view;
  const bar = (used, amount) => h("td", { class: "bar" }, h("span", { style: `width:${amount ? (100 * used) / amount : 100}%` }));
  return h("div", {},
    h("p", { class: "muted" }, "A row with an empty field caps the rows under it, so a site can have room of its own (unused) yet none to spare once its business total is reached."),
    h("table", {},
      h("thead", {}, h("tr", {}, ["Source", "Used", "Capacity", "Unused", "Spare", ""].map((t) => h("th", {}, t)))),
      h("tbody", {}, slack.map((s, i) =>
        h("tr", {},
          h("td", {}, sources.rows[i].label),
          h("td", { class: "num" }, fmt(s.used)),
          h("td", { class: "num" }, fmt(s.amount)),
          h("td", { class: "num" }, fmt(s.unused)),
          h("td", { class: "num" }, fmt(s.spare)),
          bar(s.used, s.amount))))),
    capSlack?.length ? [
      h("h3", {}, "Source caps"),
      h("p", { class: "muted" }, "What ships under each cap, what it still allows, and how much of it is used. A full cap is why the rows under it have no spare."),
      h("table", {},
        h("thead", {}, h("tr", {}, ["Source cap", "Used", "Cap", "Unused", "Used %", ""].map((t) => h("th", {}, t)))),
        h("tbody", {}, capSlack.map((c, i) =>
          h("tr", { class: c.unused === 0 ? "full" : null },
            h("td", {}, caps.rows[i].label),
            h("td", { class: "num" }, fmt(c.used)),
            h("td", { class: "num" }, fmt(c.amount)),
            h("td", { class: "num" }, fmt(c.unused)),
            h("td", { class: "num" }, `${c.percentUsed}%`),
            bar(c.used, c.amount))))),
    ] : null);
}

// ---- cost trace

const isSelected = (s, k) => selected && selected[0] === s && selected[1] === k;

function select(s, k) {
  selected = isSelected(s, k) ? null : [s, k];
  renderPanel();
  loadTrace();
}

/** Asks the worker for the selected pair's trace, and draws it. */
async function loadTrace() {
  trace = null;
  renderTrace();
  const pair = selected;
  if (!view || !pair) return;
  try {
    const result = await call("trace", pair);
    if (selected !== pair || solving) return;
    trace = result;
  } catch {
    // the pair isn't in the new inputs, or the worker was stopped
    if (selected === pair) selected = null;
  }
  renderTrace();
}

function renderTrace() {
  const el = $("trace");
  if (!view || !selected || !trace) return (el.hidden = true);
  const [s, k] = selected;
  const say = { set: "sets", scale: "scales by", add: "adds", block: "blocks" };
  const steps = trace.steps.map((step) => {
    const layer = view.layers[step.layer];
    const rule = step.rule == null ? null : layer.rules[step.rule];
    const what = rule == null
      ? "no rule matches, unchanged"
      : step.op === "block"
        ? `rule ${step.rule + 1} (${rule.text}) blocks the pair`
        : `rule ${step.rule + 1} (${rule.text}) ${say[step.op]} ${fmt(rule.value)}`;
    return h("li", {}, h("span", { class: "op" }, step.op), ` ${layer.name}: ${what}`, h("strong", {}, ` → ${fmt(step.cost)}`));
  });
  const base = trace.base == null ? "no base cost" : `base cost ${fmt(trace.base)}`;
  el.replaceChildren(
    h("h3", {}, `${view.sources.rows[s].label} → ${view.sinks.rows[k].label}`),
    h("ol", {}, h("li", {}, h("span", { class: "op" }, "base"), ` ${base}`), steps),
    h("p", {}, trace.cost == null ? "Result: no route, this pair can't be used." : `Result: cost ${fmt(trace.cost)} per unit.`),
  );
  el.hidden = false;
}

// ---- downloads

async function download(what, format) {
  if (!view?.solution || solving) return;
  let bytes;
  try {
    bytes = await call("export", [what, format]);
  } catch (e) {
    return e instanceof Cancelled || showError(e.message);
  }
  const types = { csv: "text/csv", json: "application/json", xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" };
  const url = URL.createObjectURL(new Blob([bytes], { type: types[format] }));
  const names = { flows: "flows", slack: "unused", caps: "cap-use" };
  h("a", { href: url, download: `${names[what]}.${format}` }).click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// ---- wiring

// light or Catppuccin Mocha; without a choice the system setting decides
const dark = () => matchMedia("(prefers-color-scheme: dark)").matches;
function setTheme(theme) {
  if (theme) document.documentElement.dataset.theme = theme;
  const mocha = (document.documentElement.dataset.theme ?? (dark() ? "mocha" : "light")) === "mocha";
  $("theme").textContent = mocha ? "Light theme" : "Mocha theme";
}
try {
  setTheme(localStorage.getItem("polu-theme"));
} catch {
  setTheme(null);
}
$("theme").onclick = () => {
  const next = $("theme").textContent.startsWith("Mocha") ? "mocha" : "light";
  setTheme(next);
  try {
    localStorage.setItem("polu-theme", next);
  } catch {}
};

$("example").onchange = (e) => loadExample(e.target.value);
$("generate").onclick = () => loadExample("generated");
$("add-layer").onclick = () => {
  spec.layers.push({ name: "layer.csv", text: "source.business,value\n", op: "scale" });
  renderCards();
};
$("solve").onclick = solve;
$("cancel").onclick = cancel;
for (const b of document.querySelectorAll("[role=tab]")) b.onclick = () => { tab = b.dataset.tab; renderPanel(); };
for (const b of document.querySelectorAll("[data-export]")) b.onclick = () => download(b.dataset.export, b.dataset.format);

// a workbook dropped outside the cards replaces every input
document.addEventListener("dragover", (e) => e.preventDefault());
document.addEventListener("drop", async (e) => {
  e.preventDefault();
  const file = e.dataTransfer.files[0];
  if (!file) return;
  try {
    const input = await readFile(file);
    if (!input.bytes) throw new Error("drop CSV and JSON files on the card they replace");
    spec = fromWorkbook(file.name, input.bytes);
    renderCards();
    changed(true);
  } catch (err) {
    showError(err.message);
  }
});

await init();
$("status").textContent = "Ready";
try {
  const options = (await scenarios()).map((s) => h("option", { value: s.scenario }, s.name));
  $("example").prepend(...options);
  $("example").value = options[0]?.value ?? "generated";
} catch {
  // the generated network needs no files
  $("example").value = "generated";
}
await loadExample($("example").value);
