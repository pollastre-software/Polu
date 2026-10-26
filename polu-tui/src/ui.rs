//! Drawing: one function per tab and popup. Long lists only build their visible rows.

use std::ops::Range;

use polu_core::{Header, Key, Op};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};

use crate::app::{App, LineEdit, ListView, Pick, Popup, Side, Slot, SolveState, Sort, Tab};
use crate::io::{Format, op_label};

const SELECTED: Style = Style::new()
    .bg(Color::DarkGray)
    .add_modifier(Modifier::BOLD);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [tabs, body, status, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let titles = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}", i + 1, t.title()));
    frame.render_widget(
        Tabs::new(titles)
            .select(app.tab as usize)
            .highlight_style(Style::new().fg(Color::Yellow).bold()),
        tabs,
    );

    match app.tab {
        Tab::Inputs => inputs(frame, body, app),
        Tab::Data => data(frame, body, app),
        Tab::Costs => costs(frame, body, app),
        Tab::Solve => solve(frame, body, app),
        Tab::Save => save(frame, body, app),
    }

    let mut message = Vec::new();
    if app.stale {
        message.push("inputs changed, press l to reload  ".yellow());
    }
    match &app.message {
        Some((true, text)) => message.push(text.as_str().red()),
        Some((false, text)) => message.push(text.as_str().green()),
        None => {}
    }
    frame.render_widget(Paragraph::new(Line::from(message)), status);
    frame.render_widget(Paragraph::new(hint(app)).dark_gray(), hints);

    if app.popup.is_some() {
        popup(frame, app);
    }
}

fn hint(app: &App) -> &'static str {
    if app.typing() {
        return "Enter apply · Esc cancel · Ctrl+C quit";
    }
    if app.popup.is_some() {
        return "↑↓ select · Enter pick · Esc close · Ctrl+C quit";
    }
    match app.tab {
        Tab::Inputs => {
            "q quit · ? help · ↑↓ select · Enter pick file · ←→ operator · [ ] move · x remove · l load · w/o save/open scenario · r search root"
        }
        Tab::Data => "q quit · ? help · ↑↓ scroll · / filter · s sources/sinks · l reload",
        Tab::Costs => {
            "q quit · ? help · ↑↓ scroll · / filter · o sort · h no-route pairs · t trace"
        }
        Tab::Solve => "q quit · ? help · s solve · / filter · u use vs capacity · ↑↓ scroll",
        Tab::Save => {
            "q quit · ? help · ↑↓ flows/unused capacity · Enter edit path, Enter again to save"
        }
    }
}

// ----- tabs -----

