//! Excel store. The columns are described in [`crate::store`].

use std::path::PathBuf;

use rust_xlsxwriter::{Format, Workbook};

use super::{columns, flows};
use crate::core::BoxError;
use crate::core::traits::Store;
use crate::core::types::{Assignment, SinkList, SourceList};

/// Writes the assignment to one sheet of a new xlsx file, one row per flow.
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
        let mut book = Workbook::new();
        let sheet = book.add_worksheet();
        sheet.set_name(&self.sheet)?;
        let bold = Format::new().set_bold();
        for (c, name) in columns(sources, sinks).iter().enumerate() {
            sheet.write_string_with_format(0, c as u16, name, &bold)?;
        }
        for (r, (cells, units)) in flows(assignment, sources, sinks).enumerate() {
            let r = r as u32 + 1;
            for (c, cell) in cells.iter().enumerate() {
                if let Some(text) = cell {
                    sheet.write_string(r, c as u16, *text)?;
                }
            }
            sheet.write_number(r, cells.len() as u16, units as f64)?;
        }
        sheet.set_freeze_panes(1, 0)?;
        sheet.autofit();
        Ok(book)
    }
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
}
