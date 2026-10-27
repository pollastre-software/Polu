//! CSV store. The columns are described in [`crate::store`].

use std::io::Write;
use std::path::PathBuf;

use ::csv::WriterBuilder;

use super::{Rows, cap_slack_table, flow_table, slack_table};
use crate::core::BoxError;
use crate::core::traits::Store;
use crate::core::types::{Assignment, CapSlack, SinkList, Slack, SourceList, Table};

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
        self.write_table(writer, flow_table(assignment, sources, sinks))
    }

    /// Writes the unused source capacity as CSV to any writer.
    pub fn write_slack(
        &self,
        writer: impl Write,
        slack: &[Slack],
        sources: &SourceList,
    ) -> Result<(), BoxError> {
        self.write_table(writer, slack_table(slack, sources))
    }

    /// Writes how much of each source cap is used as CSV to any writer.
    pub fn write_cap_slack(
        &self,
        writer: impl Write,
        slack: &[CapSlack],
        caps: &Table,
    ) -> Result<(), BoxError> {
        self.write_table(writer, cap_slack_table(slack, caps))
    }

    fn write_table(
        &self,
        writer: impl Write,
        (columns, rows): (Vec<String>, Rows),
    ) -> Result<(), BoxError> {
        let mut out = WriterBuilder::new()
            .delimiter(self.delimiter)
            .from_writer(writer);
        out.write_record(columns)?;
        for (cells, numbers) in rows {
            let numbers: Vec<String> = numbers.iter().map(i64::to_string).collect();
            out.write_record(
                cells
                    .iter()
                    .map(|c| c.unwrap_or_default())
                    .chain(numbers.iter().map(String::as_str)),
            )?;
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

    fn store_slack(&self, slack: &[Slack], sources: &SourceList) -> Result<(), BoxError> {
        self.write_slack(std::fs::File::create(&self.path)?, slack, sources)
    }

    fn store_cap_slack(&self, slack: &[CapSlack], caps: &Table) -> Result<(), BoxError> {
        self.write_cap_slack(std::fs::File::create(&self.path)?, slack, caps)
    }
}
