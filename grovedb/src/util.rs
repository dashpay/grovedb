/// Macro to execute same piece of code on different storage contexts
/// (transactional or not) using path argument.
macro_rules! storage_context_optional_tx {
    ($db:expr, $path:expr, $transaction:ident, $storage:ident, { $($body:tt)* }) => {
        {
            use ::storage::Storage;
            if let Some(tx) = $transaction {
                let $storage = $db
                    .get_transactional_storage_context($path, tx);
                $($body)*
            } else {
                let $storage = $db
                    .get_storage_context($path);
                $($body)*
            }
        }
    };
}

/// Macro to execute same piece of code on different storage contexts with empty
/// prefix.
macro_rules! meta_storage_context_optional_tx {
    ($db:expr, $transaction:ident, $storage:ident, { $($body:tt)* }) => {
        {
            use ::storage::Storage;
            if let Some(tx) = $transaction {
                let $storage = $db
                    .get_transactional_storage_context(::std::iter::empty(), tx);
                $($body)*
            } else {
                let $storage = $db
                    .get_storage_context(::std::iter::empty());
                $($body)*
            }
        }
    };
}

/// Macro to execute same piece of code on Merk with varying storage contexts.
macro_rules! merk_optional_tx {
    (
        &mut $cost:ident,
        $db:expr,
        $path:expr,
        $transaction:ident,
        mut $subtree:ident,
        { $($body:tt)* }
    ) => {
        {
            use crate::util::storage_context_optional_tx;
            storage_context_optional_tx!($db, $path, $transaction, storage, {
                let mut $subtree = cost_return_on_error!(
                    &mut $cost,
                    ::merk::Merk::open(storage)
                        .map(|merk_res|
                             merk_res
                                .map_err(|_| crate::Error::CorruptedData(
                                    "cannot open a subtree".to_owned()
                                ))
                             )
                );
                $($body)*
            })
        }
    };

    (
        &mut $cost:ident,
        $db:expr,
        $path:expr,
        $transaction:ident,
        $subtree:ident,
        { $($body:tt)* }
    ) => {
        {
            use crate::util::storage_context_optional_tx;
            storage_context_optional_tx!($db, $path, $transaction, storage, {
                let $subtree = cost_return_on_error!(
                    &mut $cost,
                    ::merk::Merk::open(storage)
                        .map(|merk_res|
                             merk_res
                                .map_err(|_| crate::Error::CorruptedData(
                                    "cannot open a subtree".to_owned()
                                ))
                        )
                );
                $($body)*
            })
        }
    };
}

pub(crate) use merk_optional_tx;
pub(crate) use meta_storage_context_optional_tx;
pub(crate) use storage_context_optional_tx;
