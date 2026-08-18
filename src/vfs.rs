// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Virtual filesystem: explicit CLI shares at the root, optional deep maps, merged dirs.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One mapping from a real path into the virtual tree.
///
/// * `virt = None`, `flatten = false` — mount under the path's basename.
/// * `virt = None`, `flatten = true` — mount directory *contents* at the root.
/// * `virt = Some(path)` — expose at that virtual path (may contain `/`).
/// * Multiple directory maps to the same virtual path **merge** their children.
#[derive(Debug, Clone)]
pub(crate) struct ShareSpec {
    pub virt: Option<String>,
    pub path: PathBuf,
    pub flatten: bool,
}

impl ShareSpec {
    /// Parse a positional share token: `PATH` or `PATH/` (contents at root).
    pub(crate) fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("empty share path".into());
        }

        let flatten = token.ends_with('/') && token != "/" && token != "//";
        let path_str = if flatten {
            token.trim_end_matches('/')
        } else {
            token
        };
        if path_str.is_empty() {
            return Err(format!("invalid share '{token}': empty path"));
        }
        Ok(ShareSpec {
            virt: None,
            path: PathBuf::from(path_str),
            flatten,
        })
    }

    /// Build a named mapping from `--map PATH VIRT` (VIRT may be `a/b/c`).
    pub(crate) fn map(path_token: &str, virt: &str) -> Result<Self, String> {
        let virt = virt.trim().trim_matches('/');
        if virt.is_empty() {
            return Err("virtual path must not be empty".into());
        }
        let components: Vec<&str> = virt.split('/').filter(|c| !c.is_empty()).collect();
        if components.is_empty() {
            return Err("virtual path must not be empty".into());
        }
        for c in &components {
            if *c == "." || *c == ".." {
                return Err(format!("invalid virtual path component '{c}'"));
            }
            if c.contains('\0') {
                return Err("invalid virtual path: NUL".into());
            }
        }
        if components[0] == "incoming"
            || components[0] == "upload"
            || components[0] == "message"
            || components[0] == "certificate.pem"
        {
            return Err(format!(
                "virtual path starting with '{}' is reserved",
                components[0]
            ));
        }

        let path_token = path_token.trim();
        if path_token.is_empty() {
            return Err("map path must not be empty".into());
        }
        let flatten = path_token.ends_with('/') && path_token != "/";
        let path_str = if flatten {
            path_token.trim_end_matches('/')
        } else {
            path_token
        };
        if path_str.is_empty() {
            return Err("map path must not be empty".into());
        }
        Ok(ShareSpec {
            virt: Some(components.join("/")),
            path: PathBuf::from(path_str),
            flatten,
        })
    }
}

#[derive(Debug)]
enum Node {
    File(PathBuf),
    /// Merged virtual directory: children of all `reals` plus explicit `children`.
    Dir {
        reals: Vec<PathBuf>,
        children: HashMap<String, Node>,
    },
}

impl Node {
    fn empty_dir() -> Self {
        Node::Dir {
            reals: Vec::new(),
            children: HashMap::new(),
        }
    }
}

pub(crate) struct Vfs {
    root: Node,
    pub(crate) follow_symlinks: bool,
}

