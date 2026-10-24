//! Stores, one module per output format, each behind the cargo feature of the same name
//! (`csv` is on by default; `xlsx` and `json` are opt-in). Every format writes the same table: one row per flow with the
//! source's category columns (`source.<column>`), the sink's (`sink.<column>`), then `units`.

#![cfg_attr(
    not(any(feature = "csv", feature = "xlsx", feature = "json")),
    allow(dead_code)
)]

#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "json")]
pub mod json;
#[cfg(feature = "xlsx")]
pub mod xlsx;

use crate::core::types::{Assignment, Header, SinkList, SourceList};

/// The column names: `source.<column>`..., `sink.<column>`..., `units`.
fn columns(sources: &SourceList, sinks: &SinkList) -> Vec<String> {
    let mut columns: Vec<String> = Vec::new();
    for (prefix, header) in [("source", sources.header()), ("sink", sinks.header())] {
        columns.extend(
            header
                .iter()
                .flatten()
                .map(|name| format!("{prefix}.{name}")),
        );
    }
    columns.push("units".into());
    columns
}

/// One row per flow: the category cells under [`columns`] (`None` where the row leaves a field
/// empty), then the units.
fn flows<'a>(
    assignment: &'a Assignment,
    sources: &'a SourceList,
    sinks: &'a SinkList,
) -> impl Iterator<Item = (Vec<Option<&'a str>>, i64)> + 'a {
    let (source_slots, sink_slots) = (named(sources.header()), named(sinks.header()));
    assignment.flows.iter().map(move |flow| {
        let source = &sources.rows()[flow.source].0;
        let sink = &sinks.rows()[flow.sink].0;
        let cells = source_slots
            .iter()
            .map(|&s| source.get_slot(s))
            .chain(sink_slots.iter().map(|&s| sink.get_slot(s)))
            .collect();
        (cells, flow.units)
    })
}

/// The slots that have a column name.
fn named(header: &Header) -> Vec<usize> {
    (0..header.len()).filter(|&s| header[s].is_some()).collect()
}
