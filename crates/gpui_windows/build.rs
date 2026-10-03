#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

fn main() {
    // Release builds bake the compiled shaders into the binary. Debug builds
    // compile the HLSL at startup and take nothing from here.
    #[cfg(not(debug_assertions))]
    {
        // Compile HLSL shaders
        #[cfg(target_os = "windows")]
        compile_shaders();

        // `target_os` above is the host's. A build for Windows from anywhere
        // else has no fxc.exe to run and takes the committed bytes instead.
        #[cfg(not(target_os = "windows"))]
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            prebuilt_shaders::install();
        }
    }
}

/// The shaders as `fxc.exe` compiled them, committed so that a host without
/// fxc can still build for Windows.
///
/// fxc ships only with the Windows SDK, and nothing off Windows emits the
/// `vs_4_1`/`ps_4_1` bytecode the renderer loads, so a cross build cannot make
/// `shaders_bytes.rs` for itself. It takes `src/shaders_prebuilt.rs`, which a
/// Windows build writes when `GPUI_WRITE_PREBUILT_SHADERS` names a path for it.
/// The file's first line records a hash of the HLSL it was compiled from, and
/// a cross build refuses bytes whose sources have changed since.
#[cfg(not(debug_assertions))]
mod prebuilt_shaders {
    use std::{fs, path::PathBuf};

    /// Every file fxc reads: the two it is given and the one they include.
    const SOURCES: [&str; 3] = [
        "src/alpha_correction.hlsl",
        "src/color_text_raster.hlsl",
        "src/shaders.hlsl",
    ];
    #[cfg(not(target_os = "windows"))]
    const PREBUILT: &str = "src/shaders_prebuilt.rs";
    const HASH_PREFIX: &str = "// hlsl-hash: ";

    fn manifest_dir() -> PathBuf {
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
    }

    /// FNV-1a over the sources. Carriage returns are skipped, so a CRLF
    /// checkout on Windows hashes as an LF one does everywhere else.
    fn source_hash() -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for byte in bytes.iter().filter(|byte| **byte != b'\r') {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for source in SOURCES {
            let contents = fs::read(manifest_dir().join(source))
                .unwrap_or_else(|error| panic!("Failed to read {source}: {error}"));
            feed(source.as_bytes());
            feed(&[0]);
            feed(&contents);
            feed(&[0]);
        }
        format!("{hash:016x}")
    }

    #[cfg(not(target_os = "windows"))]
    pub fn install() {
        let prebuilt_path = manifest_dir().join(PREBUILT);
        println!("cargo:rerun-if-changed={}", prebuilt_path.display());
        for source in SOURCES {
            println!(
                "cargo:rerun-if-changed={}",
                manifest_dir().join(source).display()
            );
        }

        let regenerate = format!(
            "build gpui_windows in release on Windows with \
             GPUI_WRITE_PREBUILT_SHADERS set to the path of {PREBUILT}, and commit the file"
        );
        let Ok(prebuilt) = fs::read_to_string(&prebuilt_path) else {
            fail(&format!(
                "{PREBUILT} is missing, and this host has no fxc.exe to compile the shaders: \
                 {regenerate}"
            ));
        };
        let recorded = prebuilt
            .lines()
            .next()
            .and_then(|line| line.strip_prefix(HASH_PREFIX));
        let current = source_hash();
        if recorded != Some(current.as_str()) {
            fail(&format!(
                "{PREBUILT} was compiled from different HLSL (it records {}, the sources hash \
                 to {current}): {regenerate}",
                recorded.unwrap_or("no hash")
            ));
        }

        let out_dir = std::env::var("OUT_DIR").unwrap();
        fs::write(format!("{out_dir}/shaders_bytes.rs"), prebuilt)
            .expect("Failed to write Rust binding file");
    }

    #[cfg(not(target_os = "windows"))]
    fn fail(message: &str) -> ! {
        println!("cargo::error={message}");
        std::process::exit(1);
    }

    /// Write the bytes fxc just produced to the path in
    /// `GPUI_WRITE_PREBUILT_SHADERS`, if there is one.
    #[cfg(target_os = "windows")]
    pub fn write(rust_binding_path: &str) {
        println!("cargo:rerun-if-env-changed=GPUI_WRITE_PREBUILT_SHADERS");
        let Ok(prebuilt_path) = std::env::var("GPUI_WRITE_PREBUILT_SHADERS") else {
            return;
        };
        let bindings =
            fs::read_to_string(rust_binding_path).expect("Failed to read Rust binding file");
        let prebuilt = format!(
            "{HASH_PREFIX}{}\n\
             // Compiled by fxc.exe from the HLSL beside this file; build.rs wrote it. Do not edit.\n\
             {}",
            source_hash(),
            bindings.replace("\r\n", "\n")
        );
        fs::write(&prebuilt_path, prebuilt)
            .unwrap_or_else(|error| panic!("Failed to write {prebuilt_path}: {error}"));
    }
}

