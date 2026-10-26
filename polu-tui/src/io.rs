//! Files: picking a polu-core loader or store by extension, scenario files, and the file search.

use std::path::{Path, PathBuf};

use polu_core::{BoxError, CostMap, Header, Load, Op, Store, Table};
use serde::{Deserialize, Serialize};

/// A file format, from the extension. Formats this build leaves out still parse, so the
/// error can name the cargo feature that adds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
    Excel,
}

impl Format {
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => Some(Self::Excel),
            _ => None,
        }
    }

    pub fn enabled(self) -> bool {
        match self {
            Self::Csv => cfg!(feature = "csv"),
            Self::Json => cfg!(feature = "json"),
            Self::Excel => cfg!(feature = "xlsx"),
        }
    }

    fn feature(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Excel => "xlsx",
        }
    }

    fn check(path: &Path) -> Result<Self, BoxError> {
        let format = Self::of(path)
            .ok_or_else(|| format!("{}: not a .csv, .json or Excel file", path.display()))?;
        if !format.enabled() {
            return Err(format!(
                "{}: this build has no `{}` feature (cargo run -p polu-tui --features {})",
                path.display(),
                format.feature(),
                format.feature()
            )
            .into());
        }
        Ok(format)
    }
}

/// One input: a file, plus the sheet for Excel files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRef {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
}

impl FileRef {
    /// The file name and sheet, for lists.
    pub fn label(&self) -> String {
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match &self.sheet {
            Some(sheet) => format!("{name} [{sheet}]"),
            None => name,
        }
    }
}

/// Sources, sinks or source caps: the file and its quantity column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableInput {
    #[serde(flatten)]
    pub file: FileRef,
    pub quantity: String,
}

/// One costmap layer: the file and what it does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInput {
    #[serde(flatten)]
    pub file: FileRef,
    #[serde(with = "op_name")]
    pub op: Op,
}

/// Everything a run needs, saved as `*.polu.json` with paths relative to that file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub sources: Option<TableInput>,
    /// Caps over the source rows each of its rows matches, see [`polu_core::Table::caps`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<TableInput>,
    pub sinks: Option<TableInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<FileRef>,
    #[serde(default)]
    pub layers: Vec<LayerInput>,
}

