#[cfg(feature = "full")]
use std::ops::{Deref, DerefMut};

#[cfg(feature = "full")]
/// A container type which holds a value that may be temporarily owned by a
/// consumer.
#[derive(Debug)]
pub struct Owner<T> {
    inner: Option<T>,
}

#[cfg(feature = "full")]
impl<T> Owner<T> {
    /// Creates a new `Owner` which holds the given value.
    pub const fn new(value: T) -> Self {
        Self { inner: Some(value) }
    }

    /// Takes temporary ownership of the contained value by passing it to `f`.
    /// The function must return a value of the same type (the same value, or a
    /// new value to take its place).
    ///
    /// # Example
    /// ```
    /// # use merk::owner::Owner;
    /// # struct SomeType();
    /// # impl SomeType {
    /// #     fn method_which_requires_ownership(self) -> SomeType { self }
    /// # }
    /// #
    /// let mut owner = Owner::new(SomeType());
    /// owner.own(|value| {
    ///     value.method_which_requires_ownership();
    ///     SomeType() // now give back a value of the same type
    /// });
    /// ```
    pub fn own<F: FnOnce(T) -> T>(&mut self, f: F) {
        let old_value = unwrap(self.inner.take());
        let new_value = f(old_value);
        self.inner = Some(new_value);
    }

    /// Takes temporary ownership of the contained value by passing it to `f`.
    /// The function must return a value of the same type (the same value, or a
    /// new value to take its place).
    ///
    /// Like `own`, but uses a tuple return type which allows specifying a value
    /// to return from the call to `own_return` for convenience.
    ///
    /// # Example
    /// ```
    /// # use merk::owner::Owner;
    /// let mut owner = Owner::new(123);
    /// let doubled = owner.own_return(|n| (n, n * 2));
    /// ```
    pub fn own_return<R, F>(&mut self, f: F) -> R
    where
        R: Sized,
        F: FnOnce(T) -> (T, R),
    {
        let old_value = unwrap(self.inner.take());
        let (new_value, return_value) = f(old_value);
        self.inner = Some(new_value);
        return_value
    }

    /// Sheds the `Owner` container and returns the value it contained.
    pub fn into_inner(mut self) -> T {
        unwrap(self.inner.take())
    }
}

#[cfg(feature = "full")]
impl<T> Deref for Owner<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unwrap(self.inner.as_ref())
    }
}

#[cfg(feature = "full")]
impl<T> DerefMut for Owner<T> {
    fn deref_mut(&mut self) -> &mut T {
        unwrap(self.inner.as_mut())
    }
}

#[cfg(feature = "full")]
fn unwrap<T>(option: Option<T>) -> T {
    match option {
        Some(value) => value,
        None => unreachable!("value should be Some"),
    }
}

// TODO: unit tests
