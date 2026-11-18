//! The acceptance test for the list of public GroveDB operations (BASE-01):
//! [`PUBLIC_OPERATIONS`] names every public operation of GroveDB as produced
//! by the workloads or as left out, with the reason.
//!
//! Nothing here is kept in step by hand. The public operations are read from
//! GroveDB's source when the test runs: every `pub fn` of an inherent
//! `impl GroveDb` block, every method of a trait implemented for `GroveDb`,
//! and every public free function that takes a `GroveDb` or one of its
//! transactions, in the modules the crate is built from outside its tests.
//! A method added to GroveDB therefore fails
//! `every_public_operation_is_classified` until it is classified.
//!
//! What a classification claims is checked as well, against the same
//! source:
//!
//! - A method listed as produced is one `workload::apply` calls on the
//!   database or on a transaction, read from the source of `apply.rs`, and
//!   the operations it is listed under are ones the suite must produce
//!   (`the_suite_produces_every_operation` in `tests/workload.rs`).
//! - A method listed as called by another is reached from it: the calls of
//!   every function of GroveDB and of its storage crate are read, and the
//!   method has to be among those the caller leads to.
//! - A method listed as not produced is one `workload::apply` does not
//!   call.
//!
//! Calls are followed by name, so two functions of one name count as one.
//! That can make a method look reached when it is not, and never the other
//! way round.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use lignin_testkit::workload::{
    surface::{Status, PUBLIC_OPERATIONS},
    Operation,
};
use syn::{
    punctuated::Punctuated,
    visit::{self, Visit},
    Attribute, Expr, FnArg, ImplItem, Item, ItemMod, Meta, Token, Type, Visibility,
};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate is a member of the workspace")
        .to_path_buf()
}

fn parse(file: &Path) -> syn::File {
    let source = fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("reading {}: {error}", file.display()));
    syn::parse_file(&source).unwrap_or_else(|error| panic!("parsing {}: {error}", file.display()))
}

/// Whether a `cfg` predicate holds in test builds only: `test`, or an
/// `all(…)` that holds one such predicate, or an `any(…)` of nothing else.
fn holds_in_tests_only(predicate: &Meta) -> bool {
    let nested = |list: &syn::MetaList| {
        list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .map(|predicates| predicates.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    match predicate {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => {
            nested(list).iter().any(holds_in_tests_only)
        }
        Meta::List(list) if list.path.is_ident("any") => {
            let predicates = nested(list);
            !predicates.is_empty() && predicates.iter().all(holds_in_tests_only)
        }
        _ => false,
    }
}

/// Whether an item is only built for tests.
fn test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Meta>()
                .is_ok_and(|predicate| holds_in_tests_only(&predicate))
    })
}

/// Whether a type names `name`, at any depth: `&GroveDb`, `Option<&Transaction>`.
fn names(ty: &Type, name: &str) -> bool {
    struct Finder<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Finder<'_> {
        fn visit_ident(&mut self, ident: &'ast syn::Ident) {
            self.found |= ident == self.name;
        }
    }
    let mut finder = Finder { name, found: false };
    finder.visit_type(ty);
    finder.found
}

/// The name a call is made on: `db` for `db.get(…)` and for
/// `self.db.get(…)`.
fn receiver(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Path(path) => path.path.get_ident().map(ToString::to_string),
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => Some(name.to_string()),
            syn::Member::Unnamed(_) => None,
        },
        Expr::Reference(reference) => receiver(&reference.expr),
        Expr::Paren(paren) => receiver(&paren.expr),
        _ => None,
    }
}

/// The calls of a piece of code: each method called, with the name it is
/// called on where that is a plain name, and each function called by path.
#[derive(Default)]
struct Calls {
    methods: BTreeSet<(Option<String>, String)>,
    functions: BTreeSet<String>,
}

impl Calls {
    /// The names of everything called.
    fn names(&self) -> BTreeSet<String> {
        let methods = self.methods.iter().map(|(_, method)| method.clone());
        methods.chain(self.functions.iter().cloned()).collect()
    }
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.methods
            .insert((receiver(&call.receiver), call.method.to_string()));
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*call.func
            && let Some(function) = path.path.segments.last()
        {
            self.functions.insert(function.ident.to_string());
        }
        visit::visit_expr_call(self, call);
    }

    /// The arguments of a macro such as `cost_return_on_error!` are
    /// expressions too.
    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let arguments = Punctuated::<Expr, Token![,]>::parse_terminated;
        if let Ok(arguments) = invocation.parse_body_with(arguments) {
            let mut inner = Calls::default();
            for argument in &arguments {
                inner.visit_expr(argument);
            }
            self.methods.extend(inner.methods);
            self.functions.extend(inner.functions);
        }
    }
}