impl Scenario {
    pub fn open(path: &Path) -> Result<Self, BoxError> {
        let text = std::fs::read_to_string(path)?;
        let mut scenario: Self =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new(""));
        scenario.files_mut(|file| file.path = dir.join(&file.path));
        Ok(scenario)
    }

    pub fn save(&self, path: &Path) -> Result<(), BoxError> {
        let dir = absolute(path.parent().unwrap_or(Path::new("")));
        let mut relative = self.clone();
        relative.files_mut(|file| {
            let full = absolute(&file.path);
            if let Ok(rest) = full.strip_prefix(&dir) {
                file.path = rest.to_path_buf();
            }
        });
        std::fs::write(path, serde_json::to_string_pretty(&relative)? + "\n")?;
        Ok(())
    }

    fn files_mut(&mut self, mut f: impl FnMut(&mut FileRef)) {
        let tables = [&mut self.sources, &mut self.caps, &mut self.sinks];
        tables.into_iter().flatten().for_each(|t| f(&mut t.file));
        self.base.iter_mut().for_each(&mut f);
        self.layers.iter_mut().for_each(|l| f(&mut l.file));
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

mod op_name {
    use polu_core::Op;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(op: &Op, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(super::op_label(*op))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Op, D::Error> {
        let name = String::deserialize(d)?;
        super::OPS
            .into_iter()
            .find(|op| super::op_label(*op).eq_ignore_ascii_case(&name))
            .ok_or_else(|| D::Error::custom(format!("unknown op `{name}`")))
    }
}

pub const OPS: [Op; 4] = [Op::Set, Op::Scale, Op::Add, Op::Block];

pub fn op_label(op: Op) -> &'static str {
    match op {
        Op::Set => "set",
        Op::Scale => "scale",
        Op::Add => "add",
        Op::Block => "block",
    }
}

/// The sheets of an Excel file, or `None` for other formats.
pub fn sheets(path: &Path) -> Result<Option<Vec<String>>, BoxError> {
    if Format::check(path)? != Format::Excel {
        return Ok(None);
    }
    #[cfg(feature = "xlsx")]
    return Ok(Some(polu_core::load::xlsx::sheet_names(&std::fs::read(
        path,
    )?)?));
    #[cfg(not(feature = "xlsx"))]
    unreachable!("checked above")
}

/// Every column of a sources or sinks file, so the user can pick the quantity column.
pub fn column_names(file: &FileRef) -> Result<Vec<String>, BoxError> {
    match Format::check(&file.path)? {
        #[cfg(feature = "csv")]
        Format::Csv => polu_core::load::csv::CsvTable::from_path(&file.path, "")?.column_names(),
        #[cfg(feature = "json")]
        Format::Json => polu_core::load::json::JsonTable::from_path(&file.path, "")?.column_names(),
        #[cfg(feature = "xlsx")]
        Format::Excel => polu_core::load::xlsx::XlsxTable::from_path(&file.path, sheet(file)?, "")?
            .column_names(),
        #[allow(unreachable_patterns)]
        _ => unreachable!("checked above"),
    }
}

/// A loader for sources, sinks or source caps, and the header the costmap loader needs.
pub fn table_loader(input: &TableInput) -> Result<(Box<dyn Load<Table>>, Header), BoxError> {
    let (file, quantity) = (&input.file, input.quantity.as_str());
    match Format::check(&file.path)? {
        #[cfg(feature = "csv")]
        Format::Csv => {
            let table = polu_core::load::csv::CsvTable::from_path(&file.path, quantity)?;
            let header = table.header()?;
            Ok((Box::new(table), header))
        }
        #[cfg(feature = "json")]
        Format::Json => {
            let table = polu_core::load::json::JsonTable::from_path(&file.path, quantity)?;
            let header = table.header()?;
            Ok((Box::new(table), header))
        }
        #[cfg(feature = "xlsx")]
        Format::Excel => {
            let table =
                polu_core::load::xlsx::XlsxTable::from_path(&file.path, sheet(file)?, quantity)?;
            let header = table.header()?;
            Ok((Box::new(table), header))
        }
        #[allow(unreachable_patterns)]
        _ => unreachable!("checked above"),
    }
}

/// Whether a base file is a rule table (`source.` and `sink.` columns, like a `Set` layer) rather
/// than a headerless matrix. A rule table base is applied as a `Set` layer before the others.
pub fn base_is_rules(file: &FileRef) -> bool {
    column_names(file).is_ok_and(|columns| {
        columns.iter().any(|c| {
            let c = c.to_ascii_lowercase();
            c.starts_with("source.") || c.starts_with("sink.")
        })
    })
}

/// Parses the base and every layer, each with the loader of its own format, into one costmap.
/// Files may mix formats, e.g. an Excel base with CSV layers. The base can be a matrix or a rule
/// table (see [`base_is_rules`]).
pub fn costmap(
    scenario: &Scenario,
    source_header: &Header,
    sink_header: &Header,
) -> Result<CostMap, BoxError> {
    // each format's loader parses its files; the layers are then gathered in scenario order
    let one = |op: Option<Op>, file: &FileRef, what: &str| -> Result<CostMap, BoxError> {
        let (s, k) = (source_header.clone(), sink_header.clone());
        let what = format!("{what} {}", file.label());
        let loaded: Result<CostMap, BoxError> = match Format::check(&file.path)? {
            #[cfg(feature = "csv")]
            Format::Csv => {
                let text = std::fs::read_to_string(&file.path)?;
                let cm = polu_core::load::csv::CsvCostMap::new(s, k);
                match op {
                    Some(op) => cm.layer(op, text),
                    None => cm.base(text),
                }
                .load()
            }
            #[cfg(feature = "json")]
            Format::Json => {
                let text = std::fs::read_to_string(&file.path)?;
                let cm = polu_core::load::json::JsonCostMap::new(s, k);
                match op {
                    Some(op) => cm.layer(op, text),
                    None => cm.base(text),
                }
                .load()
            }
            #[cfg(feature = "xlsx")]
            Format::Excel => {
                let cm = polu_core::load::xlsx::XlsxCostMap::from_path(&file.path, s, k)?;
                let sheet = sheet(file)?;
                match op {
                    Some(op) => cm.layer(op, sheet),
                    None => cm.base(sheet),
                }
                .load()
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("checked above"),
        };
        loaded.map_err(|e| format!("{what}: {e}").into())
    };

    let (rules, matrix) = match &scenario.base {
        Some(file) if base_is_rules(file) => (Some(file), None),
        base => (None, base.as_ref()),
    };
    let mut layers = Vec::new();
    if let Some(file) = rules {
        layers.extend(one(Some(Op::Set), file, "base")?.layers().iter().cloned());
    }
    for layer in &scenario.layers {
        let what = format!("{} layer", op_label(layer.op));
        layers.extend(
            one(Some(layer.op), &layer.file, &what)?
                .layers()
                .iter()
                .cloned(),
        );
    }
    let costmap = CostMap::new(source_header.clone(), sink_header.clone(), layers)?;
    match matrix {
        None => Ok(costmap),
        Some(file) => {
            let base = one(None, file, "base")?;
            Ok(costmap.with_base(base.base().unwrap_or_default().to_vec())?)
        }
    }
}

/// A store for the output path, by its extension.
pub fn store(path: &Path) -> Result<Box<dyn Store>, BoxError> {
    match Format::check(path)? {
        #[cfg(feature = "csv")]
        Format::Csv => Ok(Box::new(polu_core::store::csv::CsvStore::new(path))),
        #[cfg(feature = "json")]
        Format::Json => Ok(Box::new(polu_core::store::json::JsonStore::new(path))),
        #[cfg(feature = "xlsx")]
        Format::Excel => {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !ext.eq_ignore_ascii_case("xlsx") {
                return Err("Excel output must be .xlsx".into());
            }
            Ok(Box::new(polu_core::store::xlsx::XlsxStore::new(path)))
        }
        #[allow(unreachable_patterns)]
        _ => unreachable!("checked above"),
    }
}

#[cfg(feature = "xlsx")]
fn sheet(file: &FileRef) -> Result<&str, BoxError> {
    file.sheet
        .as_deref()
        .ok_or_else(|| format!("{}: pick a sheet", file.path.display()).into())
}

/// Files under `root` the TUI can read, skipping hidden folders and build output, at most `limit`.
pub fn walk(root: &Path, limit: usize) -> Vec<PathBuf> {
    const SKIP: [&str; 3] = ["target", "node_modules", "__pycache__"];
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if !SKIP.contains(&name) {
                    dirs.push(path);
                }
            } else if Format::of(&path).is_some_and(Format::enabled) {
                found.push(path);
                if found.len() >= limit {
                    return found;
                }
            }
        }
    }
    found.sort();
    found
}

