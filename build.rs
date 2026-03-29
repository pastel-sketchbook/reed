use std::env;
use std::path::PathBuf;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Find the libghostty-vt dylib built by the -sys crate and set rpath
    // so the binary can find it at runtime without DYLD_LIBRARY_PATH.
    if let Ok(out_dir) = env::var("OUT_DIR") {
        // Walk up from our OUT_DIR to find the -sys crate's build output.
        let target_dir = PathBuf::from(&out_dir)
            .ancestors()
            .find(|p| p.join("build").is_dir())
            .map(|p| p.join("build"))
            .unwrap_or_default();

        if target_dir.is_dir() {
            // Search for the ghostty-install/lib directory.
            if let Some(lib_dir) = find_ghostty_lib_dir(&target_dir) {
                let lib_dir_str = lib_dir.display().to_string();
                println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir_str}");

                if target_os == "macos" {
                    // Also add @loader_path based rpath for relocatable builds.
                    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
                }
            }
        }
    }
}

fn find_ghostty_lib_dir(build_dir: &std::path::Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(build_dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("libghostty-vt-sys-"))
        {
            let lib_dir = path.join("out/ghostty-install/lib");
            if lib_dir.is_dir() {
                return Some(lib_dir);
            }
        }
    }
    None
}
