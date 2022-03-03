/// Macro to execute same piece of code on different Merk
/// implementations (transactional or not).
#[macro_export]
macro_rules! merk_optional_tx {
    ($db:expr, $path:expr, $transaction:ident, mut $subtree:ident, { $($body:tt)* }) => {
        if let Some(tx) = $transaction {
            let subtree_storage = $db
                .get_prefixed_transactional_context_from_path($path, tx);
            let mut $subtree = ::merk::Merk::open(subtree_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            $($body)*
        } else {
            let subtree_storage = $db
                .get_prefixed_context_from_path($path);
            let mut $subtree = ::merk::Merk::open(subtree_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            $($body)*
        }
    };

    ($db:expr, $path:expr, $transaction:ident, $subtree:ident, { $($body:tt)* }) => {
        if let Some(tx) = $transaction {
            let subtree_storage = $db
                .get_prefixed_transactional_context_from_path($path, tx);
            let $subtree = ::merk::Merk::open(subtree_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            $($body)*
        } else {
            let subtree_storage = $db
                .get_prefixed_context_from_path($path);
            let $subtree = ::merk::Merk::open(subtree_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            $($body)*
        }
    };
}
