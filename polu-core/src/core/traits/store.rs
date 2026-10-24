use crate::core::BoxError;
use crate::core::types::{Assignment, SinkList, Slack, SourceList};

/// Something that can store a solved assignment. Any detail parameters should be owned by the implementor of this trait.
/// The flows index into `sources` and `sinks`, which are passed along so stores can write labelled results.
pub trait Store {
    fn store(
        &self,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError>;

    /// Stores the unused source capacity, one entry per source row (see [`crate::Problem::slack`]).
    /// Optional: stores that only write flows keep this default, which refuses.
    fn store_slack(&self, slack: &[Slack], sources: &SourceList) -> Result<(), BoxError> {
        let _ = (slack, sources);
        Err("this store doesn't write slack".into())
    }
}
