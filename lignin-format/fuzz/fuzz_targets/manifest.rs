//! `MANIFEST` (`segment-manifest.md` section 3).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::manifest::{LiveFile, Manifest, TableCheckpoint};
use lignin_format_fuzz::{default_frame_len, ok, sealed_file, Input};

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let bytes = sealed_file(&input, default_frame_len);
    let Some(manifest) = ok(Manifest::decode(&bytes)) else {
        return;
    };
    assert_eq!(ok(manifest.encode()).as_deref(), Some(bytes.as_slice()));

    let table = manifest.table_checkpoint().map(|table| {
        ok(TableCheckpoint::new(table.file(), table.valid_len(), table.covers()))
            .expect("a checkpoint that was read")
    });
    let files: Vec<LiveFile> = manifest
        .files()
        .iter()
        .rev()
        .map(|file| {
            ok(LiveFile::new(file.kind(), file.id(), file.state(), file.valid_len()))
                .expect("a file that was read")
        })
        .collect();
    let again = Manifest::new(manifest.page_size(), manifest.last_file_ids(), table, files);
    assert_eq!(ok(again).as_ref(), Some(&manifest));
    assert_eq!(manifest.format_version(), lignin_format::FormatVersion::V1);
});