fn inputs(frame: &mut Frame, area: Rect, app: &mut App) {
    let [info, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    let scenario = app
        .scenario_path
        .as_ref()
        .map_or("(unsaved)".into(), |p| p.display().to_string());
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec!["Scenario: ".dark_gray(), scenario.into()]),
            Line::from(vec![
                "File search root: ".dark_gray(),
                app.root.display().to_string().into(),
            ]),
        ]),
        info,
    );

    let loaded = app.loaded.as_ref().filter(|_| !app.stale);
    let rows: Vec<Row> = app
        .slots()
        .into_iter()
        .map(|slot| {
            let (name, file, detail, count) = match slot {
                Slot::Sources | Slot::Sinks => {
                    let (name, input, side) = match slot {
                        Slot::Sources => ("Sources", &app.scenario.sources, 0),
                        _ => ("Sinks", &app.scenario.sinks, 1),
                    };
                    let count = loaded.map(|l| {
                        let (s, k) = l.tables();
                        format!("{} rows", [s, k][side].rows().len())
                    });
                    (
                        name.to_string(),
                        input.as_ref().map(|i| i.file.label()),
                        input.as_ref().map(|i| format!("quantity: {}", i.quantity)),
                        count,
                    )
                }
                Slot::Caps => (
                    "Caps".into(),
                    app.scenario.caps.as_ref().map(|i| i.file.label()),
                    Some(match &app.scenario.caps {
                        Some(i) => format!("quantity: {}", i.quantity),
                        None => "source caps, optional".into(),
                    }),
                    loaded
                        .and_then(|ld| ld.polu.source_caps())
                        .map(|caps| format!("{} rows", caps.rows().len())),
                ),
                Slot::Base => (
                    "Base".into(),
                    app.scenario.base.as_ref().map(|f| f.label()),
                    Some("matrix or rules, optional".into()),
                    loaded
                        .filter(|_| app.scenario.base.is_some())
                        .map(|ld| match ld.base_layer {
                            true => rules(ld.costmap().layers()[0].rules.len()),
                            false => "matrix".into(),
                        }),
                ),
                Slot::Layer(l) => {
                    let layer = &app.scenario.layers[l];
                    let count = loaded
                        .and_then(|ld| ld.costmap().layers().get(l + ld.base_layer as usize))
                        .map(|layer| rules(layer.rules.len()));
                    (
                        format!("Layer {}", l + 1),
                        Some(layer.file.label()),
                        Some(format!("‹ {} ›", op_label(layer.op))),
                        count,
                    )
                }
                Slot::NewLayer => ("+ add layer".into(), None, None, None),
            };
            Row::new(vec![
                Cell::from(name).bold(),
                Cell::from(file.unwrap_or_else(|| "—".into())),
                Cell::from(detail.unwrap_or_default()),
                Cell::from(count.unwrap_or_default()).dark_gray(),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Length(12),
        ],
    )
    .header(Row::new(["Input", "File", "", ""]).dark_gray())
    .row_highlight_style(SELECTED)
    .highlight_symbol("› ")
    .block(Block::bordered().title(" Inputs "));
    let mut state = TableState::default().with_selected(Some(app.slot));
    frame.render_stateful_widget(table, list, &mut state);
}

fn data(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(loaded) = &app.loaded else {
        return not_loaded(frame, area);
    };
    let (sources, sinks) = loaded.tables();
    let (table, covers, title) = match app.data_side {
        Side::Sources => (sources, &loaded.covers.0, "Sources"),
        Side::Sinks => (sinks, &loaded.covers.1, "Sinks"),
    };
    let [filter_area, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    filter_line(frame, filter_area, &app.data, "quantity");

    let header = table.header();
    let slots = named(header);
    let height = list.height.saturating_sub(3) as usize;
    let range = window(&mut app.data, height);
    let rows = app.data.rows[range.clone()].iter().map(|&i| {
        let (key, amount) = &table.rows()[i];
        let mut cells = fields(key, &slots);
        cells.push(amount.to_string());
        cells.push(match covers[i] {
            0 => String::new(),
            n => format!("caps {n} rows"),
        });
        Row::new(cells)
    });
    let mut names: Vec<String> = slots.iter().map(|&s| name(header, s)).collect();
    names.extend(["quantity".into(), String::new()]);
    let mut widths = vec![Constraint::Fill(1); slots.len()];
    widths.extend([Constraint::Length(10), Constraint::Length(14)]);
    let shown = format!(
        " {title} ({} of {}) ",
        app.data.rows.len(),
        table.rows().len()
    );
    list_table(frame, list, rows, names, widths, shown, &app.data, range);
}

fn costs(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(loaded) = &app.loaded else {
        return not_loaded(frame, area);
    };
    let (sources, sinks) = loaded.tables();
    let nk = sinks.rows().len();
    let trace_height = if app.show_trace {
        loaded.costmap().layers().len() as u16 + 4
    } else {
        0
    };
    let [filter_area, list, trace_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(trace_height),
    ])
    .areas(area);
    filter_line(frame, filter_area, &app.costs, "cost");

    let (ss, ks) = (named(sources.header()), named(sinks.header()));
    let height = list.height.saturating_sub(3) as usize;
    let range = window(&mut app.costs, height);
    let rows = app.costs.rows[range.clone()].iter().map(|&c| {
        let mut cells = fields(&sources.rows()[c / nk].0, &ss);
        cells.extend(fields(&sinks.rows()[c % nk].0, &ks));
        cells.push(number(loaded.costs[c]));
        Row::new(cells)
    });
    let mut names: Vec<String> = ss
        .iter()
        .map(|&s| format!("source.{}", name(sources.header(), s)))
        .collect();
    names.extend(
        ks.iter()
            .map(|&s| format!("sink.{}", name(sinks.header(), s))),
    );
    names.push("cost".into());
    let mut widths = vec![Constraint::Fill(1); ss.len() + ks.len()];
    widths.push(Constraint::Length(10));
    let sort = match app.sort {
        Sort::Input => "",
        Sort::CostUp => ", cheapest first",
        Sort::CostDown => ", dearest first",
    };
    let hidden = if app.show_hidden {
        ", with no-route pairs"
    } else {
        ""
    };
    let title = format!(
        " Pairs ({} of {}{hidden}{sort}) ",
        app.costs.rows.len(),
        loaded.costs.len()
    );
    list_table(frame, list, rows, names, widths, title, &app.costs, range);

    if app.show_trace {
        trace(frame, trace_area, app);
    }
}

fn trace(frame: &mut Frame, area: Rect, app: &App) {
    let (Some(loaded), Some(cell)) = (&app.loaded, app.costs.current()) else {
        frame.render_widget(Block::bordered().title(" Trace "), area);
        return;
    };
    let (sources, sinks) = loaded.tables();
    let trace = loaded.trace(cell);
    let mut lines = vec![Line::from(vec![
        "base ".dark_gray(),
        match trace.base {
            Some(cost) => number(cost).into(),
            None => "no matrix, pairs start without a route".dark_gray(),
        },
    ])];
    let layers = loaded.costmap().layers();
    let files = &loaded.layer_files;
    let width = files.iter().map(|f| f.chars().count()).max().unwrap_or(0);
    for step in &trace.steps {
        let file = files.get(step.layer).map_or("", String::as_str);
        let mut spans = vec![
            Span::from(format!("{:>2} {:<6}", step.layer + 1, op_label(step.op))).bold(),
            Span::from(format!("{file:<width$}  ")).dark_gray(),
        ];
        match step.rule {
            None => spans.push("no rule matches".dark_gray()),
            Some(r) => {
                let rule = &layers[step.layer].rules[r];
                let mut pattern: Vec<String> = Vec::new();
                for (prefix, key, header) in [
                    ("source", &rule.source, sources.header()),
                    ("sink", &rule.sink, sinks.header()),
                ] {
                    pattern.extend(named(header).into_iter().filter_map(|s| {
                        key.get_slot(s)
                            .map(|v| format!("{prefix}.{}={v}", name(header, s)))
                    }));
                }
                if pattern.is_empty() {
                    pattern.push("every pair".into());
                }
                spans.push(format!("rule {} ", r + 1).into());
                spans.push(pattern.join(" ").cyan());
                if rule.priority != 0 {
                    spans.push(format!("  priority {}", rule.priority).dark_gray());
                }
                if step.op != Op::Block {
                    spans.push(format!("  value {}", number(rule.value)).into());
                }
            }
        }
        spans.push("  → ".dark_gray());
        spans.push(number(step.cost).bold());
        lines.push(Line::from(spans));
    }
    if trace.steps.len() < layers.len() {
        lines.push("   blocked: later layers don't apply".dark_gray().into());
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Trace of the selected pair ")),
        area,
    );
}

fn solve(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(loaded) = &app.loaded else {
        return not_loaded(frame, area);
    };
    let [summary, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
    let problem = loaded.problem();
    let size = format!(
        "{} routes, {} supply and {} demand limits",
        problem.routes.len(),
        problem.supply.len(),
        problem.demand.len()
    );
    let lines: Vec<Line> = match &app.solve {
        SolveState::Idle => vec![
            size.into(),
            "press s to solve with HiGHS".dark_gray().into(),
        ],
        SolveState::Running { started, .. } => vec![
            size.into(),
            format!("solving… {:.1}s", started.elapsed().as_secs_f64())
                .yellow()
                .into(),
        ],
        SolveState::Done {
            result: Ok(a),
            took,
        } => vec![
            size.into(),
            Line::from(vec![
                "objective ".dark_gray(),
                number(a.objective).bold(),
                format!(
                    "   {} flows, {} units, {:.2}s",
                    a.flows.len(),
                    a.flows.iter().map(|f| f.units).sum::<i64>(),
                    took.as_secs_f64()
                )
                .into(),
            ]),
        ],
        SolveState::Done { result: Err(e), .. } => vec![
            Line::from(format!("failed: {e}")).red(),
            app.infeasible_hint().unwrap_or_default().yellow().into(),
        ],
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), summary);

    let SolveState::Done { result: Ok(a), .. } = &app.solve else {
        return;
    };
    let (sources, sinks) = loaded.tables();
    let nk = sinks.rows().len();
    let (ss, ks) = (named(sources.header()), named(sinks.header()));

    if app.show_usage {
        let (mut out, mut into) = (vec![0i64; sources.rows().len()], vec![0i64; nk]);
        for f in &a.flows {
            out[f.source] += f.units;
            into[f.sink] += f.units;
        }
        // unused capacity (the limit's slack) and spare (what the row could still ship under every cap over it)
        let slack = problem.slack(a);
        let [left, right] =
            Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]).areas(rest);
        for (area, title, limits, table, slots, sent, verb, slack) in [
            (
                left,
                " Sources: shipped / capacity, unused, spare ",
                &problem.supply,
                sources,
                &ss,
                &out,
                "shipped",
                Some(&slack),
            ),
            (
                right,
                " Sinks: received / demand ",
                &problem.demand,
                sinks,
                &ks,
                &into,
                "received",
                None,
            ),
        ] {
            let rows = limits
                .iter()
                .zip(table.rows())
                .enumerate()
                .map(|(i, (limit, (key, _)))| {
                    let used: i64 = limit.rows.iter().map(|&r| sent[r]).sum();
                    let mut cells = fields(key, slots);
                    cells.push(format!("{used} / {}", limit.amount));
                    if let Some(slack) = slack {
                        cells.extend([slack[i].unused().to_string(), slack[i].spare.to_string()]);
                    }
                    let row = Row::new(cells);
                    if used == limit.amount {
                        row.bold()
                    } else {
                        row
                    }
                });
            let mut names: Vec<String> = slots.iter().map(|&s| name(table.header(), s)).collect();
            names.push(verb.into());
            let mut widths = vec![Constraint::Fill(1); slots.len()];
            widths.push(Constraint::Length(14));
            if slack.is_some() {
                names.extend(["unused".into(), "spare".into()]);
                widths.extend([Constraint::Length(8), Constraint::Length(8)]);
            }
            frame.render_widget(
                Table::new(rows, widths)
                    .header(Row::new(names).dark_gray())
                    .block(Block::bordered().title(title)),
                area,
            );
        }
        return;
    }

    let [filter_area, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(rest);
    filter_line(frame, filter_area, &app.flows, "units");
    let height = list.height.saturating_sub(3) as usize;
    let range = window(&mut app.flows, height);
    let rows = app.flows.rows[range.clone()].iter().map(|&f| {
        let flow = &a.flows[f];
        let mut cells = fields(&sources.rows()[flow.source].0, &ss);
        cells.extend(fields(&sinks.rows()[flow.sink].0, &ks));
        cells.push(flow.units.to_string());
        cells.push(number(loaded.costs[flow.source * nk + flow.sink]));
        Row::new(cells)
    });
    let mut names: Vec<String> = ss
        .iter()
        .map(|&s| format!("source.{}", name(sources.header(), s)))
        .collect();
    names.extend(
        ks.iter()
            .map(|&s| format!("sink.{}", name(sinks.header(), s))),
    );
    names.extend(["units".into(), "cost".into()]);
    let mut widths = vec![Constraint::Fill(1); ss.len() + ks.len()];
    widths.extend([Constraint::Length(8), Constraint::Length(10)]);
    let title = format!(" Flows ({} of {}) ", app.flows.rows.len(), a.flows.len());
    list_table(frame, list, rows, names, widths, title, &app.flows, range);
}

