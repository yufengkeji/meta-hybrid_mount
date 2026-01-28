use std::{
    collections::HashSet,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink},
    },
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
#[cfg(any(target_os = "linux", target_os = "android"))]
use extattr::{Flags as XattrFlags, lgetxattr, llistxattr, lsetxattr};
use rustix::fs::ioctl_ficlone;
use walkdir::WalkDir;

const SELINUX_XATTR: &str = "security.selinux";
const OVERLAY_OPAQUE_XATTR: &str = "trusted.overlay.opaque";
const CONTEXT_SYSTEM: &str = "u:object_r:system_file:s0";
const CONTEXT_VENDOR: &str = "u:object_r:vendor_file:s0";
const CONTEXT_HAL: &str = "u:object_r:same_process_hal_file:s0";
const CONTEXT_VENDOR_EXEC: &str = "u:object_r:vendor_file:s0";
const CONTEXT_ROOTFS: &str = "u:object_r:rootfs:s0";
const XATTR_TEST_FILE: &str = ".xattr_test";

pub fn atomic_write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, content: C) -> Result<()> {
    let path = path.as_ref();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let temp_name = format!(".{}_{}.tmp", pid, now);
    let temp_file = dir.join(temp_name);

    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_file)?;
        file.write_all(content.as_ref())?;
    }

    if let Err(e) = fs::rename(&temp_file, path) {
        if let Err(copy_err) = fs::copy(&temp_file, path) {
            let _ = fs::remove_file(&temp_file);
            return Err(copy_err).context("atomic_write copy fallback failed");
        }
        let _ = fs::remove_file(&temp_file);
    }
    Ok(())
}

