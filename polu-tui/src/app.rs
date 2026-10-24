//! The TUI's state and key handling. Nothing here draws, so tests can drive it without a terminal.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use polu_core::{
    Assignment, BoxError, CostMap, Header, HighsSolver, Key, Load, Op, Polu, Problem, SinkList,
    Solver, SourceList, Store, Table, Trace,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::io::{self, FileRef, LayerInput, OPS, Scenario, TableInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Inputs,
    Data,
    Costs,
    Solve,
    Save,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Inputs, Tab::Data, Tab::Costs, Tab::Solve, Tab::Save];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Inputs => "Inputs",
            Tab::Data => "Data",
            Tab::Costs => "Costs",
            Tab::Solve => "Solve",
            Tab::Save => "Save",
        }
    }
}

/// A row of the Inputs tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Sources,
    Sinks,
    Base,
    Layer(usize),
    NewLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Sources,
    Sinks,
}

/// One line of editable text; `cursor` counts chars.
#[derive(Debug, Clone, Default)]
pub struct LineEdit {
    pub text: String,
    pub cursor: usize,
}

impl LineEdit {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    fn byte(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Applies an editing key; returns false for keys it doesn't use.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let len = self.text.chars().count();
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Char('u') => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }
}

/// A scrollable list of row indexes with a filter line.
#[derive(Debug, Default)]
pub struct ListView {
    pub filter: LineEdit,
    pub editing: bool,
    /// The rows shown, as indexes into whatever the view lists.
    pub rows: Vec<usize>,
    pub selected: usize,
    /// First visible row, kept by the drawing code.
    pub offset: usize,
    pub error: Option<String>,
}

impl ListView {
    fn scroll(&mut self, key: KeyCode, page: usize) -> bool {
        let last = self.rows.len().saturating_sub(1);
        self.selected = match key {
            KeyCode::Up | KeyCode::Char('k') => self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected.saturating_sub(page),
            KeyCode::PageDown => (self.selected + page).min(last),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            _ => return false,
        };
        true
    }

    pub fn current(&self) -> Option<usize> {
        self.rows.get(self.selected).copied()
    }

    fn set_rows(&mut self, rows: Vec<usize>) {
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Input,
    CostUp,
    CostDown,
}

pub enum Popup {
    Files {
        slot: Slot,
        query: LineEdit,
        all: Vec<PathBuf>,
        matches: Vec<usize>,
        selected: usize,
    },
    Pick {
        purpose: Pick,
        title: String,
        options: Vec<String>,
        selected: usize,
    },
    Prompt {
        purpose: Prompt,
        title: String,
        edit: LineEdit,
    },
    Help,
}

pub enum Pick {
    Sheet { slot: Slot, path: PathBuf },
    Quantity { slot: Slot, file: FileRef },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    SaveScenario,
    OpenScenario,
    Root,
}

/// What a load produced. The Polu keeps the tables, costmap and problem.
pub struct Loaded {
    pub polu: Polu,
    /// Every pair's cost, row-major `sources × sinks`; `NaN` means no route.
    pub costs: Vec<f64>,
    /// How many other rows each source and sink row caps.
    pub covers: (Vec<usize>, Vec<usize>),
    /// The file behind each costmap layer, for traces.
    pub layer_files: Vec<String>,
    /// The base is a rule table, loaded as the first costmap layer.
    pub base_layer: bool,
    pub took: Duration,
}

impl Loaded {
    pub fn tables(&self) -> (&SourceList, &SinkList) {
        self.polu.tables().expect("loaded")
    }

    pub fn costmap(&self) -> &CostMap {
        self.polu.costmap().expect("loaded")
    }

    pub fn problem(&self) -> &Problem {
        self.polu.problem().expect("loaded")
    }

    pub fn routes(&self) -> usize {
        self.problem().routes.len()
    }