fn save(frame: &mut Frame, area: Rect, app: &mut App) {
    let formats: Vec<&str> = [
        (Format::Csv, ".csv"),
        (Format::Json, ".json"),
        (Format::Excel, ".xlsx"),
    ]
    .into_iter()
    .filter(|(f, _)| f.enabled())
    .map(|(_, e)| e)
    .collect();
    let ready = match app.assignment() {
        Some(a) => format!("{} flows ready to save", a.flows.len()).green(),
        None => "nothing to save yet: solve on the Solve tab".yellow(),
    };
    let block = Block::bordered().title(" Output files ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [flows, slack, notes] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .areas(inner);
    for (row, area, label, edit) in [
        (0, flows, "Flows:  ", &app.output),
        (1, slack, "Unused: ", &app.slack_output),
    ] {
        let selected = app.save_row == row;
        let [marker, line] =
            Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(if selected { "›" } else { " " }).bold(),
            marker,
        );
        edit_line(frame, line, label, edit, selected && app.editing_output);
    }
    frame.render_widget(
        Paragraph::new(vec![
            ready.into(),
            Line::from(
                "Unused writes each source row's used, capacity, unused (capacity - used) and spare \
                 (what it could still ship under every cap over it).",
            )
            .dark_gray(),
            Line::from(format!(
                "The format comes from the extension: {}",
                formats.join(", ")
            ))
            .dark_gray(),
        ])
        .wrap(Wrap { trim: true }),
        notes,
    );
}

