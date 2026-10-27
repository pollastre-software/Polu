use crate::core::BoxError;
use crate::core::types::{Assignment, CapSlack, SinkList, Slack, SourceList, Table};

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

    /// Stores how much of each source cap is used, one entry per row of `caps` (see
    /// [`crate::Problem::cap_slack`]). Optional, like `store_slack`.
    fn store_cap_slack(&self, slack: &[CapSlack], caps: &Table) -> Result<(), BoxError> {
        let _ = (slack, caps);
        Err("this store doesn't write cap slack".into())
    }
}
