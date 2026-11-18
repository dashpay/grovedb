//! The flat keyspaces of the RocksDB storage contexts.
//!
//! A context is the handle to its subtree's data keyspace, and hands out
//! handles to the subtree's aux and meta keyspaces. Each forwards to the
//! context method of the matching column family, so a call through
//! [`FlatStore`] costs what the same call through [`StorageContext`] costs.

use grovedb_costs::{storage_cost::key_value_cost::KeyValueStorageCost, CostResult};

use super::{PrefixedRocksDbImmediateStorageContext, PrefixedRocksDbTransactionContext};
use crate::{Error, FlatStore, StorageContext};

/// The aux keyspace of a storage context's subtree.
pub struct AuxKeyspace<'a, C>(&'a C);

/// The meta keyspace of a storage context's subtree.
pub struct MetaKeyspace<'a, C>(&'a C);

macro_rules! impl_flat_keyspaces {
    ($context:ident) => {
        impl<'db> $context<'db> {
            /// The aux keyspace of this context's subtree.
            pub fn aux(&self) -> AuxKeyspace<'_, Self> {
                AuxKeyspace(self)
            }

            /// The meta keyspace of this context's subtree.
            pub fn meta(&self) -> MetaKeyspace<'_, Self> {
                MetaKeyspace(self)
            }
        }

        impl<'db> FlatStore for $context<'db> {
            type Cursor<'a>
                = <Self as StorageContext<'db>>::RawIterator
            where
                Self: 'a;

            #[inline]
            fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
                StorageContext::get(self, key)
            }

            #[inline]
            fn put<K: AsRef<[u8]>>(
                &self,
                key: K,
                value: &[u8],
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostResult<(), Error> {
                StorageContext::put(self, key, value, None, cost_info)
            }

            #[inline]
            fn delete<K: AsRef<[u8]>>(
                &self,
                key: K,
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostResult<(), Error> {
                StorageContext::delete(self, key, cost_info)
            }

            #[inline]
            fn cursor(&self) -> Self::Cursor<'_> {
                self.raw_iter()
            }
        }

        impl<'db> FlatStore for AuxKeyspace<'_, $context<'db>> {
            type Cursor<'a>
                = <$context<'db> as StorageContext<'db>>::RawIterator
            where
                Self: 'a;

            #[inline]
            fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
                self.0.get_aux(key)
            }

            #[inline]
            fn put<K: AsRef<[u8]>>(
                &self,
                key: K,
                value: &[u8],
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostResult<(), Error> {
                self.0.put_aux(key, value, cost_info)
            }

            #[inline]
            fn delete<K: AsRef<[u8]>>(
                &self,
                key: K,
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostResult<(), Error> {
                self.0.delete_aux(key, cost_info)
            }

            #[inline]
            fn cursor(&self) -> Self::Cursor<'_> {
                self.0.raw_iter_aux()
            }
        }

        impl<'db> FlatStore for MetaKeyspace<'_, $context<'db>> {
            type Cursor<'a>
                = <$context<'db> as StorageContext<'db>>::RawIterator
            where
                Self: 'a;

            #[inline]
            fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
                self.0.get_meta(key)
            }

            #[inline]
            fn put<K: AsRef<[u8]>>(
                &self,
                key: K,
                value: &[u8],
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostResult<(), Error> {
                self.0.put_meta(key, value, cost_info)
            }

            #[inline]
            fn delete<K: AsRef<[u8]>>(
                &self,
                key: K,
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostResult<(), Error> {
                self.0.delete_meta(key, cost_info)
            }

            #[inline]
            fn cursor(&self) -> Self::Cursor<'_> {
                self.0.raw_iter_meta()
            }
        }
    };
}

impl_flat_keyspaces!(PrefixedRocksDbTransactionContext);
impl_flat_keyspaces!(PrefixedRocksDbImmediateStorageContext);
