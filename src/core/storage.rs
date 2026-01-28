use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail, ensure};
use jwalk::WalkDir;
use rustix::{
    fs::Mode,
    mount::{MountPropagationFlags, UnmountFlags, mount_change, unmount as umount},
};
use serde::Serialize;

#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::try_umount::send_umountable;
use crate::{
    core::state::RuntimeState,
    defs,
    mount::overlayfs::utils as overlay_utils,
    utils::{self, ensure_dir_exists, lsetfilecon},
};

const DEFAULT_SELINUX_CONTEXT: &str = "u:object_r:system_file:s0";

pub struct StorageHandle {
    pub mount_point: PathBuf,
    pub mode: String,
    pub backing_image: Option<PathBuf>,
}

impl StorageHandle {
    pub fn commit(&mut self, disable_umount: bool) -> Result<()> {
        if self.mode == "erofs_staging" {
            let image_path = self
                .backing_image
                .as_ref()
                .context("EROFS backing image path missing")?;

            create_erofs_image(&self.mount_point, image_path)
                .context("Failed to pack EROFS image")?;

            umount(&self.mount_point, UnmountFlags::DETACH)
                .context("Failed to unmount staging tmpfs")?;

            mount_erofs_image(image_path, &self.mount_point)
                .context("Failed to mount finalized EROFS image")?;

            if let Err(e) = mount_change(&self.mount_point, MountPropagationFlags::PRIVATE) {
                log::warn!("Failed to make EROFS storage private: {}", e);
            }

            #[cfg(any(target_os = "linux", target_os = "android"))]
            if !disable_umount {
                let _ = send_umountable(&self.mount_point);
            }

            self.mode = "erofs".to_string();
        }

        Ok(())
    }
}

#[derive(Serialize)]
struct StorageStatus {
    #[serde(rename = "type")]
    mode: String,
    mount_point: String,
    usage_percent: u8,
    total_size: u64,
    used_size: u64,
    supported_modes: Vec<String>,
}

pub fn get_usage(path: &Path) -> (u64, u64, u8) {
    if let Ok(stat) = rustix::fs::statvfs(path) {
        let total = stat.f_blocks * stat.f_frsize;

        let free = stat.f_bfree * stat.f_frsize;

        let used = total - free;

        let percent = (used * 100).checked_div(total).unwrap_or(0) as u8;

        (total, used, percent)
    } else {
        (0, 0, 0)
    }
}

fn calculate_total_size(path: &Path) -> Result<u64> {
    let mut total_size = 0;
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_file() {
                total_size += entry.metadata()?.len();
            } else if file_type.is_dir() {
                total_size += calculate_total_size(&entry.path())?;
            }
        }
    }
    Ok(total_size)
}

fn check_image<P>(img: P) -> Result<()>
where
    P: AsRef<Path>,
{
    let path = img.as_ref();
    let path_str = path.to_str().context("Invalid path string")?;
    let result = Command::new("e2fsck")
        .args(["-yf", path_str])
        .status()
        .with_context(|| format!("Failed to exec e2fsck {}", path.display()))?;
    let code = result.code();

    log::info!("e2fsck exit code: {}", code.unwrap_or(-1));
    Ok(())
}

pub fn setup(
    mnt_base: &Path,
    img_path: &Path,
    moduledir: &Path,
    force_ext4: bool,
    use_erofs: bool,
    mount_source: &str,
    disable_umount: bool,
) -> Result<StorageHandle> {
    if utils::is_mounted(mnt_base) {
        let _ = umount(mnt_base, UnmountFlags::DETACH);
    }

    let try_hide = |path: &Path| {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if !disable_umount {
            let _ = send_umountable(path);
        }

        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let _ = path;
    };

    let make_private = |path: &Path| {
        if let Err(e) = mount_change(path, MountPropagationFlags::PRIVATE) {
            log::warn!("Failed to make storage private: {}", e);
        }
    };

    if use_erofs && is_erofs_supported() {
        let erofs_path = img_path.with_extension("erofs");

        utils::mount_tmpfs(mnt_base, mount_source)?;

        make_private(mnt_base);

        try_hide(mnt_base);

        return Ok(StorageHandle {
            mount_point: mnt_base.to_path_buf(),
            mode: "erofs_staging".to_string(),
            backing_image: Some(erofs_path),
        });
    }

    if !force_ext4 && try_setup_tmpfs(mnt_base, mount_source)? {
        make_private(mnt_base);

        try_hide(mnt_base);

        let erofs_path = img_path.with_extension("erofs");

        if erofs_path.exists() {
            let _ = fs::remove_file(erofs_path);
        }

        return Ok(StorageHandle {
            mount_point: mnt_base.to_path_buf(),
            mode: "tmpfs".to_string(),
            backing_image: None,
        });
    }

    let handle = setup_ext4_image(mnt_base, img_path, moduledir)?;

    make_private(mnt_base);

    try_hide(mnt_base);

    Ok(handle)
}

fn try_setup_tmpfs(target: &Path, mount_source: &str) -> Result<bool> {
    if utils::mount_tmpfs(target, mount_source).is_ok() {
        if utils::is_overlay_xattr_supported().unwrap_or(false) {
            log::info!("Tmpfs mounted and supports xattrs (CONFIG_TMPFS_XATTR=y).");
            return Ok(true);
        } else {
            let _ = umount(target, UnmountFlags::DETACH);
        }
    }

    Ok(false)
}

