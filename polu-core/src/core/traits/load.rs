use crate::core::BoxError;

/// Something that can load T. Any detail parameters should be owned by the implementor of this trait
pub trait Load<T> {
    fn load(&self) -> Result<T, BoxError>;
}
