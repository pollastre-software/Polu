//! Excel store. The columns are described in [`crate::store`].

use std::path::PathBuf;

use rust_xlsxwriter::{Format, Workbook};

use super::{Rows, cap_slack_table, flow_table, slack_table};
use crate::core::BoxError;
use crate::core::traits::Store;
use crate::core::types::{Assignment, CapSlack, SinkList, Slack, SourceList, Table};

/// Writes the assignment to one sheet of a new xlsx file, one row per flow. Slack goes to a sheet
/// named `slack`, cap slack to one named `caps`.
#[derive(Debug, Clone)]
pub struct XlsxStore {
    path: PathBuf,
    sheet: String,
}

impl XlsxStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            sheet: "flows".into(),
        }
    }

    /// The sheet name, `flows` by default.
    pub fn with_sheet(mut self, sheet: impl Into<String>) -> Self {
        self.sheet = sheet.into();
        self
    }

    /// The assignment as a workbook, e.g. to hand back as bytes in WASM.
    pub fn workbook(
        &self,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<Workbook, BoxError> {
        workbook(&self.sheet, flow_table(assignment, sources, sinks))
    }

    /// The unused source capacity as a workbook.
    pub fn slack_workbook(
        &self,
        slack: &[Slack],
        sources: &SourceList,
    ) -> Result<Workbook, BoxError> {
        workbook("slack", slack_table(slack, sources))
    }

    /// How much of each source cap is used, as a workbook.
    pub fn cap_slack_workbook(
        &self,
        slack: &[CapSlack],
        caps: &Table,
    ) -> Result<Workbook, BoxError> {
        workbook("caps", cap_slack_table(slack, caps))
    }
}

fn workbook(name: &str, (columns, rows): (Vec<String>, Rows)) -> Result<Workbook, BoxError> {
    let mut book = Workbook::new();
    let sheet = book.add_worksheet();
    sheet.set_name(name)?;
    let bold = Format::new().set_bold();
    for (c, name) in columns.iter().enumerate() {
        sheet.write_string_with_format(0, c as u16, name, &bold)?;
    }
    for (r, (cells, numbers)) in rows.enumerate() {
        let r = r as u32 + 1;
        for (c, cell) in cells.iter().enumerate() {
            if let Some(text) = cell {
                sheet.write_string(r, c as u16, *text)?;
            }
        }
        for (c, number) in numbers.iter().enumerate() {
            sheet.write_number(r, (cells.len() + c) as u16, *number as f64)?;
        }
    }
    sheet.set_freeze_panes(1, 0)?;
    sheet.autofit();
    Ok(book)
}

impl Store for XlsxStore {
    fn store(
        &self,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError> {
        Ok(self
            .workbook(assignment, sources, sinks)?
            .save(&self.path)?)
    }

    fn store_slack(&self, slack: &[Slack], sources: &SourceList) -> Result<(), BoxError> {
        Ok(self.slack_workbook(slack, sources)?.save(&self.path)?)
    }

    fn store_cap_slack(&self, slack: &[CapSlack], caps: &Table) -> Result<(), BoxError> {
        Ok(self.cap_slack_workbook(slack, caps)?.save(&self.path)?)
    }
}