fn not_loaded(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("Nothing loaded yet: pick the inputs on the Inputs tab and press l.")
            .dark_gray()
            .block(Block::bordered()),
        area,
    );
}

// ----- popups -----

fn popup(frame: &mut Frame, app: &mut App) {
    let area = centered(frame.area(), 80, 70);
    frame.render_widget(Clear, area);
    match app.popup.as_mut().expect("popup") {
        Popup::Help => {
            let lines: Vec<Line> = HELP.lines().map(Line::from).collect();
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(" Help (Esc closes) ")),
                area,
            );
        }
        Popup::Files {
            slot,
            query,
            all,
            matches,
            selected,
        } => {
            let title = match slot {
                Slot::Sources => " Sources file ",
                Slot::Caps => " Source caps file (each row caps the source rows it matches) ",
                Slot::Sinks => " Sinks file ",
                Slot::Base => " Base file (a matrix, or rules applied first) ",
                Slot::Layer(_) | Slot::NewLayer => " Layer file ",
            };
            let block = Block::bordered().title(title);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [input, list] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
            edit_line(frame, input, "Search: ", query, true);
            let height = list.height as usize;
            let start = selected.saturating_sub(height.saturating_sub(1));
            let root = &app.root;
            let lines: Vec<Line> = matches
                .iter()
                .enumerate()
                .skip(start)
                .take(height)
                .map(|(n, &i)| {
                    let shown = all[i].strip_prefix(root).unwrap_or(&all[i]);
                    let line = Line::from(shown.display().to_string());
                    if n == *selected {
                        line.style(SELECTED)
                    } else {
                        line
                    }
                })
                .collect();
            if lines.is_empty() {
                frame.render_widget(Paragraph::new("no matching files").dark_gray(), list);
            } else {
                frame.render_widget(Paragraph::new(lines), list);
            }
        }
        Popup::Pick {
            purpose,
            title,
            options,
            selected,
        } => {
            let file = match purpose {
                Pick::Sheet { path, .. } => path.display().to_string(),
                Pick::Quantity { file, .. } => file.label(),
            };
            let lines: Vec<Line> = options
                .iter()
                .enumerate()
                .map(|(n, o)| {
                    let line = Line::from(o.as_str());
                    if n == *selected {
                        line.style(SELECTED)
                    } else {
                        line
                    }
                })
                .collect();
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(format!(" {title}: {file} "))),
                area,
            );
        }
        Popup::Prompt { title, edit, .. } => {
            let area = Rect { height: 3, ..area };
            let block = Block::bordered().title(format!(" {title} "));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            edit_line(frame, inner, "", edit, true);
        }
    }
}

