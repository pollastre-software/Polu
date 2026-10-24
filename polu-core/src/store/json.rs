//! JSON store, in the wide layout the JSON loaders read: column names under `headers`, one row per
//! flow under `data`, with `null` where a row leaves a field empty. The columns are described in
//! [`crate::store`].

use std::io::{BufWriter, Write};
use std::path::PathBuf;

use super::{columns, flows};
use crate::core::BoxError;
use crate::core::traits::Store;
use crate::core::types::{Assignment, SinkList, SourceList};

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
        mut writer: impl Write,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError> {
        write!(writer, "{{\"headers\":")?;
        serde_json::to_writer(&mut writer, &columns(sources, sinks))?;
        write!(writer, ",\n\"data\":[")?;
        for (r, (cells, units)) in flows(assignment, sources, sinks).enumerate() {
            writer.write_all(if r == 0 { b"\n" } else { b",\n" })?;
            let mut row: Vec<serde_json::Value> = cells.into_iter().map(Into::into).collect();
            row.push(units.into());
            serde_json::to_writer(&mut writer, &row)?;
        }
        writeln!(writer, "]}}")?;
        writer.flush()?;
        Ok(())
    }
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
}
