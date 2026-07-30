//! Virtual filesystem: only explicit CLI paths under /pub/, extra mounts (incoming).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct Vfs {
    pub(crate) files: HashMap<String, PathBuf>,
    pub(crate) dirs: HashMap<String, PathBuf>,
    pub(crate) follow_symlinks: bool,
}

impl Vfs {
    pub(crate) fn from_paths(paths: &[PathBuf], follow_symlinks: bool) -> io::Result<Self> {
        let mut files = HashMap::new();
        let mut dirs = HashMap::new();

        for p in paths {
            let meta = fs::symlink_metadata(p).map_err(|e| {
                io::Error::new(e.kind(), format!("cannot access {}: {e}", p.display()))
            })?;

            // Reject symlink roots unless --follow-symlinks
            if meta.file_type().is_symlink() && !follow_symlinks {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{} is a symbolic link (pass --follow-symlinks to allow)",
                        p.display()
                    ),
                ));
            }

            let path = if follow_symlinks {
                p.canonicalize().map_err(|e| {
                    io::Error::new(e.kind(), format!("canonicalize {}: {e}", p.display()))
                })?
            } else if p.is_absolute() {
                p.clone()
            } else {
                env::current_dir()?.join(p)
            };

            // Use symlink_metadata so we classify without following
            let final_meta = if follow_symlinks {
                fs::metadata(&path)
            } else {
                fs::symlink_metadata(&path)
            }
            .map_err(|e| {
                io::Error::new(e.kind(), format!("cannot stat {}: {e}", path.display()))
            })?;

            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "item".into());

            if final_meta.is_dir() {
                if files.contains_key(&name) || dirs.contains_key(&name) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("name collision for directory '{name}'"),
                    ));
                }
                dirs.insert(name, path);
            } else if final_meta.is_file() {
                if files.contains_key(&name) || dirs.contains_key(&name) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("name collision for file '{name}'"),
                    ));
                }
                files.insert(name, path);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is neither a regular file nor a directory", p.display()),
                ));
            }
        }

        Ok(Vfs {
            files,
            dirs,
            follow_symlinks,
        })
    }

    /// Mount an extra directory under a virtual name (e.g. "incoming").
    pub(crate) fn add_dir(&mut self, name: &str, path: PathBuf) -> io::Result<()> {
        if self.files.contains_key(name) || self.dirs.contains_key(name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("name collision for directory '{name}'"),
            ));
        }
        self.dirs.insert(name.to_string(), path);
        Ok(())
    }

    /// Reject path components that can escape or confuse resolution.
    pub(crate) fn validate_components(rest: &str) -> Option<Vec<&str>> {
        if rest.contains('\0') || rest.contains('\\') {
            return None;
        }
        let mut out = Vec::new();
        for c in rest.split('/') {
            if c.is_empty() || c == "." {
                continue; // skip empty / current-dir segments
            }
            if c == ".." {
                return None;
            }
            // No absolute-looking segments
            if c.starts_with('/') {
                return None;
            }
            out.push(c);
        }
        Some(out)
    }

    /// True if `child` is equal to `root` or strictly inside it (component-aware).
    pub(crate) fn is_within(root: &Path, child: &Path) -> bool {
        let root_c: Vec<_> = root.components().collect();
        let child_c: Vec<_> = child.components().collect();
        if child_c.len() < root_c.len() {
            return false;
        }
        child_c
            .iter()
            .zip(root_c.iter())
            .all(|(a, b)| a == b)
    }

    /// Walk `components` under `dir_root` without leaving the tree.
    /// When `follow_symlinks` is false, any symlink component is rejected.
    pub(crate) fn safe_join(&self, dir_root: &Path, components: &[&str]) -> Option<PathBuf> {
        let mut cur = dir_root.to_path_buf();
        for comp in components {
            let next = cur.join(comp);
            let meta = fs::symlink_metadata(&next).ok()?;
            if meta.file_type().is_symlink() {
                if !self.follow_symlinks {
                    return None;
                }
                // Resolve this step and ensure we remain under the canonical root
                let root_canon = dir_root.canonicalize().ok()?;
                let next_canon = next.canonicalize().ok()?;
                if !Self::is_within(&root_canon, &next_canon) {
                    return None;
                }
                cur = next_canon;
            } else {
                cur = next;
            }
        }
        // Final containment check against canonical root when possible
        if let (Ok(root_canon), Ok(cur_canon)) =
            (dir_root.canonicalize(), cur.canonicalize())
        {
            if !Self::is_within(&root_canon, &cur_canon) {
                return None;
            }
            // Prefer canonical path when available (stable for open)
            if self.follow_symlinks || !fs::symlink_metadata(&cur).ok()?.file_type().is_symlink() {
                return Some(cur_canon);
            }
        }
        Some(cur)
    }

    /// Resolve a request path.
    /// Shared CLI paths live under `/pub/`. Extra mounts (e.g. `incoming`) stay at their name.
    pub(crate) fn resolve(&self, req_path: &str) -> Option<Resolved> {
        let req_path = req_path.trim_start_matches('/');
        if req_path.is_empty() || req_path == "." {
            return Some(Resolved::Index);
        }

        if req_path.contains('\0') || req_path.contains('\\') {
            return None;
        }

        // /pub and /pub/ → listing of shared CLI paths
        if req_path == "pub" {
            return Some(Resolved::PubIndex);
        }

        // /pub/<rest> → shared file or directory
        if let Some(rest) = req_path.strip_prefix("pub/") {
            return self.resolve_shared(rest);
        }

        // Other top-level mounts (e.g. incoming) and their children
        let mut parts = req_path.splitn(2, '/');
        let first = parts.next()?;
        let rest = parts.next().unwrap_or("");

        // Do not allow shared file names at virtual root (they are under /pub/)
        if rest.is_empty() {
            if self.files.contains_key(first) {
                return None;
            }
        }

        if let Some(dir_root) = self.dirs.get(first) {
            // Outside /pub/, only the explicit "incoming" mount is reachable.
            // Shared CLI dirs live under /pub/ via resolve_shared().
            if first != "incoming" {
                return None;
            }
            if rest.is_empty() {
                return Some(Resolved::Dir(dir_root.clone(), first.to_string()));
            }
            let components = Self::validate_components(rest)?;
            let resolved = self.safe_join(dir_root, &components)?;
            let meta = fs::symlink_metadata(&resolved).ok()?;
            if meta.is_file()
                || (self.follow_symlinks && fs::metadata(&resolved).map(|m| m.is_file()).unwrap_or(false))
            {
                if !self.follow_symlinks && meta.file_type().is_symlink() {
                    return None;
                }
                return Some(Resolved::File(resolved));
            }
            if meta.is_dir()
                || (self.follow_symlinks && fs::metadata(&resolved).map(|m| m.is_dir()).unwrap_or(false))
            {
                return Some(Resolved::Dir(resolved, req_path.to_string()));
            }
        }

        None
    }

    /// Resolve a path relative to the shared CLI virtual root (under /pub/).
    pub(crate) fn resolve_shared(&self, rest: &str) -> Option<Resolved> {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() || rest == "." {
            return Some(Resolved::PubIndex);
        }
        if rest.contains('\0') || rest.contains('\\') {
            return None;
        }

        // Exact file (basename)
        if !rest.contains('/') {
            if let Some(real) = self.files.get(rest) {
                return Some(Resolved::File(real.clone()));
            }
        }

        let mut parts = rest.splitn(2, '/');
        let first = parts.next()?;
        let sub = parts.next().unwrap_or("");

        if let Some(dir_root) = self.dirs.get(first) {
            // Shared CLI dir: exclude the incoming mount from /pub/
            if first == "incoming" {
                return None;
            }
            if sub.is_empty() {
                return Some(Resolved::Dir(
                    dir_root.clone(),
                    format!("pub/{first}"),
                ));
            }
            let components = Self::validate_components(sub)?;
            let resolved = self.safe_join(dir_root, &components)?;
            let meta = fs::symlink_metadata(&resolved).ok()?;
            if meta.is_file()
                || (self.follow_symlinks && fs::metadata(&resolved).map(|m| m.is_file()).unwrap_or(false))
            {
                if !self.follow_symlinks && meta.file_type().is_symlink() {
                    return None;
                }
                return Some(Resolved::File(resolved));
            }
            if meta.is_dir()
                || (self.follow_symlinks && fs::metadata(&resolved).map(|m| m.is_dir()).unwrap_or(false))
            {
                return Some(Resolved::Dir(
                    resolved,
                    format!("pub/{rest}"),
                ));
            }
        }

        None
    }
}