/// Scores `candidate` against `pattern` as a case-insensitive subsequence, higher is better;
/// `None` when the pattern's letters don't all appear in order. Consecutive letters and letters
/// at word starts score more, and shorter candidates win ties.
pub fn fuzzy(pattern: &str, candidate: &str) -> Option<i64> {
    let mut score = 0i64;
    let mut last: Option<usize> = None;
    let chars: Vec<char> = candidate.chars().collect();
    let mut at = 0;
    for p in pattern.chars().filter(|c| !c.is_whitespace()) {
        let p = p.to_ascii_lowercase();
        let found = (at..chars.len()).find(|&i| chars[i].to_ascii_lowercase() == p)?;
        score += 1;
        if last == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        if found == 0 || matches!(chars[found - 1], '/' | '\\' | '_' | '-' | '.' | ' ') {
            score += 3;
        }
        last = Some(found);
        at = found + 1;
    }
    Some(score * 100 - chars.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_tight_matches() {
        assert!(fuzzy("src", "sample-data/sources.csv").is_some());
        assert!(fuzzy("xyz", "sources.csv").is_none());
        let tight = fuzzy("sources", "data/sources.csv").unwrap();
        let loose = fuzzy("sources", "data/sinks-of-our-resources.csv").unwrap();
        assert!(tight > loose);
    }

    #[cfg(feature = "csv")]
    #[test]
    fn a_base_is_rules_when_it_has_source_or_sink_columns() {
        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/apples");
        let file = |path: PathBuf| FileRef { path, sheet: None };
        assert!(base_is_rules(&file(samples.join("costs-set.csv"))));
        let matrix = std::env::temp_dir().join(format!("polu-tui-base-{}.csv", std::process::id()));
        std::fs::write(&matrix, "1,2\n3,nan\n").unwrap();
        assert!(!base_is_rules(&file(matrix.clone())));
        std::fs::remove_file(&matrix).ok();
    }

    #[test]
    fn scenario_paths_are_relative_to_the_scenario_file() {
        let dir = std::env::temp_dir().join(format!("polu-tui-scenario-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let file = |name: &str| FileRef {
            path: dir.join("data").join(name),
            sheet: None,
        };
        let scenario = Scenario {
            sources: Some(TableInput {
                file: file("sources.csv"),
                quantity: "capacity".into(),
            }),
            caps: Some(TableInput {
                file: file("caps.csv"),
                quantity: "cap".into(),
            }),
            sinks: None,
            base: None,
            layers: vec![LayerInput {
                file: file("scale.csv"),
                op: Op::Scale,
            }],
        };
        let path = dir.join("run.polu.json");
        scenario.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""path": "data/scale.csv""#), "{text}");
        assert!(text.contains(r#""op": "scale""#));
        assert!(text.contains(r#""path": "data/caps.csv""#), "{text}");
        assert_eq!(Scenario::open(&path).unwrap(), scenario);
        std::fs::remove_dir_all(&dir).ok();
    }
}
