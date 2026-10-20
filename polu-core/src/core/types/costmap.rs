use super::{Header, Key, Sink, Source};
use crate::core::PoluError;

/// Cost of moving 1 unit from any source matching `source` to any sink matching `sink`.
/// `None` slots in either key match any value.
#[derive(Debug, Clone)]
pub struct CostRule {
    pub source: Key,
    pub sink: Key,
    pub priority: i8,
    pub cost: f64,
}

impl CostRule {
    /// Number of fields this rule pins down, across both keys.
    pub fn specificity(&self) -> u8 {
        self.source.specificity() + self.sink.specificity()
    }
}

/// Ordered list of cost rules. The rule keys use the same slot layout as the source and sink headers.
#[derive(Debug, Clone)]
pub struct CostMap {
    source_header: Header,
    sink_header: Header,
    rules: Vec<CostRule>,
}

impl CostMap {
    /// Rejects non-finite costs, which the solver cannot handle.
    pub fn new(
        source_header: Header,
        sink_header: Header,
        rules: Vec<CostRule>,
    ) -> Result<Self, PoluError> {
        if let Some(i) = rules.iter().position(|r| !r.cost.is_finite()) {
            return Err(PoluError::Invalid(format!(
                "costmap rule {i} has a non-finite cost"
            )));
        }
        Ok(Self {
            source_header,
            sink_header,
            rules,
        })
    }

    pub fn source_header(&self) -> &Header {
        &self.source_header
    }

    pub fn sink_header(&self) -> &Header {
        &self.sink_header
    }

    pub fn rules(&self) -> &[CostRule] {
        &self.rules
    }

    /// Cost for a source/sink pair, or `None` if no rule matches (the pair can't be used).
    /// Among matching rules: highest priority wins, then highest specificity, then the earliest row.
    pub fn resolve(&self, source: &Source, sink: &Sink) -> Option<f64> {
        self.rules
            .iter()
            .enumerate()
            .filter(|(_, r)| r.source.matches(source) && r.sink.matches(sink))
            .max_by_key(|(i, r)| (r.priority, r.specificity(), std::cmp::Reverse(*i)))
            .map(|(_, r)| r.cost)
    }
}
