//! A set synced together is the single syncs of its list: each one an
//! operation that a crash point and a fault can fall on.

use super::*;

/// A directory and two files in which nothing is durable yet, and the list
/// that syncs all of it: the root, `/a`, `/dir`, `/dir/b`.
struct Unsynced {
    fs: SimVfs,
    a: Box<dyn VfsFile>,
    b: Box<dyn VfsFile>,
}

impl Unsynced {
    fn new(seed: u64) -> Self {
        let fs = SimVfs::new(SimConfig {
            trace_ops: true,
            ..SimConfig::new(seed)
        });
        fs.create_dir(p("/dir")).unwrap();
        let a = create(&fs, "/a", b"a");
        let b = create(&fs, "/dir/b", b"b");
        Self { fs, a, b }
    }

    fn sync(&self) -> io::Result<()> {
        self.fs.sync_together(&[
            SyncItem::Dir(p("/")),
            SyncItem::Data(&*self.a),
            SyncItem::Dir(p("/dir")),
            SyncItem::All(&*self.b),
        ])
    }

    /// What a crash leaves that loses everything it may lose.
    fn durable(&self) -> Tree {
        let image = self.fs.crash_image();
        tree(&recover(&image, &image.lose_all()))
    }
}

/// What is durable after each of the four syncs.
fn after_syncs() -> [Tree; 5] {
    let dir = None;
    [
        tree_of(&[]),
        tree_of(&[("a", Some("")), ("dir", dir)]),
        tree_of(&[("a", Some("a")), ("dir", dir)]),
        tree_of(&[("a", Some("a")), ("dir", dir), ("dir/b", Some(""))]),
        tree_of(&[("a", Some("a")), ("dir", dir), ("dir/b", Some("b"))]),
    ]
}

#[test]
fn a_set_is_made_durable_by_the_single_syncs_of_its_list() {
    let set = Unsynced::new(51);
    let before = set.fs.stats();
    assert_eq!(set.durable(), after_syncs()[0]);

    set.sync().unwrap();

    let after = set.fs.stats();
    set.fs.sync_together(&[]).unwrap();
    assert_eq!(set.fs.stats(), after);
    assert_eq!(after.ops - before.ops, 4);
    assert_eq!(after.syncs - before.syncs, 4);
    assert_eq!(after.changes - before.changes, 4);
    let made: Vec<(OpKind, String)> = set
        .fs
        .trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::Op(op) if op.op > before.ops => Some((op.kind, op.path)),
            _ => None,
        })
        .collect();
    let expected = [
        (OpKind::SyncDir, "/"),
        (OpKind::SyncData, "/a"),
        (OpKind::SyncDir, "/dir"),
        (OpKind::SyncAll, "/dir/b"),
    ]
    .map(|(kind, path)| (kind, path.to_owned()));
    assert_eq!(made, expected);

    let image = set.fs.crash_image();
    assert!(image.names().is_empty() && image.changes().is_empty());
    assert_eq!(set.durable(), after_syncs()[4]);
    assert_eq!(set.durable(), tree(&set.fs));
    check(&set.fs);
}

#[test]
fn a_crash_inside_a_set_finds_the_first_syncs_made_and_the_others_not() {
    for made in 0..=4 {
        let set = Unsynced::new(52);
        let syncs = set.fs.stats().syncs;
        set.fs.crash_at(CrashPoint::AfterSync(syncs + made));
        assert_eq!(set.fs.is_powered_off(), made == 0);

        match set.sync() {
            Ok(()) => assert_eq!(made, 4),
            Err(error) => {
                assert!(SimError::is_power_loss(&error), "{error}");
                assert!(made < 4);
            }
        }
        assert_eq!(set.fs.stats().syncs - syncs, made);
        assert_eq!(set.durable(), after_syncs()[made as usize], "{made} made");

        let image = set.fs.crash_image();
        let fingerprints: BTreeSet<u64> = image
            .prefixes()
            .chain(image.holes())
            .chain(image.reorders())
            .map(|plan| recover(&image, &plan).fingerprint())
            .collect();
        assert_eq!(fingerprints.len() > 1, made < 4, "{made} made");
    }
}

