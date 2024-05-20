#[cfg(feature = "grovedbg")]
fn main() {
    use std::{
        env,
        path::PathBuf,
        process::{Command, ExitStatus},
    };

    let out_dir = PathBuf::from(&env::var_os("OUT_DIR").unwrap());

    if !Command::new("trunk")
        .arg("build")
        .arg("--release")
        .arg("--dist")
        .arg(&out_dir)
        .arg("grovedbg/index.html")
        .status()
        .as_ref()
        .map(ExitStatus::success)
        .unwrap_or(false)
    {
        panic!("Error running `trunk build --release`");
    }

    let zip_file = out_dir.join("grovedbg.zip");
    zip_extensions::write::zip_create_from_directory(&zip_file, &out_dir)
        .expect("can't create a grovedbg zip archive");
}

#[cfg(not(feature = "grovedbg"))]
fn main() {}