#[cfg(all(target_os = "windows", not(debug_assertions)))]
mod shader_compilation {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        process::{self, Command},
    };

    pub fn compile_shaders() {
        let shader_path =
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/shaders.hlsl");
        let out_dir = std::env::var("OUT_DIR").unwrap();

        println!("cargo:rerun-if-changed={}", shader_path.display());

        // Check if fxc.exe is available
        let fxc_path = find_fxc_compiler();

        // Define all modules
        let modules = [
            "quad",
            "shadow",
            "path_rasterization",
            "path_sprite",
            "underline",
            "monochrome_sprite",
            "subpixel_sprite",
            "polychrome_sprite",
        ];

        let rust_binding_path = format!("{}/shaders_bytes.rs", out_dir);
        if Path::new(&rust_binding_path).exists() {
            fs::remove_file(&rust_binding_path)
                .expect("Failed to remove existing Rust binding file");
        }
        for module in modules {
            compile_shader_for_module(
                module,
                &out_dir,
                &fxc_path,
                shader_path.to_str().unwrap(),
                &rust_binding_path,
            );
        }

        {
            let shader_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("src/color_text_raster.hlsl");
            compile_shader_for_module(
                "emoji_rasterization",
                &out_dir,
                &fxc_path,
                shader_path.to_str().unwrap(),
                &rust_binding_path,
            );
        }

        super::prebuilt_shaders::write(&rust_binding_path);
    }

    /// Locate `binary` in the newest installed Windows SDK.
    pub fn find_latest_windows_sdk_binary(
        binary: &str,
    ) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
        let key = windows_registry::LOCAL_MACHINE
            .open("SOFTWARE\\WOW6432Node\\Microsoft\\Microsoft SDKs\\Windows\\v10.0")?;

        let install_folder: String = key.get_string("InstallationFolder")?; // "C:\Program Files (x86)\Windows Kits\10\"
        let install_folder_bin = Path::new(&install_folder).join("bin");

        let mut versions: Vec<_> = std::fs::read_dir(&install_folder_bin)?
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();

        versions.sort_by_key(|s| {
            s.split('.')
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<u32>>()
        });

        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => Err(format!(
                "Unsupported architecture: {}",
                std::env::consts::ARCH
            ))?,
        };

        if let Some(highest_version) = versions.last() {
            return Ok(Some(
                install_folder_bin
                    .join(highest_version)
                    .join(arch)
                    .join(binary),
            ));
        }

        Ok(None)
    }

    /// You can set the `GPUI_FXC_PATH` environment variable to specify the path to the fxc.exe compiler.
    fn find_fxc_compiler() -> String {
        // Check environment variable
        if let Ok(path) = std::env::var("GPUI_FXC_PATH")
            && Path::new(&path).exists()
        {
            return path;
        }

        // Try to find in PATH
        // NOTE: This has to be `where.exe` on Windows, not `where`, it must be ended with `.exe`
        if let Ok(output) = std::process::Command::new("where.exe")
            .arg("fxc.exe")
            .output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout);
            return path.trim().to_string();
        }

        if let Ok(Some(path)) = find_latest_windows_sdk_binary("fxc.exe") {
            return path.to_string_lossy().into_owned();
        }

        panic!("Failed to find fxc.exe");
    }

    fn compile_shader_for_module(
        module: &str,
        out_dir: &str,
        fxc_path: &str,
        shader_path: &str,
        rust_binding_path: &str,
    ) {
        // Compile vertex shader
        let output_file = format!("{}/{}_vs.h", out_dir, module);
        let const_name = format!("{}_VERTEX_BYTES", module.to_uppercase());
        compile_shader_impl(
            fxc_path,
            &format!("{module}_vertex"),
            &output_file,
            &const_name,
            shader_path,
            "vs_4_1",
        );
        generate_rust_binding(&const_name, &output_file, rust_binding_path);

        // Compile fragment shader
        let output_file = format!("{}/{}_ps.h", out_dir, module);
        let const_name = format!("{}_FRAGMENT_BYTES", module.to_uppercase());
        compile_shader_impl(
            fxc_path,
            &format!("{module}_fragment"),
            &output_file,
            &const_name,
            shader_path,
            "ps_4_1",
        );
        generate_rust_binding(&const_name, &output_file, rust_binding_path);
    }

    fn compile_shader_impl(
        fxc_path: &str,
        entry_point: &str,
        output_path: &str,
        var_name: &str,
        shader_path: &str,
        target: &str,
    ) {
        let output = Command::new(fxc_path)
            .args([
                "/T",
                target,
                "/E",
                entry_point,
                "/Fh",
                output_path,
                "/Vn",
                var_name,
                "/O3",
                shader_path,
            ])
            .output();

        match output {
            Ok(result) => {
                if result.status.success() {
                    return;
                }
                println!(
                    "cargo::error=Shader compilation failed for {}:\n{}",
                    entry_point,
                    String::from_utf8_lossy(&result.stderr)
                );
                process::exit(1);
            }
            Err(e) => {
                println!("cargo::error=Failed to run fxc for {}: {}", entry_point, e);
                process::exit(1);
            }
        }
    }

    fn generate_rust_binding(const_name: &str, head_file: &str, output_path: &str) {
        let header_content = fs::read_to_string(head_file).expect("Failed to read header file");
        let const_definition = {
            let global_var_start = header_content.find("const BYTE").unwrap();
            let global_var = &header_content[global_var_start..];
            let equal = global_var.find('=').unwrap();
            global_var[equal + 1..].trim()
        };
        let rust_binding = format!(
            "const {}: &[u8] = &{}\n",
            const_name,
            const_definition.replace('{', "[").replace('}', "]")
        );
        let mut options = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_path)
            .expect("Failed to open Rust binding file");
        options
            .write_all(rust_binding.as_bytes())
            .expect("Failed to write Rust binding file");
    }
}

#[cfg(all(target_os = "windows", not(debug_assertions)))]
use shader_compilation::compile_shaders;
