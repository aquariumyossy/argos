//! Bundled LICENSE, NOTICE, and THIRD_PARTY_LICENSES.
//!
//! The release script copies the same files into the zip. Tauri bundle
//! resources put them inside the NSIS installer. This command shows whichever
//! copy is on disk, then the text compiled into the binary.

use std::fs;
use std::path::{Path, PathBuf};

use tauri::Manager;

pub const NOTICE_NAMES: &[&str] = &["LICENSE", "NOTICE", "THIRD_PARTY_LICENSES"];

pub fn repo_notice_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

pub fn canonical_notice_name(name: &str) -> Result<&'static str, String> {
    let trimmed = name.trim();
    NOTICE_NAMES
        .iter()
        .copied()
        .find(|item| *item == trimmed)
        .ok_or_else(|| {
            "表示できるのは LICENSE、NOTICE、THIRD_PARTY_LICENSES だけです。".to_string()
        })
}

/// First non-empty file wins. The compiled copy is the fallback.
pub fn read_notice_from_dirs(name: &str, dirs: &[PathBuf]) -> Result<String, String> {
    let name = canonical_notice_name(name)?;
    for dir in dirs {
        let path = dir.join(name);
        if let Ok(text) = fs::read_to_string(&path) {
            if text.chars().any(|ch| !ch.is_whitespace()) {
                return Ok(text);
            }
        }
    }
    embedded_notice(name)
        .map(str::to_string)
        .ok_or_else(|| format!("{name} が見つかりません。"))
}

fn embedded_notice(name: &str) -> Option<&'static str> {
    match name {
        "LICENSE" => Some(include_str!("../../LICENSE")),
        "NOTICE" => Some(include_str!("../../NOTICE")),
        "THIRD_PARTY_LICENSES" => Some(include_str!("../../THIRD_PARTY_LICENSES")),
        _ => None,
    }
}

#[tauri::command]
pub fn read_bundled_notice(app: tauri::AppHandle, name: String) -> Result<String, String> {
    let mut dirs = Vec::new();
    if let Ok(resource) = app.path().resource_dir() {
        dirs.push(resource.clone());
        dirs.push(resource.join("resources").join("legal"));
        dirs.push(resource.join("legal"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
            dirs.push(parent.join("resources").join("legal"));
        }
    }
    dirs.push(repo_notice_dir());
    read_notice_from_dirs(&name, &dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_release_script_ships_license_files_in_the_installer_and_the_zip() {
        let root = repo_notice_dir();
        let script = fs::read_to_string(root.join("scripts/package-release.ps1")).unwrap();
        assert!(
            script.contains("$LicenseNames = @(\"LICENSE\", \"NOTICE\", \"THIRD_PARTY_LICENSES\")"),
            "{script}"
        );
        assert!(
            script.contains("Join-Path $OutDir $LicenseName"),
            "{script}"
        );
        assert!(
            script.contains("Join-Path $PortableDir $LicenseName"),
            "{script}"
        );
        assert!(
            script.contains("bundle resources must include"),
            "the installer path is the tauri resource list the script checks"
        );
        assert!(
            script.contains("generate_third_party_licenses.py"),
            "{script}"
        );
        let conf = fs::read_to_string(root.join("src-tauri/tauri.conf.json")).unwrap();
        for name in NOTICE_NAMES {
            assert!(conf.contains(name), "{name} missing from tauri.conf.json");
        }
    }

    #[test]
    fn notice_contains_the_required_third_party_attributions() {
        let text = read_notice_from_dirs("NOTICE", &[repo_notice_dir()]).unwrap();
        assert!(text.contains("IPADIC"), "{text}");
        assert!(text.contains("lindera-ipadic"));
        assert!(text.contains("NO WARRANTY"));
        assert!(text.contains("without any warranty whatsoever"));
        assert!(text.contains("Nara Institute of Science"));
        assert!(text.contains("UNICODE LICENSE V3"));
        assert!(text.contains("japanese_han_map.rs"));
        assert!(text.contains("Unihan"));
        assert!(text.contains("18.0"));
        assert!(text.contains("公共データ利用規約"));
        assert!(text.contains("PDL1.0"));
        assert!(text.contains("e-Gov"));
        assert!(text.contains("statutes.db.gz"));
        assert!(text.contains("加工"));
        assert!(text.contains("MPL-2.0"));
        assert!(text.contains("unmodified"), "{text}");
        for name in [
            "cssparser",
            "cssparser-macros",
            "selectors",
            "dtoa-short",
            "option-ext",
        ] {
            assert!(text.contains(name), "{name}");
        }
        assert!(text.contains("https://github.com/servo/rust-cssparser"));
        assert!(text.contains("https://github.com/servo/stylo"));
        assert!(text.contains("https://github.com/upsuper/dtoa-short"));
        assert!(text.contains("https://github.com/soc/option-ext"));
    }

    #[test]
    fn third_party_licenses_points_at_the_generator_and_mpl_sources() {
        let text = read_notice_from_dirs("THIRD_PARTY_LICENSES", &[repo_notice_dir()]).unwrap();
        assert!(text.contains("generate_third_party_licenses.py"), "{text}");
        assert!(text.contains("unmodified"));
        assert!(text.contains("MPL-2.0"));
        assert!(text.contains("cssparser"));
        assert!(text.contains("https://github.com/servo/rust-cssparser"));
    }

    #[test]
    fn bundled_notice_reader_rejects_other_names() {
        let err = read_notice_from_dirs("../NOTICE", &[repo_notice_dir()]).unwrap_err();
        assert!(err.contains("LICENSE"), "{err}");
        let err = read_notice_from_dirs("NOTICE.md", &[repo_notice_dir()]).unwrap_err();
        assert!(err.contains("THIRD_PARTY_LICENSES"), "{err}");
    }
}
