//! Windows path normalization and UNC / public-path rewriting.

/// Strip `\\?\` / `\\?\UNC\` prefixes and normalize separators to `\`.
pub fn simplify_windows_path(s: &str) -> String {
    let mut out = s.replace('/', "\\");
    if let Some(rest) = out.strip_prefix(r"\\?\UNC\") {
        out = format!(r"\\{rest}");
    } else if let Some(rest) = out.strip_prefix(r"\\?\") {
        out = rest.to_string();
    }
    // Collapse duplicate backslashes except the leading `\\` of a UNC path.
    out = collapse_slashes(&out);
    trim_trailing_slash(&out)
}

fn collapse_slashes(s: &str) -> String {
    if s.starts_with(r"\\") {
        let rest = s[2..].replace(r"\\", r"\");
        format!(r"\\{rest}")
    } else {
        s.replace(r"\\", r"\")
    }
}

fn trim_trailing_slash(s: &str) -> String {
    if s.len() <= 3 {
        // Keep `C:\` or `\\a`
        return s.to_string();
    }
    s.trim_end_matches('\\').to_string()
}

/// Case-insensitive path prefix check (Windows).
pub fn path_starts_with(path: &str, prefix: &str) -> bool {
    let path = simplify_windows_path(path);
    let prefix = simplify_windows_path(prefix);
    if prefix.is_empty() {
        return true;
    }
    if path.len() < prefix.len() {
        return false;
    }
    if !path
        .get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(&prefix))
    {
        return false;
    }
    path.len() == prefix.len()
        || path.as_bytes().get(prefix.len()) == Some(&b'\\')
}

/// Replace `from` prefix with `to`. Returns `path` unchanged when prefix does not match.
pub fn rewrite_prefix(path: &str, from: &str, to: &str) -> String {
    let path = simplify_windows_path(path);
    let from = simplify_windows_path(from);
    let to = simplify_windows_path(to);
    if to.is_empty() || from.is_empty() || from.eq_ignore_ascii_case(&to) {
        return path;
    }
    if !path_starts_with(&path, &from) {
        return path;
    }
    let rest = &path[from.len()..];
    if rest.is_empty() {
        return to;
    }
    if to.ends_with('\\') {
        format!("{to}{}", rest.trim_start_matches('\\'))
    } else {
        format!("{to}{rest}")
    }
}

/// Resolve a mapped drive letter (`Z:`) to its UNC root via `WNetGetConnectionW`.
#[cfg(windows)]
pub fn mapped_drive_to_unc(path: &str) -> Option<String> {
    let simplified = simplify_windows_path(path);
    let drive = drive_letter(&simplified)?;
    let local_name: Vec<u16> = format!("{drive}:")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Foundation::ERROR_SUCCESS;
        use windows::Win32::NetworkManagement::WNet::WNetGetConnectionW;

        let mut buf = vec![0u16; 512];
        let mut len = buf.len() as u32;
        let status = WNetGetConnectionW(
            PCWSTR(local_name.as_ptr()),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        );
        if status != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or((len as usize).min(buf.len()));
        let unc = simplify_windows_path(&String::from_utf16_lossy(&buf[..end]));
        if unc.starts_with(r"\\") {
            Some(unc)
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
pub fn mapped_drive_to_unc(_path: &str) -> Option<String> {
    None
}

fn drive_letter(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let c = path.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase());
        }
    }
    None
}

/// If `folder_path` is under a mapped drive, return the UNC equivalent of that folder root.
/// Already-UNC paths return `None` (no separate public path needed).
pub fn suggest_public_path(folder_path: &str) -> Option<String> {
    let simplified = simplify_windows_path(folder_path);
    if simplified.starts_with(r"\\") {
        return None;
    }
    let drive = drive_letter(&simplified)?;
    let unc_root = mapped_drive_to_unc(&format!("{drive}:\\"))?;
    Some(rewrite_prefix(
        &simplified,
        &format!("{drive}:"),
        &unc_root,
    ))
}

/// Normalize a filesystem path for comparison / watcher matching.
pub fn normalize_for_compare(path: &str) -> String {
    simplify_windows_path(path)
}

/// Path stored in the index: rewrite from folder root to public root when set.
pub fn to_indexed_path(fs_path: &str, folder_path: &str, public_path: &str) -> String {
    let fs = simplify_windows_path(fs_path);
    let folder = simplify_windows_path(folder_path);
    let public = simplify_windows_path(public_path);
    if public.is_empty() || public.eq_ignore_ascii_case(&folder) {
        return fs;
    }
    rewrite_prefix(&fs, &folder, &public)
}

/// Effective public root for a folder (`public_path` if set, otherwise `path`).
pub fn effective_public_root(folder_path: &str, public_path: &str) -> String {
    let public = simplify_windows_path(public_path);
    if public.is_empty() {
        simplify_windows_path(folder_path)
    } else {
        public
    }
}

/// Path suitable for opening in Explorer (prefer non-extended form; keep UNC as-is).
pub fn path_for_explorer(path: &str) -> String {
    let simplified = simplify_windows_path(path);
    if simplified.starts_with(r"\\") {
        // canonicalize turns UNC into \\?\UNC\...; Explorer / Shell work better without that.
        return simplified;
    }
    let p = std::path::Path::new(path);
    match std::fs::canonicalize(p) {
        Ok(full) => simplify_windows_path(&full.to_string_lossy()),
        Err(_) => simplified,
    }
}

/// Open Explorer with `path` selected. Uses SHOpenFolderAndSelectItems (reliable for UNC).
/// `explorer /select` often fails on UNC and opens Documents instead.
#[cfg(windows)]
pub fn open_folder_and_select(path: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};

    let cleaned = path_for_explorer(path);
    let wide: Vec<u16> = std::ffi::OsStr::new(&cleaned)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // May return RPC_E_CHANGED_MODE if already initialized differently; still usable.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let mut pidl = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)
            .map_err(|e| format!("パスを解決できません（{e}）: {cleaned}"))?;
        if pidl.is_null() {
            return Err(format!("パスを解決できません: {cleaned}"));
        }

        // cidl == 0: pidl is the item to select; opens its parent folder.
        let result = SHOpenFolderAndSelectItems(pidl, None, 0);
        CoTaskMemFree(Some(pidl as *const _));
        result.map_err(|e| format!("Explorer を開けません（{e}）: {cleaned}"))?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn open_folder_and_select(_path: &str) -> Result<(), String> {
    Err("Windows 以外では未対応".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplify_strips_extended_unc() {
        assert_eq!(
            simplify_windows_path(r"\\?\UNC\server\share\a.pdf"),
            r"\\server\share\a.pdf"
        );
        assert_eq!(
            simplify_windows_path(r"\\?\C:\foo\bar.pdf"),
            r"C:\foo\bar.pdf"
        );
    }

    #[test]
    fn rewrite_prefix_unc() {
        assert_eq!(
            rewrite_prefix(r"C:\Published\a.pdf", r"C:\Published", r"\\Host\Published"),
            r"\\Host\Published\a.pdf"
        );
    }

    #[test]
    fn path_starts_with_boundary() {
        assert!(path_starts_with(r"C:\Data\file", r"C:\Data"));
        assert!(!path_starts_with(r"C:\Data2\file", r"C:\Data"));
    }
}
