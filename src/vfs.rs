// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Virtual filesystem: only explicit CLI paths at the virtual root, extra mounts (incoming).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One mapping from a real path into the virtual tree.
///
/// * `virt = None`, `flatten = false` — mount under the path's basename.
/// * `virt = None`, `flatten = true` — mount directory *contents* at the root.
/// * `virt = Some(name)`, `flatten = false` — mount the path as `/name`.
/// * `virt = Some(name)`, `flatten = true` — same as non-flatten for directories
///   (children appear under `/name/`); for files, trailing slash is ignored.
#[derive(Debug, Clone)]
pub(crate) struct ShareSpec {
    pub virt: Option<String>,
    pub path: PathBuf,
    pub flatten: bool,
}

impl ShareSpec {
    /// Parse a CLI share token: `PATH`, `PATH/`, `NAME=PATH`, or `NAME=PATH/`.
    pub(crate) fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("empty share path".into());
        }

        let (virt, rest) = if let Some((left, right)) = token.split_once('=') {
            if left.is_empty() || right.is_empty() {
                return Err(format!(
                    "invalid share '{token}': expected NAME=PATH (both sides non-empty)"
                ));
            }
            if left.contains('/') || left == "." || left == ".." {
                return Err(format!(
                    "invalid virtual name '{left}': must be a single path component"
                ));
            }
            if left == "incoming" || left == "upload" || left == "message" || left == "certificate.pem" {
                return Err(format!(
                    "virtual name '{left}' is reserved"
                ));
            }
            (Some(left.to_string()), right)
        } else {
            (None, token)
        };

        let flatten = rest.ends_with('/') && rest != "/" && rest != "//";
        let path_str = if flatten {
            rest.trim_end_matches('/')
        } else {
            rest
        };
        if path_str.is_empty() {
            return Err(format!("invalid share '{token}': empty path"));
        }
        // Keep "." as-is for later flatten-at-root handling
        Ok(ShareSpec {
            virt,
            path: PathBuf::from(path_str),
            flatten,
        })
    }
}

pub(crate) struct Vfs {
    pub(crate) files: HashMap<String, PathBuf>,
    pub(crate) dirs: HashMap<String, PathBuf>,
    pub(crate) follow_symlinks: bool,
}

impl Vfs {
    pub(crate) fn from_paths(paths: &[PathBuf], follow_symlinks: bool) -> io::Result<Self> {
        let specs: Vec<ShareSpec> = paths
            .iter()
            .map(|p| ShareSpec {
                virt: None,
                path: p.clone(),
                flatten: false,
            })
            .collect();
        Self::from_shares(&specs, follow_symlinks)
    }

    pub(crate) fn from_shares(specs: &[ShareSpec], follow_symlinks: bool) -> io::Result<Self> {
        let mut files = HashMap::new();
        let mut dirs = HashMap::new();

        for spec in specs {
            Self::add_share(&mut files, &mut dirs, spec, follow_symlinks)?;
        }

        Ok(Vfs {
            files,
            dirs,
            follow_symlinks,
        })
    }

