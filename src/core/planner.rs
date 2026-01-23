// Copyright 2025 Meta-Hybrid Mount Authors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use walkdir::WalkDir;

use crate::{
    conf::config,
    core::inventory::{Module, MountMode},
    defs, utils,
};

#[derive(Debug, Clone)]
pub struct OverlayOperation {
    pub partition_name: String,
    pub target: String,
    pub lowerdirs: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct MountPlan {
    pub overlay_ops: Vec<OverlayOperation>,
    pub overlay_module_ids: Vec<String>,
    pub magic_module_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictEntry {
    pub partition: String,
    pub relative_path: String,
    pub contending_modules: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum DiagnosticLevel {
    #[allow(dead_code)]
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticIssue {
    pub level: DiagnosticLevel,
    pub context: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct AnalysisReport {
    pub conflicts: Vec<ConflictEntry>,
    pub diagnostics: Vec<DiagnosticIssue>,
}

impl MountPlan {
    pub fn analyze(&self) -> AnalysisReport {
        let results: Vec<(Vec<ConflictEntry>, Vec<DiagnosticIssue>)> = self
            .overlay_ops
            .par_iter()
            .map(|op| {
                let mut local_conflicts = Vec::new();
                let mut local_diagnostics = Vec::new();
                let mut file_map: HashMap<String, Vec<String>> = HashMap::new();

                if !Path::new(&op.target).exists() {
                    local_diagnostics.push(DiagnosticIssue {
                        level: DiagnosticLevel::Critical,
                        context: op.partition_name.clone(),
                        message: format!("Target mount point does not exist: {}", op.target),
                    });
                }

                for layer_path in &op.lowerdirs {
                    if !layer_path.exists() {
                        continue;
                    }

                    let module_id =
                        utils::extract_module_id(layer_path).unwrap_or_else(|| "UNKNOWN".into());

                    for entry in WalkDir::new(layer_path).min_depth(1).into_iter().flatten() {
                        if entry.path_is_symlink()
                            && let Ok(target) = std::fs::read_link(entry.path())
                            && target.is_absolute()
                            && !target.exists()
                        {
                            local_diagnostics.push(DiagnosticIssue {
                                level: DiagnosticLevel::Warning,
                                context: module_id.clone(),
                                message: format!(
                                    "Dead absolute symlink: {} -> {}",
                                    entry.path().display(),
                                    target.display()
                                ),
                            });
                        }

                        if !entry.file_type().is_file() {
                            continue;
                        }

                        if let Ok(rel) = entry.path().strip_prefix(layer_path) {
                            let rel_str = rel.to_string_lossy().to_string();
                            file_map.entry(rel_str).or_default().push(module_id.clone());
                        }
                    }
                }

                for (rel_path, modules) in file_map {
                    if modules.len() > 1 {
                        local_conflicts.push(ConflictEntry {
                            partition: op.partition_name.clone(),
                            relative_path: rel_path,
                            contending_modules: modules,
                        });
                    }
                }

                (local_conflicts, local_diagnostics)
            })
            .collect();

        let mut report = AnalysisReport::default();
        for (c, d) in results {
            report.conflicts.extend(c);
            report.diagnostics.extend(d);
        }

        report.conflicts.sort_by(|a, b| {
            a.partition
                .cmp(&b.partition)
                .then_with(|| a.relative_path.cmp(&b.relative_path))
        });

        report
    }
}

struct ModuleContribution {
    id: String,
    overlays: Vec<(String, PathBuf)>,
    magic: bool,
}

pub fn generate(
    config: &config::Config,
    modules: &[Module],
    storage_root: &Path,
) -> Result<MountPlan> {
    let mut plan = MountPlan::default();

    let mut target_partitions = defs::BUILTIN_PARTITIONS.to_vec();

    target_partitions.extend(config.partitions.iter().map(|s| s.as_str()));

    let contributions: Vec<Option<ModuleContribution>> = modules
        .par_iter()
        .map(|module| {
            let mut content_path = storage_root.join(&module.id);

            if !content_path.exists() {
                content_path = module.source_path.clone();
            }

            if !content_path.exists() {
                return None;
            }

            let mut contrib = ModuleContribution {
                id: module.id.clone(),
                overlays: Vec::new(),
                magic: false,
            };

            let mut has_any_action = false;

            if let Ok(entries) = fs::read_dir(&content_path) {
                for entry in entries.flatten() {
                    let path = entry.path();

                    if !path.is_dir() {
                        continue;
                    }

                    let dir_name = entry.file_name().to_string_lossy().to_string();

                    if !target_partitions.contains(&dir_name.as_str()) {
                        continue;
                    }

                    if !has_files(&path) {
                        continue;
                    }

                    let mode = module.rules.get_mode(&dir_name);

                    match mode {
                        MountMode::Overlay => {
                            contrib.overlays.push((dir_name, path));

                            has_any_action = true;
                        }
                        MountMode::Magic => {
                            contrib.magic = true;

                            has_any_action = true;
                        }
                        MountMode::Ignore => {
                            log::debug!("Ignoring {}/{} per rule", module.id, dir_name);
                        }
                    }
                }
            }

            if has_any_action { Some(contrib) } else { None }
        })
        .collect();

    let mut overlay_groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut overlay_ids = HashSet::new();
    let mut magic_ids = HashSet::new();

    for contrib in contributions.into_iter().flatten() {
        if contrib.magic {
            magic_ids.insert(contrib.id.clone());
        }

        for (part, path) in contrib.overlays {
            overlay_groups.entry(part).or_default().push(path);

            overlay_ids.insert(contrib.id.clone());
        }
    }

    for (part, layers) in overlay_groups.clone() {
        if part.trim() == "vendor" {
            log::debug!("Modification of the /vendor operation is temporarily disabled!");
            continue;
        }

        let initial_target_path = format!("/{}", part);

        let target_path_obj = Path::new(&initial_target_path);

        if target_path_obj.read_link().is_ok() {
            log::warn!(
                "Skipping overlay on symlink partition: {}",
                initial_target_path
            );

            continue;
        }

        let resolved_target = if target_path_obj.exists() {
            match target_path_obj.canonicalize() {
                Ok(p) => p,
                Err(_) => continue,
            }
        } else {
            continue;
        };

        if !resolved_target.is_dir() {
            continue;
        }

        plan.overlay_ops.push(OverlayOperation {
            partition_name: part,
            target: resolved_target.to_string_lossy().to_string(),
            lowerdirs: layers,
        });
    }

    plan.overlay_module_ids = overlay_ids.into_iter().collect();

    plan.magic_module_ids = magic_ids.into_iter().collect();

    plan.overlay_module_ids.sort();

    plan.magic_module_ids.sort();

    Ok(plan)
}

fn has_files(path: &Path) -> bool {
    if let Ok(entries) = fs::read_dir(path)
        && entries.flatten().next().is_some()
    {
        return true;
    }

    false
}
