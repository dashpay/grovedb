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
    /// The closure must return `Ok(T)` on success or `Err((T, E))` on failure.
    ///
    /// **Error-safe:** In both `Ok` and `Err` paths the `Owner` retains a valid
    /// value afterward -- on error, the value bundled in the `Err` variant is
    /// restored.
    ///
    /// **Not unwind-safe:** If the closure panics, `Owner` will be left in a
    /// poisoned state (inner = None) because the value was moved out before
    /// the call and restoration happens only on normal return.
    ///
    /// # Example
    /// ```
    /// # use grovedb_merk::owner::Owner;
    /// let mut owner = Owner::new(42);
    /// let result = owner.own_result(|v| {
    ///     if v > 100 {
    ///         Ok(v + 1)
    ///     } else {
    ///         Err((v, "too small"))   // value is returned alongside the error
    ///     }
    /// });
    /// assert!(result.is_err());
    /// assert_eq!(*owner, 42); // Owner still holds the original value
    /// ```
    pub fn own_result<F, E>(&mut self, f: F) -> Result<(), E>
    where
        F: FnOnce(T) -> Result<T, (T, E)>,
    {
        let old_value = unwrap(self.inner.take());
        match f(old_value) {
            Ok(new_value) => {
                self.inner = Some(new_value);
                Ok(())
            }
            Err((restored_value, e)) => {
                self.inner = Some(restored_value);
                Err(e)
            }
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
             This can happen if a closure passed to `own_result` panicked \
             before returning Ok/Err."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_own_result_success_preserves_value() {
        let mut owner = Owner::new(42);
        let result = owner.own_result(|v| Ok::<_, (i32, ())>(v + 1));
        assert!(result.is_ok());
        assert_eq!(*owner, 43);
    }

    #[test]
    fn test_own_result_error_restores_value() {
        let mut owner = Owner::new(42);
        let result = owner.own_result(|v| Err::<i32, (i32, &str)>((v, "fail")));
        assert!(result.is_err());
        // Owner still holds the original value -- no poisoning
        assert_eq!(*owner, 42);
    }

    #[test]
    fn test_own_result_error_restores_modified_value() {
        let mut owner = Owner::new(42);
        let result = owner.own_result(|v| {
            let modified = v + 10;
            Err::<i32, (i32, &str)>((modified, "fail"))
        });
        assert!(result.is_err());
        // Owner holds the value returned in the Err variant
        assert_eq!(*owner, 52);
    }
}
