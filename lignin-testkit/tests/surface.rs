//! The acceptance test for the list of public GroveDB operations (BASE-01):
//! [`PUBLIC_OPERATIONS`] names every public operation of GroveDB as produced
//! by the workloads or as left out, with the reason.
//!
//! Nothing here is kept in step by hand. The public operations are read from
//! GroveDB's source when the test runs: every `pub fn` of an inherent
//! `impl GroveDb` block, and every public free function that takes a
//! `GroveDb` or one of its transactions, in the modules the crate is built
//! from without `cfg(test)`. A method added to GroveDB therefore fails
//! `every_public_operation_is_classified` until it is classified. What a
//! classification claims is checked as well: a method listed as produced is
//! one `workload::apply` calls, read from the source of `apply.rs`, and the
//! operation it is listed under is one the suite must produce
//! (`the_suite_produces_every_operation` in `tests/workload.rs`).

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
    Attribute, FnArg, ImplItem, Item, ItemMod, Token, Type, Visibility,
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

/// Whether an item is only built for tests.
fn test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Ident>()
                .is_ok_and(|condition| condition == "test")
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

/// The public operations of one type, found in the source of its crate.
struct Surface {
    /// The type whose inherent `impl` blocks are read.
    owner: &'static str,
    /// The `pub fn`s of those blocks, each with the files it is defined
    /// in.
    methods: BTreeMap<String, BTreeSet<String>>,
    /// The public free functions that take a database or a transaction.
    functions: BTreeMap<String, BTreeSet<String>>,
    files: usize,
}

impl Surface {
    /// The surface of the type `owner` of the crate in `directory`.
    fn of(directory: &str, owner: &'static str) -> Self {
        let source = workspace().join(directory).join("src");
        let mut surface = Surface {
            owner,
            methods: BTreeMap::new(),
            functions: BTreeMap::new(),
            files: 0,
        };
        surface.file(&source.join("lib.rs"), &source, &source);
        surface
    }

    /// Reads one file of the module tree. `children` is where the files of
    /// its child modules are.
    fn file(&mut self, file: &Path, children: &Path, source: &Path) {
        self.files += 1;
        let name = file
            .strip_prefix(source)
            .expect("a module is under the source directory")
            .display()
            .to_string();
        self.items(&parse(file).items, children, source, &name);
    }

    fn items(&mut self, items: &[Item], children: &Path, source: &Path, file: &str) {
        for item in items {
            match item {
                Item::Mod(module) if !test_only(&module.attrs) => {
                    self.module(module, children, source, file)
                }
                Item::Impl(block)
                    if block.trait_.is_none()
                        && !test_only(&block.attrs)
                        && names(&block.self_ty, self.owner) =>
                {
                    for item in &block.items {
                        if let ImplItem::Fn(method) = item
                            && matches!(method.vis, Visibility::Public(_))
                            && !test_only(&method.attrs)
                        {
                            self.methods
                                .entry(method.sig.ident.to_string())
                                .or_default()
                                .insert(file.to_string());
                        }
                    }
                }
                Item::Fn(function)
                    if matches!(function.vis, Visibility::Public(_))
                        && !test_only(&function.attrs)
                        && function.sig.inputs.iter().any(|input| match input {
                            FnArg::Typed(input) => ["GroveDb", "Transaction", "TransactionArg"]
                                .iter()
                                .any(|name| names(&input.ty, name)),
                            FnArg::Receiver(_) => false,
                        }) =>
                {
                    self.functions
                        .entry(function.sig.ident.to_string())
                        .or_default()
                        .insert(file.to_string());
                }
                _ => {}
            }
        }
    }

    fn module(&mut self, module: &ItemMod, children: &Path, source: &Path, file: &str) {
        let name = module.ident.to_string();
        let directory = children.join(&name);
        if let Some((_, items)) = &module.content {
            return self.items(items, &directory, source, file);
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
        self.file(&found, &directory, source);
    }
}

/// The names of the methods `workload::apply` calls on anything.
fn called_by_apply() -> BTreeSet<String> {
    struct Calls(BTreeSet<String>);
    impl<'ast> Visit<'ast> for Calls {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            self.0.insert(call.method.to_string());
            visit::visit_expr_method_call(self, call);
        }

        /// The arguments of a macro such as `cost_return_on_error!` are
        /// expressions too.
        fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
            let arguments = Punctuated::<syn::Expr, Token![,]>::parse_terminated;
            if let Ok(arguments) = invocation.parse_body_with(arguments) {
                let mut inner = Calls(BTreeSet::new());
                for argument in &arguments {
                    inner.visit_expr(argument);
                }
                self.0.extend(inner.0);
            }
        }
    }
    let mut calls = Calls(BTreeSet::new());
    calls.visit_file(&parse(
        &workspace().join("lignin-testkit/src/workload/apply.rs"),
    ));
    calls.0
}

/// The prefix [`PUBLIC_OPERATIONS`] names the methods of a transaction
/// with.
const TRANSACTION: &str = "Transaction::";

#[test]
fn every_public_operation_is_classified() {
    let grovedb = Surface::of("grovedb", "GroveDb");
    assert!(
        grovedb.files > 100 && grovedb.methods.len() > 150,
        "GroveDB's source was not read: {} files, {} methods",
        grovedb.files,
        grovedb.methods.len()
    );
    assert_eq!(
        grovedb.functions.keys().collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "GroveDB has public free functions that take a database or a transaction; classify \
         them in `workload::surface` and compare them here"
    );
    // `grovedb::Transaction` is the storage crate's `Tx`.
    let transaction = Surface::of("storage", "Tx");
    assert!(
        transaction.methods.contains_key("set_savepoint"),
        "the transaction's source was not read: {:?}",
        transaction.methods
    );
    let mut methods = grovedb.methods;
    for (name, files) in transaction.methods {
        methods.insert(format!("{TRANSACTION}{name}"), files);
    }

    let listed: Vec<&str> = PUBLIC_OPERATIONS.iter().map(|(name, _)| *name).collect();
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(listed, sorted, "the list is sorted and names a method once");

    let found: BTreeSet<&str> = methods.keys().map(String::as_str).collect();
    let listed: BTreeSet<&str> = listed.into_iter().collect();
    let unclassified: Vec<String> = found
        .difference(&listed)
        .map(|name| format!("{name} ({:?})", methods[*name]))
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
    let called = called_by_apply();
    let statuses: BTreeMap<&str, &Status> = PUBLIC_OPERATIONS
        .iter()
        .map(|(name, status)| (*name, status))
        .collect();
    let mut produced_operations = BTreeSet::new();
    for (name, status) in PUBLIC_OPERATIONS {
        let method = name.strip_prefix(TRANSACTION).unwrap_or(name);
        let called = |_: ()| called.contains(method);
        match status {
            Status::Produced(operations) => {
                assert!(
                    called(()),
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
                assert!(
                    !called(()),
                    "{name} is listed as called by {caller}, but `workload::apply` calls it \
                     itself"
                );
            }
            Status::Consumer(reason) | Status::LeftOut(reason) => {
                assert!(reason.len() > 20, "{name} has no reason: {reason:?}");
                assert!(
                    !called(()),
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
