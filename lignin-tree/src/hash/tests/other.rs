//! A value hash that the rules name as made with another hash of a hosted
//! root ([`made_with_other`]).

use super::{
    acceptance::{key, nested},
    grove::Grove,
    *,
};

/// The rules of the tests, which also name another hash for the root of
/// every Merk whose key begins with one of `of`, and count what they were
/// asked.
#[derive(Debug, Default)]
struct Naming {
    of: Vec<Vec<u8>>,
    asked_roots: usize,
    asked_values: usize,
}

fn other_of(root: NodeRef<'_>) -> Hash {
    digest(&[b"another hash", root.kv_hash()])
}

impl HashRules for Naming {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        Ok(value_rule(value, hosted))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        Ok(kv_rule(key, value_hash))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        Ok(node_rule_of(node))
    }

    fn other_node_hash(&mut self, node: NodeRef<'_>) -> Result<Option<Hash>, Refusal> {
        self.asked_roots += 1;
        let named = self.of.iter().any(|prefix| node.key().starts_with(prefix));
        Ok(named.then(|| other_of(node)))
    }

    fn value_hash_with_other(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Option<Hash>, Refusal> {
        self.asked_values += 1;
        Ok(Some(value_rule(value, hosted)))
    }
}

fn set_value_hash(grove: &mut Grove, at: CellAddress, hash: Hash) {
    grove
        .mem
        .write(at.page(), |page| {
            let mut node = page.node_mut(at.slot()).expect("the node");
            node.set_hash(HashField::Value, &hash)
        })
        .expect("the hash is set");
}

fn named(grove: &mut Grove, rules: &mut impl HashRules, at: CellAddress) -> bool {
    made_with_other(&mut grove.mem, &mut grove.log, rules, at).expect("the cell is read")
}

#[test]
fn a_value_hash_made_with_another_hash_of_a_hosted_root_is_told_from_any_other() {
    let (mut grove, a, b) = nested();
    let host = grove.at(a, &key(a, 123));
    let host_value = grove.node(a, &key(a, 123)).value.clone();
    let root = grove.root_of(b);
    let other = other_of(grove.mem.node(root));
    let with_other = value_rule(&host_value, &[Some(other)]);
    let of_b = || Naming {
        of: vec![format!("merk {b} ").into_bytes()],
        ..Naming::default()
    };

    // The hash the rules make is not the one made with the other hash.
    assert_eq!(grove.differing(host), Stale::NONE);
    assert!(!named(&mut grove, &mut of_b(), host));

    // The hash made with the other hash of the root differs from its
    // recomputation, and with it the key-value hash, which was made from
    // the value hash before. Rules that name no other hash do not name
    // it, and are asked for no value.
    set_value_hash(&mut grove, host, with_other);
    let differing = grove.differing(host);
    assert!(differing.has(HashField::Value), "{differing:?}");
    let reads = grove.log.reads;
    assert!(!named(&mut grove, &mut Plain, host));
    assert_eq!(grove.log.reads, reads);
    let mut rules = of_b();
    assert!(named(&mut grove, &mut rules, host));
    assert_eq!((rules.asked_roots, rules.asked_values), (1, 1));

    // Rules that name another hash for the roots of other Merks only are
    // asked for the root and make no value hash.
    let mut rules = Naming {
        of: vec![format!("merk {a} ").into_bytes()],
        ..Naming::default()
    };
    assert!(!named(&mut grove, &mut rules, host));
    assert_eq!((rules.asked_roots, rules.asked_values), (1, 0));

    // Any other hash is not the one, also with the rules that name one.
    let mut wrong = with_other;
    wrong[7] ^= 1;
    set_value_hash(&mut grove, host, wrong);
    assert!(!named(&mut grove, &mut of_b(), host));
    // Nor is the hash with the other hash of a root the Merk had before.
    let stale = value_rule(&host_value, &[Some(digest(&[b"another hash", &[9; 32]]))]);
    set_value_hash(&mut grove, host, stale);
    assert!(!named(&mut grove, &mut of_b(), host));

    // A node that hosts no Merk has no such hash, whatever it stores: no
    // root is asked for and no value hash is made.
    let item = grove.at(b, &key(b, 42));
    set_value_hash(&mut grove, item, with_other);
    let mut rules = of_b();
    assert!(!named(&mut grove, &mut rules, item));
    assert_eq!((rules.asked_roots, rules.asked_values), (0, 0));

    // Nothing was written by the asking, and no hash was marked.
    assert!(grove.hashes.is_empty());
    assert_eq!(*grove.mem.node(host).value_hash(), stale);
}

#[test]
fn a_host_whose_merk_has_no_root_has_no_value_hash_made_with_another() {
    let mut grove = Grove::new(false);
    for number in 0..5 {
        grove.put(0, &key(0, number), &[number as u8; 30]);
    }
    let hosted = grove.host(0, &key(0, 3));
    let host = grove.at(0, &key(0, 3));
    let mut rules = Naming {
        of: vec![format!("merk {hosted} ").into_bytes()],
        ..Naming::default()
    };
    set_value_hash(&mut grove, host, [5; 32]);
    assert!(!named(&mut grove, &mut rules, host));
    assert_eq!((rules.asked_roots, rules.asked_values), (0, 0));

    // A slot that holds no cell is an error, as for a check.
    let nowhere = CellAddress::new(host.page(), slot(999));
    let error = made_with_other(&mut grove.mem, &mut grove.log, &mut rules, nowhere);
    assert!(matches!(error, Err(HashError::NoNode { address }) if address == nowhere));
}
