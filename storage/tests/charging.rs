//! The storage crate works out no charge of its own: every `OperationCost`
//! the RocksDB engine reports is the result of a function of
//! `grovedb_costs::cost_model`.
//!
//! This is the check of the source. It parses every source file of the
//! crate and walks the code that is built outside tests. In a function
//! body, in the value of a constant and in a type alias, free or
//! associated, it refuses:
//!
//! - the name of a type a cost is built from, under any path, and the
//!   names of the functions that build one;
//! - `Default::default`, which builds one without naming it;
//! - a field of a cost, read or written;
//! - the functions that add costs up or measure a record, called as
//!   methods or by path;
//! - the macros that add a cost to a running one;
//! - `+` and `+=`: anywhere in the engine (`src/rocksdb_storage`), and in
//!   the rest of the crate in a function whose signature names a type that
//!   carries a cost.
//!
//! The tokens of a macro are not parsed, so the same names are looked for
//! in them as words. An import may not rename an item of `grovedb_costs`
//! or take it by glob, and may take only the names listed in [`IMPORTS`],
//! so a cost type cannot be reached under another name. Naming a cost type
//! in a signature and passing a cost on (`wrap_with_cost`) are not charges.
//!
//! Two functions of the engine-neutral part add up or drop costs without
//! working any out, and are left out by name ([`EXEMPT`]).
//!
//! A check of the source holds the code to the names it lists and cannot
//! list every way of writing a charge. `model_ledger.rs` is the check that
//! does not depend on how the code is written: it compares what the engine
//! reports with what the model handed out. What this one adds is that it
//! also refuses an addition of two costs the model handed out, which
//! balances in the ledger, and that it names the line.

use std::{
    fs,
    path::{Path, PathBuf},
};

use syn::{
    visit::{self, Visit},
    BinOp, Member, Meta, UseTree,
};

const COST_TYPES: [&str; 8] = [
    "OperationCost",
    "StorageCost",
    "KeyValueStorageCost",
    "StorageRemovedBytes",
    "NoStorageRemoval",
    "BasicStorageRemoval",
    "SectionedStorageRemoval",
    "TreeCostType",
];

const COST_FIELDS: [&str; 11] = [
    "cost",
    "seek_count",
    "storage_cost",
    "storage_loaded_bytes",
    "hash_node_calls",
    "sinsemilla_hash_calls",
    "added_bytes",
    "replaced_bytes",
    "removed_bytes",
    "key_storage_cost",
    "value_storage_cost",
];

/// The functions that add costs up or measure a record. `flat_map` and
/// `flat_map_ok` are the combinators of a `CostContext` that add the cost
/// of what they chain.
const COST_METHODS: [&str; 11] = [
    "add_key_value_storage_costs",
    "combined_removed_bytes",
    "required_space",
    "add_cost",
    "unwrap_add_cost",
    "flat_map",
    "flat_map_ok",
    "add",
    "add_assign",
    "saturating_add",
    "checked_add",
];

/// The functions that build a cost.
const COST_CONSTRUCTORS: [&str; 8] = [
    "with_seek_count",
    "with_storage_written_bytes",
    "with_storage_loaded_bytes",
    "with_storage_freed_bytes",
    "with_hash_node_calls",
    "for_updated_root_cost",
    "for_in_place_value_rewrite",
    "prepaid",
];

/// What the crate may import from `grovedb_costs`: the cost model, and the
/// types its signatures pass costs in.
const IMPORTS: [&str; 12] = [
    "cost_model",
    "BatchCharge",
    "ImmediateBatchCharge",
    "Keyspace",
    "StorageCostModel",
    "KeyValueStorageCost",
    "ChildrenSizes",
    "ChildrenSizesWithIsSumTree",
    "CostContext",
    "CostResult",
    "CostsExt",
    "OperationCost",
];

/// The types a cost travels in. A function of the engine-neutral part
/// whose signature names one may not add.
const CARRIERS: [&str; 4] = [
    "CostContext",
    "CostResult",
    "BatchCharge",
    "ImmediateBatchCharge",
];

/// The engine: every function in it is held to the rule on additions.
const ENGINE: &str = "rocksdb_storage/";