fn setup_ext4_image(target: &Path, img_path: &Path, moduledir: &Path) -> Result<StorageHandle> {
    if img_path.exists() {
        if let Err(e) = fs::remove_file(img_path) {
            log::warn!("Failed to remove old image: {}", e);
        }
    }

    let total_size = calculate_total_size(moduledir)?;
    let min_size = 64 * 1024 * 1024;
    let grow_size = std::cmp::max((total_size as f64 * 1.2) as u64, min_size);

    fs::File::create(img_path)
        .context("Failed to create ext4 image file")?
        .set_len(grow_size)
        .context("Failed to extend ext4 image")?;

    let result = Command::new("mkfs.ext4")
        .arg("-b")
        .arg("1024")
        .arg(img_path)
        .stdout(std::process::Stdio::piped())
        .output()?;

    ensure!(
        result.status.success(),
        "Failed to format ext4 image: {}",
        String::from_utf8(result.stderr)?
    );

    check_image(img_path)?;

    utils::lsetfilecon(img_path, "u:object_r:ksu_file:s0").ok();

    ensure_dir_exists(target)?;
    if overlay_utils::AutoMountExt4::try_new(img_path, target, false).is_err() {
        if utils::repair_image(img_path).is_ok() {
            overlay_utils::AutoMountExt4::try_new(img_path, target, false)
                .context("Failed to mount modules.img after repair")
                .map(|_| ())?;
        } else {
            bail!("Failed to repair modules.img");
        }
    }

    for dir_entry in WalkDir::new(target).parallelism(jwalk::Parallelism::Serial) {
        if let Some(path) = dir_entry.ok().map(|dir_entry| dir_entry.path()) {
            let _ = utils::lsetfilecon(&path, DEFAULT_SELINUX_CONTEXT);
        }
    }

    Ok(StorageHandle {
        mount_point: target.to_path_buf(),
        mode: "ext4".to_string(),
        backing_image: Some(img_path.to_path_buf()),
    })
}

pub fn print_status() -> Result<()> {
    let state = RuntimeState::load().ok();
    let fallback_mnt = crate::conf::config::Config::load_default()
        .map(|c| c.hybrid_mnt_dir)
        .unwrap_or_else(|_| defs::DEFAULT_HYBRID_MNT_DIR.to_string());
    let (mnt_base, expected_mode) = if let Some(ref s) = state {
        (s.mount_point.clone(), s.storage_mode.clone())
    } else {
        (PathBuf::from(fallback_mnt), "unknown".to_string())
    };

    let mut mode = "unknown".to_string();

    let mut total = 0;

    let mut used = 0;

    let mut percent = 0;

    if utils::is_mounted(&mnt_base)
        && let Ok(stat) = rustix::fs::statvfs(&mnt_base)
    {
        mode = if expected_mode != "unknown" {
            expected_mode
        } else {
            "active".to_string()
        };

        total = stat.f_blocks * stat.f_frsize;

        let free = stat.f_bfree * stat.f_frsize;

        used = total - free;

        percent = (used * 100).checked_div(total).unwrap_or(0) as u8;
    }

    let mut supported_modes = vec!["ext4".to_string(), "erofs".to_string()];
    let check_dir = Path::new(defs::XATTR_CHECK_DIR);
    if utils::mount_tmpfs(check_dir, "mh_check").is_ok() {
        if utils::is_overlay_xattr_supported().unwrap_or(false) {
            supported_modes.insert(0, "tmpfs".to_string());
        }
        let _ = umount(check_dir, UnmountFlags::DETACH);
        let _ = fs::remove_dir(check_dir);
    }

    let status = StorageStatus {
        mode,
        mount_point: mnt_base.to_string_lossy().to_string(),
        usage_percent: percent,
        total_size: total,
        used_size: used,
        supported_modes,
    };

    println!("{}", serde_json::to_string(&status)?);

    Ok(())
}

fn is_erofs_supported() -> bool {
    fs::read_to_string("/proc/filesystems")
        .map(|content| content.contains("erofs"))
        .unwrap_or(false)
}

fn create_erofs_image(src_dir: &Path, image_path: &Path) -> Result<()> {
    let mkfs_bin = Path::new(defs::MKFS_EROFS_PATH);
    let cmd_name = if mkfs_bin.exists() {
        mkfs_bin.as_os_str()
    } else {
        std::ffi::OsStr::new("mkfs.erofs")
    };

    if image_path.exists() {
        let _ = fs::remove_file(image_path);
    }

    let output = Command::new(cmd_name)
        .arg("-z")
        .arg("lz4hc")
        .arg("-x")
        .arg("256")
        .arg(image_path)
        .arg(src_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to execute mkfs.erofs")?;

    if !output.status.success() {
        bail!("Failed to create EROFS image");
    }

    let _ = fs::set_permissions(image_path, fs::Permissions::from_mode(0o644));
    lsetfilecon(image_path, "u:object_r:ksu_file:s0")?;
    Ok(())
}

fn mount_erofs_image(image_path: &Path, target: &Path) -> Result<()> {
    ensure_dir_exists(target)?;
    lsetfilecon(image_path, "u:object_r:ksu_file:s0").ok();
    let status = Command::new("mount")
        .args(["-t", "erofs", "-o", "loop,ro,nodev,noatime"])
        .arg(image_path)
        .arg(target)
        .status()
        .context("Failed to execute mount command for EROFS")?;

    if !status.success() {
        bail!("EROFS Mount command failed");
    }
    Ok(())
}
