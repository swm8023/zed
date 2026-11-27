//! Browser paths can refer to a remote Windows host even though WASM has no native file paths.

use crate::Url;
use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};

fn has_drive_root(path: &str) -> bool {
    matches!(path.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
}

pub(super) fn from_file_path(path: &Path, directory: bool) -> Result<Url, ()> {
    let path = path.to_str().ok_or(())?;
    let mut url = Url::parse("file:///").map_err(|_| ())?;

    let windows_path;
    let path = if path.starts_with(r"\\") || has_drive_root(path) {
        windows_path = path.replace('\\', "/");
        let unc = windows_path.strip_prefix("//?/UNC/").or_else(|| {
            windows_path
                .strip_prefix("//")
                .filter(|p| !p.starts_with("?/"))
        });
        if let Some(unc) = unc {
            let (host, path) = unc.split_once('/').ok_or(())?;
            if host.is_empty() || path.is_empty() || host == "." {
                return Err(());
            }
            url.set_host(Some(host)).map_err(|_| ())?;
            path
        } else {
            let path = windows_path.strip_prefix("//?/").unwrap_or(&windows_path);
            if !has_drive_root(path) {
                return Err(());
            }
            path
        }
    } else {
        path.strip_prefix('/').ok_or(())?
    };

    let mut segments = url.path_segments_mut().map_err(|_| ())?;
    segments.pop_if_empty().extend(path.split('/'));
    if directory && !path.ends_with('/') {
        segments.push("");
    }
    drop(segments);
    Ok(url)
}

pub(super) fn to_file_path(url: &Url) -> Result<PathBuf, ()> {
    if url.scheme() != "file" {
        return Err(());
    }
    let path = percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|_| ())?;
    if path.as_bytes().contains(&0) {
        return Err(());
    }
    if let Some(host) = url.host_str() {
        if path == "/" {
            return Err(());
        }
        return Ok(PathBuf::from(std::format!(
            r"\\{host}{}",
            path.replace('/', "\\")
        )));
    }
    if let Some(path) = path.strip_prefix('/').filter(|path| has_drive_root(path)) {
        return Ok(PathBuf::from(path.replace('/', "\\")));
    }
    Ok(PathBuf::from(path.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_round_trip_without_losing_url_metacharacters() {
        let path = Path::new(r"\\?\E:\source files\src\100% #1.rs");
        let url = from_file_path(path, false).unwrap();
        assert_eq!(
            url.as_str(),
            "file:///E:/source%20files/src/100%25%20%231.rs"
        );
        assert_eq!(
            to_file_path(&url).unwrap(),
            PathBuf::from(r"E:\source files\src\100% #1.rs")
        );
    }

    #[test]
    fn supports_remote_shares_and_browser_workspace_directories() {
        let url = from_file_path(Path::new(r"\\?\UNC\files\work\src"), true).unwrap();
        assert_eq!(url.as_str(), "file://files/work/src/");
        assert_eq!(
            to_file_path(&url).unwrap(),
            PathBuf::from(r"\\files\work\src\")
        );
        let url = from_file_path(Path::new("/workspace/a b"), true).unwrap();
        assert_eq!(url.as_str(), "file:///workspace/a%20b/");
        assert!(from_file_path(Path::new("src/main.rs"), false).is_err());
        assert!(to_file_path(&Url::parse("https://example.com/src").unwrap()).is_err());
    }
}