/// `FlatStore::clear` adds up what the calls on its handle returned, and
/// `stage_pending_prefix_drop` reports nothing for a record that cannot be
/// encoded. Neither has a cost model in scope: they name no engine.
const EXEMPT: [(&str, &str); 2] = [
    ("flat.rs", "clear"),
    ("flat.rs", "stage_pending_prefix_drop"),
];

/// The macro `FlatStore::clear` adds its costs up with.
const EXEMPT_IMPORTS: [(&str, &str); 1] = [("flat.rs", "cost_return_on_error")];

#[derive(Default)]
struct Names(Vec<String>);

impl<'ast> Visit<'ast> for Names {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.0.extend(
            path.segments
                .iter()
                .map(|segment| segment.ident.to_string()),
        );
        visit::visit_path(self, path);
    }
}

fn carries_a_cost(signature: &syn::Signature) -> bool {
    let mut names = Names::default();
    names.visit_signature(signature);
    names
        .0
        .iter()
        .any(|name| COST_TYPES.contains(&name.as_str()) || CARRIERS.contains(&name.as_str()))
}

#[derive(Default)]
struct Charges {
    file: String,
    stated: Vec<String>,
    bodies: usize,
    functions: Vec<(String, bool)>,
}

impl Charges {
    fn state(&mut self, what: String) {
        let place = self
            .functions
            .last()
            .map_or("the module", |(name, _)| name.as_str());
        self.stated.push(format!("{place}: {what}"));
    }

    fn body(&mut self, visit: impl FnOnce(&mut Self)) {
        self.bodies += 1;
        visit(self);
        self.bodies -= 1;
    }

    fn item(&mut self, name: &syn::Ident, carries_a_cost: bool, visit: impl FnOnce(&mut Self)) {
        let name = name.to_string();
        if EXEMPT.contains(&(self.file.as_str(), name.as_str())) {
            return;
        }
        self.functions.push((name, carries_a_cost));
        visit(self);
        self.functions.pop();
    }

    fn function(&mut self, signature: &syn::Signature, visit: impl FnOnce(&mut Self)) {
        self.item(&signature.ident, carries_a_cost(signature), visit);
    }

    fn may_not_add(&self) -> bool {
        self.file.starts_with(ENGINE) || self.functions.last().is_some_and(|(_, carries)| *carries)
    }

    fn import(&mut self, tree: &UseTree, path: &mut Vec<String>) {
        let from_costs = |path: &[String]| path.first().is_some_and(|root| root == "grovedb_costs");
        match tree {
            UseTree::Path(segment) => {
                path.push(segment.ident.to_string());
                self.import(&segment.tree, path);
                path.pop();
            }
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.import(tree, path);
                }
            }
            UseTree::Name(name) => {
                let name = name.ident.to_string();
                let allowed = name == "self"
                    || IMPORTS.contains(&name.as_str())
                    || EXEMPT_IMPORTS.contains(&(self.file.as_str(), name.as_str()));
                if from_costs(path) && !allowed {
                    self.state(format!("`{name}` is imported from grovedb_costs"));
                }
            }
            UseTree::Rename(rename) => {
                let name = rename.ident.to_string();
                let renames_costs = from_costs(path)
                    || name == "grovedb_costs"
                    || COST_TYPES.contains(&name.as_str());
                if renames_costs {
                    self.state(format!("`{name}` is imported as `{}`", rename.rename));
                }
            }
            UseTree::Glob(_) => {
                if from_costs(path) {
                    self.state(format!("`{}::*` is imported", path.join("::")));
                }
            }
        }
    }

    /// The tokens of a macro are not parsed, so names are looked for as
    /// words. In the definition of a macro a cost type counts where it is
    /// followed by `::`, `{` or `(`, which is how one is built, and not
    /// where a signature in the macro names it. In a call of a macro it
    /// counts wherever it stands.
    fn tokens(&mut self, tokens: String, of: &str) {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let is_definition = of == "macro_rules";
        for word in tokens.split(|c| !is_word(c)) {
            let stated = COST_FIELDS
                .iter()
                .chain(&COST_METHODS)
                .chain(&COST_CONSTRUCTORS)
                .any(|name| *name == word)
                || word.starts_with("cost_return_on_error")
                || (!is_definition && COST_TYPES.contains(&word));
            if stated {
                self.state(format!("`{word}` in `{of}!`"));
            }
        }
        if is_definition {
            let dense: String = tokens.split_whitespace().collect();
            for name in COST_TYPES {
                let built = ["::", "{", "("]
                    .iter()
                    .any(|next| dense.contains(&format!("{name}{next}")));
                if built {
                    self.state(format!("`{name}` in `{of}!`"));
                }
            }
        }
    }
}

