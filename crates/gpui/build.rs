#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

fn main() {
    println!("cargo::rustc-check-cfg=cfg(gles)");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        #[cfg(feature = "windows-manifest")]
        embed_resource();
    }
}

#[cfg(feature = "windows-manifest")]
fn embed_resource() {
    let manifest = std::path::Path::new("resources/windows/gpui.manifest.xml");
    let rc_file = std::path::Path::new("resources/windows/gpui.rc");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", rc_file.display());

    // Stage the .rc and the manifest side by side in OUT_DIR, and name the
    // manifest by file name alone.
    //
    // gpui.rc names the manifest relative to the crate root, which is where
    // rc.exe resolves it from when building on Windows. Cross-compiling from a
    // Unix host, embed-resource drives llvm-rc instead and runs it with the
    // working directory set to the .rc file's own directory, so the same path
    // resolves to resources/windows/resources/windows/gpui.manifest.xml and the
    // build dies with:
    //
    //   llvm-rc: Error in 24 statement (ID 1):
    //   error : file not found : resources/windows/gpui.manifest.xml
    //
    // A bare file name beside the .rc resolves under both compilers: llvm-rc
    // runs with OUT_DIR as its working directory, and embed-resource passes
    // OUT_DIR to rc.exe as an include directory.
    let out_dir = std::path::PathBuf::from(
        std::env::var("OUT_DIR").expect("OUT_DIR is set for build scripts"),
    );
    let manifest_name = std::path::Path::new(
        manifest
            .file_name()
            .expect("the manifest path ends in a file name"),
    );
    std::fs::copy(manifest, out_dir.join(manifest_name))
        .expect("staging the manifest into OUT_DIR");

    let staged_rc = out_dir.join(
        rc_file
            .file_name()
            .expect("the .rc path ends in a file name"),
    );
    std::fs::write(
        &staged_rc,
        std::fs::read_to_string(rc_file)
            .expect("reading gpui.rc")
            .replace(
                manifest.to_string_lossy().as_ref(),
                manifest_name.to_string_lossy().as_ref(),
            ),
    )
    .expect("staging gpui.rc into OUT_DIR");

    embed_resource::compile(&staged_rc, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