const HELP: &str = "\
Tabs        1-5 or Tab / Shift-Tab. l (re)loads the inputs from any tab.
            q quits (Esc out of a text field first); Ctrl+C quits from anywhere.

Inputs      Enter opens the file search for the selected row. Excel files then ask for a
            sheet; sources and sinks ask for the quantity column.
            ←/→ change a layer's operator (set, scale, add, block); [ and ] move it.
            x removes the row. w saves the scenario, o opens one, r changes the search root.
            The base takes a headerless matrix, or a rule table (source./sink. columns)
            that is applied as a set layer before the others.

Filters     / on Data, Costs and Solve. Terms must all match:
              business=apple        field contains `apple` (any case)
              size=                 field is empty
              source.store=ohio     pick a side when both have the column
              apple                 any field contains `apple`
              cost>5  cost<=2       compare the cost (quantity on Data, units on Solve)
            Quote values with spaces: business=\"apple inc\". Esc clears the filter.

Costs       o cycles the sort, h shows pairs with no route, t toggles the trace of
            the selected pair: the base, then each layer's winning rule and the cost after it.

Solve       s runs HiGHS in the background. u switches between the flows and each
            row's use against its capacity or demand (partial rows count every row they cap).
            Sources also show unused capacity (capacity - used) and spare: what the row could
            still ship before it or any partial row over it is full.

Save        ↑/↓ pick the flows or the unused capacity path. Enter edits it, Enter again
            writes it. The extension picks the format.";

// ----- helpers -----

/// Keeps the selection visible and returns the rows to draw.
fn window(view: &mut ListView, height: usize) -> Range<usize> {
    let height = height.max(1);
    if view.selected < view.offset {
        view.offset = view.selected;
    }
    if view.selected >= view.offset + height {
        view.offset = view.selected + 1 - height;
    }
    view.offset = view.offset.min(view.rows.len().saturating_sub(1));
    view.offset.min(view.rows.len())..(view.offset + height).min(view.rows.len())
}

