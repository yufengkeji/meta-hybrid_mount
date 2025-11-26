use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;
use crate::magic_mount;
use crate::overlay_mount;
use crate::utils;

const BUILTIN_PARTITIONS: &[&str] = &[
    "system", "vendor", "product", "system_ext", "odm", "oem",
];

/// Core mounting logic: Partition grouping -> OverlayFS -> Magic Mount fallback.
pub fn run(active_modules: HashMap<String, PathBuf>, config: &Config) -> Result<()> {
    // 1. Load Modes
    let module_modes = crate::config::load_module_modes();

    // 2. Group by Partition
    let mut partition_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut magic_force_map: HashMap<String, bool> = HashMap::new();

    let mut all_partitions = BUILTIN_PARTITIONS.to_vec();
    let extra_parts: Vec<&str> = config.partitions.iter().map(|s| s.as_str()).collect();
    all_partitions.extend(extra_parts);

    for (module_id, content_path) in &active_modules {
        if !content_path.exists() {
            log::debug!("Module {} content missing at {}", module_id, content_path.display());
            continue;
        }

        let mode = module_modes.get(module_id).map(|s| s.as_str()).unwrap_or("auto");
        let is_magic = mode == "magic";

        for &part in &all_partitions {
            let part_dir = content_path.join(part);
            if part_dir.is_dir() {
                partition_map
                    .entry(part.to_string())
                    .or_default()
                    .push(content_path.clone());

                if is_magic {
                    magic_force_map.insert(part.to_string(), true);
                    log::info!("Partition /{} forced to Magic Mount by module '{}'", part, module_id);
                }
            }
        }
    }

    // 3. Execute Mounts
    let tempdir = if let Some(t) = &config.tempdir {
        t.clone()
    } else {
        utils::select_temp_dir()?
    };
    
    let mut magic_modules: HashSet<PathBuf> = HashSet::new();

    // Phase 1: OverlayFS
    for (part, modules) in &partition_map {
        let use_magic = *magic_force_map.get(part).unwrap_or(&false);
        if !use_magic {
            let target_path = format!("/{}", part);
            let overlay_paths: Vec<String> = modules
                .iter()
                .map(|m| m.join(part).display().to_string())
                .collect();

            log::info!("Mounting {} [OVERLAY] ({} layers)", target_path, overlay_paths.len());
            
            if let Err(e) = overlay_mount::mount_overlay(&target_path, &overlay_paths, None, None) {
                log::error!(
                    "OverlayFS mount failed for {}: {:#}, falling back to Magic Mount",
                    target_path, e
                );
                magic_force_map.insert(part.to_string(), true);
            }
        }
    }

    // Phase 2: Magic Mount
    let mut magic_partitions = Vec::new();
    for (part, _) in &partition_map {
        if *magic_force_map.get(part).unwrap_or(&false) {
            magic_partitions.push(part.clone());
            if let Some(mods) = partition_map.get(part) {
                for m in mods {
                    magic_modules.insert(m.clone());
                }
            }
        }
    }

    if !magic_modules.is_empty() {
        log::info!("Starting Magic Mount Engine for partitions: {:?}", magic_partitions);
        utils::ensure_temp_dir(&tempdir)?;

        let module_list: Vec<PathBuf> = magic_modules.into_iter().collect();

        if let Err(e) = magic_mount::mount_partitions(
            &tempdir,
            &module_list,
            &config.mountsource,
            &config.partitions,
        ) {
            log::error!("Magic Mount failed: {:#}", e);
        }

        utils::cleanup_temp_dir(&tempdir);
    }

    log::info!("Hybrid Mount Completed");
    Ok(())
}
