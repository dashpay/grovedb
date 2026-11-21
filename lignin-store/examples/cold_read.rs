//! Cold reads of a page store through the system's page cache and past it.
//!
//! The measurement of LigninDB task IO-03: the same pages, each read once
//! into a buffer pool of the same size, once on [`StdVfs`] and once on
//! `lignin_io::linux::DirectVfs`. It is built on Linux with the feature
//! `direct_io`.
//!
//! ```text
//! cold_read build DIR PAGES            a store of PAGES tree pages in DIR
//! cold_read pool DIR VFS POOL_MIB [THREADS]
//!                                      every page once through the page
//!                                      loader
//! cold_read raw DIR VFS                the blocks of the same pages, read
//!                                      at the file layer and not decoded
//! cold_read open DIR VFS POOL_MIB      what `pool` does before its first
//!                                      read, and no read
//! ```
//!
//! `VFS` is `std`, `direct`, or for `raw` also `batch`, which reads 32
//! blocks at a time with `DirectVfs::read_batch`.
//!
//! **The order.** The pages are put in one order that is drawn from a fixed
//! seed, the same in every run. With one thread, and in `raw` with `std`
//! and `direct`, the reads are made in that order. With `batch` it is the
//! order in which the reads are handed to `DirectVfs::read_batch`, 32 to
//! a call and the rest in the last: in which order the reads of one call
//! are made is the file layer's and the system's. With more threads the
//! order is cut into parts whose length is the pages divided by
//! `THREADS`, rounded up; the last part is shorter where the pages are no
//! multiple of that length; each part has a thread, which can make fewer
//! threads than were asked for; and each thread reads its part in order: what is the same from run to
//! run is the pages of each thread and their order in it, not how the
//! reads of the threads fall among one another.
//!
//! **What is timed and counted.** A run prints one line: what it read, a
//! time (`wall_us`), and what the kernel counted for the process over the
//! same stretch (`/proc/self/io`): `syscr`, the read system calls; `rchar`,
//! the bytes they returned; and `read_bytes`, the bytes the process had
//! fetched from storage. The stretch is the reads and what goes with
//! them: in `pool`, the start and the end of the threads and the
//! gathering of the pool's counts; in `raw`, the buffers the blocks are
//! read into. It begins after the index was read and the order was drawn:
//! in `raw` after the files were opened as well, and in `pool` after the
//! manifest and the segment store were opened and the page table and the
//! pool were made. The kernel's counts take in the reading of
//! `/proc/self/io` that begins the stretch: `syscr` and `rchar` are a few
//! calls and some bytes more than the reads of the store. Reads that a ring
//! makes are no read system calls, so `batch` shows them in the file
//! layer's own counts.
//!
//! `open` is `pool` with no page to read and no thread started: the
//! process does what `pool` does before its first read, but for drawing
//! the order. It is there for what is measured of the whole process from
//! outside, such as a count of instructions.
//!
//! The run does not make the cache cold. Whoever runs it writes the store
//! out and evicts its files from the page cache before a run on `std`
//! (`posix_fadvise` with `POSIX_FADV_DONTNEED`), and checks afterwards what
//! of the files the page cache holds.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cold_read measures the direct I/O path, which is built on Linux");
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::{btree_map::Entry, BTreeMap},
        fs,
        path::Path,
        sync::Arc,
        thread,
        time::Instant,
    };

    use lignin_format::{
        manifest::Manifest,
        segment::{BlockContent, Compression},
        tree_page::{CellContent, CellValue, TreePageMut},
        CommitVersion, FileId, PageId, PageSize, SlotId,
    };
    use lignin_io::{
        linux::{DirectVfs, ReadAt},
        OpenMode, StdVfs, Vfs, VfsFile,
    };
    use lignin_store::{
        cache::{BufferPool, PoolConfig},
        load::{LoadBuffers, PageLoader},
        segment::{BlockLocation, FileNames, ManifestStore, SegmentStore},
        table::{Link, PageTable},
    };

    type Error = Box<dyn std::error::Error + Send + Sync>;

    const SIZE: PageSize = PageSize::DEFAULT;
    const SEED: u64 = 0x1052_0003;
    /// A segment takes this many blocks, about 100 MiB.
    const BLOCKS_A_SEGMENT: usize = 4096;
    const INDEX: &str = "cold-read.index";
    const USAGE: &str = "usage: cold_read build DIR PAGES | pool DIR VFS POOL_MIB [THREADS] | raw \
                         DIR VFS | open DIR VFS POOL_MIB";

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn fill(&mut self, bytes: &mut [u8]) {
            for chunk in bytes.chunks_mut(8) {
                let word = self.next().to_le_bytes();
                chunk.copy_from_slice(&word[..chunk.len()]);
            }
        }

        /// The numbers below `count`, each once, in an order of the seed.
        fn order(&mut self, count: usize) -> Vec<usize> {
            let mut order: Vec<usize> = (0..count).collect();
            for at in (1..count).rev() {
                order.swap(at, (self.next() % (at as u64 + 1)) as usize);
            }
            order
        }
    }

    /// The image of a tree page that is about three quarters full of cells
    /// whose keys, hashes and values do not compress.
    fn image(page: PageId, rng: &mut Rng) -> Result<Vec<u8>, Error> {
        let mut frame = vec![0; SIZE.get()];
        let mut written = TreePageMut::init(&mut frame, SIZE, page, 0)?;
        let (mut key, mut value, mut hashes) = ([0; 40], [0; 120], [0; 64]);
        let mut slot = 0;
        while written.free_bytes() > SIZE.get() / 4 {
            slot += 1;
            rng.fill(&mut key);
            rng.fill(&mut value);
            rng.fill(&mut hashes);
            let (value_hash, kv_hash) = hashes.split_at(32);
            written.insert(
                SlotId::new(slot).ok_or("a slot")?,
                &CellContent {
                    key: &key,
                    feature: &[],
                    value_hash: value_hash.try_into()?,
                    kv_hash: kv_hash.try_into()?,
                    left: None,
                    right: None,
                    hosted_roots: &[],
                    value: CellValue::Inline(&value),
                    fragment_root: false,
                },
            )?;
        }
        Ok(written.page().to_image())
    }

    fn build(dir: &Path, pages: u32) -> Result<(), Error> {
        fs::create_dir(dir)?;
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new())?;
        let mut manifest = ManifestStore::create(vfs, dir, empty)?;
        let segments = SegmentStore::open(&manifest)?;
        let mut rng = Rng(SEED);
        let mut index = String::new();
        let mut bytes = 0;
        let mut writer = None;
        for raw in 1..=pages {
            let page = PageId::new(raw).ok_or("a page id")?;
            let into = match &mut writer {
                Some(writer) => writer,
                empty => {
                    let created = segments.create()?;
                    segments.commit(&mut manifest)?;
                    empty.insert(created)
                }
            };
            let at = into.append(
                page,
                BlockContent::Base,
                Compression::Lz4,
                &image(page, &mut rng)?,
            )?;
            index.push_str(&format!(
                "{raw} {} {} {}\n",
                at.file.get(),
                at.offset,
                at.len
            ));
            bytes += u64::from(at.len);
            if into.block_count() == BLOCKS_A_SEGMENT || raw == pages {
                into.seal()?;
                segments.commit(&mut manifest)?;
                writer = None;
            }
        }
        fs::write(dir.join(INDEX), index)?;
        println!("built pages={pages} block_bytes={bytes}");
        Ok(())
    }

    /// Where the block of each page is, as `build` wrote it down.
    fn index(dir: &Path) -> Result<Vec<(PageId, BlockLocation)>, Error> {
        let mut blocks = Vec::new();
        for line in fs::read_to_string(dir.join(INDEX))?.lines() {
            let mut fields = line.split(' ');
            let mut field = || fields.next().ok_or("a field of the index");
            let page = PageId::new(field()?.parse()?).ok_or("a page id")?;
            let location = BlockLocation {
                file: FileId::new(field()?.parse()?).ok_or("a file id")?,
                offset: field()?.parse()?,
                len: field()?.parse()?,
            };
            blocks.push((page, location));
        }
        Ok(blocks)
    }

    /// The read system calls of this process, the bytes they returned, and
    /// the bytes the process had fetched from storage.
    fn kernel_counts() -> Result<[u64; 3], Error> {
        let io = fs::read_to_string("/proc/self/io")?;
        let field = |name: &str| -> Result<u64, Error> {
            let line = io.lines().find(|line| line.starts_with(name));
            Ok(line
                .and_then(|line| line.split_whitespace().nth(1))
                .ok_or("a field of /proc/self/io")?
                .parse()?)
        };
        Ok([field("syscr:")?, field("rchar:")?, field("read_bytes:")?])
    }

    /// Runs `reads` and prints what it cost: the time, and what the kernel
    /// and the direct file layer counted while it ran.
    fn measured(
        what: &str,
        direct: &DirectVfs,
        reads: impl FnOnce() -> Result<(u64, String), Error>,
    ) -> Result<(), Error> {
        let (before, layer_before) = (kernel_counts()?, direct.stats());
        let started = Instant::now();
        let (sum, counts) = reads()?;
        let micros = started.elapsed().as_micros();
        let (after, layer) = (kernel_counts()?, direct.stats());
        let [syscr, rchar, read_bytes] = [0, 1, 2].map(|at| after[at] - before[at]);
        println!(
            "{what} {counts} wall_us={micros} syscr={syscr} rchar={rchar} \
             read_bytes={read_bytes} direct_preads={} ring_calls={} ring_reads={} rings_lost={} \
             direct_bytes={} sum={sum:016x}",
            layer.preads - layer_before.preads,
            layer.ring_calls - layer_before.ring_calls,
            layer.ring_reads - layer_before.ring_reads,
            layer.rings_lost - layer_before.rings_lost,
            layer.bytes - layer_before.bytes
        );
        Ok(())
    }

    fn layer(name: &str, direct: &DirectVfs) -> Result<Arc<dyn Vfs>, Error> {
        match name {
            "std" => Ok(Arc::new(StdVfs)),
            "direct" => Ok(Arc::new(direct.clone())),
            other => Err(format!("VFS is std or direct here, not {other}").into()),
        }
    }

    /// Every page once through the page loader, or with `read` unset only
    /// what comes before the first read.
    fn pool(
        dir: &Path,
        vfs: &str,
        pool_mib: usize,
        threads: usize,
        read: bool,
    ) -> Result<(), Error> {
        let direct = DirectVfs::new();
        let manifest = ManifestStore::open(layer(vfs, &direct)?, dir)?;
        let segments = SegmentStore::open(&manifest)?;
        let blocks = index(dir)?;
        let table = PageTable::new();
        let version = CommitVersion::new(1).ok_or("a version")?;
        for (page, location) in &blocks {
            table.push(*page, Link::base(version, *location))?;
        }
        let pool = BufferPool::new(PoolConfig {
            page_size: SIZE,
            capacity: pool_mib << 20,
        })?;
        let loader = PageLoader::new(&pool, &table, &segments, SIZE)?;
        let order = Rng(SEED).order(if read { blocks.len() } else { 0 });
        let what = format!("pool vfs={vfs} pool_mib={pool_mib} threads={threads}");
        measured(&what, &direct, || {
            let share = order.len().div_ceil(threads.max(1)).max(1);
            let sums = thread::scope(|scope| {
                let readers: Vec<_> = order
                    .chunks(share)
                    .map(|pages| {
                        let blocks = &blocks;
                        scope.spawn(move || -> Result<u64, Error> {
                            let mut buffers = LoadBuffers::new();
                            let mut sum = 0u64;
                            for at in pages {
                                let frame = loader.get(blocks[*at].0, version, &mut buffers)?;
                                let head: [u8; 8] = frame[..8].try_into()?;
                                sum = sum.rotate_left(1) ^ u64::from_le_bytes(head);
                            }
                            Ok(sum)
                        })
                    })
                    .collect();
                readers
                    .into_iter()
                    .map(|reader| reader.join().map_err(|_| "a reader panicked")?)
                    .collect::<Result<Vec<u64>, Error>>()
            })?;
            let made = pool.stats();
            let counts = format!(
                "pages={} hits={} misses={} evictions={} overflows={}",
                order.len(),
                made.hits,
                made.misses,
                made.evictions,
                made.overflows
            );
            Ok((sums.iter().fold(0, |sum, part| sum ^ part), counts))
        })
    }

    /// The blocks of every page once, read at the file layer.
    fn raw(dir: &Path, vfs: &str) -> Result<(), Error> {
        let direct = DirectVfs::new();
        let batch = vfs == "batch";
        let files_of = layer(if batch { "direct" } else { vfs }, &direct)?;
        let blocks = index(dir)?;
        let mut files: BTreeMap<FileId, Box<dyn VfsFile>> = BTreeMap::new();
        for (_, location) in &blocks {
            if let Entry::Vacant(unopened) = files.entry(location.file) {
                let path = FileNames::SEGMENT.path(dir, location.file);
                unopened.insert(files_of.open(&path, OpenMode::Read)?);
            }
        }
        let order = Rng(SEED).order(blocks.len());
        measured(&format!("raw vfs={vfs}"), &direct, || {
            let mut sum = 0u64;
            let mut bufs: Vec<Vec<u8>> = Vec::new();
            for group in order.chunks(if batch { 32 } else { 1 }) {
                bufs.resize_with(group.len(), Vec::new);
                let mut reads: Vec<ReadAt<'_>> = group
                    .iter()
                    .zip(bufs.iter_mut())
                    .map(|(at, buf)| {
                        let location = blocks[*at].1;
                        buf.resize(location.len as usize, 0);
                        ReadAt {
                            file: &*files[&location.file],
                            offset: location.offset,
                            buf,
                        }
                    })
                    .collect();
                if batch {
                    direct.read_batch(&mut reads)?;
                } else {
                    for read in &mut reads {
                        read.file.read_exact_at(read.buf, read.offset)?;
                    }
                }
                for read in &reads {
                    let head: [u8; 8] = read.buf[..8].try_into()?;
                    sum = sum.rotate_left(1) ^ u64::from_le_bytes(head);
                }
            }
            Ok((sum, format!("blocks={}", order.len())))
        })
    }

    pub(super) fn main() -> Result<(), Error> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match args[..] {
            ["build", dir, pages] => build(Path::new(dir), pages.parse()?),
            ["pool", dir, vfs, pool_mib] => pool(Path::new(dir), vfs, pool_mib.parse()?, 1, true),
            ["pool", dir, vfs, pool_mib, threads] => pool(
                Path::new(dir),
                vfs,
                pool_mib.parse()?,
                threads.parse()?,
                true,
            ),
            ["open", dir, vfs, pool_mib] => pool(Path::new(dir), vfs, pool_mib.parse()?, 1, false),
            ["raw", dir, vfs] => raw(Path::new(dir), vfs),
            _ => Err(USAGE.into()),
        }
    }
}
