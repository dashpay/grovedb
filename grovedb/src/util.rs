/// Macro to execute same piece of code on different storage_cost contexts
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

/// Macro to execute same piece of code on different storage_cost contexts
/// (transactional or not) using path argument.
macro_rules! storage_context_with_parent_optional_tx {
    (&mut $cost:ident, $db:expr, $path:expr, $transaction:ident, $storage:ident, $root_key:ident, { $($body:tt)* }) => {
        {
            use ::storage::Storage;
            if let Some(tx) = $transaction {
                let $storage = $db
                    .get_transactional_storage_context($path, tx).unwrap_add_cost(&mut $cost);
                if let Some(last) = $path.next_back() {
                    let parent_storage = $db.get_transactional_storage_context($path, tx).unwrap_add_cost(&mut $cost);
                    let element = cost_return_on_error!(
                                    &mut $cost,
                                    Element::get_from_storage(&parent_storage, last).map_err(|_| {
                                        Error::CorruptedData(
                                            "could not get key for parent of subtree".to_owned(),
                                        )
                                    })
                                );
                    if let Element::Tree(root_key, _) = element {
                        let $root_key = root_key;
                        $($body)*
                    } else {
                        return Err(Error::CorruptedData(
                            "parent is not a tree"
                                .to_owned(),
                        )).wrap_with_cost($cost);
                    }

                } else {
                    return Err(Error::CorruptedData(
                                            "path is empty".to_owned(),
                                        )).wrap_with_cost($cost);
                }
            } else {
                let $storage = $db
                    .get_storage_context($path).unwrap_add_cost(&mut $cost);
                if let Some(last) = $path.next_back() {
                    let parent_storage = $db.get_storage_context($path).unwrap_add_cost(&mut $cost);
                    let element = cost_return_on_error!(
                                    &mut $cost,
                                    Element::get_from_storage(&parent_storage, last).map_err(|_| {
                                        Error::CorruptedData(
                                            "could not get key for parent of subtree".to_owned(),
                                        )
                                    })
                                );
                    if let Element::Tree(root_key, _) = element {
                        let $root_key = root_key;
                                $($body)*
                            } else {
                                return Err(Error::CorruptedData(
                                    "parent is not a tree"
                                        .to_owned(),
                                )).wrap_with_cost($cost);
                            }
                } else {
                    return Err(Error::CorruptedData(
                                            "path is empty".to_owned(),
                                        )).wrap_with_cost($cost);
                }
            }
        }
    };
}

/// Macro to execute same piece of code on different storage_cost contexts with
/// empty prefix.
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

/// Macro to execute same piece of code on Merk with varying storage_cost
/// contexts.
macro_rules! merk_optional_tx {
    (
        &mut $cost:ident,
        $db:expr,
        $path:expr,
        $transaction:ident,
        $subtree:ident,
        { $($body:tt)* }
    ) => {
        {
            use crate::util::storage_context_with_parent_optional_tx;
            storage_context_with_parent_optional_tx!(&mut $cost, $db, $path, $transaction, storage, root_key, {
                let mut $subtree = cost_return_on_error!(
                    &mut $cost,
                    ::merk::Merk::open_with_root_key(storage, root_key)
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
pub(crate) use storage_context_with_parent_optional_tx;