    fn insert_file(
        files: &mut HashMap<String, PathBuf>,
        dirs: &HashMap<String, PathBuf>,
        name: &str,
        path: PathBuf,
    ) -> io::Result<()> {
        if files.contains_key(name) || dirs.contains_key(name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("name collision for file '{name}'"),
            ));
        }
        files.insert(name.to_string(), path);
        Ok(())
    }

    fn insert_dir(
        files: &HashMap<String, PathBuf>,
        dirs: &mut HashMap<String, PathBuf>,
        name: &str,
        path: PathBuf,
    ) -> io::Result<()> {
        if files.contains_key(name) || dirs.contains_key(name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("name collision for directory '{name}'"),
            ));
        }
        dirs.insert(name.to_string(), path);
        Ok(())
    }

    fn resolve_real(p: &Path, follow_symlinks: bool) -> io::Result<(PathBuf, fs::Metadata)> {
        let meta = fs::symlink_metadata(p).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot access {}: {e}", p.display()))
        })?;

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
            p.to_path_buf()
        } else {
            env::current_dir()?.join(p)
        };

        let final_meta = if follow_symlinks {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        }
        .map_err(|e| {
            io::Error::new(e.kind(), format!("cannot stat {}: {e}", path.display()))
        })?;

        Ok((path, final_meta))
    }

    fn add_share(
        files: &mut HashMap<String, PathBuf>,
        dirs: &mut HashMap<String, PathBuf>,
        spec: &ShareSpec,
        follow_symlinks: bool,
    ) -> io::Result<()> {
        let (path, final_meta) = Self::resolve_real(&spec.path, follow_symlinks)?;

        // Flatten: directory contents at root (or treat "." the same)
        let is_dot = spec.path.as_os_str() == "." || spec.path.as_os_str() == "./";
        let do_flatten = (spec.flatten || is_dot) && final_meta.is_dir() && spec.virt.is_none();
        if do_flatten {
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                let child = entry.path();
                let cmeta = if follow_symlinks {
                    fs::metadata(&child)
                } else {
                    fs::symlink_metadata(&child)
                }?;
                let cname = entry.file_name().to_string_lossy().into_owned();
                if cname == "." || cname == ".." {
                    continue;
                }
                if cmeta.is_dir() {
                    Self::insert_dir(files, dirs, &cname, child)?;
                } else if cmeta.is_file() {
                    Self::insert_file(files, dirs, &cname, child)?;
                }
            }
            return Ok(());
        }

        let name = if let Some(ref v) = spec.virt {
            v.clone()
        } else {
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "item".into())
        };

        if final_meta.is_dir() {
            Self::insert_dir(files, dirs, &name, path)?;
        } else if final_meta.is_file() {
            Self::insert_file(files, dirs, &name, path)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is neither a regular file nor a directory",
                    spec.path.display()
                ),
            ));
        }
        Ok(())
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
    /// Shared CLI paths live at the virtual root. Extra mounts (e.g. `incoming`)
    /// stay at their name. Reserved names (`incoming`, etc.) take precedence
    /// only when mounted; CLI paths must not collide with them at creation time.
    pub(crate) fn resolve(&self, req_path: &str) -> Option<Resolved> {
        let req_path = req_path.trim_start_matches('/');
        if req_path.is_empty() || req_path == "." {
            return Some(Resolved::Index);
        }

        if req_path.contains('\0') || req_path.contains('\\') {
            return None;
        }

        // Exact top-level shared file
        if !req_path.contains('/') {
            if let Some(real) = self.files.get(req_path) {
                return Some(Resolved::File(real.clone()));
            }
        }

        let mut parts = req_path.splitn(2, '/');
        let first = parts.next()?;
        let rest = parts.next().unwrap_or("");

        if let Some(dir_root) = self.dirs.get(first) {
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
}

#[derive(Debug)]
pub(crate) enum Resolved {
    /// Listing / landing at `/` (shared CLI paths + nav links)
    Index,
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
    fn shared_paths_live_at_root() {
        let dir = tmpdir("shared");
        let file = dir.join("a.txt");
        fs::write(&file, b"hi").unwrap();
        let vfs = Vfs::from_paths(&[file.clone()], false).unwrap();
        assert!(matches!(vfs.resolve("/"), Some(Resolved::Index)));
        match vfs.resolve("/a.txt") {
            Some(Resolved::File(p)) => assert_eq!(p, file),
            other => panic!("expected file, got {other:?}"),
        }
        // Old /pub/ layout is gone
        assert!(vfs.resolve("/pub").is_none() || matches!(vfs.resolve("/pub"), Some(Resolved::Dir(_, _))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_dotdot_in_dir_share() {
        let dir = tmpdir("dotdot");
        let sub = dir.join("shareddir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("x"), b"1").unwrap();
        let vfs = Vfs::from_paths(&[sub.clone()], false).unwrap();
        let ok = vfs.resolve("/shareddir/x");
        assert!(
            matches!(ok, Some(Resolved::File(_))),
            "expected /shareddir/x, got {ok:?}; dirs={:?}",
            vfs.dirs.keys().collect::<Vec<_>>()
        );
        assert!(
            vfs.resolve("/shareddir/../x").is_none(),
            "dot-dot must be rejected"
        );
        assert!(vfs.resolve("/shareddir/../../etc/passwd").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dot_share_flattens_contents() {
        let dir = tmpdir("dotflat");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("b.txt"), b"b").unwrap();
        // Simulate being inside `dir` and sharing "."
        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let vfs = Vfs::from_paths(&[PathBuf::from(".")], false).unwrap();
        std::env::set_current_dir(&old).unwrap();
        match vfs.resolve("/a.txt") {
            Some(Resolved::File(_)) => {}
            other => panic!("expected /a.txt file, got {other:?}"),
        }
        match vfs.resolve("/sub") {
            Some(Resolved::Dir(_, _)) => {}
            other => panic!("expected /sub dir, got {other:?}"),
        }
        // Should not mount under the basename of the temp dir
        assert!(vfs.resolve(&format!("/{}", dir.file_name().unwrap().to_string_lossy())).is_none()
            || !vfs.dirs.contains_key(&dir.file_name().unwrap().to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn share_spec_parse_forms() {
        let s = ShareSpec::parse("file.txt").unwrap();
        assert!(s.virt.is_none());
        assert!(!s.flatten);
        assert_eq!(s.path, PathBuf::from("file.txt"));

        let s = ShareSpec::parse("photos/").unwrap();
        assert!(s.virt.is_none());
        assert!(s.flatten);
        assert_eq!(s.path, PathBuf::from("photos"));

        let s = ShareSpec::parse("docs=./MyDocs").unwrap();
        assert_eq!(s.virt.as_deref(), Some("docs"));
        assert!(!s.flatten);
        assert_eq!(s.path, PathBuf::from("./MyDocs"));

        let s = ShareSpec::parse("docs=./MyDocs/").unwrap();
        assert_eq!(s.virt.as_deref(), Some("docs"));
        assert!(s.flatten);
        assert_eq!(s.path, PathBuf::from("./MyDocs"));

        assert!(ShareSpec::parse("=foo").is_err());
        assert!(ShareSpec::parse("a/b=foo").is_err());
        assert!(ShareSpec::parse("incoming=foo").is_err());
    }

    #[test]
    fn trailing_slash_flattens_at_root() {
        let dir = tmpdir("flat");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let token = format!("{}/", dir.display());
        let spec = ShareSpec::parse(&token).unwrap();
        assert!(spec.flatten);
        let vfs = Vfs::from_shares(&[spec], false).unwrap();
        assert!(matches!(vfs.resolve("/a.txt"), Some(Resolved::File(_))));
        assert!(matches!(vfs.resolve("/sub"), Some(Resolved::Dir(_, _))));
        // should not appear under the temp dir basename
        let base = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!vfs.dirs.contains_key(&base));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_mount_renames() {
        let dir = tmpdir("rename");
        let file = dir.join("notes.txt");
        fs::write(&file, b"hi").unwrap();
        let spec = ShareSpec {
            virt: Some("readme".into()),
            path: file.clone(),
            flatten: false,
        };
        let vfs = Vfs::from_shares(&[spec], false).unwrap();
        match vfs.resolve("/readme") {
            Some(Resolved::File(p)) => assert_eq!(p, file),
            other => panic!("expected /readme, got {other:?}"),
        }
        assert!(vfs.resolve("/notes.txt").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_dir_mount() {
        let dir = tmpdir("namdir");
        let sub = dir.join("photos");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("x.jpg"), b"img").unwrap();
        let spec = ShareSpec {
            virt: Some("pics".into()),
            path: sub.clone(),
            flatten: false,
        };
        let vfs = Vfs::from_shares(&[spec], false).unwrap();
        assert!(matches!(vfs.resolve("/pics"), Some(Resolved::Dir(_, _))));
        assert!(matches!(vfs.resolve("/pics/x.jpg"), Some(Resolved::File(_))));
        assert!(vfs.resolve("/photos").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incoming_mount_at_its_name() {
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
        // Shared CLI paths and incoming share the same root namespace
        let _ = fs::remove_dir_all(&dir);
    }
}