    pub fn trace(&self, cell: usize) -> Trace {
        let (sources, sinks) = self.tables();
        let nk = sinks.rows().len();
        self.costmap().trace(sources, sinks, cell / nk, cell % nk)
    }
}

pub enum SolveState {
    Idle,
    Running {
        rx: Receiver<Result<Assignment, String>>,
        started: Instant,
    },
    Done {
        result: Result<Assignment, String>,
        took: Duration,
    },
}

pub struct App {
    pub tab: Tab,
    pub root: PathBuf,
    pub scenario: Scenario,
    pub scenario_path: Option<PathBuf>,
    pub slot: usize,
    pub popup: Option<Popup>,
    pub loaded: Option<Loaded>,
    /// Inputs changed since the last load.
    pub stale: bool,
    pub message: Option<(bool, String)>,
    pub data: ListView,
    pub data_side: Side,
    pub costs: ListView,
    pub sort: Sort,
    pub show_hidden: bool,
    pub show_trace: bool,
    pub solve: SolveState,
    pub flows: ListView,
    pub show_usage: bool,
    pub output: LineEdit,
    pub editing_output: bool,
    /// The path the user was warned would be overwritten.
    pub overwrite: Option<PathBuf>,
    pub quit: bool,
}

/// Rows a page key moves by.
const PAGE: usize = 20;
/// Files listed by the file search.
const MAX_FILES: usize = 20_000;

impl App {
    pub fn new(root: PathBuf) -> Self {
        let output = LineEdit::new(root.join("flows.csv").display().to_string());
        Self {
            tab: Tab::Inputs,
            root,
            scenario: Scenario::default(),
            scenario_path: None,
            slot: 0,
            popup: None,
            loaded: None,
            stale: false,
            message: None,
            data: ListView::default(),
            data_side: Side::Sources,
            costs: ListView::default(),
            sort: Sort::Input,
            show_hidden: false,
            show_trace: true,
            solve: SolveState::Idle,
            flows: ListView::default(),
            show_usage: false,
            output,
            editing_output: false,
            overwrite: None,
            quit: false,
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some((false, text.into()));
    }

    pub fn error(&mut self, text: impl std::fmt::Display) {
        self.message = Some((true, text.to_string()));
    }

    pub fn slots(&self) -> Vec<Slot> {
        let mut slots = vec![Slot::Sources, Slot::Sinks, Slot::Base];
        slots.extend((0..self.scenario.layers.len()).map(Slot::Layer));
        slots.push(Slot::NewLayer);
        slots
    }

    /// True while a text field takes the keys, so letters don't trigger commands.
    pub fn typing(&self) -> bool {
        matches!(self.popup, Some(Popup::Files { .. } | Popup::Prompt { .. }))
            || self.data.editing
            || self.costs.editing
            || self.flows.editing
            || self.editing_output
    }

    pub fn open_scenario(&mut self, path: &Path) {
        match Scenario::open(path) {
            Ok(scenario) => {
                self.scenario = scenario;
                self.scenario_path = Some(path.to_path_buf());
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    self.root = dir.to_path_buf();
                }
                self.changed();
                self.info(format!("opened {}", path.display()));
            }
            Err(e) => self.error(e),
        }
    }

    fn changed(&mut self) {
        self.stale = self.loaded.is_some();
        self.message = None;
    }

    /// Loads every input and formulates the problem.
    pub fn load(&mut self) {
        let started = Instant::now();
        match self.try_load() {
            Ok((polu, scenario)) => {
                let problem = polu.problem().expect("loaded");
                let (sources, sinks) = polu.tables().expect("loaded");
                let (ns, nk) = (sources.rows().len(), sinks.rows().len());
                let mut costs = vec![f64::NAN; ns * nk];
                for route in &problem.routes {
                    costs[route.source * nk + route.sink] = route.cost;
                }
                let covers = |limits: &[polu_core::Limit]| {
                    limits.iter().map(|l| l.rows.len() - 1).collect::<Vec<_>>()
                };
                let covers = (covers(&problem.supply), covers(&problem.demand));
                let base_rules = scenario.base.as_ref().filter(|b| io::base_is_rules(b));
                let mut layer_files: Vec<String> = base_rules
                    .map(|b| format!("base {}", b.label()))
                    .into_iter()
                    .collect();
                layer_files.extend(scenario.layers.iter().map(|l| l.file.label()));
                let base_layer = base_rules.is_some();
                let loaded = Loaded {
                    polu,
                    costs,
                    covers,
                    layer_files,
                    base_layer,
                    took: started.elapsed(),
                };
                self.info(format!(
                    "loaded {ns} sources, {nk} sinks, {} routes in {:.2}s",
                    loaded.routes(),
                    loaded.took.as_secs_f64()
                ));
                self.loaded = Some(loaded);
                self.stale = false;
                self.solve = SolveState::Idle;
                self.flows = ListView::default();
                for view in [&mut self.data, &mut self.costs] {
                    view.selected = 0;
                }
                self.refilter_data();
                self.refilter_costs();
            }
            Err(e) => self.error(e),
        }
    }

    fn try_load(&self) -> Result<(Polu, Scenario), BoxError> {
        let sources = self
            .scenario
            .sources
            .as_ref()
            .ok_or("pick a sources file")?;
        let sinks = self.scenario.sinks.as_ref().ok_or("pick a sinks file")?;
        let (source_loader, source_header) = io::table_loader(sources)?;
        let (sink_loader, sink_header) = io::table_loader(sinks)?;
        let scenario = self.scenario.clone();
        let for_costmap = scenario.clone();
        let costmap = Deferred(move || io::costmap(&for_costmap, &source_header, &sink_header));
        let mut polu = Polu::new(
            source_loader,
            sink_loader,
            Box::new(costmap),
            Box::new(HighsSolver::default()),
            Box::new(NoStore),
        );
        polu.load_data()?;
        Ok((polu, scenario))
    }

