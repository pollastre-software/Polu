//! CSV store. The columns are described in [`crate::store`].

use std::io::Write;
use std::path::PathBuf;

use ::csv::WriterBuilder;

use super::{columns, flows};
use crate::core::BoxError;
use crate::core::traits::Store;
use crate::core::types::{Assignment, SinkList, SourceList};

/// Writes the assignment to a CSV file, one line per flow.
#[derive(Debug, Clone)]
pub struct CsvStore {
    path: PathBuf,
    delimiter: u8,
}

impl CsvStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            delimiter: b',',
        }
    }

    pub fn with_delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = delimiter;
        self
    }

    /// Writes the assignment as CSV to any writer, e.g. a `Vec<u8>` in tests or WASM.
    pub fn write(
        &self,
        writer: impl Write,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError> {
        let mut out = WriterBuilder::new()
            .delimiter(self.delimiter)
            .from_writer(writer);
        out.write_record(columns(sources, sinks))?;
        for (cells, units) in flows(assignment, sources, sinks) {
            let units = units.to_string();
            out.write_record(cells.iter().map(|c| c.unwrap_or_default()).chain([&*units]))?;
        }
        out.flush()?;
        Ok(())
    }
}

impl Store for CsvStore {
    fn store(
        &self,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError> {
        self.write(
            std::fs::File::create(&self.path)?,
            assignment,
            sources,
            sinks,
        )
    }
}
