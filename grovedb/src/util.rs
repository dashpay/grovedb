// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

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

/// Macro to execute same piece of code on different storage contexts
/// (transactional or not) using path argument.
macro_rules! storage_context_with_parent_optional_tx {
    (
	&mut $cost:ident,
	$db:expr,
	$path:expr,
	$transaction:ident,
	$storage:ident,
	$root_key:ident,
    $is_sum_tree:ident,
	{ $($body:tt)* }
    ) => {
        {
            use ::storage::Storage;
	    let mut path = $path.clone();
            if let Some(tx) = $transaction {
                let $storage = $db
                    .get_transactional_storage_context(path.clone(), tx)
		    .unwrap_add_cost(&mut $cost);
                if let Some(last) = path.next_back() {
                    let parent_storage = $db.get_transactional_storage_context(path, tx)
			.unwrap_add_cost(&mut $cost);
                    let element = cost_return_on_error!(
                        &mut $cost,
                        Element::get_from_storage(&parent_storage, last).map_err(|e| {
                            Error::PathParentLayerNotFound(
                                format!(
				    "could not get key for parent of subtree optional on tx: {}",
				    e
				)
                            )
                        })
                    );
                    match element {
                        Element::Tree(root_key, _) => {
                            let $root_key = root_key;
                            let $is_sum_tree = false;
                            $($body)*
                        }
                        Element::SumTree(root_key, ..) => {
                            let $root_key = root_key;
                            let $is_sum_tree = true;
                            $($body)*
                        }
                        _ => {
                            return Err(Error::CorruptedData(
                                "parent is not a tree"
                                    .to_owned(),
                            )).wrap_with_cost($cost);
                        }
                    }
                } else {
                    return Err(Error::CorruptedData(
                        "path is empty".to_owned(),
                    )).wrap_with_cost($cost);
                }
            } else {
                let $storage = $db
                    .get_storage_context(path.clone()).unwrap_add_cost(&mut $cost);
                if let Some(last) = path.next_back() {
                    let parent_storage = $db.get_storage_context(
			path.clone()
		    ).unwrap_add_cost(&mut $cost);
                    let element = cost_return_on_error!(
                        &mut $cost,
			Element::get_from_storage(&parent_storage, last).map_err(|e| {
                            Error::PathParentLayerNotFound(
                                format!(
				    "could not get key for parent of subtree optional no tx: {}",
				    e
				)
                            )
                        })
                    );
                    match element {
                        Element::Tree(root_key, _) => {
                            let $root_key = root_key;
                            let $is_sum_tree = false;
                            $($body)*
                        }
                        Element::SumTree(root_key, ..) => {
                            let $root_key = root_key;
                            let $is_sum_tree = true;
                            $($body)*
                        }
                        _ => {
                            return Err(Error::CorruptedData(
                                "parent is not a tree"
                                    .to_owned(),
                            )).wrap_with_cost($cost);
                        }
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

/// Macro to execute same piece of code on different storage contexts with
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

/// Macro to execute same piece of code on Merk with varying storage
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
            storage_context_with_parent_optional_tx!(
                &mut $cost,
                $db,
                $path,
                $transaction,
                storage,
                root_key,
                is_sum_tree,
                {
                    #[allow(unused_mut)]
                    let mut $subtree = cost_return_on_error!(
                        &mut $cost,
                        ::merk::Merk::open_layered_with_root_key(storage, root_key, is_sum_tree)
                            .map(|merk_res|
                                 merk_res
                                 .map_err(|_| crate::Error::CorruptedData(
                                     "cannot open a subtree".to_owned()
                                 ))
                            )
                    );
                    $($body)*
                }
            )
        }
    };
}

/// Macro to execute same piece of code on Merk with varying storage
/// contexts.
macro_rules! root_merk_optional_tx {
    (
        &mut $cost:ident,
        $db:expr,
        $transaction:ident,
        $subtree:ident,
        { $($body:tt)* }
    ) => {
        {
            use crate::util::storage_context_optional_tx;
            storage_context_optional_tx!($db, [], $transaction, storage, {
                let $subtree = cost_return_on_error!(
                    &mut $cost,
                    ::merk::Merk::open_base(storage.unwrap_add_cost(&mut $cost), false)
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
pub(crate) use root_merk_optional_tx;
pub(crate) use storage_context_optional_tx;
// pub(crate) use storage_context_with_parent_no_tx;
pub(crate) use storage_context_with_parent_optional_tx;
// pub(crate) use storage_context_with_parent_using_tx;
