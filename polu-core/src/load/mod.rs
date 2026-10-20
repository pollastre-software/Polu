/// CSV loader (not implemented yet). Holds the CSV text rather than a file path,
/// so the same loader works from the CLI, WASM and Python.
pub struct CsvLoad {
    pub text: String,
    pub separator: char,
}