fn copy_extended_attributes(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if let Ok(mut ctx) = lgetfilecon(src) {
            if ctx.contains("u:object_r:rootfs:s0") {
                ctx = CONTEXT_SYSTEM.to_string();
            }
            let _ = lsetfilecon(dst, &ctx);
        } else {
            let _ = lsetfilecon(dst, CONTEXT_SYSTEM);
        }
        if let Ok(opaque) = lgetxattr(src, OVERLAY_OPAQUE_XATTR) {
            let _ = lsetxattr(dst, OVERLAY_OPAQUE_XATTR, &opaque, XattrFlags::empty());
        }
        if let Ok(xattrs) = llistxattr(src) {
            for xattr_name in xattrs {
                let name_bytes = xattr_name.as_bytes();
                let name_str = String::from_utf8_lossy(name_bytes);

                #[allow(clippy::collapsible_if)]
                if name_str.starts_with("trusted.overlay.") && name_str != OVERLAY_OPAQUE_XATTR {
                    if let Ok(val) = lgetxattr(src, &xattr_name) {
                        let _ = lsetxattr(dst, &xattr_name, &val, XattrFlags::empty());
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn set_overlay_opaque<P: AsRef<Path>>(path: P) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        lsetxattr(
            path.as_ref(),
            OVERLAY_OPAQUE_XATTR,
            b"y",
            XattrFlags::empty(),
        )?;
    }
    Ok(())
}

pub fn lsetfilecon<P: AsRef<Path>>(path: P, con: &str) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if let Err(e) = lsetxattr(
            path.as_ref(),
            SELINUX_XATTR,
            con.as_bytes(),
            XattrFlags::empty(),
        ) {
            let _ = e;
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn lgetfilecon<P: AsRef<Path>>(path: P) -> Result<String> {
    let con = extattr::lgetxattr(path.as_ref(), SELINUX_XATTR).with_context(|| {
        format!(
            "Failed to get SELinux context for {}",
            path.as_ref().display()
        )
    })?;
    let con_str = String::from_utf8_lossy(&con).trim_matches('\0').to_string();

    Ok(con_str)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn lgetfilecon<P: AsRef<Path>>(_path: P) -> Result<String> {
    unimplemented!();
}

pub fn copy_path_context<S: AsRef<Path>, D: AsRef<Path>>(src: S, dst: D) -> Result<()> {
    let mut context = if src.as_ref().exists() {
        lgetfilecon(&src).unwrap_or_else(|_| CONTEXT_SYSTEM.to_string())
    } else {
        CONTEXT_SYSTEM.to_string()
    };

    if context.contains("u:object_r:rootfs:s0") {
        context = CONTEXT_SYSTEM.to_string();
    }

    lsetfilecon(dst, &context)
}

pub fn ensure_dir_exists<T: AsRef<Path>>(dir: T) -> Result<()> {
    if !dir.as_ref().exists() {
        fs::create_dir_all(&dir)?;
    }
    Ok(())
}

pub fn is_xattr_supported(path: &Path) -> bool {
    let test_file = path.join(XATTR_TEST_FILE);
    if fs::write(&test_file, b"test").is_err() {
        return false;
    }
    let result = lsetfilecon(&test_file, "u:object_r:system_file:s0");
    let supported = result.is_ok();
    let _ = fs::remove_file(test_file);
    supported
}

pub fn is_overlay_xattr_supported() -> Result<bool> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let output = Command::new("zcat")
            .arg("/proc/config.gz")
            .output()
            .context("Failed to read config.gz")
            .unwrap();
        let config = String::from_utf8_lossy(&output.stdout);

        for i in config.lines() {
            if i.starts_with("#") {
                continue;
            }

            let Some((k, v)) = i.split_once('=') else {
                continue;
            };

            if k.trim() == "CONFIG_TMPFS_XATTR" && v.trim() == "y" {
                return Ok(true);
            }
        }

        return Ok(false);
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    Ok(true)
}

pub fn reflink_or_copy(src: &Path, dest: &Path) -> Result<u64> {
    let src_file = File::open(src)?;
    let dest_file = File::create(dest)?;

    if ioctl_ficlone(&dest_file, &src_file).is_ok() {
        let metadata = src_file.metadata()?;
        let len = metadata.len();
        dest_file.set_permissions(metadata.permissions())?;
        return Ok(len);
    }
    drop(dest_file);
    drop(src_file);
    fs::copy(src, dest).map_err(|e| e.into())
}

fn make_device_node(path: &Path, mode: u32, rdev: u64) -> Result<()> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())?;
    let dev = rdev as libc::dev_t;
    unsafe {
        if libc::mknod(c_path.as_ptr(), mode as libc::mode_t, dev) != 0 {
            let err = std::io::Error::last_os_error();
            bail!("mknod failed for {}: {}", path.display(), err);
        }
    }
    Ok(())
}

fn guess_context_by_path(path: &Path) -> &'static str {
    let path_str = path.to_string_lossy();

    if path_str.starts_with("/vendor") || path_str.starts_with("/odm") {
        if path_str.contains("/lib/") || path_str.contains("/lib64/") || path_str.ends_with(".so") {
            return CONTEXT_HAL;
        }

        if path_str.contains("/bin/") {
            return CONTEXT_VENDOR_EXEC;
        }

        if path_str.contains("/firmware") {
            return CONTEXT_VENDOR;
        }

        return CONTEXT_VENDOR;
    }

    CONTEXT_SYSTEM
}

fn apply_system_context(current: &Path, relative: &Path) -> Result<()> {
    if let Some(name) = current.file_name().and_then(|n| n.to_str())
        && (name == "upperdir" || name == "workdir")
        && let Some(parent) = current.parent()
        && let Ok(ctx) = lgetfilecon(parent)
    {
        return lsetfilecon(current, &ctx);
    }

    let current_ctx = lgetfilecon(current).ok();
    if let Some(ctx) = &current_ctx
        && !ctx.is_empty()
        && ctx != CONTEXT_ROOTFS
        && ctx != "u:object_r:unlabeled:s0"
    {
        return Ok(());
    }

    let system_path = Path::new("/").join(relative);
    if system_path.exists() {
        if let Ok(sys_ctx) = lgetfilecon(&system_path) {
            let target_ctx = if sys_ctx == CONTEXT_ROOTFS {
                CONTEXT_SYSTEM
            } else {
                &sys_ctx
            };
            return lsetfilecon(current, target_ctx);
        }
    } else if let Some(parent) = system_path.parent()
        && parent.exists()
        && let Ok(parent_ctx) = lgetfilecon(parent)
        && parent_ctx != CONTEXT_ROOTFS
    {
        let guessed = guess_context_by_path(&system_path);
        if guessed == CONTEXT_HAL && parent_ctx == CONTEXT_VENDOR {
            return lsetfilecon(current, CONTEXT_HAL);
        }
        return lsetfilecon(current, &parent_ctx);
    }

    let target_context = guess_context_by_path(&system_path);
    lsetfilecon(current, target_context)
}

fn native_cp_r(
    src: &Path,
    dst: &Path,
    relative: &Path,
    repair: bool,
    visited: &mut HashSet<(u64, u64)>,
) -> Result<()> {
    if !dst.exists() {
        if src.is_dir() {
            fs::create_dir_all(dst)?;
        }
        if let Ok(src_meta) = src.metadata() {
            let _ = fs::set_permissions(dst, src_meta.permissions());
        }

        if repair && relative.as_os_str().is_empty() {
            let _ = apply_system_context(dst, relative);
        } else if !repair {
            let _ = copy_extended_attributes(src, dst);
        }
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);
        let next_relative = relative.join(&file_name);

        let metadata = entry.metadata()?;
        let ft = metadata.file_type();
        let dev = metadata.dev();
        let ino = metadata.ino();

        if ft.is_dir() {
            if !visited.insert((dev, ino)) {
                continue;
            }
            native_cp_r(&src_path, &dst_path, &next_relative, repair, visited)?;
        } else if ft.is_symlink() {
            if dst_path.exists() {
                fs::remove_file(&dst_path)?;
            }
            let link_target = fs::read_link(&src_path)?;
            symlink(&link_target, &dst_path)?;
        } else if ft.is_char_device() || ft.is_block_device() || ft.is_fifo() {
            if dst_path.exists() {
                fs::remove_file(&dst_path)?;
            }
            let mode = metadata.permissions().mode();
            let rdev = metadata.rdev();
            make_device_node(&dst_path, mode, rdev)?;
        } else {
            reflink_or_copy(&src_path, &dst_path)?;
        }

        let _ = copy_extended_attributes(&src_path, &dst_path);

        if repair {
            let _ = apply_system_context(&dst_path, &next_relative);
        }
    }
    Ok(())
}

pub fn sync_dir(src: &Path, dst: &Path, repair_context: bool) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    ensure_dir_exists(dst)?;
    let mut visited = HashSet::new();
    native_cp_r(src, dst, Path::new(""), repair_context, &mut visited).with_context(|| {
        format!(
            "Failed to natively sync {} to {}",
            src.display(),
            dst.display()
        )
    })
}

pub fn cleanup_temp_dir(temp_dir: &Path) {
    if let Err(e) = fs::remove_dir_all(temp_dir) {
        let _ = e;
    }
}

pub fn ensure_temp_dir(temp_dir: &Path) -> Result<()> {
    if temp_dir.exists() {
        fs::remove_dir_all(temp_dir).ok();
    }
    fs::create_dir_all(temp_dir)?;
    Ok(())
}

pub fn prune_empty_dirs<P: AsRef<Path>>(root: P) -> Result<()> {
    let root = root.as_ref();
    if !root.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(root)
        .min_depth(1)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
            let path = entry.path();
            if fs::remove_dir(path).is_ok() {}
        }
    }
    Ok(())
}
