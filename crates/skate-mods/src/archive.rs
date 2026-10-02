//! Bounded ZIP extraction. A session cache never overwrites another package.
use std::{collections::{BTreeMap, BTreeSet}, io::{Read, Write}, path::{Path, PathBuf}};
const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct Cache {
    session: Option<PathBuf>,
    entries: BTreeMap<PathBuf, (blake3::Hash, PathBuf)>,
    /// Cheap identity stamp (mtime, size) per archive, so an unchanged archive
    /// skips the read-and-hash that would otherwise run on every scan.
    stamps: BTreeMap<PathBuf, (Option<std::time::SystemTime>, u64)>,
    next: u64,
}

fn safe_name(name: &str) -> Result<PathBuf, String> {
    let name = name.strip_suffix('/').unwrap_or(name);
    if name.is_empty() || name.len() > 240 || name.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
        || name.chars().any(char::is_control) {
        return Err("Invalid ZIP path".into());
    }
    for part in name.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        if part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' '])
            || matches!(stem.as_str(), "con" | "prn" | "aux" | "nul" | "com1" | "com2" | "com3" | "com4" | "com5" | "com6" | "com7" | "com8" | "com9" | "lpt1" | "lpt2" | "lpt3" | "lpt4" | "lpt5" | "lpt6" | "lpt7" | "lpt8" | "lpt9") {
            return Err("ZIP paths must be ordinary relative paths without traversal or device names".into());
        }
    }
    Ok(PathBuf::from(name))
}

impl Cache {
    pub fn materialize(&mut self, root: &Path, archive: &Path) -> Result<PathBuf, String> {
        // Steady state: the archive is untouched, so its bytes cannot differ.
        // Identifying it by mtime and size keeps the 500ms rescan off the
        // multi-megabyte read plus blake3 hash it used to pay unconditionally.
        let meta = std::fs::metadata(archive).map_err(|e| e.to_string())?;
        let stamp = (meta.modified().ok(), meta.len());
        if self.stamps.get(archive) == Some(&stamp) {
            if let Some((_, path)) = self.entries.get(archive) {
                return Ok(path.clone());
            }
        }
        let mut bytes = Vec::new();
        std::fs::File::open(archive).map_err(|e| e.to_string())?.take(LIMIT + 1)
            .read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT { return Err("ZIP exceeds 64 MiB compressed".into()); }
        let hash = blake3::hash(&bytes);
        if let Some((old, path)) = self.entries.get(archive) {
            if old == &hash { return Ok(path.clone()); }
        }
        // Validate and decompress into bounded memory before publishing anything.
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).map_err(|e| e.to_string())?;
        if zip.len() > 512 { return Err("ZIP exceeds 512 entries".into()); }
        let mut names = BTreeSet::new();
        let mut files = Vec::new();
        let mut total = 0_u64;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
            let path = safe_name(file.name())?;
            if !names.insert(path.to_string_lossy().to_lowercase()) { return Err("Duplicate/case-colliding ZIP path".into()); }
            let kind = file.unix_mode().unwrap_or(0) & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 { return Err("ZIP links and special files are unsupported".into()); }
            if file.is_dir() { continue; }
            if file.size() > LIMIT - total { return Err("ZIP exceeds 64 MiB expanded".into()); }
            let mut contents = Vec::new();
            file.by_ref().take(LIMIT - total + 1).read_to_end(&mut contents).map_err(|e| e.to_string())?;
            total += contents.len() as u64;
            if total > LIMIT { return Err("ZIP exceeds 64 MiB expanded".into()); }
            files.push((path, contents));
        }
        if !files.iter().any(|(p, _)| p == Path::new("mod.json")) {
            return Err("ZIP must contain mod.json at its root, not inside a wrapper folder".into());
        }
        if self.session.is_none() {
            let root = root.canonicalize().map_err(|e| e.to_string())?;
            let cache = root.join(".cache");
            if cache.exists() && std::fs::symlink_metadata(&cache).map_err(|e|e.to_string())?.file_type().is_symlink() {
                return Err("ZIP cache must not be a link".into());
            }
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
            if !cache.canonicalize().map_err(|e|e.to_string())?.starts_with(&root) { return Err("ZIP cache escapes mods folder".into()); }
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos();
            let session = cache.join(format!("{}-{nonce}", std::process::id()));
            std::fs::create_dir(&session).map_err(|e| e.to_string())?;
            self.session = Some(session);
        }
        self.next += 1;
        let destination = self.session.as_ref().unwrap().join(self.next.to_string());
        std::fs::create_dir(&destination).map_err(|e| e.to_string())?;
        for (path, contents) in files {
            let target = destination.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::OpenOptions::new().write(true).create_new(true).open(target)
                .and_then(|mut f| f.write_all(&contents)).map_err(|e| e.to_string())?;
        }
        self.entries.insert(archive.to_owned(), (hash, destination.clone()));
        self.stamps.insert(archive.to_owned(), stamp);
        Ok(destination)
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        if let Some(path) = &self.session {
            // Only this process's uniquely created session, and never a redirected path.
            if let (Ok(actual), Ok(parent)) = (path.canonicalize(), path.parent().unwrap().canonicalize()) {
                if actual.parent() == Some(parent.as_path()) && actual.file_name() == path.file_name() {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        w.start_file("mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(br#"{"id":"example"}"#).unwrap();
        w.start_file("payload.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(payload).unwrap();
        w.finish().unwrap();
        buf
    }

    /// Steady state must reuse the extracted directory without re-reading, but a
    /// rewritten archive must still be re-extracted.
    #[test]
    fn unchanged_archive_skips_work_and_changed_one_is_reextracted() {
        let dir = std::env::temp_dir().join(format!("skate-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("example.zip");
        std::fs::write(&archive, zip_with(b"FIRST")).unwrap();

        let mut cache = Cache::default();
        let first = cache.materialize(&dir, &archive).unwrap();
        assert_eq!(std::fs::read(first.join("payload.txt")).unwrap(), b"FIRST");

        // Same bytes: the stamp matches and the cached extraction is reused.
        let again = cache.materialize(&dir, &archive).unwrap();
        assert_eq!(again, first);

        // Same length, new mtime: re-extracted with the new contents.
        let later = std::fs::metadata(&archive).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(5);
        std::fs::write(&archive, zip_with(b"SECOND")).unwrap();
        let f = std::fs::File::options().write(true).open(&archive).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(later)).unwrap();
        drop(f);
        let second = cache.materialize(&dir, &archive).unwrap();
        assert_eq!(std::fs::read(second.join("payload.txt")).unwrap(), b"SECOND");
        std::fs::remove_dir_all(&dir).ok();
    }
}
