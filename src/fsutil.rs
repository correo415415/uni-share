//! Filesystem helpers: directory walking, path sanitising, unique names,
//! tar.zst packing and human formatting.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

/// One entry of a transfer manifest (file only; directories are implicit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path using `/` as separator (portable across OSes).
    pub rel_path: String,
    pub size: u64,
    /// Absolute source path (never serialised / sent over the wire).
    #[serde(skip)]
    pub abs_path: PathBuf,
}

/// Collect every regular file under `root`. If `root` is a file, returns a
/// single entry whose `rel_path` is its file name. If it is a directory, the
/// directory name itself is the first path component (so the receiver
/// recreates `folder/…`).
pub fn collect_files(root: &Path) -> Result<Vec<FileEntry>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("path not found: {}", root.display()))?;
    let meta = std::fs::metadata(&root)?;
    if meta.is_file() {
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        return Ok(vec![FileEntry {
            rel_path: name,
            size: meta.len(),
            abs_path: root,
        }]);
    }
    anyhow::ensure!(meta.is_dir(), "unsupported path type: {}", root.display());
    let base_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".into());
    let mut out = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&root)
            .context("strip prefix")?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        out.push(FileEntry {
            rel_path: format!("{base_name}/{rel}"),
            size: entry.metadata()?.len(),
            abs_path: entry.into_path(),
        });
    }
    anyhow::ensure!(!out.is_empty(), "folder is empty: {}", root.display());
    Ok(out)
}

pub fn total_size(files: &[FileEntry]) -> u64 {
    files.iter().map(|f| f.size).sum()
}

/// Sanitise a relative path received from the network: strips `..`, roots,
/// drive letters and characters invalid on Windows. Never escapes `base`.
pub fn safe_join(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    let mut pushed = false;
    for part in rel.split(['/', '\\']) {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        // Reject any component that would be interpreted as root/prefix.
        if Path::new(part)
            .components()
            .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)))
        {
            continue;
        }
        let cleaned: String = part
            .chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
                c if (c as u32) < 0x20 => '_',
                c => c,
            })
            .collect();
        out.push(cleaned);
        pushed = true;
    }
    if !pushed {
        out.push("unnamed_file");
    }
    out
}

/// If `path` exists, return `name (1).ext`, `name (2).ext`… (first free).
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    for i in 1.. {
        let name = match &ext {
            Some(e) if !e.is_empty() => format!("{stem} ({i}).{e}"),
            _ => format!("{stem} ({i})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Destination for an incoming file honouring `--force`.
pub fn destination_path(base: &Path, rel: &str, force: bool) -> PathBuf {
    let p = safe_join(base, rel);
    if force { p } else { unique_path(&p) }
}

/// Human-readable byte count (IEC).
pub fn human_bytes(n: u64) -> String {
    bytesize::ByteSize::b(n).display().iec().to_string()
}

/// Human-readable rate.
pub fn human_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec.max(0.0) as u64))
}

/// Pack a directory into a `.tar.zst` file (blocking; call via spawn_blocking).
pub fn pack_tar_zst(src_dir: &Path, dest: &Path, level: i32) -> Result<u64> {
    let file = std::fs::File::create(dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    let enc = zstd::stream::write::Encoder::new(file, level)?.auto_finish();
    let mut tar = tar::Builder::new(enc);
    tar.follow_symlinks(false);
    let name = src_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".into());
    tar.append_dir_all(&name, src_dir)
        .with_context(|| format!("archiving {}", src_dir.display()))?;
    tar.into_inner()?; // flush tar + zstd
    Ok(std::fs::metadata(dest)?.len())
}

/// Unpack a `.tar.zst` archive into `dest_dir` (blocking).
pub fn unpack_tar_zst(archive: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let dec = zstd::stream::read::Decoder::new(file)?;
    let mut ar = tar::Archive::new(dec);
    ar.set_overwrite(false);
    ar.unpack(dest_dir)
        .with_context(|| format!("unpacking to {}", dest_dir.display()))?;
    Ok(())
}

/// Build a compact tree preview (for the accept prompt), limited to `max_lines`.
pub fn tree_preview(files: &[FileEntry], max_lines: usize) -> String {
    let mut lines = Vec::new();
    for f in files.iter().take(max_lines) {
        lines.push(format!("  {}  ({})", f.rel_path, human_bytes(f.size)));
    }
    if files.len() > max_lines {
        lines.push(format!("  … and {} more files", files.len() - max_lines));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn collect_dir_preserves_hierarchy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/x.txt"), b"hello").unwrap();
        fs::write(root.join("y.bin"), b"12").unwrap();
        let files = collect_files(&root).unwrap();
        let rels: Vec<_> = files.iter().map(|f| f.rel_path.clone()).collect();
        assert_eq!(rels, vec!["proj/a/b/x.txt", "proj/y.bin"]);
        assert_eq!(total_size(&files), 7);
    }

    #[test]
    fn collect_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("one.txt");
        fs::write(&p, b"abc").unwrap();
        let files = collect_files(&p).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel_path, "one.txt");
    }

    #[test]
    fn safe_join_blocks_traversal() {
        let base = Path::new("/tmp/dl");
        assert_eq!(safe_join(base, "../../etc/passwd"), PathBuf::from("/tmp/dl/etc/passwd"));
        assert_eq!(safe_join(base, "/abs/path.txt"), PathBuf::from("/tmp/dl/abs/path.txt"));
        assert_eq!(safe_join(base, "a\\b\\c.txt"), PathBuf::from("/tmp/dl/a/b/c.txt"));
        assert_eq!(safe_join(base, "bad:name?.txt"), PathBuf::from("/tmp/dl/bad_name_.txt"));
        assert_eq!(safe_join(base, "..//"), PathBuf::from("/tmp/dl/unnamed_file"));
    }

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("file.txt");
        assert_eq!(unique_path(&p), p);
        fs::write(&p, b"").unwrap();
        let p1 = unique_path(&p);
        assert_eq!(p1.file_name().unwrap(), "file (1).txt");
        fs::write(&p1, b"").unwrap();
        assert_eq!(unique_path(&p).file_name().unwrap(), "file (2).txt");
        let noext = dir.path().join("README");
        fs::write(&noext, b"").unwrap();
        assert_eq!(unique_path(&noext).file_name().unwrap(), "README (1)");
        assert_eq!(destination_path(dir.path(), "file.txt", true), p);
    }

    #[test]
    fn tar_zst_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("data");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/a.txt"), b"AAAA").unwrap();
        let archive = dir.path().join("data.tar.zst");
        let sz = pack_tar_zst(&src, &archive, 3).unwrap();
        assert!(sz > 0);
        let out = dir.path().join("out");
        unpack_tar_zst(&archive, &out).unwrap();
        assert_eq!(fs::read(out.join("data/sub/a.txt")).unwrap(), b"AAAA");
    }

    #[test]
    fn human() {
        assert_eq!(human_bytes(0), "0 B");
        assert!(human_bytes(1536).starts_with("1.5"));
    }
}
