//! What each error says.

use std::error::Error;

use super::{super::Malformed, *};

#[test]
fn every_error_says_what_it_is() {
    let prefix: Prefix = {
        let mut prefix = [0; 32];
        prefix[..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        prefix
    };
    let flat = DirectoryError::from(FlatError::ValueTooLong { len: 9 });
    let source = flat.source().expect("a source").to_string();
    assert_eq!(flat.to_string(), format!("subtree directory: {source}"));

    let own = [
        (
            DirectoryError::Damaged {
                key: vec![2, 0xAB],
                what: "an entry that ends early",
            },
            "subtree directory record 02ab: an entry that ends early",
        ),
        (
            DirectoryError::Exists { prefix },
            "a Merk is entered under the prefix deadbeef..",
        ),
        (
            DirectoryError::NotFound { prefix },
            "no Merk is entered under the prefix deadbeef..",
        ),
        (
            DirectoryError::NoParent { parent: prefix },
            "the host's Merk, under the prefix deadbeef.., is not entered",
        ),
        (
            DirectoryError::HostTaken {
                parent: prefix,
                position: 2,
            },
            "the host in the Merk under the prefix deadbeef.. has a Merk at position 2",
        ),
        (
            DirectoryError::Inside { prefix },
            "the Merk under the prefix deadbeef.. would move below itself",
        ),
        (
            DirectoryError::NotEmpty { prefix },
            "the Merk under the prefix deadbeef.. has no root and Merks below it",
        ),
        (
            DirectoryError::UnknownRoot {
                address: cell(4, 2),
            },
            "no Merk has its root at 4/2",
        ),
        (
            DirectoryError::RootTaken {
                address: cell(4, 2),
            },
            "two Merks would have their root at 4/2",
        ),
        (
            DirectoryError::Twice { prefix },
            "the Merk under the prefix deadbeef.. is named twice in one change",
        ),
        (
            DirectoryError::TooLong {
                what: "a tree type",
                len: 300,
            },
            "a tree type of 300 bytes is longer than 255 bytes",
        ),
        (
            DirectoryError::TooDeep,
            "a Merk is more than 65535 Merks deep",
        ),
        (DirectoryError::IdsExhausted, "every Merk id was given out"),
        (
            DirectoryError::InUse { prefix },
            "the Merk under the prefix deadbeef.. is in use, and no left-over",
        ),
        (
            DirectoryError::Nested { id: id(12) },
            "records of Merks below the dropped Merk 12 are still there",
        ),
        (
            DirectoryError::NotDropped { id: id(12) },
            "no dropped Merk has the id 12",
        ),
    ];
    for (error, says) in own {
        assert_eq!(error.to_string(), says);
        assert!(error.source().is_none());
        assert!(!format!("{error:?}").is_empty());
    }
    let malformed = Malformed("not a prefix");
    assert_eq!(malformed.to_string(), "not a prefix");
    assert!(malformed.source().is_none());
}

#[test]
fn a_merk_that_cannot_be_entered_is_refused_and_writes_nothing() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    grove.add(&["a", "b"]);
    grove.unlink(&["a"]);
    let pages = grove.pages.pages.clone();
    let top = prefix_of(&[]);
    let long = [1; 256];
    let host = |parent, key, position| {
        Some(Host {
            parent,
            key,
            position,
        })
    };
    let nothing = [4; 32];
    let left = prefix_of(&path(&["a"]));
    let below = prefix_of(&path(&["a", "b"]));
    let new = [5; 32];
    let cases: [(&Prefix, &[u8], Option<Host<'_>>, &str); 7] = [
        (
            &top,
            b"t",
            None,
            "a Merk is entered under the prefix 00000000..",
        ),
        (
            &new,
            b"t",
            host(&nothing, b"k", 0),
            "the host's Merk, under the prefix 04040404..",
        ),
        // The Merk the host is in was dropped, or is below a dropped one.
        (
            &new,
            b"t",
            host(&left, b"k", 0),
            "the host's Merk, under the prefix",
        ),
        (
            &new,
            b"t",
            host(&below, b"k", 0),
            "the host's Merk, under the prefix",
        ),
        (&new, &long, host(&top, b"k", 0), "a tree type of 256 bytes"),
        (
            &new,
            b"t",
            host(&top, &long, 0),
            "a host's key of 256 bytes",
        ),
        (&new, &long, None, "a tree type of 256 bytes"),
    ];
    for (prefix, tree_type, host, says) in cases {
        let added = grove
            .directory
            .add(&mut grove.pages, prefix, tree_type, host);
        let error = added.expect_err("refused");
        assert!(error.to_string().contains(says), "{error}");
        assert_eq!(grove.pages.pages, pages, "a refused call writes nothing");
    }

    // A host has one Merk at a position.
    grove.add(&["c"]);
    let pages = grove.pages.pages.clone();
    let added = grove
        .directory
        .add(&mut grove.pages, &new, b"t", host(&top, b"c", 0));
    let error = added.expect_err("the host is taken");
    assert!(
        matches!(error, DirectoryError::HostTaken { parent, position: 0 } if parent == top),
        "{error}"
    );
    let c = prefix_of(&path(&["c"]));
    let error = grove
        .directory
        .set_tree_type(&mut grove.pages, &c, &long)
        .expect_err("too long");
    assert!(
        matches!(error, DirectoryError::TooLong { len: 256, .. }),
        "{error}"
    );
    assert_eq!(grove.pages.pages, pages);
    grove.check();
}
