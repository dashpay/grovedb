//! Owner

use std::ops::{Deref, DerefMut};

/// A container type which holds a value that may be temporarily owned by a
/// consumer.
#[derive(Debug)]
pub struct Owner<T> {
    inner: Option<T>,
}

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
    /// # use grovedb_merk::owner::Owner;
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
    /// # use grovedb_merk::owner::Owner;
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

    /// Takes temporary ownership of the contained value by passing it to `f`.
    /// The function must return a result of the same type (the same value, or a
    /// new value to take its place).
    ///
    /// # Warning
    ///
    /// If `f` returns `Err`, the contained value has been consumed and the
    /// `Owner` is left in a poisoned state (`inner = None`). Any subsequent
    /// access (deref, `own`, `own_return`, etc.) will panic. Callers **must**
    /// not use the `Owner` after `own_result` returns an error.
    pub fn own_result<F, E>(&mut self, f: F) -> Result<(), E>
    where
        F: FnOnce(T) -> Result<T, E>,
    {
        let old_value = unwrap(self.inner.take());
        match f(old_value) {
            Ok(new_value) => {
                self.inner = Some(new_value);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Sheds the `Owner` container and returns the value it contained.
    pub fn into_inner(mut self) -> T {
        unwrap(self.inner.take())
    }
}

impl<T> Deref for Owner<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unwrap(self.inner.as_ref())
    }
}

impl<T> DerefMut for Owner<T> {
    fn deref_mut(&mut self) -> &mut T {
        unwrap(self.inner.as_mut())
    }
}

fn unwrap<T>(option: Option<T>) -> T {
    match option {
        Some(value) => value,
        None => panic!(
            "Owner is in a poisoned state (inner value is None). \
             This can happen if `own_result` was called and the closure returned Err, \
             consuming the value. The Owner must not be used after such an error."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_own_result_success_preserves_value() {
        let mut owner = Owner::new(42);
        let result = owner.own_result(|v| Ok::<_, ()>(v + 1));
        assert!(result.is_ok());
        assert_eq!(*owner, 43);
    }

    #[test]
    #[should_panic(expected = "Owner is in a poisoned state")]
    fn test_own_result_error_poisons_owner() {
        let mut owner = Owner::new(42);
        let _ = owner.own_result(|_v| Err::<i32, &str>("fail"));
        // Accessing the poisoned Owner should panic with a clear message
        let _ = *owner;
    }
}
