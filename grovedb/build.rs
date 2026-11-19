use std::{env, process::exit};

const TEST_ENGINES: &str = include_str!("test-engines.txt");
const TEST_ENGINE_VARIABLE: &str = "GROVEDB_TEST_ENGINE";

fn main() {
    select_test_engine();
    #[cfg(feature = "grovedbg")]
    fetch_grovedbg();
}

/// Sets the cfg `grovedb_test_engine` to the engine the test suite of this
/// build runs on: the one `GROVEDB_TEST_ENGINE` names, or the first of
/// `test-engines.txt`.
fn select_test_engine() {
    let engines: Vec<&str> = TEST_ENGINES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let values: Vec<String> = engines.iter().map(|name| format!("\"{name}\"")).collect();
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=test-engines.txt");
    println!("cargo::rerun-if-env-changed={TEST_ENGINE_VARIABLE}");
    println!(
        "cargo::rustc-check-cfg=cfg(grovedb_test_engine, values({}))",
        values.join(", ")
    );

    let named = env::var(TEST_ENGINE_VARIABLE).unwrap_or_default();
    let selected = if named.is_empty() {
        engines[0]
    } else if let Some(engine) = engines.iter().find(|engine| **engine == named) {
        engine
    } else {
        eprintln!(
            "{TEST_ENGINE_VARIABLE}={named} names no engine of grovedb/test-engines.txt, which \
             has: {}",
            engines.join(", ")
        );
        exit(1);
    };
    println!("cargo::rustc-cfg=grovedb_test_engine=\"{selected}\"");
    println!("cargo::rustc-env=GROVEDB_SUITE_ENGINE={selected}");
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
