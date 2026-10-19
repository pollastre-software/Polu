use std::error::Error;

pub trait Store<T> {
    ///
    /// Something that can store T. Any detail parameters should be owned by the implementor of this trait
    ///
    type Error: Error + Default;

    fn store(&self, data: &T) -> Result<(), Self::Error>;
}
