#[cfg(feature = "grovedbg")]
fn main() {
    use std::{
        env,
        path::PathBuf,
        process::{Command, ExitStatus, Output},
    };

    let out_dir = PathBuf::from(&env::var_os("OUT_DIR").unwrap());

    let Output {
        status,
        stdout,
        stderr,
    } = Command::new("trunk")
        .arg("build")
        .arg("--release")
        .arg("--dist")
        .arg(&out_dir)
        .arg("grovedbg/index.html")
        .output()
        .expect("cannot start trunk process");

    if !status.success() {
        let stdout_msg = String::from_utf8_lossy(&stdout);
        let stderr_msg = String::from_utf8_lossy(&stderr);
        let bindgen_version = env::var_os("TRUNK_TOOLS_WASM_BINDGEN").unwrap_or_default();
        panic!("Error running `trunk build --release`\nbindgen version:{bindgen_version:?}\n{stdout_msg}\n{stderr_msg}");
    }

    let zip_file = out_dir.join("grovedbg.zip");
    zip_extensions::write::zip_create_from_directory(&zip_file, &out_dir)
        .expect("can't create a grovedbg zip archive");
}

#[cfg(not(feature = "grovedbg"))]
fn main() {}
