use std::error::Error;

pub trait Load<T> {
    ///
    /// Something that can load T. Any detail parameters should be owned by the implementor of this trait
    ///
    type Error: Error;

    fn load(&self) -> Result<T, Self::Error>;
}