fn is_test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Meta>()
                .is_ok_and(|predicate| predicate.path().is_ident("test"))
    })
}

impl<'ast> Visit<'ast> for Charges {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if !is_test_only(&module.attrs) {
            visit::visit_item_mod(self, module);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.import(&item.tree, &mut Vec::new());
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if let Some((_, rename)) = &item.rename {
            self.state(format!("`{}` is imported as `{rename}`", item.ident));
        }
    }

    fn visit_item_type(&mut self, alias: &'ast syn::ItemType) {
        self.item(&alias.ident, false, |this| {
            this.body(|this| this.visit_type(&alias.ty))
        });
    }

    fn visit_impl_item_type(&mut self, alias: &'ast syn::ImplItemType) {
        self.item(&alias.ident, false, |this| {
            this.body(|this| this.visit_type(&alias.ty))
        });
    }

    fn visit_trait_item_type(&mut self, alias: &'ast syn::TraitItemType) {
        self.item(&alias.ident, false, |this| {
            this.body(|this| visit::visit_trait_item_type(this, alias))
        });
    }

    fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
        self.item(&item.ident, false, |this| {
            this.body(|this| visit::visit_item_const(this, item))
        });
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        self.item(&item.ident, false, |this| {
            this.body(|this| visit::visit_item_static(this, item))
        });
    }

    fn visit_impl_item_const(&mut self, item: &'ast syn::ImplItemConst) {
        self.item(&item.ident, false, |this| {
            this.body(|this| visit::visit_impl_item_const(this, item))
        });
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        self.function(&function.sig, |this| visit::visit_item_fn(this, function));
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        self.function(&function.sig, |this| {
            visit::visit_impl_item_fn(this, function)
        });
    }

    fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
        self.function(&function.sig, |this| {
            visit::visit_trait_item_fn(this, function)
        });
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.body(|this| visit::visit_block(this, block));
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if self.bodies > 0 {
            let names: Vec<String> = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            for name in &names {
                if COST_TYPES.contains(&name.as_str()) {
                    self.state(format!("`{}`", names.join("::")));
                }
            }
            if names.ends_with(&["Default".to_string(), "default".to_string()]) {
                self.state("`Default::default`".to_string());
            }
            let called = names.last().is_some_and(|last| {
                COST_METHODS
                    .iter()
                    .chain(&COST_CONSTRUCTORS)
                    .any(|name| name == last)
            });
            if called {
                self.state(format!("`{}`", names.join("::")));
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_member(&mut self, member: &'ast Member) {
        if let Member::Named(name) = member
            && COST_FIELDS.iter().any(|field| name == field)
        {
            self.state(format!("the field `{name}`"));
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let stated = COST_METHODS
            .iter()
            .chain(&COST_CONSTRUCTORS)
            .any(|method| call.method == method);
        if stated {
            self.state(format!("a call of `{}`", call.method));
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_binary(&mut self, binary: &'ast syn::ExprBinary) {
        match binary.op {
            BinOp::Add(_) if self.may_not_add() => self.state("`+`".to_string()),
            BinOp::AddAssign(_) if self.may_not_add() => self.state("`+=`".to_string()),
            _ => {}
        }
        visit::visit_expr_binary(self, binary);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let name = invocation
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default();
        if name.starts_with("cost_return_on_error") {
            self.state(format!("`{name}!`"));
        }
        self.tokens(invocation.tokens.to_string(), &name);
    }
}

fn charges_stated_in(file: &str, source: &str) -> Vec<String> {
    let parsed = syn::parse_file(source).unwrap_or_else(|error| panic!("parsing {file}: {error}"));
    let mut charges = Charges {
        file: file.to_string(),
        ..Default::default()
    };
    charges.visit_file(&parsed);
    charges.stated
}

fn crate_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![root.clone()];
    let mut sources = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries = fs::read_dir(&path).expect("a source directory");
            pending.extend(entries.map(|entry| entry.expect("a directory entry").path()));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = fs::read_to_string(&path).expect("a source file");
            sources.push((relative(&root, &path), source));
        }
    }
    sources.sort();
    sources
}

fn relative(root: &Path, path: &Path) -> String {
    let relative: PathBuf = path.strip_prefix(root).unwrap_or(path).into();
    relative.to_string_lossy().replace('\\', "/")
}

/// The one file of the crate that is built for tests only as a whole
/// (`#[cfg(test)] mod tests;`).
const TESTS: &str = "rocksdb_storage/tests.rs";

#[test]
fn the_storage_crate_charges_only_through_the_cost_model() {
    let sources = crate_sources();
    let files: Vec<&str> = sources.iter().map(|(file, _)| file.as_str()).collect();
    for expected in [
        "rocksdb_storage/storage.rs",
        "rocksdb_storage/storage_context/batch.rs",
        "rocksdb_storage/storage_context/context_immediate.rs",
        "rocksdb_storage/storage_context/context_tx.rs",
        "rocksdb_storage/storage_context/flat.rs",
        "rocksdb_storage/storage_context/raw_iterator.rs",
        "flat.rs",
        "storage.rs",
        TESTS,
    ] {
        assert!(files.contains(&expected), "{expected} was not read");
    }

    for (file, source) in &sources {
        if file == TESTS {
            continue;
        }
        let stated = charges_stated_in(file, source);
        assert!(stated.is_empty(), "{file} states a charge: {stated:#?}");

        let reports_costs = source.contains("wrap_with_cost") || source.contains("cost_acc.");
        if file.starts_with("rocksdb_storage/") {
            assert_eq!(
                source.contains("COST_MODEL"),
                reports_costs,
                "{file} reports costs and names no cost model, or the reverse"
            );
        }
    }
}

fn stated(body: &str) -> Vec<String> {
    charges_stated_in(
        "rocksdb_storage/engine.rs",
        &format!("fn engine() {{ {body} }}"),
    )
}

#[test]
fn a_charge_stated_outside_the_model_is_found() {
    for (body, charge) in [
        (
            "OperationCost::with_seek_count(1);",
            "`OperationCost::with_seek_count`",
        ),
        (
            "grovedb_costs::OperationCost::default();",
            "`grovedb_costs::OperationCost::default`",
        ),
        (
            "let cost = OperationCost { seek_count: 1, ..zero };",
            "`OperationCost`",
        ),
        ("let make = StorageCost::verify;", "`StorageCost::verify`"),
        (
            "let removed = BasicStorageRemoval(5);",
            "`BasicStorageRemoval`",
        ),
        ("let zero: Cost = Default::default();", "`Default::default`"),
        (
            "let zero = <Cost as Default>::default();",
            "`Default::default`",
        ),
        ("let zero = <OperationCost>::default();", "`OperationCost`"),
        ("let seeks = |cost: OperationCost| cost;", "`OperationCost`"),
        ("cost.seek_count = 2;", "the field `seek_count`"),
        (
            "let loaded = cost.storage_loaded_bytes;",
            "the field `storage_loaded_bytes`",
        ),
        ("context.cost = other;", "the field `cost`"),
        (
            "let Cost { hash_node_calls, .. } = cost;",
            "the field `hash_node_calls`",
        ),
        (
            "cost.add_key_value_storage_costs(1, 2, None, None);",
            "a call of `add_key_value_storage_costs`",
        ),
        (
            "let len = key_len.required_space();",
            "a call of `required_space`",
        ),
        ("let both = context.add_cost(cost);", "a call of `add_cost`"),
        (
            "let value = context.unwrap_add_cost(&mut cost);",
            "a call of `unwrap_add_cost`",
        ),
        ("let both = first.add(second);", "a call of `add`"),
        (
            "let both = first.saturating_add(second);",
            "a call of `saturating_add`",
        ),
        ("let both = COST_MODEL.read(found) + other;", "`+`"),
        ("cost += COST_MODEL.cursor_move();", "`+=`"),
        (
            "let value = cost_return_on_error!(&mut cost, read());",
            "`cost_return_on_error!`",
        ),
        (
            "let value = cost_return_on_error_no_add!(cost, read());",
            "`cost_return_on_error_no_add!`",
        ),
        (
            "let costs = vec![OperationCost::default()];",
            "`OperationCost` in `vec!`",
        ),
        (
            "assert!(cost.seek_count == 0);",
            "`seek_count` in `assert!`",
        ),
        (
            "let cost = build!(StorageCost { added_bytes: 1 });",
            "`StorageCost` in `build!`",
        ),
        (
            "let removed = build!(BasicStorageRemoval(5));",
            "`BasicStorageRemoval` in `build!`",
        ),
    ] {
        let found = stated(body);
        assert!(
            found
                .iter()
                .any(|stated| stated == &format!("engine: {charge}")),
            "{body}: {found:?}"
        );
    }

    for (item, charge) in [
        (
            "use grovedb_costs::OperationCost as C;",
            "the module: `OperationCost` is imported as `C`",
        ),
        (
            "use grovedb_costs::{CostsExt, OperationCost as C};",
            "the module: `OperationCost` is imported as `C`",
        ),
        (
            "use grovedb_costs as costs;",
            "the module: `grovedb_costs` is imported as `costs`",
        ),
        (
            "use grovedb_costs::cost_model as model;",
            "the module: `cost_model` is imported as `model`",
        ),
        (
            "use crate::helpers::OperationCost as C;",
            "the module: `OperationCost` is imported as `C`",
        ),
        (
            "use grovedb_costs::*;",
            "the module: `grovedb_costs::*` is imported",
        ),
        (
            "use grovedb_costs::storage_cost::*;",
            "the module: `grovedb_costs::storage_cost::*` is imported",
        ),
        (
            "use grovedb_costs::storage_cost::StorageCost;",
            "the module: `StorageCost` is imported from grovedb_costs",
        ),
        (
            "use grovedb_costs::cost_return_on_error;",
            "the module: `cost_return_on_error` is imported from grovedb_costs",
        ),
        (
            "extern crate grovedb_costs as costs;",
            "the module: `grovedb_costs` is imported as `costs`",
        ),
        ("type C = OperationCost;", "C: `OperationCost`"),
        (
            "type C = grovedb_costs::CostContext<grovedb_costs::StorageCost>;",
            "C: `grovedb_costs::StorageCost`",
        ),
        (
            "const SEEK: OperationCost = make();",
            "SEEK: `OperationCost`",
        ),
        (
            "static ZERO: Cost = Cost { seek_count: 0 };",
            "ZERO: the field `seek_count`",
        ),
        ("impl Engine { const ONE: u32 = 1 + 0; }", "ONE: `+`"),
        (
            "impl Engine { fn get(&self) { OperationCost::default(); } }",
            "get: `OperationCost::default`",
        ),
        (
            "trait Engine { fn get(&self) { OperationCost::default(); } }",
            "get: `OperationCost::default`",
        ),
        (
            "mod inner { fn get() { OperationCost::default(); } }",
            "get: `OperationCost::default`",
        ),
    ] {
        let found = charges_stated_in("rocksdb_storage/engine.rs", item);
        assert_eq!(found, [charge], "{item}");
    }
}

#[test]
fn a_charge_reached_by_an_associated_type_a_macro_or_a_path_is_found() {
    let finds = |source: &str, charge: &str| {
        let found = charges_stated_in("rocksdb_storage/engine.rs", source);
        assert!(
            found.iter().any(|stated| stated == charge),
            "{source}: {found:?}"
        );
    };

    let associated = "trait HiddenCost { type C; fn charge() -> OperationCost; }
impl HiddenCost                       for RocksDbStorage { type C = OperationCost; fn charge() -> OperationCost {                       <Self as HiddenCost>::C::with_seek_count(1) } }";
    finds(associated, "C: `OperationCost`");
    finds(associated, "charge: `HiddenCost::C::with_seek_count`");
    finds(
        "trait HiddenCost { type C = OperationCost; }",
        "C: `OperationCost`",
    );

    let by_macro = "macro_rules! make_cost { ($ty:ty) => { <$ty>::with_seek_count(1) }; }
fn                     charge() -> OperationCost { make_cost!(OperationCost) }";
    finds(by_macro, "the module: `with_seek_count` in `macro_rules!`");
    finds(by_macro, "charge: `OperationCost` in `make_cost!`");
    finds(
        "macro_rules! add_up { ($cost:ident, $call:expr) => { cost_return_on_error!(&mut $cost,          $call) }; }",
        "the module: `cost_return_on_error` in `macro_rules!`",
    );

    let by_path = "fn wrong() -> CostResult<(), Error> { Ok(()).wrap_with_cost(std::ops::Add::add(                    COST_MODEL.read(None), COST_MODEL.cursor_move())) }";
    finds(by_path, "wrong: `std::ops::Add::add`");
    finds(
        "fn wrong(cost: &mut OperationCost) { AddAssign::add_assign(cost, COST_MODEL.cursor_move());          }",
        "wrong: `AddAssign::add_assign`",
    );
    finds(
        "fn wrong() -> CostContext<()> { first().flat_map(|()| second()) }",
        "wrong: a call of `flat_map`",
    );
    finds(
        "fn wrong() -> CostResult<(), Error> { CostContext::flat_map_ok(first(), |()| second()) }",
        "wrong: `CostContext::flat_map_ok`",
    );
    finds(
        "fn wrong() -> OperationCost { Cost::with_hash_node_calls(1) }",
        "wrong: `Cost::with_hash_node_calls`",
    );
    finds(
        "fn wrong(key: &[u8]) { let info = hidden().for_updated_root_cost(None, 4); }",
        "wrong: a call of `for_updated_root_cost`",
    );
}

#[test]
fn what_is_not_a_charge_is_not_taken_for_one() {
    let engine = r#"
        use grovedb_costs::{
            cost_model::{BatchCharge, Keyspace, StorageCostModel},
            storage_cost::key_value_cost::KeyValueStorageCost,
            ChildrenSizesWithIsSumTree, CostContext, CostResult, CostsExt, OperationCost,
        };
        use grovedb_costs::{self, cost_model};
        use rocksdb::{Error as RocksError, *};

        pub(crate) const COST_MODEL: StorageCostModel = StorageCostModel::V0;

        impl Engine {
            pub fn get<'b, B>(&self, key: B) -> CostResult<Option<Vec<u8>>, Error>
            where
                B: AsRef<[u8]> + 'b,
            {
                let read = self.db.get(key);
                let found = read.as_ref().ok().and_then(Option::as_ref).map(Vec::len);
                read.wrap_with_cost(COST_MODEL.read(found))
            }

            pub fn commit(
                &self,
                pending_costs: OperationCost,
                cost_info: Option<KeyValueStorageCost>,
            ) -> CostContext<OperationCost> {
                let batch = WriteBatchWithTransaction::<true>::default();
                let last = prefix[31].wrapping_add(1);
                let message = format!("refused {last}");
                pending_costs.wrap_with_cost(COST_MODEL.batch_written(pending_costs, true))
            }
        }

        macro_rules! impl_keyspace {
            ($context:ident) => {
                impl FlatStore for $context {
                    fn put(&self, cost_info: Option<KeyValueStorageCost>) -> CostResult<(), Error> {
                        self.0.put_aux(cost_info)
                    }
                }
            };
        }

        #[cfg(test)]
        mod tests {
            fn cost() -> OperationCost {
                OperationCost::default() + OperationCost::with_seek_count(1)
            }
        }
    "#;
    assert_eq!(
        charges_stated_in("rocksdb_storage/engine.rs", engine),
        [] as [&str; 0]
    );

    let exempt =
        "pub trait FlatStore { fn clear(&self) { let mut cost = OperationCost::default(); \
                  } }\nfn stage_pending_prefix_drop() { OperationCost::default(); }\nfn other() { \
                  OperationCost::default(); }";
    assert_eq!(
        charges_stated_in("flat.rs", exempt),
        ["other: `OperationCost::default`"]
    );
    assert_eq!(charges_stated_in("storage.rs", exempt).len(), 3);

    let adding = "impl StorageBatch { fn len(&self) -> usize { self.data.len() + self.aux.len() }                   }
fn walk(model: StorageCostModel) -> CostResult<(), Error> {                   Ok(()).wrap_with_cost(model.cursor_move() + model.cursor_move()) }
fn                   twice(cost: &mut OperationCost, other: u32) { *cost += other; }";
    assert_eq!(
        charges_stated_in("storage.rs", adding),
        ["walk: `+`", "twice: `+=`"]
    );
    assert_eq!(
        charges_stated_in("rocksdb_storage/storage.rs", adding),
        ["len: `+`", "walk: `+`", "twice: `+=`"]
    );
}
