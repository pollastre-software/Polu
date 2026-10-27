//! Stores, one module per output format, each behind the cargo feature of the same name
//! (`csv` is on by default; `xlsx` and `json` are opt-in). Every format writes the same table: one row per flow with the
//! source's category columns (`source.<column>`), the sink's (`sink.<column>`), then `units`.
//!
//! `store_slack` writes the unused source capacity the same way: one row per source row with its
//! `source.<column>`s, then `used`, `capacity`, `unused` and `spare` (see [`crate::Slack`]).
//! `store_cap_slack` writes one row per source cap with the cap table's `source.<column>`s, then
//! `used`, `cap`, `unused` and `percent_used` (see [`crate::CapSlack`]).

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

use crate::core::types::{Assignment, CapSlack, Header, SinkList, Slack, SourceList, Table};

/// A table to write: column names, then rows of category cells (`None` where a row leaves a field
/// empty) followed by numbers, so every format writes flows and slack alike.
type Rows<'a> = Box<dyn Iterator<Item = (Vec<Option<&'a str>>, Vec<i64>)> + 'a>;

/// The flow table: [`columns`] and [`flows`].
fn flow_table<'a>(
    assignment: &'a Assignment,
    sources: &'a SourceList,
    sinks: &'a SinkList,
) -> (Vec<String>, Rows<'a>) {
    (
        columns(sources, sinks),
        Box::new(flows(assignment, sources, sinks).map(|(cells, units)| (cells, vec![units]))),
    )
}

/// The slack table: `source.<column>`..., `used`, `capacity`, `unused`, `spare`, one row per source row.
fn slack_table<'a>(slack: &'a [Slack], sources: &'a SourceList) -> (Vec<String>, Rows<'a>) {
    let (mut columns, cells) = source_cells(sources);
    columns.extend(["used", "capacity", "unused", "spare"].map(String::from));
    let rows = slack
        .iter()
        .zip(cells)
        .map(|(s, cells)| (cells, vec![s.used, s.amount, s.unused(), s.spare]));
    (columns, Box::new(rows))
}

/// The cap slack table: the cap table's `source.<column>`..., `used`, `cap`, `unused`,
/// `percent_used`, one row per cap.
fn cap_slack_table<'a>(slack: &'a [CapSlack], caps: &'a Table) -> (Vec<String>, Rows<'a>) {
    let (mut columns, cells) = source_cells(caps);
    columns.extend(["used", "cap", "unused", "percent_used"].map(String::from));
    let rows = slack
        .iter()
        .zip(cells)
        .map(|(s, cells)| (cells, vec![s.used, s.amount, s.unused(), s.percent_used()]));
    (columns, Box::new(rows))
}

/// A source-side table's `source.<column>` names, and each row's cells under them.
fn source_cells(table: &Table) -> (Vec<String>, impl Iterator<Item = Vec<Option<&str>>>) {
    let slots = named(table.header());
    let columns = slots
        .iter()
        .map(|&s| {
            format!(
                "source.{}",
                table.header()[s].as_deref().unwrap_or_default()
            )
        })
        .collect();
    let cells = table
        .rows()
        .iter()
        .map(move |(key, _)| slots.iter().map(|&slot| key.get_slot(slot)).collect());
    (columns, cells)
}

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
