use std::collections::HashMap;

use super::{Header, Key, SLOTS, Sink, Source, Table};
use crate::core::PoluError;

/// What a layer does to the source/sink pairs its rules match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Sets the cost, opening the route if it had none.
    Set,
    /// Multiplies an existing cost by the rule's value. Pairs without a cost stay without one.
    Scale,
    /// Adds the rule's value to an existing cost. Pairs without a cost stay without one.
    Add,
    /// Removes the route. Final: later layers can't bring it back.
    Block,
}

/// A rule matching any source like `source` and any sink like `sink`. `None` slots match any value.
/// `value` is the cost for `Set`, the factor for `Scale`, the amount for `Add`, and is ignored by `Block`.
#[derive(Debug, Clone)]
pub struct CostRule {
    pub source: Source,
    pub sink: Sink,
    pub priority: i8,
    pub value: f64,
}

impl CostRule {
    /// Number of fields this rule pins down, across both keys.
    pub fn specificity(&self) -> u8 {
        self.source.specificity() + self.sink.specificity()
    }
}

/// One stage of the costmap. When several of its rules match a pair, the highest priority wins,
/// then the highest specificity, then the earliest row.
#[derive(Debug, Clone)]
pub struct Layer {
    pub op: Op,
    pub rules: Vec<CostRule>,
}

/// An optional base cost per pair, then layers applied in order, e.g. `Scale` preferences, then a `Block` list.
/// The rule keys use the same slot layout as the source and sink headers.
#[derive(Debug, Clone)]
pub struct CostMap {
    source_header: Header,
    sink_header: Header,
    base: Option<Vec<f64>>,
    layers: Vec<Layer>,
}

impl CostMap {
    /// Rejects non-finite values, which the solver cannot handle.
    pub fn new(
        source_header: Header,
        sink_header: Header,
        layers: Vec<Layer>,
    ) -> Result<Self, PoluError> {
        for (l, layer) in layers.iter().enumerate() {
            if layer.op == Op::Block {
                continue;
            }
            if let Some(i) = layer.rules.iter().position(|r| !r.value.is_finite()) {
                return Err(PoluError::Invalid(format!(
                    "costmap layer {l}, rule {i} has a non-finite value"
                )));
            }
        }
        Ok(Self {
            source_header,
            sink_header,
            base: None,
            layers,
        })
    }

    /// Starts from one cost per pair (e.g. a distance), row-major `sources × sinks` in table row order,
    /// instead of from no routes. `NaN` marks a pair that can't be used. Much faster than one `Set`
    /// rule per pair. The size is checked against the tables when the costmap is resolved.
    pub fn with_base(mut self, costs: Vec<f64>) -> Result<Self, PoluError> {
        if let Some(c) = costs.iter().position(|c| c.is_infinite()) {
            return Err(PoluError::Invalid(format!("base cost {c} is infinite")));
        }
        self.base = Some(costs);
        Ok(self)
    }

    pub fn base(&self) -> Option<&[f64]> {
        self.base.as_deref()
    }

    pub fn source_header(&self) -> &Header {
        &self.source_header
    }