    pub fn refilter_data(&mut self) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let (sources, sinks) = loaded.tables();
        let table = match self.data_side {
            Side::Sources => sources,
            Side::Sinks => sinks,
        };
        let empty = Header::default();
        match Filter::parse(&self.data.filter.text, table.header(), &empty, "quantity") {
            Ok(filter) => {
                let ok = filter.row_matches(table, &empty_table());
                let rows = (0..table.rows().len())
                    .filter(|&i| filter.pair(&ok, i, 0) && filter.number(table.rows()[i].1 as f64))
                    .collect();
                self.data.set_rows(rows);
                self.data.error = None;
            }
            Err(e) => self.data.error = Some(e),
        }
    }

    pub fn refilter_costs(&mut self) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let (sources, sinks) = loaded.tables();
        let filter = match Filter::parse(
            &self.costs.filter.text,
            sources.header(),
            sinks.header(),
            "cost",
        ) {
            Ok(filter) => filter,
            Err(e) => {
                self.costs.error = Some(e);
                return;
            }
        };
        let ok = filter.row_matches(sources, sinks);
        let nk = sinks.rows().len();
        let mut rows: Vec<usize> = (0..loaded.costs.len())
            .filter(|&c| {
                let cost = loaded.costs[c];
                (self.show_hidden || !cost.is_nan())
                    && filter.pair(&ok, c / nk, c % nk)
                    && (filter.numbers.is_empty() || filter.number(cost))
            })
            .collect();
        let cost = |c: &usize| loaded.costs[*c];
        match self.sort {
            Sort::Input => {}
            // NaN (no route) sorts last either way
            Sort::CostUp => rows.sort_by(|a, b| cost(a).total_cmp(&cost(b))),
            Sort::CostDown => rows.sort_by(|a, b| match (cost(a).is_nan(), cost(b).is_nan()) {
                (false, false) => cost(b).total_cmp(&cost(a)),
                (x, y) => x.cmp(&y),
            }),
        }
        self.costs.set_rows(rows);
        self.costs.error = None;
    }

    pub fn refilter_flows(&mut self) {
        let (Some(loaded), SolveState::Done { result: Ok(a), .. }) = (&self.loaded, &self.solve)
        else {
            return;
        };
        let (sources, sinks) = loaded.tables();
        match Filter::parse(
            &self.flows.filter.text,
            sources.header(),
            sinks.header(),
            "units",
        ) {
            Ok(filter) => {
                let ok = filter.row_matches(sources, sinks);
                let rows = (0..a.flows.len())
                    .filter(|&f| {
                        let flow = &a.flows[f];
                        filter.pair(&ok, flow.source, flow.sink) && filter.number(flow.units as f64)
                    })
                    .collect();
                self.flows.set_rows(rows);
                self.flows.error = None;
            }
            Err(e) => self.flows.error = Some(e),
        }
    }

    /// Starts HiGHS on a copy of the problem in a background thread.
    pub fn start_solve(&mut self) {
        if matches!(self.solve, SolveState::Running { .. }) {
            return;
        }
        let Some(loaded) = &self.loaded else {
            return self.error("load the inputs first (l on the Inputs tab)");
        };
        let problem = loaded.problem().clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = HighsSolver::default()
                .solve(&problem)
                .map_err(|e| e.to_string());
            tx.send(result).ok();
        });
        self.solve = SolveState::Running {
            rx,
            started: Instant::now(),
        };
        self.info("solving…");
    }

    /// Picks up a finished solve; call on every tick.
    pub fn poll(&mut self) {
        let SolveState::Running { rx, started } = &self.solve else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the solver thread stopped".into()),
        };
        let took = started.elapsed();
        match &result {
            Ok(a) => self.info(format!(
                "solved in {:.2}s: objective {}, {} flows",
                took.as_secs_f64(),
                a.objective,
                a.flows.len()
            )),
            Err(e) => self.error(format!("solve failed: {e}")),
        }
        self.solve = SolveState::Done { result, took };
        self.flows.selected = 0;
        self.refilter_flows();
    }

    /// Waits for a running solve, for tests.
    #[cfg(test)]
    pub fn wait(&mut self) {
        while matches!(self.solve, SolveState::Running { .. }) {
            std::thread::sleep(Duration::from_millis(5));
            self.poll();
        }
    }

    pub fn assignment(&self) -> Option<&Assignment> {
        match &self.solve {
            SolveState::Done { result: Ok(a), .. } => Some(a),
            _ => None,
        }
    }

    /// Writes the assignment with the store the output path's extension picks.
    pub fn save_output(&mut self) {
        let path = PathBuf::from(self.output.text.trim());
        if self.assignment().is_none() {
            return self.error("solve first (Solve tab)");
        }
        if path.exists() && self.overwrite.as_ref() != Some(&path) {
            self.overwrite = Some(path.clone());
            return self.error(format!(
                "{} exists, press Enter again to overwrite",
                path.display()
            ));
        }
        self.overwrite = None;
        let (Some(loaded), Some(assignment)) = (&self.loaded, self.assignment()) else {
            return;
        };
        let (sources, sinks) = loaded.tables();
        let flows = assignment.flows.len();
        let result = io::store(&path).and_then(|store| store.store(assignment, sources, sinks));
        match result {
            Ok(()) => self.info(format!("saved {flows} flows to {}", path.display())),
            Err(e) => self.error(format!("save failed: {e}")),
        }
    }

    /// Why a solve might be infeasible: totals over the rows nothing else caps, and sinks with no route.
    pub fn infeasible_hint(&self) -> Option<String> {
        let loaded = self.loaded.as_ref()?;
        let problem = loaded.problem();
        let top = |limits: &[polu_core::Limit]| -> i64 {
            let mut covered = vec![false; limits.len()];
            for limit in limits {
                limit.rows[1..].iter().for_each(|&r| covered[r] = true);
            }
            limits
                .iter()
                .zip(&covered)
                .filter(|(_, c)| !**c)
                .map(|(l, _)| l.amount)
                .sum()
        };
        let mut routed = vec![false; problem.demand.len()];
        problem.routes.iter().for_each(|r| routed[r.sink] = true);
        let unrouted = routed.iter().filter(|r| !**r).count();
        Some(format!(
            "supply at most {}, demand {}, sinks with no route: {unrouted}",
            top(&problem.supply),
            top(&problem.demand)
        ))
    }

    // ----- keys -----

    pub fn handle(&mut self, key: KeyEvent) {
        // raw mode turns Ctrl+C into a key press, so it quits from anywhere, even mid-edit
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.quit = true;
        }
        if self.popup.is_some() {
            return self.handle_popup(key);
        }
        if self.typing() {
            return self.handle_typing(key);
        }
        match key.code {
            KeyCode::Char('q') => return self.quit = true,
            KeyCode::Char('?') => return self.popup = Some(Popup::Help),
            KeyCode::Char(c @ '1'..='5') => {
                return self.tab = Tab::ALL[c as usize - '1' as usize];
            }
            KeyCode::Tab => return self.tab = Tab::ALL[(self.tab as usize + 1) % 5],
            KeyCode::BackTab => return self.tab = Tab::ALL[(self.tab as usize + 4) % 5],
            KeyCode::Char('l') => return self.load(),
            KeyCode::Esc => return self.message = None,
            _ => {}
        }
        match self.tab {
            Tab::Inputs => self.handle_inputs(key),
            Tab::Data => {
                if !self.data.scroll(key.code, PAGE) {
                    match key.code {
                        KeyCode::Char('/') => self.data.editing = true,
                        KeyCode::Char('s') => {
                            self.data_side = match self.data_side {
                                Side::Sources => Side::Sinks,
                                Side::Sinks => Side::Sources,
                            };
                            self.data.filter = LineEdit::default();
                            self.data.selected = 0;
                            self.refilter_data();
                        }
                        _ => {}
                    }
                }
            }
            Tab::Costs => {
                if !self.costs.scroll(key.code, PAGE) {
                    match key.code {
                        KeyCode::Char('/') => self.costs.editing = true,
                        KeyCode::Char('o') => {
                            self.sort = match self.sort {
                                Sort::Input => Sort::CostUp,
                                Sort::CostUp => Sort::CostDown,
                                Sort::CostDown => Sort::Input,
                            };
                            self.refilter_costs();
                        }
                        KeyCode::Char('h') => {
                            self.show_hidden = !self.show_hidden;
                            self.refilter_costs();
                        }
                        KeyCode::Char('t') | KeyCode::Enter => self.show_trace = !self.show_trace,
                        _ => {}
                    }
                }
            }
            Tab::Solve => {
                if !self.flows.scroll(key.code, PAGE) {
                    match key.code {
                        KeyCode::Char('s') | KeyCode::Enter => self.start_solve(),
                        KeyCode::Char('/') => self.flows.editing = true,
                        KeyCode::Char('u') => self.show_usage = !self.show_usage,
                        _ => {}
                    }
                }
            }
            Tab::Save => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char('e')) {
                    self.editing_output = true;
                }
            }
        }
    }

    fn handle_typing(&mut self, key: KeyEvent) {
        let done = matches!(key.code, KeyCode::Enter | KeyCode::Esc);
        if self.editing_output {
            if key.code == KeyCode::Enter {
                self.save_output();
            }
            if done {
                self.editing_output = false;
            } else {
                self.overwrite = None;
                self.output.handle(key);
            }
            return;
        }
        let (view, refilter): (&mut ListView, fn(&mut App)) = if self.data.editing {
            (&mut self.data, App::refilter_data)
        } else if self.costs.editing {
            (&mut self.costs, App::refilter_costs)
        } else {
            (&mut self.flows, App::refilter_flows)
        };
        if done {
            view.editing = false;
            if key.code == KeyCode::Esc {
                view.filter = LineEdit::default();
            }
            view.selected = 0;
            refilter(self);
        } else {
            view.filter.handle(key);
        }
    }

    fn handle_inputs(&mut self, key: KeyEvent) {
        let slots = self.slots();
        let slot = slots[self.slot.min(slots.len() - 1)];
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.slot = self.slot.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.slot = (self.slot + 1).min(slots.len() - 1),
            KeyCode::Enter | KeyCode::Char('f') => self.open_files(slot),
            KeyCode::Left | KeyCode::Right => {
                if let Slot::Layer(l) = slot {
                    let layer = &mut self.scenario.layers[l];
                    let at = OPS.iter().position(|op| *op == layer.op).unwrap_or(0);
                    let step = if key.code == KeyCode::Right { 1 } else { 3 };
                    layer.op = OPS[(at + step) % OPS.len()];
                    self.changed();
                }
            }
            KeyCode::Char('[') | KeyCode::Char(']') => {
                if let Slot::Layer(l) = slot {
                    let to = if key.code == KeyCode::Char('[') {
                        l.checked_sub(1)
                    } else {
                        Some(l + 1).filter(|&t| t < self.scenario.layers.len())
                    };
                    if let Some(to) = to {
                        self.scenario.layers.swap(l, to);
                        self.slot = 3 + to;
                        self.changed();
                    }
                }
            }
            KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                match slot {
                    Slot::Sources => self.scenario.sources = None,
                    Slot::Sinks => self.scenario.sinks = None,
                    Slot::Base => self.scenario.base = None,
                    Slot::Layer(l) => {
                        self.scenario.layers.remove(l);
                    }
                    Slot::NewLayer => return,
                }
                self.changed();
            }
            KeyCode::Char('w') => {
                let path = self
                    .scenario_path
                    .clone()
                    .unwrap_or_else(|| self.root.join("scenario.polu.json"));
                self.popup = Some(Popup::Prompt {
                    purpose: Prompt::SaveScenario,
                    title: "Save scenario as".into(),
                    edit: LineEdit::new(path.display().to_string()),
                });
            }
            KeyCode::Char('o') => {
                self.popup = Some(Popup::Prompt {
                    purpose: Prompt::OpenScenario,
                    title: "Open scenario".into(),
                    edit: LineEdit::new(self.root.join("scenario.polu.json").display().to_string()),
                });
            }
            KeyCode::Char('r') => {
                self.popup = Some(Popup::Prompt {
                    purpose: Prompt::Root,
                    title: "Search files under".into(),
                    edit: LineEdit::new(self.root.display().to_string()),
                });
            }
            _ => {}
        }
    }

    pub fn open_files(&mut self, slot: Slot) {
        let all = io::walk(&self.root, MAX_FILES);
        let matches = (0..all.len()).collect();
        self.popup = Some(Popup::Files {
            slot,
            query: LineEdit::default(),
            all,
            matches,
            selected: 0,
        });
    }

    fn handle_popup(&mut self, key: KeyEvent) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        if key.code == KeyCode::Esc {
            return;
        }
        match popup {
            Popup::Help => {}
            Popup::Files {
                slot,
                mut query,
                all,
                mut matches,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Enter => {
                        if let Some(&i) = matches.get(selected) {
                            return self.picked_file(slot, all[i].clone());
                        }
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(matches.len().saturating_sub(1)),
                    KeyCode::PageUp => selected = selected.saturating_sub(PAGE),
                    KeyCode::PageDown => {
                        selected = (selected + PAGE).min(matches.len().saturating_sub(1))
                    }
                    _ => {
                        if query.handle(key) {
                            matches = rank(&query.text, &all, &self.root);
                            selected = 0;
                        }
                    }
                }
                self.popup = Some(Popup::Files {
                    slot,
                    query,
                    all,
                    matches,
                    selected,
                });
            }
            Popup::Pick {
                purpose,
                title,
                options,
                mut selected,
            } => match key.code {
                KeyCode::Enter => {
                    let choice = options[selected].clone();
                    self.picked(purpose, choice);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.popup = Some(Popup::Pick {
                        purpose,
                        title,
                        options,
                        selected,
                    });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(options.len() - 1);
                    self.popup = Some(Popup::Pick {
                        purpose,
                        title,
                        options,
                        selected,
                    });
                }
                _ => {
                    self.popup = Some(Popup::Pick {
                        purpose,
                        title,
                        options,
                        selected,
                    })
                }
            },
            Popup::Prompt {
                purpose,
                title,
                mut edit,
            } => {
                if key.code != KeyCode::Enter {
                    edit.handle(key);
                    self.popup = Some(Popup::Prompt {
                        purpose,
                        title,
                        edit,
                    });
                    return;
                }
                let path = PathBuf::from(edit.text.trim());
                match purpose {
                    Prompt::SaveScenario => match self.scenario.save(&path) {
                        Ok(()) => {
                            self.info(format!("saved {}", path.display()));
                            self.scenario_path = Some(path);
                        }
                        Err(e) => self.error(e),
                    },
                    Prompt::OpenScenario => self.open_scenario(&path),
                    Prompt::Root if path.is_dir() => self.root = path,
                    Prompt::Root => self.error(format!("{} is not a folder", path.display())),
                }
            }
        }
    }

    /// After the file search: Excel asks for a sheet, sources and sinks for the quantity column.
    pub fn picked_file(&mut self, slot: Slot, path: PathBuf) {
        match io::sheets(&path) {
            Err(e) => self.error(e),
            Ok(Some(sheets)) => {
                self.popup = Some(Popup::Pick {
                    purpose: Pick::Sheet { slot, path },
                    title: "Sheet".into(),
                    options: sheets,
                    selected: 0,
                });
            }
            Ok(None) => self.picked_input(slot, FileRef { path, sheet: None }),
        }
    }

    fn picked(&mut self, purpose: Pick, choice: String) {
        match purpose {
            Pick::Sheet { slot, path } => self.picked_input(
                slot,
                FileRef {
                    path,
                    sheet: Some(choice),
                },
            ),
            Pick::Quantity { slot, file } => {
                let input = TableInput {
                    file,
                    quantity: choice,
                };
                match slot {
                    Slot::Sinks => self.scenario.sinks = Some(input),
                    _ => self.scenario.sources = Some(input),
                }
                self.changed();
            }
        }
    }

    fn picked_input(&mut self, slot: Slot, file: FileRef) {
        match slot {
            Slot::Sources | Slot::Sinks => match io::column_names(&file) {
                Ok(columns) => {
                    let guess = ["capacity", "supply", "demand", "quantity", "qty", "units"];
                    let selected = columns
                        .iter()
                        .position(|c| guess.contains(&c.to_ascii_lowercase().as_str()))
                        .unwrap_or(columns.len().saturating_sub(1));
                    self.popup = Some(Popup::Pick {
                        purpose: Pick::Quantity { slot, file },
                        title: "Quantity column".into(),
                        options: columns,
                        selected,
                    });
                }
                Err(e) => self.error(e),
            },
            Slot::Base => {
                self.scenario.base = Some(file);
                self.changed();
            }
            Slot::Layer(l) => {
                self.scenario.layers[l].file = file;
                self.changed();
            }
            Slot::NewLayer => {
                let op = if self.scenario.layers.is_empty() && self.scenario.base.is_none() {
                    Op::Set
                } else {
                    Op::Scale
                };
                self.scenario.layers.push(LayerInput { file, op });
                self.slot = 3 + self.scenario.layers.len() - 1;
                self.changed();
            }
        }
    }
}

