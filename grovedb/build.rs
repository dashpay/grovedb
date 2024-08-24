#[cfg(feature = "grovedbg")]
fn main() {
    use std::{env, fs::File, io::Cursor, path::PathBuf};

    use hex_literal::hex;
    use sha2::{digest::FixedOutput, Digest, Sha256};

    const GROVEDBG_SHA256: [u8; 32] =
        hex!("ea7d9258973aa765eaf5064451fc83efa22e0ce6eaf2938507e2703571364e35");
    const GROVEDBG_VERSION: &str = "v1.0.0-rc.6";

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

#[cfg(not(feature = "grovedbg"))]
fn main() {}