/// What the source of one crate holds.
#[derive(Default)]
struct Source {
    /// The public methods of each type asked for: the `pub fn`s of its
    /// inherent `impl` blocks and the methods of the traits implemented
    /// for it, each with the files it is defined in.
    methods: BTreeMap<&'static str, BTreeMap<String, BTreeSet<String>>>,
    /// The public free functions that take a database or a transaction.
    functions: BTreeMap<String, BTreeSet<String>>,
    /// What every function and method calls, by name.
    calls: BTreeMap<String, BTreeSet<String>>,
    files: usize,
}

impl Source {
    /// Reads the crate in `directory`, for the public methods of `owners`.
    fn of(directory: &str, owners: &[&'static str]) -> Self {
        let root = workspace().join(directory).join("src");
        let mut source = Source::default();
        for owner in owners {
            source.methods.insert(owner, BTreeMap::new());
        }
        source.file(&root.join("lib.rs"), &root, &root);
        source
    }

    /// Reads one file of the module tree. `children` is where the files of
    /// its child modules are.
    fn file(&mut self, file: &Path, children: &Path, root: &Path) {
        self.files += 1;
        let name = file
            .strip_prefix(root)
            .expect("a module is under the source directory")
            .display()
            .to_string();
        self.items(&parse(file).items, children, root, &name);
    }

    fn called(&mut self, function: &syn::Ident, block: &syn::Block) {
        let mut calls = Calls::default();
        calls.visit_block(block);
        self.calls
            .entry(function.to_string())
            .or_default()
            .extend(calls.names());
    }

    fn items(&mut self, items: &[Item], children: &Path, root: &Path, file: &str) {
        for item in items {
            match item {
                Item::Mod(module) if !test_only(&module.attrs) => {
                    self.module(module, children, root, file)
                }
                Item::Impl(block) if !test_only(&block.attrs) => {
                    let owner = self
                        .methods
                        .keys()
                        .copied()
                        .find(|owner| names(&block.self_ty, owner));
                    for item in &block.items {
                        let ImplItem::Fn(method) = item else { continue };
                        if test_only(&method.attrs) {
                            continue;
                        }
                        self.called(&method.sig.ident, &method.block);
                        // A method of a trait is as public as the trait.
                        let public =
                            block.trait_.is_some() || matches!(method.vis, Visibility::Public(_));
                        if let Some(owner) = owner
                            && public
                        {
                            self.methods
                                .get_mut(owner)
                                .expect("an owner asked for")
                                .entry(method.sig.ident.to_string())
                                .or_default()
                                .insert(file.to_string());
                        }
                    }
                }
                Item::Fn(function) if !test_only(&function.attrs) => {
                    self.called(&function.sig.ident, &function.block);
                    let takes_a_database = function.sig.inputs.iter().any(|input| match input {
                        FnArg::Typed(input) => ["GroveDb", "Transaction", "TransactionArg"]
                            .iter()
                            .any(|name| names(&input.ty, name)),
                        FnArg::Receiver(_) => false,
                    });
                    if matches!(function.vis, Visibility::Public(_)) && takes_a_database {
                        self.functions
                            .entry(function.sig.ident.to_string())
                            .or_default()
                            .insert(file.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    fn module(&mut self, module: &ItemMod, children: &Path, root: &Path, file: &str) {
        let name = module.ident.to_string();
        let directory = children.join(&name);
        if let Some((_, items)) = &module.content {
            return self.items(items, &directory, root, file);
        }
        assert!(
            !module
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("path")),
            "{file}: module {name} is at a path of its own, which this test does not follow"
        );
        let flat = children.join(format!("{name}.rs"));
        let nested = directory.join("mod.rs");
        let found = if flat.is_file() { flat } else { nested };
        self.file(&found, &directory, root);
    }
}

/// GroveDB and its storage crate, read once for all the tests.
struct Sources {
    grovedb: Source,
    storage: Source,
}

impl Sources {
    fn read() -> Self {
        // `grovedb::Transaction` is the storage crate's `Tx`.
        let sources = Sources {
            grovedb: Source::of("grovedb", &["GroveDb"]),
            storage: Source::of("storage", &["Tx"]),
        };
        assert!(
            sources.grovedb.files > 100 && sources.grovedb.methods["GroveDb"].len() > 150,
            "GroveDB's source was not read: {} files",
            sources.grovedb.files
        );
        assert!(
            sources.storage.methods["Tx"].contains_key("set_savepoint"),
            "the transaction's source was not read: {:?}",
            sources.storage.methods
        );
        sources
    }

    /// Every public operation, under the name [`PUBLIC_OPERATIONS`] gives
    /// it, with the files it is defined in.
    fn operations(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut operations = self.grovedb.methods["GroveDb"].clone();
        for (name, files) in &self.storage.methods["Tx"] {
            operations.insert(format!("{TRANSACTION}{name}"), files.clone());
        }
        operations
    }

    /// Everything the function `from` leads to, itself included, following
    /// calls by name through both crates.
    fn reached_from(&self, from: &str) -> BTreeSet<String> {
        let mut reached = BTreeSet::from([from.to_string()]);
        let mut pending = vec![from.to_string()];
        while let Some(function) = pending.pop() {
            for source in [&self.grovedb, &self.storage] {
                for called in source.calls.get(&function).into_iter().flatten() {
                    if reached.insert(called.clone()) {
                        pending.push(called.clone());
                    }
                }
            }
        }
        reached
    }
}

/// The calls `workload::apply` makes.
fn calls_of_apply() -> Calls {
    let mut calls = Calls::default();
    calls.visit_file(&parse(
        &workspace().join("lignin-testkit/src/workload/apply.rs"),
    ));
    calls
}

/// The prefix [`PUBLIC_OPERATIONS`] names the methods of a transaction
/// with.
const TRANSACTION: &str = "Transaction::";

/// The names `workload::apply` calls a database by, and a transaction.
const DATABASES: [&str; 1] = ["db"];
const TRANSACTIONS: [&str; 1] = ["transaction"];

/// Whether `workload::apply` calls the operation `name` on a database, or
/// for a method of a transaction on a transaction.
fn applied(calls: &Calls, name: &str) -> bool {
    let (receivers, method) = match name.strip_prefix(TRANSACTION) {
        Some(method) => (&TRANSACTIONS, method),
        None => (&DATABASES, name),
    };
    receivers.iter().any(|receiver| {
        calls
            .methods
            .contains(&(Some(receiver.to_string()), method.to_string()))
    })
}

#[test]
fn every_public_operation_is_classified() {
    let sources = Sources::read();
    assert_eq!(
        sources.grovedb.functions.keys().collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "GroveDB has public free functions that take a database or a transaction; classify \
         them in `workload::surface` and compare them here"
    );
    let operations = sources.operations();

    let listed: Vec<&str> = PUBLIC_OPERATIONS.iter().map(|(name, _)| *name).collect();
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(listed, sorted, "the list is sorted and names a method once");

    let found: BTreeSet<&str> = operations.keys().map(String::as_str).collect();
    let listed: BTreeSet<&str> = listed.into_iter().collect();
    let unclassified: Vec<String> = found
        .difference(&listed)
        .map(|name| format!("{name} ({:?})", operations[*name]))
        .collect();
    assert!(
        unclassified.is_empty(),
        "public GroveDB operations that `workload::surface::PUBLIC_OPERATIONS` does not \
         classify:\n{}",
        unclassified.join("\n")
    );
    let gone: Vec<&&str> = listed.difference(&found).collect();
    assert!(
        gone.is_empty(),
        "`PUBLIC_OPERATIONS` classifies methods GroveDB does not have: {gone:?}"
    );
}

#[test]
fn a_classification_is_what_apply_does() {
    let sources = Sources::read();
    let calls = calls_of_apply();
    let statuses: BTreeMap<&str, &Status> = PUBLIC_OPERATIONS
        .iter()
        .map(|(name, status)| (*name, status))
        .collect();
    let mut produced_operations = BTreeSet::new();
    for (name, status) in PUBLIC_OPERATIONS {
        let method = name.strip_prefix(TRANSACTION).unwrap_or(name);
        let is_applied = applied(&calls, name);
        match status {
            Status::Produced(operations) => {
                assert!(
                    is_applied,
                    "{name} is listed as produced, but `workload::apply` does not call it"
                );
                assert!(!operations.is_empty(), "{name} is produced by no operation");
                produced_operations.extend(operations.iter().copied());
            }
            Status::Inside { caller, .. } => {
                assert!(
                    matches!(
                        statuses.get(caller),
                        Some(Status::Produced(_) | Status::Inside { .. } | Status::Consumer(_))
                    ),
                    "{name} is listed as called by {caller}, which is not produced"
                );
                let caller = caller.strip_prefix(TRANSACTION).unwrap_or(caller);
                assert!(
                    caller != method && sources.reached_from(caller).contains(method),
                    "{name} is listed as called by {caller}, which does not lead to it"
                );
                assert!(
                    !is_applied,
                    "{name} is listed as called by {caller}, but `workload::apply` calls it \
                     itself"
                );
            }
            Status::Consumer(reason) | Status::LeftOut(reason) => {
                assert!(reason.len() > 20, "{name} has no reason: {reason:?}");
                assert!(
                    !is_applied,
                    "{name} is listed as not produced, but `workload::apply` calls it"
                );
            }
        }
    }

    let untraced: Vec<&Operation> = Operation::ALL
        .iter()
        .filter(|operation| !produced_operations.contains(operation))
        .collect();
    assert_eq!(
        untraced,
        Vec::<&Operation>::new(),
        "operations no public method is listed for"
    );
}

/// The checks above see what they are meant to see: a call on something
/// that is no database is no call of an operation, a call that is not made
/// is not reached, and a predicate that holds outside tests hides nothing.
#[test]
fn the_checks_tell_calls_apart() {
    let calls = calls_of_apply();
    assert!(applied(&calls, "get_raw"));
    assert!(applied(&calls, "Transaction::set_savepoint"));
    // `apply.rs` calls `get` on no database and `commit` on no transaction:
    // the first is `GroveDb::get`, the second `GroveDb::commit_transaction`.
    assert!(applied(&calls, "get"));
    assert!(!applied(&calls, "Transaction::commit"));
    assert!(!applied(&calls, "wipe"));
    let mut stray = Calls::default();
    let call: Expr = syn::parse_str("statuses.get(caller)").expect("an expression");
    stray.visit_expr(&call);
    assert!(stray.names().contains("get"));
    assert!(!applied(&stray, "get"));

    let sources = Sources::read();
    let reached = sources.reached_from("delete_up_tree_while_empty");
    assert!(reached.contains("add_delete_operations_for_delete_up_tree_while_empty"));
    assert!(!sources.reached_from("has_raw").contains("wipe"));
    assert!(sources
        .reached_from("commit_transaction")
        .contains("commit"));

    let predicate = |text: &str| -> Meta { syn::parse_str(text).expect("a predicate") };
    for test_only in [
        "test",
        "all(test, feature = \"minimal\")",
        "all(feature = \"minimal\", all(test))",
        "any(test)",
    ] {
        assert!(holds_in_tests_only(&predicate(test_only)), "{test_only}");
    }
    for built in [
        "feature = \"minimal\"",
        "any(test, feature = \"minimal\")",
        "not(test)",
        "all(feature = \"minimal\")",
        "any()",
    ] {
        assert!(!holds_in_tests_only(&predicate(built)), "{built}");
    }
}

#[test]
fn the_list_counts() {
    let count = |matches: fn(&Status) -> bool| {
        PUBLIC_OPERATIONS
            .iter()
            .filter(|(_, status)| matches(status))
            .count()
    };
    let produced = count(|status| matches!(status, Status::Produced(_)));
    let inside = count(|status| matches!(status, Status::Inside { .. }));
    let consumer = count(|status| matches!(status, Status::Consumer(_)));
    let left_out = count(|status| matches!(status, Status::LeftOut(_)));
    println!(
        "{} public operations: {produced} produced, {inside} called by a produced one, \
         {consumer} called by the consumer of a stream, {left_out} left out",
        PUBLIC_OPERATIONS.len()
    );
    assert_eq!(
        produced + inside + consumer + left_out,
        PUBLIC_OPERATIONS.len()
    );
    for (name, status) in PUBLIC_OPERATIONS {
        println!("{name}: {status}");
    }
}