/// Directory listing entry for HTML.
#[derive(Debug)]
pub(crate) struct ListEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
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
        let mut root = Node::empty_dir();
        for spec in specs {
            Self::add_share(&mut root, spec, follow_symlinks)?;
        }
        Ok(Vfs {
            root,
            follow_symlinks,
        })
    }

    pub(crate) fn add_dir(&mut self, name: &str, path: PathBuf) -> io::Result<()> {
        Self::place_node(
            &mut self.root,
            &[name],
            Node::Dir {
                reals: vec![path],
                children: HashMap::new(),
            },
            true,
        )
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

    fn add_share(root: &mut Node, spec: &ShareSpec, follow_symlinks: bool) -> io::Result<()> {
        let (path, final_meta) = Self::resolve_real(&spec.path, follow_symlinks)?;

        let is_dot = spec.path.as_os_str() == "." || spec.path.as_os_str() == "./";
        let flatten_root =
            (spec.flatten || is_dot) && final_meta.is_dir() && spec.virt.is_none();
        if flatten_root {
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
                let node = if cmeta.is_dir() {
                    Node::Dir {
                        reals: vec![child],
                        children: HashMap::new(),
                    }
                } else if cmeta.is_file() {
                    Node::File(child)
                } else {
                    continue;
                };
                Self::place_node(root, &[cname.as_str()], node, true)?;
            }
            return Ok(());
        }

        // Flatten under a virtual prefix: insert each child under virt/
        if spec.flatten && final_meta.is_dir() {
            if let Some(ref virt) = spec.virt {
                let prefix: Vec<&str> = virt.split('/').filter(|c| !c.is_empty()).collect();
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
                    let mut comps: Vec<&str> = prefix.clone();
                    let owned = cname.clone();
                    comps.push(&owned);
                    let node = if cmeta.is_dir() {
                        Node::Dir {
                            reals: vec![child],
                            children: HashMap::new(),
                        }
                    } else if cmeta.is_file() {
                        Node::File(child)
                    } else {
                        continue;
                    };
                    Self::place_node(root, &comps, node, true)?;
                }
                return Ok(());
            }
        }

        let comps: Vec<String> = if let Some(ref v) = spec.virt {
            v.split('/').filter(|c| !c.is_empty()).map(|s| s.to_string()).collect()
        } else {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "item".into());
            vec![name]
        };
        let comps_ref: Vec<&str> = comps.iter().map(|s| s.as_str()).collect();

        let node = if final_meta.is_dir() {
            Node::Dir {
                reals: vec![path],
                children: HashMap::new(),
            }
        } else if final_meta.is_file() {
            Node::File(path)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is neither a regular file nor a directory",
                    spec.path.display()
                ),
            ));
        };
        Self::place_node(root, &comps_ref, node, true)
    }

    /// Insert `node` at `components` under `root`.
    /// When `merge_dirs` and both sides are dirs, merge reals + children.
    fn place_node(
        root: &mut Node,
        components: &[&str],
        node: Node,
        merge_dirs: bool,
    ) -> io::Result<()> {
        if components.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty virtual path",
            ));
        }
        let Node::Dir { children, .. } = root else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "internal: root is not a directory",
            ));
        };

        let (first, rest) = components.split_first().unwrap();
        if rest.is_empty() {
            match children.get_mut(*first) {
                None => {
                    children.insert((*first).to_string(), node);
                    Ok(())
                }
                Some(existing) => match (existing, node) {
                    (
                        Node::Dir {
                            reals,
                            children: ch,
                        },
                        Node::Dir {
                            reals: mut r2,
                            children: ch2,
                        },
                    ) if merge_dirs => {
                        reals.append(&mut r2);
                        for (k, v) in ch2 {
                            match ch.get_mut(&k) {
                                None => {
                                    ch.insert(k, v);
                                }
                                Some(Node::Dir {
                                    reals: er,
                                    children: ec,
                                }) => {
                                    if let Node::Dir {
                                        reals: mut nr,
                                        children: nc,
                                    } = v
                                    {
                                        er.append(&mut nr);
                                        for (k2, v2) in nc {
                                            if ec.contains_key(&k2) {
                                                return Err(io::Error::new(
                                                    io::ErrorKind::AlreadyExists,
                                                    format!("name collision for '{k2}'"),
                                                ));
                                            }
                                            ec.insert(k2, v2);
                                        }
                                    } else {
                                        return Err(io::Error::new(
                                            io::ErrorKind::AlreadyExists,
                                            format!("name collision for '{k}'"),
                                        ));
                                    }
                                }
                                Some(Node::File(_)) => {
                                    return Err(io::Error::new(
                                        io::ErrorKind::AlreadyExists,
                                        format!("name collision for '{k}'"),
                                    ));
                                }
                            }
                        }
                        Ok(())
                    }
                    _ => Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("name collision for '{first}'"),
                    )),
                },
            }
        } else {
            if !children.contains_key(*first) {
                children.insert((*first).to_string(), Node::empty_dir());
            }
            let child = children.get_mut(*first).unwrap();
            match child {
                Node::Dir { .. } => Self::place_node(child, rest, node, merge_dirs),
                Node::File(_) => Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("name collision for '{first}' (file in the way)"),
                )),
            }
        }
    }

    pub(crate) fn validate_components(rest: &str) -> Option<Vec<&str>> {
        if rest.contains('\0') || rest.contains('\\') {
            return None;
        }
        let mut out = Vec::new();
        for c in rest.split('/') {
            if c.is_empty() || c == "." {
                continue;
            }
            if c == ".." {
                return None;
            }
            out.push(c);
        }
        Some(out)
    }

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

    pub(crate) fn safe_join(&self, dir_root: &Path, components: &[&str]) -> Option<PathBuf> {
        let mut cur = dir_root.to_path_buf();
        for comp in components {
            let next = cur.join(comp);
            let meta = fs::symlink_metadata(&next).ok()?;
            if meta.file_type().is_symlink() {
                if !self.follow_symlinks {
                    return None;
                }
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
        if let (Ok(root_canon), Ok(cur_canon)) =
            (dir_root.canonicalize(), cur.canonicalize())
        {
            if !Self::is_within(&root_canon, &cur_canon) {
                return None;
            }
            if self.follow_symlinks
                || !fs::symlink_metadata(&cur)
                    .ok()?
                    .file_type()
                    .is_symlink()
            {
                return Some(cur_canon);
            }
        }
        Some(cur)
    }

    fn walk<'a>(&'a self, components: &[&str]) -> Option<&'a Node> {
        let mut node = &self.root;
        for c in components {
            match node {
                Node::Dir { children, .. } => {
                    node = children.get(*c)?;
                }
                Node::File(_) => return None,
            }
        }
        Some(node)
    }

    /// Whether a top-level name exists (for nav links etc.).
    pub(crate) fn has_top_level(&self, name: &str) -> bool {
        matches!(&self.root, Node::Dir { children, .. } if children.contains_key(name))
    }

    /// Count top-level entries (for verbose).
    pub(crate) fn top_level_count(&self) -> (usize, usize) {
        let mut files = 0;
        let mut dirs = 0;
        if let Node::Dir { children, .. } = &self.root {
            for n in children.values() {
                match n {
                    Node::File(_) => files += 1,
                    Node::Dir { .. } => dirs += 1,
                }
            }
        }
        (files, dirs)
    }

    /// Lines describing the virtual tree (for `--tree`).
    pub(crate) fn describe_shares(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push("/".to_string());
        Self::describe_node(&self.root, "", "  ", &mut out);
        out
    }

    fn describe_node(node: &Node, virt: &str, indent: &str, out: &mut Vec<String>) {
        match node {
            Node::File(p) => {
                out.push(format!("{indent}{virt}  →  {}", p.display()));
            }
            Node::Dir { reals, children } => {
                if !virt.is_empty() {
                    if reals.is_empty() {
                        out.push(format!("{indent}{virt}/  (virtual)"));
                    } else if reals.len() == 1 {
                        out.push(format!("{indent}{virt}/  →  {}", reals[0].display()));
                    } else {
                        out.push(format!("{indent}{virt}/  (merged {} dirs)", reals.len()));
                        for r in reals {
                            out.push(format!("{indent}  ↳ {}", r.display()));
                        }
                    }
                } else if !reals.is_empty() {
                    // root with real backends (unusual)
                    for r in reals {
                        out.push(format!("{indent}(root)  →  {}", r.display()));
                    }
                }
                let mut names: Vec<_> = children.keys().cloned().collect();
                names.sort();
                for name in names {
                    let child = &children[&name];
                    let next = if virt.is_empty() {
                        format!("/{name}")
                    } else {
                        format!("{virt}/{name}")
                    };
                    Self::describe_node(child, &next, indent, out);
                }
            }
        }
    }

    pub(crate) fn list(&self, virt_path: &str) -> Option<Vec<ListEntry>> {
        let virt_path = virt_path.trim_matches('/');
        let node = if virt_path.is_empty() {
            &self.root
        } else {
            let comps = Self::validate_components(virt_path)?;
            self.walk(&comps)?
        };
        let Node::Dir { reals, children } = node else {
            return None;
        };

        let mut map: HashMap<String, ListEntry> = HashMap::new();

        for real in reals {
            if let Ok(entries) = fs::read_dir(real) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(".upload-") && name.ends_with(".tmp") {
                        continue;
                    }
                    if name == "." || name == ".." {
                        continue;
                    }
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let size = if is_dir {
                        None
                    } else {
                        entry.metadata().ok().map(|m| m.len())
                    };
                    map.entry(name.clone()).or_insert(ListEntry {
                        name,
                        is_dir,
                        size,
                    });
                }
            }
        }

        for (name, child) in children {
            match child {
                Node::File(p) => {
                    let size = fs::metadata(p).ok().map(|m| m.len());
                    map.insert(
                        name.clone(),
                        ListEntry {
                            name: name.clone(),
                            is_dir: false,
                            size,
                        },
                    );
                }
                Node::Dir { .. } => {
                    map.insert(
                        name.clone(),
                        ListEntry {
                            name: name.clone(),
                            is_dir: true,
                            size: None,
                        },
                    );
                }
            }
        }

        let mut items: Vec<_> = map.into_values().collect();
        items.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        Some(items)
    }

    pub(crate) fn resolve(&self, req_path: &str) -> Option<Resolved> {
        let req_path = req_path.trim_start_matches('/');
        if req_path.is_empty() || req_path == "." {
            return Some(Resolved::Index);
        }
        if req_path.contains('\0') || req_path.contains('\\') {
            return None;
        }
        let comps = Self::validate_components(req_path)?;
        if comps.is_empty() {
            return Some(Resolved::Index);
        }

        // Walk explicit tree; when a Dir has reals, remaining path may continue into a real dir.
        let mut node = &self.root;
        let mut i = 0;
        while i < comps.len() {
            match node {
                Node::File(p) => {
                    if i == comps.len() {
                        return Some(Resolved::File(p.clone()));
                    }
                    return None;
                }
                Node::Dir { children, reals } => {
                    let name = comps[i];
                    if let Some(child) = children.get(name) {
                        node = child;
                        i += 1;
                        continue;
                    }
                    // Fall through into real directory backends
                    if reals.is_empty() {
                        return None;
                    }
                    let rest = &comps[i..];
                    for real in reals {
                        if rest.is_empty() {
                            return Some(Resolved::Dir(real.clone(), req_path.to_string()));
                        }
                        if let Some(resolved) = self.safe_join(real, rest) {
                            let meta = fs::symlink_metadata(&resolved).ok()?;
                            if meta.is_file()
                                || (self.follow_symlinks
                                    && fs::metadata(&resolved)
                                        .map(|m| m.is_file())
                                        .unwrap_or(false))
                            {
                                if !self.follow_symlinks && meta.file_type().is_symlink() {
                                    continue;
                                }
                                return Some(Resolved::File(resolved));
                            }
                            if meta.is_dir()
                                || (self.follow_symlinks
                                    && fs::metadata(&resolved)
                                        .map(|m| m.is_dir())
                                        .unwrap_or(false))
                            {
                                return Some(Resolved::Dir(resolved, req_path.to_string()));
                            }
                        }
                    }
                    return None;
                }
            }
        }

        match node {
            Node::File(p) => Some(Resolved::File(p.clone())),
            Node::Dir { reals, children } => {
                // Virtual dir (possibly merged): prefer first real for PathBuf, virt path for listing
                if !reals.is_empty() && children.is_empty() && reals.len() == 1 {
                    Some(Resolved::Dir(reals[0].clone(), req_path.to_string()))
                } else {
                    // Merged or purely virtual: listing uses virt path
                    Some(Resolved::VirtualDir(req_path.to_string()))
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum Resolved {
    Index,
    File(PathBuf),
    /// Single real directory backed listing
    Dir(PathBuf, String),
    /// Merged / intermediate virtual directory (list via Vfs::list)
    VirtualDir(String),
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
            "expected /shareddir/x, got {ok:?}"
        );
        assert!(vfs.resolve("/shareddir/../x").is_none());
        assert!(vfs.resolve("/shareddir/../../etc/passwd").is_none());
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

        let s = ShareSpec::parse("foo=bar.txt").unwrap();
        assert!(s.virt.is_none());
        assert_eq!(s.path, PathBuf::from("foo=bar.txt"));

        let s = ShareSpec::map("./MyDocs", "docs").unwrap();
        assert_eq!(s.virt.as_deref(), Some("docs"));
        assert_eq!(s.path, PathBuf::from("./MyDocs"));

        let s = ShareSpec::map("./MyDocs", "a/b/c").unwrap();
        assert_eq!(s.virt.as_deref(), Some("a/b/c"));

        assert!(ShareSpec::map("foo", "").is_err());
        assert!(ShareSpec::map("foo", "incoming").is_err());
        assert!(ShareSpec::map("foo", "a/../b").is_err());
    }

    #[test]
    fn trailing_slash_flattens_at_root() {
        let dir = tmpdir("flat");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let token = format!("{}/", dir.display());
        let spec = ShareSpec::parse(&token).unwrap();
        let vfs = Vfs::from_shares(&[spec], false).unwrap();
        assert!(matches!(vfs.resolve("/a.txt"), Some(Resolved::File(_))));
        assert!(matches!(
            vfs.resolve("/sub"),
            Some(Resolved::Dir(_, _) | Resolved::VirtualDir(_))
        ));
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
        assert!(matches!(
            vfs.resolve("/pics"),
            Some(Resolved::Dir(_, _) | Resolved::VirtualDir(_))
        ));
        assert!(matches!(vfs.resolve("/pics/x.jpg"), Some(Resolved::File(_))));
        assert!(vfs.resolve("/photos").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_same_virt_name() {
        let dir = tmpdir("merge");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("from_a.txt"), b"a").unwrap();
        fs::write(b.join("from_b.txt"), b"b").unwrap();
        let specs = vec![
            ShareSpec {
                virt: Some("pub".into()),
                path: a.clone(),
                flatten: false,
            },
            ShareSpec {
                virt: Some("pub".into()),
                path: b.clone(),
                flatten: false,
            },
        ];
        let vfs = Vfs::from_shares(&specs, false).unwrap();
        assert!(matches!(vfs.resolve("/pub/from_a.txt"), Some(Resolved::File(_))));
        assert!(matches!(vfs.resolve("/pub/from_b.txt"), Some(Resolved::File(_))));
        let listing = vfs.list("pub").unwrap();
        let names: Vec<_> = listing.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"from_a.txt"));
        assert!(names.contains(&"from_b.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deep_alias() {
        let dir = tmpdir("deep");
        let file = dir.join("x.txt");
        fs::write(&file, b"x").unwrap();
        let spec = ShareSpec {
            virt: Some("a/b/c".into()),
            path: file.clone(),
            flatten: false,
        };
        let vfs = Vfs::from_shares(&[spec], false).unwrap();
        assert!(matches!(vfs.resolve("/a"), Some(Resolved::VirtualDir(_))));
        assert!(matches!(vfs.resolve("/a/b"), Some(Resolved::VirtualDir(_))));
        match vfs.resolve("/a/b/c") {
            Some(Resolved::File(p)) => assert_eq!(p, file),
            other => panic!("expected file, got {other:?}"),
        }
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
        assert!(vfs.has_top_level("incoming"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dot_share_flattens_contents() {
        let dir = tmpdir("dotflat");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let vfs = Vfs::from_paths(&[PathBuf::from(".")], false).unwrap();
        std::env::set_current_dir(&old).unwrap();
        assert!(matches!(vfs.resolve("/a.txt"), Some(Resolved::File(_))));
        assert!(matches!(
            vfs.resolve("/sub"),
            Some(Resolved::Dir(_, _) | Resolved::VirtualDir(_))
        ));
        let _ = fs::remove_dir_all(&dir);
    }
}
