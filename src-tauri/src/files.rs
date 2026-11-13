//! Script file I/O and small JSON persistence helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

const MAX_SCRIPT_BYTES: u64 = 20 * 1024 * 1024;

/// Read a script as UTF-8 (a leading BOM is dropped).
pub fn read_script(path: &Path) -> io::Result<String> {
    let meta = fs::metadata(path)?;
    if meta.len() > MAX_SCRIPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file is too large for a script",
        ));
    }
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is not valid UTF-8 text"))?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text))
}

/// Write to a temp file and rename it over the target, so a failed write can't
/// truncate the existing file.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let mut tmp: PathBuf = path.to_path_buf();
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".localcue-tmp");
    tmp.set_file_name(name);
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

pub fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string(value).map_err(io::Error::other)?;
    write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_including_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("script.txt");
        write_atomic(&p, "Привет 😀\nline two").unwrap();
        assert_eq!(read_script(&p).unwrap(), "Привет 😀\nline two");
        // no temp file left behind
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn bom_is_stripped_and_invalid_utf8_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, b"\xef\xbb\xbfhello").unwrap();
        assert_eq!(read_script(&p).unwrap(), "hello");
        fs::write(&p, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert!(read_script(&p).is_err());
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_script(&dir.path().join("nope.txt")).is_err());
    }

    #[test]
    fn json_helpers_create_directories_and_ignore_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/dir/x.json");
        save_json(&p, &vec![1, 2, 3]).unwrap();
        assert_eq!(load_json::<Vec<i32>>(&p), Some(vec![1, 2, 3]));
        fs::write(&p, "{not json").unwrap();
        assert_eq!(load_json::<Vec<i32>>(&p), None);
    }
}
