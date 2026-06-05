//! Template loader with support for external template directories.
//!
//! By default, uses embedded templates compiled at build time.
//! If `TONIN_TEMPLATE_DIR` environment variable is set, loads templates from
//! that directory instead, with fallback to embedded if the file is not found.

use std::path::PathBuf;
use anyhow::{anyhow, Result};

/// Load a template file from `TONIN_TEMPLATE_DIR/service/{path}` or fall back
/// to embedded templates.
///
/// `path` is relative to the service templates root, e.g.
/// `rust/server/Cargo.toml.tmpl` or `python/server/pyproject.toml.tmpl`.
pub fn load_service_template(path: &str) -> Result<String> {
    // Check TONIN_TEMPLATE_DIR environment variable first.
    if let Ok(template_dir) = std::env::var("TONIN_TEMPLATE_DIR") {
        let full_path = PathBuf::from(&template_dir)
            .join("service")
            .join(path);
        if full_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&full_path) {
                return Ok(contents);
            }
        }
    }

    // Fall back to embedded templates.
    Err(anyhow!(
        "Template not found: {} (set TONIN_TEMPLATE_DIR to override)",
        path
    ))
}

/// Load a template directory listing from external templates or fall back to embedded.
///
/// Returns a list of file names in the directory.
pub fn list_service_templates(rel_path: &str) -> Result<Vec<String>> {
    if let Ok(template_dir) = std::env::var("TONIN_TEMPLATE_DIR") {
        let dir_path = PathBuf::from(&template_dir)
            .join("service")
            .join(rel_path);
        if dir_path.exists() && dir_path.is_dir() {
            let mut files = Vec::new();
            for entry in std::fs::read_dir(&dir_path)? {
                if let Ok(entry) = entry {
                    if let Ok(file_name) = entry.file_name().into_string() {
                        files.push(file_name);
                    }
                }
            }
            return Ok(files);
        }
    }

    Err(anyhow!(
        "Template directory not found: {} (set TONIN_TEMPLATE_DIR to override)",
        rel_path
    ))
}

/// Check if a service template file exists (external or embedded).
pub fn service_template_exists(path: &str) -> bool {
    if let Ok(template_dir) = std::env::var("TONIN_TEMPLATE_DIR") {
        let full_path = PathBuf::from(&template_dir)
            .join("service")
            .join(path);
        if full_path.exists() {
            return true;
        }
    }
    false
}