    pub fn sink_header(&self) -> &Header {
        &self.sink_header
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Cost of every source/sink pair, row-major (`sources × sinks`). `NaN` means the pair can't be used.
    ///
    /// Each rule's matching rows come from per-slot bitsets, and rules are painted lowest rank first,
    /// so the winner of each pair is painted last. Each layer only visits the cells its rules touch,
    /// so work grows with those cells, not with `pairs × rules` or `pairs × layers`.
    pub fn resolve(&self, sources: &Table, sinks: &Table) -> Result<Vec<f64>, PoluError> {
        let (source_index, sink_index) = (RowIndex::new(sources), RowIndex::new(sinks));
        // rules often repeat patterns (e.g. a base layer with one rule per pair), so masks are cached
        let mut source_rows: HashMap<&Key, Vec<usize>> = HashMap::new();
        let mut sink_rows: HashMap<&Key, Vec<usize>> = HashMap::new();

        let nk = sinks.rows().len();
        let cells = sources.rows().len() * nk;
        let mut cost = match &self.base {
            Some(base) if base.len() != cells => {
                return Err(PoluError::Invalid(format!(
                    "base has {} costs, expected {cells} (sources × sinks)",
                    base.len()
                )));
            }
            Some(base) => base.clone(),
            None => vec![f64::NAN; cells],
        };
        let mut blocked = vec![false; cells];
        // winning value per cell for the current layer, and the cells painted so far in it
        let mut winner = vec![f64::NAN; cells];
        let mut touched = Vec::new();

        for layer in &self.layers {
            let mut order: Vec<(usize, &CostRule)> = layer.rules.iter().enumerate().collect();
            order.sort_by_cached_key(|(i, r)| (r.priority, r.specificity(), std::cmp::Reverse(*i)));

            for (_, rule) in order {
                // values are finite (checked in new), so NaN keeps meaning "not painted"
                let value = if layer.op == Op::Block {
                    1.0
                } else {
                    rule.value
                };
                let ss = source_rows
                    .entry(&rule.source)
                    .or_insert_with(|| source_index.rows_matching(&rule.source, true));
                let ks = sink_rows
                    .entry(&rule.sink)
                    .or_insert_with(|| sink_index.rows_matching(&rule.sink, true));
                for &i in ss.iter() {
                    for &j in ks.iter() {
                        let c = i * nk + j;
                        if winner[c].is_nan() {
                            touched.push(c);
                        }
                        winner[c] = value;
                    }
                }
            }

            for c in touched.drain(..) {
                let w = std::mem::replace(&mut winner[c], f64::NAN);
                if blocked[c] {
                    continue;
                }
                match layer.op {
                    Op::Set => cost[c] = w,
                    Op::Scale => cost[c] *= w,
                    Op::Add => cost[c] += w,
                    Op::Block => {
                        blocked[c] = true;
                        cost[c] = f64::NAN;
                    }
                }
            }
        }
        Ok(cost)
    }
}

/// Rows carrying each value of one slot, as bitsets, and the rows with the slot empty.
struct SlotIndex<'a> {
    empty: Vec<u64>,
    values: HashMap<&'a str, Vec<u64>>,
}

/// Per-slot bitset indexes over a table's rows.
pub(super) struct RowIndex<'a> {
    rows: usize,
    slots: Vec<SlotIndex<'a>>,
}

impl<'a> RowIndex<'a> {
    pub(super) fn new(table: &'a Table) -> Self {
        let rows = table.rows().len();
        let words = rows.div_ceil(64);
        let slots = (0..SLOTS)
            .map(|s| {
                let mut empty = vec![0u64; words];
                let mut values: HashMap<&str, Vec<u64>> = HashMap::new();
                for (i, (key, _)) in table.rows().iter().enumerate() {
                    let bits = match key.0[s].as_deref() {
                        None => &mut empty,
                        Some(v) => values.entry(v).or_insert_with(|| vec![0; words]),
                    };
                    bits[i / 64] |= 1 << (i % 64);
                }
                SlotIndex { empty, values }
            })
            .collect();
        Self { rows, slots }
    }

    /// Rows agreeing with every set slot of `pattern`: the AND of those slots' bitsets.
    /// With `loose`, a row's empty slot also agrees with any value (as in `Key::matches`).
    pub(super) fn rows_matching(&self, pattern: &Key, loose: bool) -> Vec<usize> {
        let mut mask: Option<Vec<u64>> = None; // None: every row
        for (slot, value) in self.slots.iter().zip(&pattern.0) {
            let Some(v) = value.as_deref() else {
                continue;
            };
            let raw = slot.values.get(v);
            let word = |w: usize| raw.map_or(0, |r| r[w]) | if loose { slot.empty[w] } else { 0 };
            match &mut mask {
                None => mask = Some((0..slot.empty.len()).map(word).collect()),
                Some(m) => m.iter_mut().enumerate().for_each(|(w, m)| *m &= word(w)),
            }
        }
        let Some(mask) = mask else {
            return (0..self.rows).collect();
        };
        let mut rows = Vec::new();
        for (w, &word) in mask.iter().enumerate() {
            let mut word = word;
            while word != 0 {
                rows.push(w * 64 + word.trailing_zeros() as usize);
                word &= word - 1;
            }
        }
        rows
    }
}