#[allow(clippy::too_many_arguments)]
fn list_table<'a>(
    frame: &mut Frame,
    area: Rect,
    rows: impl IntoIterator<Item = Row<'a>>,
    names: Vec<String>,
    widths: Vec<Constraint>,
    title: String,
    view: &ListView,
    range: Range<usize>,
) {
    let table = Table::new(rows, widths)
        .header(Row::new(names).dark_gray())
        .row_highlight_style(SELECTED)
        .highlight_symbol("› ")
        .block(Block::new().borders(Borders::ALL).title(title));
    let selected = (!view.rows.is_empty()).then(|| view.selected - range.start);
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn filter_line(frame: &mut Frame, area: Rect, view: &ListView, number: &str) {
    if let Some(error) = &view.error
        && !view.editing
    {
        let line = Line::from(vec![
            "Filter: ".dark_gray(),
            view.filter.text.as_str().into(),
            "  ".into(),
            error.as_str().red(),
        ]);
        return frame.render_widget(Paragraph::new(line), area);
    }
    if view.editing || !view.filter.text.is_empty() {
        edit_line(frame, area, "Filter: ", &view.filter, view.editing);
    } else {
        frame.render_widget(
            Paragraph::new(format!("/ to filter, e.g. business=apple {number}>2")).dark_gray(),
            area,
        );
    }
}

fn edit_line(frame: &mut Frame, area: Rect, label: &str, edit: &LineEdit, active: bool) {
    let style = if active {
        Style::new().fg(Color::White)
    } else {
        Style::new()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::from(label).dark_gray(),
            Span::styled(edit.text.as_str(), style),
        ])),
        area,
    );
    if active {
        let x = area.x + (label.chars().count() + edit.cursor) as u16;
        frame.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), area.y));
    }
}

fn centered(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    area.centered(
        Constraint::Percentage(width_percent),
        Constraint::Percentage(height_percent),
    )
}

fn rules(n: usize) -> String {
    match n {
        1 => "1 rule".into(),
        n => format!("{n} rules"),
    }
}

/// The slots that have a column name.
fn named(header: &Header) -> Vec<usize> {
    (0..header.len()).filter(|&s| header[s].is_some()).collect()
}

fn name(header: &Header, slot: usize) -> String {
    header[slot].as_deref().unwrap_or_default().to_string()
}

fn fields(key: &Key, slots: &[usize]) -> Vec<String> {
    slots
        .iter()
        .map(|&s| key.get_slot(s).unwrap_or("").to_string())
        .collect()
}

/// A cost or objective: integers plainly, others to 4 decimals, `NaN` as no route.
fn number(value: f64) -> String {
    if value.is_nan() {
        return "no route".into();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[cfg_attr(not(feature = "csv"), allow(dead_code))]
    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[cfg(feature = "csv")]
    #[test]
    fn every_tab_draws_the_loaded_scenario() {
        let mut app = crate::app::tests::solved("apples.polu.json");
        app.tab = Tab::Inputs;
        let text = screen(&mut app);
        assert!(text.contains("costs-scale.csv"), "{text}");
        assert!(text.contains("‹ block ›"));
        assert!(text.contains("4 rows"));

        app.tab = Tab::Costs;
        let text = screen(&mut app);
        assert!(text.contains("Pairs (5 of 8)"), "{text}");
        assert!(text.contains("ohio fruits"));
        assert!(text.contains("Trace of the selected pair"));

        app.tab = Tab::Solve;
        let text = screen(&mut app);
        assert!(text.contains("objective 20"), "{text}");
        app.show_usage = true;
        let text = screen(&mut app);
        assert!(text.contains("5 / 5"), "{text}");
        assert!(text.contains("unused") && text.contains("spare"), "{text}");
        assert!(text.contains("4 / 20"), "{text}");

        app.tab = Tab::Data;
        assert!(screen(&mut app).contains("caps 2 rows"));
        app.tab = Tab::Save;
        let text = screen(&mut app);
        assert!(text.contains("flows ready to save"), "{text}");
        assert!(text.contains("slack.csv"), "{text}");
        app.popup = Some(Popup::Help);
        assert!(screen(&mut app).contains("Filters"));
    }

    #[test]
    fn numbers_read_plainly() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(2.5), "2.5");
        assert_eq!(number(1.0 / 3.0), "0.3333");
        assert_eq!(number(f64::NAN), "no route");
    }
}
