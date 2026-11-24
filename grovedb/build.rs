use std::{env, process::exit};

const TEST_ENGINES: &str = include_str!("test-engines.txt");
const MANIFEST: &str = include_str!("Cargo.toml");
const TEST_ENGINE_VARIABLE: &str = "GROVEDB_TEST_ENGINE";
const DEFAULT_ENGINE_VARIABLE: &str = "GROVEDB_DEFAULT_ENGINE";

fn main() {
    declare_default_engine();
    select_test_engine();
    #[cfg(feature = "grovedbg")]
    fetch_grovedbg();
}

/// Declares the cfg `grovedb_default_engine`, sets it where the build asks
/// for it by `GROVEDB_DEFAULT_ENGINE=either`, and holds it to its one
/// value, in every build of the crate.
///
/// The crate reads the cfg only where both engines are compiled in. There,
/// outside its unit tests, `grovedb_default_engine = "either"` puts
/// `GroveDb` with no engine named on the engine that is chosen when a
/// database is opened. A build sets the cfg by that variable, which
/// reaches this crate alone, or hands it to the compiler itself
/// (`--cfg 'grovedb_default_engine="either"'`), or both.
///
/// An unset or empty variable emits nothing. The script rejects other
/// nonempty variable values besides `either` and any Cargo-reported cfg
/// value other than `either`. A flag that is given to one invocation of
/// the compiler alone (`cargo rustc -- --cfg ..`) is not reported to it.
fn declare_default_engine() {
    println!("cargo::rustc-check-cfg=cfg(grovedb_default_engine, values(\"either\"))");
    println!("cargo::rerun-if-env-changed={DEFAULT_ENGINE_VARIABLE}");
    // What Cargo tells this script of a cfg of that name among the flags
    // of the build (`RUSTFLAGS`, `build.rustflags`): all of its values in
    // one variable. Set to anything but the one value, it fails the build.
    match env::var_os("CARGO_CFG_GROVEDB_DEFAULT_ENGINE") {
        Some(told) if told != "either" => {
            eprintln!(
                "the cfg grovedb_default_engine is set to {told:?} among the flags of the \
                 build: its one value is \"either\""
            );
            exit(1);
        }
        _ => {}
    }
    // A variable that is not set, or is set to nothing, asks for nothing.
    let named = env::var_os(DEFAULT_ENGINE_VARIABLE).unwrap_or_default();
    match named.to_str() {
        Some("") => {}
        Some("either") => println!("cargo::rustc-cfg=grovedb_default_engine=\"either\""),
        _ => {
            eprintln!(
                "{DEFAULT_ENGINE_VARIABLE}={named:?} names no default engine: it is `either`, \
                 empty or not set"
            );
            exit(1);
        }
    }
}

/// The Cargo features the manifest declares: the keys of its `[features]`
/// table.
fn declared_features() -> Vec<&'static str> {
    MANIFEST
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "[features]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once('='))
        .map(|(key, _)| key.trim())
        .filter(|key| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .collect()
}

/// Sets the cfg `grovedb_test_engine` to the engine the test suite of this build runs
/// on: the one `GROVEDB_TEST_ENGINE` names, or the first of `test-engines.txt`. The cfg is
/// set, with no value and with the name, only if every Cargo feature the
/// engine is registered with is on. Without them the suite has no engine,
/// which is an error if the variable asked for one.
fn select_test_engine() {
    let engines: Vec<(&str, &str)> = TEST_ENGINES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(name), Some(features), None) => (name, features),
                _ => {
                    eprintln!("grovedb/test-engines.txt: `{line}` is not a name and its features");
                    exit(1);
                }
            }
        })
        .collect();
    let declared = declared_features();
    for (name, features) in &engines {
        for feature in features.split(',') {
            if !declared.contains(&feature) {
                eprintln!(
                    "grovedb/test-engines.txt: `{name}` names the feature `{feature}`, which grovedb has not"
                );
                exit(1);
            }
        }
    }
    let names: Vec<&str> = engines.iter().map(|(name, _)| *name).collect();
    let values: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=test-engines.txt");
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-env-changed={TEST_ENGINE_VARIABLE}");
    println!(
        "cargo::rustc-check-cfg=cfg(grovedb_test_engine, values(none(), {}))",
        values.join(", ")
    );

    let named = env::var(TEST_ENGINE_VARIABLE).unwrap_or_default();
    let selected = if named.is_empty() {
        engines.first().copied()
    } else {
        engines.iter().copied().find(|(name, _)| *name == named)
    };
    let Some((name, features)) = selected else {
        eprintln!(
            "{TEST_ENGINE_VARIABLE}={named} names no engine of grovedb/test-engines.txt, which \
             has: {}",
            names.join(", ")
        );
        exit(1);
    };
    let is_on = |feature: &str| {
        let variable = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
        env::var_os(variable).is_some()
    };
    if features.split(',').all(is_on) {
        println!("cargo::rustc-cfg=grovedb_test_engine");
        println!("cargo::rustc-cfg=grovedb_test_engine=\"{name}\"");
        println!("cargo::rustc-env=GROVEDB_SUITE_ENGINE={name}");
    } else if !named.is_empty() {
        eprintln!(
            "{TEST_ENGINE_VARIABLE}={named} selects an engine that is compiled with the features \
             `{features}`, which are not all on in this build"
        );
        exit(1);
    }
}

#[cfg(feature = "grovedbg")]
fn fetch_grovedbg() {
    use std::{fs::File, io::Cursor, path::PathBuf};

    use hex_literal::hex;
    use sha2::{digest::FixedOutput, Digest, Sha256};

    const GROVEDBG_SHA256: [u8; 32] =
        hex!("da31eb2c93d553abb730455b53761a6e3913d876d0371b4a4a7e08a7398322ed");
    const GROVEDBG_VERSION: &str = "v1.2.0";

    let out_dir = PathBuf::from(&env::var_os("OUT_DIR").unwrap());
    let grovedbg_zip_path = out_dir.join("grovedbg.zip");

    if !grovedbg_zip_path.exists() {
        let response = reqwest::blocking::get(format!(
            "https://github.com/dashpay/grovedbg/releases/download/\
{GROVEDBG_VERSION}/grovedbg-{GROVEDBG_VERSION}.zip"
        ))
        .expect("can't download GroveDBG artifact");

        let mut grovedbg_zip = File::create(&grovedbg_zip_path).unwrap();
        let mut content = Cursor::new(response.bytes().unwrap());
        std::io::copy(&mut content, &mut grovedbg_zip).unwrap();
    }

    let mut grovedbg_zip = File::open(&grovedbg_zip_path).unwrap();

    let mut sha256 = Sha256::new();
    std::io::copy(&mut grovedbg_zip, &mut sha256).unwrap();
    let hash = sha256.finalize_fixed();

    assert_eq!(hash.as_slice(), GROVEDBG_SHA256);
}
