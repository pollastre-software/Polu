//! JSON store, in the wide layout the JSON loaders read: column names under `headers`, one row per
//! flow under `data`, with `null` where a row leaves a field empty. The columns are described in
//! [`crate::store`].

use std::io::{BufWriter, Write};
use std::path::PathBuf;

use super::{Rows, cap_slack_table, flow_table, slack_table};
use crate::core::BoxError;
use crate::core::traits::Store;
use crate::core::types::{Assignment, CapSlack, SinkList, Slack, SourceList, Table};

/// Writes the assignment to a JSON file, one line per flow.
#[derive(Debug, Clone)]
pub struct JsonStore {
    path: PathBuf,
}

impl JsonStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Writes the assignment as JSON to any writer, e.g. a `Vec<u8>` in tests or WASM.
    pub fn write(
        &self,
        writer: impl Write,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError> {
        write_table(writer, flow_table(assignment, sources, sinks))
    }

    /// Writes the unused source capacity as JSON to any writer.
    pub fn write_slack(
        &self,
        writer: impl Write,
        slack: &[Slack],
        sources: &SourceList,
    ) -> Result<(), BoxError> {
        write_table(writer, slack_table(slack, sources))
    }

    /// Writes how much of each source cap is used as JSON to any writer.
    pub fn write_cap_slack(
        &self,
        writer: impl Write,
        slack: &[CapSlack],
        caps: &Table,
    ) -> Result<(), BoxError> {
        write_table(writer, cap_slack_table(slack, caps))
    }
}

fn write_table(
    mut writer: impl Write,
    (columns, rows): (Vec<String>, Rows),
) -> Result<(), BoxError> {
    write!(writer, "{{\"headers\":")?;
    serde_json::to_writer(&mut writer, &columns)?;
    write!(writer, ",\n\"data\":[")?;
    for (r, (cells, numbers)) in rows.enumerate() {
        writer.write_all(if r == 0 { b"\n" } else { b",\n" })?;
        let mut row: Vec<serde_json::Value> = cells.into_iter().map(Into::into).collect();
        row.extend(numbers.into_iter().map(serde_json::Value::from));
        serde_json::to_writer(&mut writer, &row)?;
    }
    writeln!(writer, "]}}")?;
    writer.flush()?;
    Ok(())
}

impl Store for JsonStore {
    fn store(
        &self,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError> {
        self.write(
            BufWriter::new(std::fs::File::create(&self.path)?),
            assignment,
            sources,
            sinks,
        )
    }

    fn store_slack(&self, slack: &[Slack], sources: &SourceList) -> Result<(), BoxError> {
        self.write_slack(
            BufWriter::new(std::fs::File::create(&self.path)?),
            slack,
            sources,
        )
    }

    fn store_cap_slack(&self, slack: &[CapSlack], caps: &Table) -> Result<(), BoxError> {
        self.write_cap_slack(
            BufWriter::new(std::fs::File::create(&self.path)?),
            slack,
            caps,
        )
    }
}