/// Files ranked by fuzzy score on their path under `root`.
fn rank(query: &str, all: &[PathBuf], root: &Path) -> Vec<usize> {
    let mut scored: Vec<(i64, usize)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let shown = p.strip_prefix(root).unwrap_or(p).to_string_lossy();
            io::fuzzy(query, &shown).map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// A loader that runs a closure, so the costmap is built from the table headers at load time.
struct Deferred<F>(F);

impl<T, F: Fn() -> Result<T, BoxError>> Load<T> for Deferred<F> {
    fn load(&self) -> Result<T, BoxError> {
        (self.0)()
    }
}

/// The Polu's store; the Save tab picks the real one by file extension.
struct NoStore;

impl Store for NoStore {
    fn store(&self, _: &Assignment, _: &SourceList, _: &SinkList) -> Result<(), BoxError> {
        Err("pick an output file on the Save tab".into())
    }
}

fn empty_table() -> Table {
    Table::new(Header::default(), Vec::new()).expect("empty")
}

// ----- filters -----

/// A parsed filter line: field terms that must all match, and comparisons on one number column.
///
/// `business=apple` matches rows whose `business` contains `apple` (case-insensitive); `business=`
/// matches rows that leave it empty; `source.store=x` picks a side when both have that column;
/// bare text matches any field; `cost>5`, `cost<=2` or `cost=3` compare the number column.
#[derive(Debug, Default)]
pub struct Filter {
    terms: Vec<Term>,
    numbers: Vec<(Cmp, f64)>,
}

#[derive(Debug)]
struct Term {
    source_slots: Vec<usize>,
    sink_slots: Vec<usize>,
    value: String,
    /// `field=` with nothing after it: the field is empty.
    empty: bool,
}

#[derive(Debug, Clone, Copy)]
enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

/// Per term, which source rows and which sink rows satisfy it.
pub struct RowMatches(Vec<(Vec<bool>, Vec<bool>)>);

impl Filter {
    pub fn parse(
        text: &str,
        source_header: &Header,
        sink_header: &Header,
        number: &str,
    ) -> Result<Self, String> {
        let mut filter = Self::default();
        let slots = |header: &Header, name: &str| -> Vec<usize> {
            (0..header.len())
                .filter(|&s| {
                    header[s]
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(name))
                })
                .collect()
        };
        let all = |header: &Header| -> Vec<usize> {
            (0..header.len()).filter(|&s| header[s].is_some()).collect()
        };
        for token in tokens(text) {
            let ops = [
                (">=", Cmp::Ge),
                ("<=", Cmp::Le),
                (">", Cmp::Gt),
                ("<", Cmp::Lt),
                ("=", Cmp::Eq),
            ];
            let split = ops
                .iter()
                .filter_map(|(s, cmp)| token.find(s).map(|at| (at, *s, *cmp)))
                .min_by_key(|(at, s, _)| (*at, std::cmp::Reverse(s.len())));
            let Some((at, op, cmp)) = split else {
                filter.terms.push(Term {
                    source_slots: all(source_header),
                    sink_slots: all(sink_header),
                    value: token.to_lowercase(),
                    empty: false,
                });
                continue;
            };
            let (name, value) = (token[..at].trim(), token[at + op.len()..].trim());
            if name.eq_ignore_ascii_case(number) {
                let value: f64 = value
                    .parse()
                    .map_err(|_| format!("`{value}` is not a number"))?;
                filter.numbers.push((cmp, value));
                continue;
            }
            if op != "=" {
                return Err(format!("`{op}` only works on {number}"));
            }
            let (source_slots, sink_slots) = match name.split_once('.') {
                Some((side, field)) if side.eq_ignore_ascii_case("source") => {
                    (slots(source_header, field), Vec::new())
                }
                Some((side, field)) if side.eq_ignore_ascii_case("sink") => {
                    (Vec::new(), slots(sink_header, field))
                }
                _ => (slots(source_header, name), slots(sink_header, name)),
            };
            match (source_slots.is_empty(), sink_slots.is_empty()) {
                (true, true) => return Err(format!("no `{name}` column")),
                (false, false) => {
                    return Err(format!(
                        "`{name}` is on both sides: write source.{name} or sink.{name}"
                    ));
                }
                _ => {}
            }
            filter.terms.push(Term {
                source_slots,
                sink_slots,
                value: value.to_lowercase(),
                empty: value.is_empty(),
            });
        }
        Ok(filter)
    }

    pub fn row_matches(&self, sources: &Table, sinks: &Table) -> RowMatches {
        let side = |table: &Table, slots: &[usize], term: &Term| -> Vec<bool> {
            table
                .rows()
                .iter()
                .map(|(key, _)| !slots.is_empty() && term.matches(key, slots))
                .collect()
        };
        RowMatches(
            self.terms
                .iter()
                .map(|t| {
                    (
                        side(sources, &t.source_slots, t),
                        side(sinks, &t.sink_slots, t),
                    )
                })
                .collect(),
        )
    }

    /// Whether source row `i` and sink row `j` together satisfy every field term.
    pub fn pair(&self, ok: &RowMatches, i: usize, j: usize) -> bool {
        ok.0.iter()
            .all(|(s, k)| s.get(i).copied().unwrap_or(false) || k.get(j).copied().unwrap_or(false))
    }

    pub fn number(&self, value: f64) -> bool {
        self.numbers.iter().all(|&(cmp, x)| match cmp {
            Cmp::Lt => value < x,
            Cmp::Le => value <= x,
            Cmp::Eq => value == x,
            Cmp::Ge => value >= x,
            Cmp::Gt => value > x,
        })
    }
}

