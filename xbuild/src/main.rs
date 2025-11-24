mod zip_ext;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use fs_extra::{dir, file};
use zip::{CompressionMethod, write::FileOptions};

use crate::zip_ext::zip_create_from_directory_with_options;

fn main() -> Result<()> {
    let temp_dir = temp_dir();

    // Clean temp dir
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;

    let mut cargo = cargo_ndk();
    let args = vec![
        "build",
        "--target",
        "aarch64-linux-android",
        "-Z",
        "build-std",
        "-Z",
        "trim-paths",
        "--release",
    ];

    cargo.args(args);

    let status = cargo.spawn()?.wait()?;
    if !status.success() {
        anyhow::bail!("Cargo build failed");
    }

    // Copy module files
    let module_dir = module_dir();
    dir::copy(
        &module_dir,
        &temp_dir,
        &dir::CopyOptions::new().overwrite(true).content_only(true),
    )?;
    
    // Remove gitignore if exists
    if temp_dir.join(".gitignore").exists() {
        fs::remove_file(temp_dir.join(".gitignore"))?;
    }

    // Inject Git Commit ID into module.prop
    if let Err(e) = inject_version(&temp_dir) {
        println!("Warning: Failed to inject version: {}", e);
    }

    // Copy binary (Rename to meta-hybrid)
    file::copy(
        bin_path(),
        temp_dir.join("meta-hybrid"),
        &file::CopyOptions::new().overwrite(true),
    )?;

    // Build WebUI
    build_webui()?;

    // Zip it
    let options = FileOptions::<()>::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(9));
        
    let output_zip = Path::new("output").join("meta-hybrid.zip");
    if let Some(parent) = output_zip.parent() {
        fs::create_dir_all(parent)?;
    }

    zip_create_from_directory_with_options(
        &output_zip,
        &temp_dir,
        |_| options,
    )?;

    println!("Build success: {}", output_zip.display());
    Ok(())
}

fn inject_version(temp_dir: &Path) -> Result<()> {
    // Get git short hash
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()?;
    
    if output.status.success() {
        let hash = String::from_utf8(output.stdout)?.trim().to_string();
        let prop_path = temp_dir.join("module.prop");
        
        if prop_path.exists() {
            let content = fs::read_to_string(&prop_path)?;
            let new_content = content.lines().map(|line| {
                if line.starts_with("version=") {
                    format!("{}-g{}", line, hash)
                } else {
                    line.to_string()
                }
            }).collect::<Vec<_>>().join("\n");
            
            fs::write(prop_path, new_content)?;
            println!("Injected version with commit hash: {}", hash);
        }
    }
    Ok(())
}

fn module_dir() -> PathBuf {
    Path::new("module").to_path_buf()
}

fn temp_dir() -> PathBuf {
    Path::new("output").join(".temp")
}

// Binary name in Cargo.toml is meta-hybrid
fn bin_path() -> PathBuf {
    Path::new("target")
        .join("aarch64-linux-android")
        .join("release")
        .join("meta-hybrid")
}

fn cargo_ndk() -> Command {
    let mut command = Command::new("cargo");
    command
        .args(["ndk", "--platform", "31", "-t", "arm64-v8a"])
        .env("RUSTFLAGS", "-C default-linker-libraries")
        .env("CARGO_CFG_BPF_TARGET_ARCH", "aarch64");
    command
}

fn build_webui() -> Result<()> {
    println!("Building WebUI...");
    let npm = || {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "npm"]);
            c
        } else {
            Command::new("npm")
        };
        command.current_dir("webui");
        command
    };

    let status = npm().arg("install").spawn()?.wait()?;
    if !status.success() { anyhow::bail!("npm install failed"); }
    
    let status = npm().args(["run", "build"]).spawn()?.wait()?;
    if !status.success() { anyhow::bail!("npm run build failed"); }

    Ok(())
}