#[test]
fn before_the_first_sync_of_a_set_a_crash_may_keep_a_later_one_without_an_earlier() {
    let set = Unsynced::new(53);
    let image = set.fs.crash_image();
    let (names, changes) = (image.names(), image.changes());
    assert_eq!((names.len(), changes.len()), (3, 2));
    assert_eq!(changes[1].path, "/dir/b");

    let b_without_a = CrashPlan {
        names: (0..names.len()).collect(),
        changes: vec![Survivor::whole(1)],
    };
    assert_eq!(
        tree(&recover(&image, &b_without_a)),
        tree_of(&[("a", Some("")), ("dir", None), ("dir/b", Some("b"))])
    );
    assert!(every_plan(&image).contains(&b_without_a));
}

#[test]
fn a_fault_on_one_sync_of_a_set_fails_the_set_there() {
    let set = Unsynced::new(54);
    let syncs = set.fs.stats().syncs;
    set.fs
        .add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS).at("/a"));

    let error = set.sync().unwrap_err();
    assert_eq!(SimError::injected(&error), Some(Fault::Eio));
    assert_eq!(set.fs.stats().syncs - syncs, 2);
    assert_eq!(set.fs.stats().faults, 1);
    assert_eq!(set.durable(), after_syncs()[1]);
    let image = set.fs.crash_image();
    let unknown: Vec<&str> = image
        .changes()
        .iter()
        .filter(|change| change.unknown)
        .map(|change| change.path.as_str())
        .collect();
    assert_eq!(unknown, ["/a"]);

    for _ in 0..2 {
        assert_eq!(kind(set.sync()), ErrorKind::Other);
        assert_eq!(kind(set.a.sync_data()), ErrorKind::Other);
        assert_eq!(kind(set.a.sync_all()), ErrorKind::Other);
    }
    assert_eq!(set.fs.stats().faults, 1);

    let without_a = [
        SyncItem::Dir(p("/")),
        SyncItem::Dir(p("/dir")),
        SyncItem::All(&*set.b),
    ];
    set.fs.sync_together(&without_a).unwrap();
    assert_eq!(
        set.durable(),
        tree_of(&[("a", Some("")), ("dir", None), ("dir/b", Some("b"))])
    );
    check(&set.fs);
}

#[test]
fn a_fault_on_a_directory_of_a_set_can_be_made_good_by_the_next_set() {
    let set = Unsynced::new(55);
    set.fs
        .add_rule(FaultRule::new(Fault::Enospc).on(OpKind::SyncDir).skip(1));

    let error = set.sync().unwrap_err();
    assert_eq!(SimError::injected(&error), Some(Fault::Enospc));
    assert_eq!(error.kind(), ErrorKind::StorageFull);
    assert_eq!(set.durable(), after_syncs()[2]);

    set.sync().unwrap();
    assert_eq!(set.durable(), after_syncs()[4]);
    check(&set.fs);
}

#[test]
fn every_sync_of_a_set_can_fail() {
    for failing in 0..4 {
        let set = Unsynced::new(56);
        let next = set.fs.stats().ops + 1;
        set.fs
            .add_rule(FaultRule::new(Fault::Eio).from_op(next + failing));

        let error = set.sync().unwrap_err();
        assert_eq!(SimError::injected(&error), Some(Fault::Eio));
        assert_eq!(set.fs.stats().ops, next + failing);
        assert_eq!(set.durable(), after_syncs()[failing as usize]);
        assert_eq!(set.a.sync_data().is_err(), failing == 1);
        assert_eq!(set.b.sync_data().is_err(), failing == 3);
        check(&set.fs);
    }
}
