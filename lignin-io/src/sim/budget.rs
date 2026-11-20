//! A capacity in bytes for the simulated device: an append or a write is
//! refused when the files hold too much for it, and there is room again
//! when a file is removed or cut short. [`SimVfs::set_capacity`] says what
//! is counted, and what the capacity does not bound.

use super::{fault::OpKind, state::State, SimVfs};

impl State {
    /// Whether the device has no room for what an operation of `kind` with
    /// a buffer of `len` bytes needs.
    pub(super) fn lacks_room(&self, kind: OpKind, len: u64) -> bool {
        let Some(capacity) = self.capacity else {
            return false;
        };
        let needs = match kind {
            OpKind::Append | OpKind::Write => len,
            _ => return false,
        };
        self.used_bytes().saturating_add(needs) > capacity
    }
}

impl SimVfs {
    /// Gives the device a capacity of `bytes`, or with `None` takes it
    /// away. From here on an `append` and a `write_at` fail with
    /// [`Fault::Enospc`](super::Fault) when the bytes the files hold and
    /// the bytes the operation needs (the table below) are together more
    /// than that, as one that a rule fails does, and what a failed
    /// operation leaves is what the fault model says it leaves.
    ///
    /// A [`FaultRule`](super::FaultRule) makes the device full from one
    /// operation on, whatever is removed from it. That is enough to see
    /// what code does when it is refused space, and not to see whether it
    /// gets space back by what it removes. A capacity is.
    ///
    /// A capacity below what the files hold already is one that is used
    /// up: nothing that needs room succeeds until enough is removed.
    ///
    /// # What is counted
    ///
    /// **The bytes of the files, as operations see them**
    /// ([`Self::used_bytes`]): the length of every file that a name or an
    /// open handle leads to now, each file once, however many names it
    /// has. A file whose last name is removed holds nothing once its last
    /// handle is closed, whether or not the removal was synced, and a file
    /// that is cut short holds its new length. A lock that is held on a
    /// file is no handle here: a file that has neither a name nor a handle
    /// is not counted while its lock is held.
    ///
    /// **What an operation needs:**
    ///
    /// | Operation | Needs |
    /// | --- | --- |
    /// | `append` | the length of its buffer |
    /// | `write_at` | the length of its buffer, wherever it writes: also where it overwrites, since a device that writes anew what is overwritten needs that room, and no more where it begins behind the end of the file, since what lies between is a hole |
    /// | Every other one | nothing: a name, a directory and a sync take no room, and a `set_len` that grows a file makes a hole |
    ///
    /// So a name can be made on a device that is full, and the first byte
    /// that is written under it fails. A rule fails the making of a name
    /// as well, and a sync, for code that has to be shown those. The
    /// length of a hole counts like any other once it is there.
    ///
    /// # What the capacity is not
    ///
    /// - **It is no bound on what the files hold.** It bounds what an
    ///   operation is given, by the table above. The files can hold more
    ///   than the capacity in two ways. A hole takes none of it when it
    ///   is made, by `set_len` or by a `write_at` behind the end of a
    ///   file, and its length is counted from then on. And each append or
    ///   write that fails can leave some of its sectors in the file, as
    ///   the fault model has it: less than its buffer each time, and with
    ///   no limit over many of them.
    /// - **It is not carried into what a crash leaves.** A file system
    ///   that [`CrashImage::recover`](super::CrashImage::recover) makes
    ///   has no capacity, as it has no rule: whoever wants one there sets
    ///   it.
    /// - **It does not count what a crash can still bring back.** The
    ///   bytes of a file whose removal is not durable are held by the
    ///   device until it is, and are not counted here.
    pub fn set_capacity(&self, bytes: Option<u64>) {
        self.shared.state().capacity = bytes;
    }

    /// The capacity of the device, if it has one.
    pub fn capacity(&self) -> Option<u64> {
        self.shared.state().capacity
    }

    /// The bytes the files hold now: the length of every file that a name
    /// or an open handle leads to, each file once.
    pub fn used_bytes(&self) -> u64 {
        self.shared.state().used_bytes()
    }
}
