// Plain ES modules, no bundler. build.sh puts polu-wasm in ./pkg, highs-js in ./vendor and the
// demo-scenarios in ./scenarios; serve its output over HTTP (browsers don't load .wasm from file://).
import init, { Model, columnNames, sheetNames } from "./pkg/polu_wasm.js";
import highsLoader from "./vendor/highs.mjs";

const OPS = ["set", "scale", "add", "block"];
const MATRIX_LIMIT = 60; // rows and columns drawn in the cost map
const LP_LIMIT = 200_000; // characters of LP text shown

const $ = (id) => document.getElementById(id);

/** Inputs as Model wants them: `{ name, format?, text | bytes, sheet?, quantity | op }`. */
let spec = null;
let model = null;
let highs = null;
let tab = "flows";
let selected = null; // [source, sink] being traced

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
  const file = async ({ path, sheet }) => {
    const response = await fetchOk(new URL(path, dir));
    const name = path.split("/").pop();
    return /\.(csv|json)$/i.test(name)
      ? { name, text: await response.text() }
      : { name, sheet, bytes: new Uint8Array(await response.arrayBuffer()) };
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

/** Maps a workbook's sheets onto inputs by name: sources, caps, sinks, base, costs-<op>. */
function fromWorkbook(name, bytes) {
  const sheets = sheetNames(bytes);
  const sheet = (s, extra) => ({ name, bytes, sheet: s, ...extra });
  const quantity = (s) => {
    const columns = columnNames(sheet(s));
    return columns[columns.length - 1];
  };
  if (!sheets.includes("sources") || !sheets.includes("sinks")) {
    throw new Error(`${name} needs sheets named "sources" and "sinks" (it has ${sheets.join(", ")})`);
  }
  return {
    sources: sheet("sources", { quantity: quantity("sources") }),
    caps: sheets.includes("caps") ? sheet("caps", { quantity: quantity("caps") }) : null,
    sinks: sheet("sinks", { quantity: quantity("sinks") }),
    base: sheets.includes("base") ? sheet("base") : null,
    layers: sheets
      .filter((s) => /^costs-/.test(s) && OPS.includes(s.slice(6)))
      .map((s) => sheet(s, { op: s.slice(6) })),
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
    : h("div", { class: "workbook" }, sheetPicker(input));
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

function sheetPicker(input) {
  let sheets = [];
  try {
    sheets = sheetNames(input.bytes);
  } catch (e) {
    return h("span", { class: "error-text" }, e.message);
  }
  input.sheet ??= sheets[0];
  return h("label", {}, "Sheet ",
    h("select", { onchange: (e) => { input.sheet = e.target.value; renderCards(); changed(); } },
      sheets.map((s) => h("option", { value: s, selected: s === input.sheet }, s))));
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

// ---- solving

let timer = null;
function changed(now = false) {
  if (!$("auto").checked && !now) return;
  clearTimeout(timer);
  timer = setTimeout(solve, now ? 0 : 350);
}

function solve() {
  if (!highs || !spec) return;
  $("error").hidden = true;
  const t0 = performance.now();
  try {
    model?.free();
    model = null;
    // an empty base card means no base
    const inputs = { ...spec, base: spec.base?.text === "" ? null : spec.base };
    model = new Model(inputs, (lp) => highs.solve(lp, { output_flag: false }));
    const t1 = performance.now();
    const solution = model.solve();
    const t2 = performance.now();
    const routes = model.routes().length;
    $("status").textContent = `Loaded in ${(t1 - t0).toFixed(0)} ms, solved in ${(t2 - t1).toFixed(0)} ms`;
    renderSummary(solution, routes);
  } catch (e) {
    $("error").textContent = e.message ?? String(e);
    $("error").hidden = false;
    $("status").textContent = model ? "Loaded, but not solved" : "Not loaded";
    $("summary").replaceChildren();
  }
  renderPanel();
}

function renderSummary(solution, routes) {
  const [sources, sinks] = [model.sources(), model.sinks()];
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

function renderPanel() {
  for (const b of document.querySelectorAll("[role=tab]")) b.setAttribute("aria-selected", b.dataset.tab === tab);
  const panel = $("panel");
  const solution = model?.solution();
  if (!model) {
    panel.replaceChildren();
    return renderTrace();
  }
  if (tab === "flows") panel.replaceChildren(flowsTable(solution));
  if (tab === "costmap") panel.replaceChildren(costMatrix(solution));
  if (tab === "unused") panel.replaceChildren(solution ? unusedTable() : h("p", { class: "muted" }, "Solve first."));
  if (tab === "lp") {
    const lp = model.lp();
    panel.replaceChildren(
      h("p", { class: "muted" }, "The CPLEX LP text handed to HiGHS: column xi is route i, rows si and dj are the supply and demand limits."),
      h("pre", { class: "lp" }, lp.length > LP_LIMIT ? lp.slice(0, LP_LIMIT) + "\n…" : lp),
    );
  }
  renderTrace();
}

function flowsTable(solution) {
  if (!solution) return h("p", { class: "muted" }, "Solve first.");
  const [sources, sinks] = [model.sources(), model.sinks()];
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
  const [sources, sinks] = [model.sources(), model.sinks()];
  const cost = new Map(model.routes().map((r) => [`${r.source},${r.sink}`, r.cost]));
  const units = new Map((solution?.flows ?? []).map((f) => [`${f.source},${f.sink}`, f.units]));
  const ns = Math.min(sources.rows.length, MATRIX_LIMIT);
  const nk = Math.min(sinks.rows.length, MATRIX_LIMIT);
  const note = ns < sources.rows.length || nk < sinks.rows.length
    ? h("p", { class: "muted" }, `Showing the first ${ns} sources and ${nk} sinks.`) : null;
  const head = h("tr", {}, h("th", {}), sinks.rows.slice(0, nk).map((k) => h("th", { class: "vertical" }, h("span", {}, k.label))));
  const body = sources.rows.slice(0, ns).map((s, i) =>
    h("tr", {}, h("th", {}, s.label), Array.from({ length: nk }, (_, j) => {
      const c = cost.get(`${i},${j}`);
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
  const sources = model.sources();
  const slack = model.slack();
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
          h("td", { class: "bar" }, h("span", { style: `width:${s.amount ? (100 * s.used) / s.amount : 0}%` })))))));
}

// ---- cost trace

const isSelected = (s, k) => selected && selected[0] === s && selected[1] === k;

function select(s, k) {
  selected = isSelected(s, k) ? null : [s, k];
  renderPanel();
}

function renderTrace() {
  const el = $("trace");
  if (!model || !selected) return (el.hidden = true);
  const [s, k] = selected;
  const trace = model.trace(s, k);
  const layers = model.layers();
  const say = { set: "sets", scale: "scales by", add: "adds", block: "blocks" };
  const steps = trace.steps.map((step) => {
    const layer = layers[step.layer];
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
    h("h3", {}, `${model.sources().rows[s].label} → ${model.sinks().rows[k].label}`),
    h("ol", {}, h("li", {}, h("span", { class: "op" }, "base"), ` ${base}`), steps),
    h("p", {}, trace.cost == null ? "Result: no route, this pair can't be used." : `Result: cost ${fmt(trace.cost)} per unit.`),
  );
  el.hidden = false;
}

// ---- downloads

function download(what, format) {
  if (!model?.solution()) return;
  const bytes = what === "flows" ? model.exportFlows(format) : model.exportSlack(format);
  const types = { csv: "text/csv", json: "application/json", xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" };
  const url = URL.createObjectURL(new Blob([bytes], { type: types[format] }));
  h("a", { href: url, download: `${what === "flows" ? "flows" : "unused"}.${format}` }).click();
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
    $("error").textContent = err.message;
    $("error").hidden = false;
  }
});

await init();
highs = await highsLoader({ locateFile: (file) => `./vendor/${file}` });
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