impl Term {
    fn matches(&self, key: &Key, slots: &[usize]) -> bool {
        slots.iter().any(|&s| match key.get_slot(s) {
            None => self.empty,
            Some(v) => !self.empty && v.to_lowercase().contains(&self.value),
        })
    }
}

/// Splits on whitespace, keeping "double quoted" runs together (quotes removed).
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEventKind;

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    #[cfg_attr(not(feature = "csv"), allow(dead_code))]
    fn key(app: &mut App, code: KeyCode) {
        let mut event = KeyEvent::new(code, KeyModifiers::NONE);
        event.kind = KeyEventKind::Press;
        app.handle(event);
    }

    pub(crate) fn sample(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("samples/apples")
            .join(name)
    }

    /// Opens a sample scenario, loads it and solves it.
    pub(crate) fn solved(scenario: &str) -> App {
        let mut app = App::new(sample(""));
        app.open_scenario(&sample(scenario));
        assert!(!app.message.as_ref().unwrap().0, "{:?}", app.message);
        press(&mut app, "l");
        assert!(app.loaded.is_some(), "{:?}", app.message);
        press(&mut app, "4s");
        app.wait();
        app
    }

    #[cfg(feature = "csv")]
    #[test]
    fn csv_scenario_runs_end_to_end() {
        let mut app = solved("apples.polu.json");
        assert_eq!(app.assignment().unwrap().objective, 37.0);

        let out = std::env::temp_dir().join(format!("polu-tui-flows-{}.csv", std::process::id()));
        app.output = LineEdit::new(out.display().to_string());
        press(&mut app, "5");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        let written = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_file(&out).ok();
        assert!(written.starts_with("source.business,source.size,sink.store,sink.state,units"));
        assert_eq!(written.lines().count(), 5);
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_scenario_runs_end_to_end() {
        assert_eq!(
            solved("apples-json.polu.json")
                .assignment()
                .unwrap()
                .objective,
            37.0
        );
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn excel_scenario_runs_end_to_end() {
        assert_eq!(
            solved("apples-xlsx.polu.json")
                .assignment()
                .unwrap()
                .objective,
            37.0
        );
    }

    #[cfg(feature = "csv")]
    #[test]
    fn costs_filter_sort_and_trace() {
        let mut app = solved("apples.polu.json");
        press(&mut app, "3");
        // 4 sources x 2 sinks, the three apple inc -> stores and us pairs are blocked
        assert_eq!(app.costs.rows.len(), 5);
        press(&mut app, "h");
        assert_eq!(app.costs.rows.len(), 8);
        press(&mut app, "h/business=apple");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.costs.rows.len(), 3);
        press(&mut app, "/");
        app.costs.filter = LineEdit::new("store=ohio cost>1");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.costs.rows.len(), 3);

        press(&mut app, "o");
        let loaded = app.loaded.as_ref().unwrap();
        let first = loaded.costs[app.costs.rows[0]];
        assert_eq!(first, 2.0);
        // [apple inc, small] -> ohio fruits: Set 1 from the business rule, doubled by the size rule
        let small = 2 * 2 + 1;
        let trace = loaded.trace(small);
        let steps: Vec<_> = trace.steps.iter().map(|s| (s.rule, s.cost)).collect();
        assert_eq!(steps, [(Some(1), 1.0), (Some(0), 2.0), (None, 2.0)]);
    }

    /// A rule table in the Base slot is applied as a Set layer before the others.
    #[cfg(feature = "csv")]
    #[test]
    fn a_rules_base_runs_as_the_first_layer() {
        let mut app = App::new(sample(""));
        app.open_scenario(&sample("apples.polu.json"));
        let set = app.scenario.layers.remove(0);
        app.scenario.base = Some(set.file);
        press(&mut app, "l");
        let loaded = app.loaded.as_ref().expect("loaded");
        assert!(loaded.base_layer);
        assert_eq!(loaded.layer_files[0], "base costs-set.csv");
        assert_eq!(loaded.costmap().layers().len(), 3);
        assert_eq!(loaded.trace(5).steps[0].cost, 1.0);
        press(&mut app, "4s");
        app.wait();
        assert_eq!(app.assignment().unwrap().objective, 37.0);
    }

    #[cfg(feature = "csv")]
    #[test]
    fn filter_errors_are_shown_not_applied() {
        let mut app = solved("apples.polu.json");
        app.costs.filter = LineEdit::new("colour=red");
        app.refilter_costs();
        assert_eq!(app.costs.error.as_deref(), Some("no `colour` column"));
        assert_eq!(app.costs.rows.len(), 5);
        app.costs.filter = LineEdit::new("business>2");
        app.refilter_costs();
        assert!(
            app.costs
                .error
                .as_ref()
                .unwrap()
                .contains("only works on cost")
        );
    }

    #[cfg(feature = "csv")]
    #[test]
    fn data_tab_filters_each_side() {
        let mut app = solved("apples.polu.json");
        press(&mut app, "2");
        assert_eq!(app.data.rows.len(), 4);
        app.data.filter = LineEdit::new("size=");
        app.refilter_data();
        assert_eq!(app.data.rows, [0, 3]);
        app.data.filter = LineEdit::new("quantity>=7");
        app.refilter_data();
        assert_eq!(app.data.rows, [2, 3]);
        assert_eq!(app.loaded.as_ref().unwrap().covers.0, [2, 0, 0, 0]);
        press(&mut app, "s");
        assert_eq!(app.data.rows.len(), 2);
    }

    #[cfg(feature = "csv")]
    #[test]
    fn inputs_are_picked_from_files() {
        let mut app = App::new(sample(""));
        // sources: the file search, then the quantity column (guessed as `capacity`)
        key(&mut app, KeyCode::Enter);
        let Some(Popup::Files { all, .. }) = &app.popup else {
            panic!("no file search")
        };
        assert!(all.iter().any(|p| p.ends_with("sources.csv")));
        press(&mut app, "sources.csv");
        key(&mut app, KeyCode::Enter);
        let Some(Popup::Pick {
            options, selected, ..
        }) = &app.popup
        else {
            panic!("no quantity pick")
        };
        assert_eq!(options[*selected], "capacity");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.scenario.sources.as_ref().unwrap().quantity, "capacity");

        // a new layer starts as Set, and arrows cycle its operator
        app.slot = 3;
        key(&mut app, KeyCode::Enter);
        press(&mut app, "costs-scale.csv");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.scenario.layers[0].op, Op::Set);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.scenario.layers[0].op, Op::Scale);
        press(&mut app, "x");
        assert!(app.scenario.layers.is_empty());
    }

    #[test]
    fn ctrl_c_quits_even_while_typing() {
        let mut app = App::new(sample(""));
        app.tab = Tab::Save;
        key(&mut app, KeyCode::Enter);
        assert!(app.typing());
        press(&mut app, "q");
        assert!(!app.quit);
        app.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn tokens_keep_quoted_values() {
        assert_eq!(
            tokens(r#"business="apple inc"  cost>2"#),
            ["business=apple inc", "cost>2"]
        );
    }
}
