use crate::core::BoxError;
use crate::core::types::{Assignment, SinkList, SourceList};

/// Something that can store a solved assignment. Any detail parameters should be owned by the implementor of this trait.
/// The flows index into `sources` and `sinks`, which are passed along so stores can write labelled results.
pub trait Store {
    fn store(
        &self,
        assignment: &Assignment,
        sources: &SourceList,
        sinks: &SinkList,
    ) -> Result<(), BoxError>;
}