#[derive(Debug)]
pub(crate) enum Resolved {
    /// Site landing page at `/`
    Index,
    /// Listing of shared CLI paths at `/pub/`
    PubIndex,
    File(PathBuf),
    Dir(PathBuf, String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "http-share-vfs-{}-{}-{}",
            std::process::id(),
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn shared_paths_live_under_pub() {
        let dir = tmpdir("shared");
        let file = dir.join("a.txt");
        fs::write(&file, b"hi").unwrap();
        let vfs = Vfs::from_paths(&[file.clone()], false).unwrap();
        assert!(matches!(vfs.resolve("/"), Some(Resolved::Index)));
        assert!(matches!(vfs.resolve("/pub"), Some(Resolved::PubIndex)));
        assert!(matches!(vfs.resolve("/pub/"), Some(Resolved::PubIndex)));
        match vfs.resolve("/pub/a.txt") {
            Some(Resolved::File(p)) => assert_eq!(p, file),
            other => panic!("expected file, got {other:?}"),
        }
        assert!(vfs.resolve("/a.txt").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_dotdot_in_dir_share() {
        let dir = tmpdir("dotdot");
        let sub = dir.join("shareddir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("x"), b"1").unwrap();
        let vfs = Vfs::from_paths(&[sub.clone()], false).unwrap();
        let ok = vfs.resolve("/pub/shareddir/x");
        assert!(
            matches!(ok, Some(Resolved::File(_))),
            "expected /pub/shareddir/x, got {ok:?}; dirs={:?}",
            vfs.dirs.keys().collect::<Vec<_>>()
        );
        assert!(
            vfs.resolve("/pub/shareddir/../x").is_none(),
            "dot-dot must be rejected"
        );
        assert!(vfs.resolve("/pub/shareddir/../../etc/passwd").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incoming_mount_not_under_pub() {
        let dir = tmpdir("incoming");
        let inc = dir.join("in");
        fs::create_dir(&inc).unwrap();
        fs::write(inc.join("u.txt"), b"up").unwrap();
        let mut vfs = Vfs::from_paths(&[], false).unwrap();
        vfs.add_dir("incoming", inc.clone()).unwrap();
        assert!(matches!(
            vfs.resolve("/incoming/u.txt"),
            Some(Resolved::File(_))
        ));
        assert!(vfs.resolve("/pub/incoming/u.txt").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
