use crate::core::BoxError;
use crate::core::types::Assignment;

/// Something that can store a solved assignment. Any detail parameters should be owned by the implementor of this trait
pub trait Store {
    fn store(&self, assignment: &Assignment) -> Result<(), BoxError>;
}
