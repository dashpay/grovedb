/// Holds a type that must always exist, while allow for atomic swaps
struct GuardedOption<T> {
    inner: Option<T>,
}

impl<T> GuardedOption<T> {
    /// Returns a new instance given some value
    fn new(value: T) -> Self {
        Self { inner: Some(value) }
    }

    /// Returns a reference to the inner value
    fn value(&self) -> &T {
        self.inner.as_ref().expect("value always exists")
    }

    /// Replaces the inner value with the output of some function
    /// that makes to the same type
    fn replace<F>(&mut self, replace_fn: F)
    where
        F: Fn() -> T,
    {
        let old_value = self.inner.take().expect("value always exists");
        drop(old_value);
        self.inner = Some(replace_fn())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdsf() {
        let mut a = GuardedOption::new(5);
        assert_eq!(a.value(), &5);

        let retrun_10 = || 10;

        a.replace(retrun_10);
        assert_eq!(a.value(), &10);
    }
}
