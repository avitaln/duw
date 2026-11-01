//! Parallel directory walker that measures *actual* disk usage.
//!
//! Sizes come from `st_blocks * 512` (what `du` reports), not `st_size`
//! (what `ls -l` and Finder's "Size" report). Sparse files such as VM disk
//! images therefore count only the blocks they really occupy. Hard-linked
//! files are counted once.

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type NodeId = usize;

/// A file or directory. Directory sizes include all descendants.
pub struct Node {
    pub name: String,
    pub parent: Option<NodeId>,
    /// Sorted by `alloc`, largest first.
    pub children: Vec<NodeId>,
    pub is_dir: bool,
    /// Bytes actually allocated on disk.
    pub alloc: u64,
    /// Logical length, as shown by `ls -l`.
    pub apparent: u64,
    pub files: u64,
    /// Seconds since the epoch; for directories, the newest entry inside.
    pub mtime: i64,
    /// The directory could not be read (usually missing permissions).
    pub unreadable: bool,
    /// Deleted after the scan; no longer reachable from the root.
    pub removed: bool,
}

impl Node {
    /// Logical size is much larger than what is stored on disk.
    pub fn is_sparse(&self) -> bool {
        self.apparent > self.alloc.saturating_mul(2) && self.apparent - self.alloc > 1 << 20
    }
}

pub struct Tree {
    pub root_path: PathBuf,
    pub nodes: Vec<Node>,
}

impl Tree {
    pub const ROOT: NodeId = 0;

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut path = self.root_path.clone();
        for a in self.ancestors(id).into_iter().skip(1) {
            path.push(&self.nodes[a].name);
        }
        path
    }

    /// Root first, `id` last.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = vec![id];
        let mut cur = id;
        while let Some(p) = self.nodes[cur].parent {
            chain.push(p);
            cur = p;
        }
        chain.reverse();
        chain
    }

    /// Forget `id` and everything under it, shrinking its ancestors to match.
    fn remove(&mut self, id: NodeId) {
        let Some(parent) = self.nodes[id].parent else { return };
        let (alloc, apparent, files) = {
            let n = &self.nodes[id];
            (n.alloc, n.apparent, n.files)
        };
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            self.nodes[n].removed = true;
            stack.extend_from_slice(&self.nodes[n].children);
        }
        self.nodes[parent].children.retain(|&c| c != id);

        let ancestors = self.ancestors(parent);
        for &a in &ancestors {
            let n = &mut self.nodes[a];
            n.alloc = n.alloc.saturating_sub(alloc);
            n.apparent = n.apparent.saturating_sub(apparent);
            n.files = n.files.saturating_sub(files);
        }
        // The shrunken ancestors may now sort lower among their siblings.
        for &a in &ancestors {
            let mut kids = std::mem::take(&mut self.nodes[a].children);
            kids.sort_by(|&x, &y| self.nodes[y].alloc.cmp(&self.nodes[x].alloc));
            self.nodes[a].children = kids;
        }
    }
}

impl Scan {
    /// Forget a file or folder that was deleted after the scan.
    pub fn remove(&mut self, id: NodeId) {
        self.tree.remove(id);
        collect_stats(self);
    }
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
}

pub struct TypeStat {
    pub ext: String,
    pub alloc: u64,
    pub count: u64,
}

pub struct AgeBucket {
    pub label: &'static str,
    max_age: i64,
    pub alloc: u64,
    pub count: u64,
}

pub struct Scan {
    pub tree: Tree,
    /// Largest files, biggest first.
    pub largest: Vec<NodeId>,
    pub types: Vec<TypeStat>,
    pub ages: Vec<AgeBucket>,
    pub unreadable: usize,
    pub elapsed: Duration,
}

pub fn scan(root: &Path, one_volume: bool, progress: &Progress) -> Result<Scan, String> {
    let started = Instant::now();
    let meta = fs::metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let walker = Walker {
        progress,
        root_dev: meta.dev(),
        one_volume,
        hardlinks: Mutex::new(HashSet::new()),
    };
    // Deep trees recurse through nested rayon jobs; give workers room.
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(16 << 20)
        .build()
        .map_err(|e| e.to_string())?;
    let raw = pool.install(|| walker.dir(root, root.display().to_string(), &meta));
    if progress.cancel.load(Ordering::Relaxed) {
        return Err("Scan cancelled".into());
    }

    let mut nodes = Vec::new();
    flatten(raw, None, &mut nodes);
    let tree = Tree { root_path: root.to_path_buf(), nodes };
    let mut scan = Scan {
        tree,
        largest: Vec::new(),
        types: Vec::new(),
        ages: Vec::new(),
        unreadable: 0,
        elapsed: Duration::ZERO,
    };
    collect_stats(&mut scan);
    scan.elapsed = started.elapsed();
    Ok(scan)
}

struct Raw {
    name: String,
    is_dir: bool,
    alloc: u64,
    apparent: u64,
    files: u64,
    mtime: i64,
    unreadable: bool,
    children: Vec<Raw>,
}

struct Walker<'a> {
    progress: &'a Progress,
    root_dev: u64,
    one_volume: bool,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
}

impl Walker<'_> {
    fn dir(&self, path: &Path, name: String, meta: &fs::Metadata) -> Raw {
        let mut raw = Raw {
            name,
            is_dir: true,
            alloc: meta.blocks() * 512,
            apparent: meta.len(),
            files: 0,
            mtime: meta.mtime(),
            unreadable: false,
            children: Vec::new(),
        };
        if self.progress.cancel.load(Ordering::Relaxed) {
            return raw;
        }
        let entries: Vec<fs::DirEntry> = match fs::read_dir(path) {
            Ok(rd) => rd.filter_map(Result::ok).collect(),
            Err(_) => {
                raw.unreadable = true;
                return raw;
            }
        };
        raw.children = entries.par_iter().filter_map(|e| self.entry(e)).collect();
        for c in &raw.children {
            raw.alloc += c.alloc;
            raw.apparent += c.apparent;
            raw.files += c.files;
            raw.mtime = raw.mtime.max(c.mtime);
        }
        raw
    }

    fn entry(&self, e: &fs::DirEntry) -> Option<Raw> {
        // DirEntry::metadata does not follow symlinks.
        let meta = e.metadata().ok()?;
        let name = e.file_name().to_string_lossy().into_owned();
        if meta.is_dir() {
            if self.one_volume && meta.dev() != self.root_dev {
                return None;
            }
            return Some(self.dir(&e.path(), name, &meta));
        }
        let mut alloc = meta.blocks() * 512;
        let mut apparent = meta.len();
        if meta.nlink() > 1 && !self.hardlinks.lock().unwrap().insert((meta.dev(), meta.ino())) {
            // Already counted through another hard link.
            alloc = 0;
            apparent = 0;
        }
        self.progress.files.fetch_add(1, Ordering::Relaxed);
        self.progress.bytes.fetch_add(alloc, Ordering::Relaxed);
        Some(Raw {
            name,
            is_dir: false,
            alloc,
            apparent,
            files: 1,
            mtime: meta.mtime(),
            unreadable: false,
            children: Vec::new(),
        })
    }
}

fn flatten(raw: Raw, parent: Option<NodeId>, nodes: &mut Vec<Node>) -> NodeId {
    let id = nodes.len();
    nodes.push(Node {
        name: raw.name,
        parent,
        children: Vec::new(),
        is_dir: raw.is_dir,
        alloc: raw.alloc,
        apparent: raw.apparent,
        files: raw.files,
        mtime: raw.mtime,
        unreadable: raw.unreadable,
        removed: false,
    });
    let mut kids = raw.children;
    kids.sort_unstable_by(|a, b| b.alloc.cmp(&a.alloc));
    let ids = kids.into_iter().map(|k| flatten(k, Some(id), nodes)).collect();
    nodes[id].children = ids;
    id
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn collect_stats(scan: &mut Scan) {
    const DAY: i64 = 86_400;
    let bucket = |label, max_age| AgeBucket { label, max_age, alloc: 0, count: 0 };
    let mut ages = vec![
        bucket("Last week", 7 * DAY),
        bucket("Last month", 30 * DAY),
        bucket("Last 3 months", 91 * DAY),
        bucket("Last year", 365 * DAY),
        bucket("Last 3 years", 3 * 365 * DAY),
        bucket("Older", i64::MAX),
    ];
    let now = now_secs();
    let mut types: HashMap<String, (u64, u64)> = HashMap::new();
    let mut files = Vec::new();
    scan.unreadable = 0;

    for (id, n) in scan.tree.nodes.iter().enumerate() {
        if n.removed {
            continue;
        }
        if n.is_dir {
            scan.unreadable += n.unreadable as usize;
            continue;
        }
        files.push(id);
        let ext = Path::new(&n.name)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            .unwrap_or_else(|| "(no extension)".into());
        let t = types.entry(ext).or_default();
        t.0 += n.alloc;
        t.1 += 1;
        let age = now - n.mtime;
        if let Some(b) = ages.iter_mut().find(|b| age <= b.max_age) {
            b.alloc += n.alloc;
            b.count += 1;
        }
    }

    let nodes = &scan.tree.nodes;
    files.sort_unstable_by(|&a, &b| nodes[b].alloc.cmp(&nodes[a].alloc));
    files.truncate(1000);
    scan.largest = files;

    let mut types: Vec<TypeStat> = types
        .into_iter()
        .map(|(ext, (alloc, count))| TypeStat { ext, alloc, count })
        .collect();
    types.sort_unstable_by(|a, b| b.alloc.cmp(&a.alloc));
    scan.types = types;
    scan.ages = ages;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sparse_files_count_allocated_blocks_and_hardlinks_count_once() {
        let dir = std::env::temp_dir().join(format!("duw-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("sub")).unwrap();

        // 1 GB logical, almost nothing on disk.
        let sparse = fs::File::create(dir.join("sparse.img")).unwrap();
        sparse.set_len(1 << 30).unwrap();
        // 1 MB of real data, plus a hard link to it.
        let mut real = fs::File::create(dir.join("sub/real.bin")).unwrap();
        real.write_all(&vec![7u8; 1 << 20]).unwrap();
        real.sync_all().unwrap();
        fs::hard_link(dir.join("sub/real.bin"), dir.join("link.bin")).unwrap();

        let scan = scan(&dir, true, &Progress::default()).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        let tree = &scan.tree;
        let find = |name: &str| tree.nodes.iter().find(|n| n.name == name).unwrap();
        let img = find("sparse.img");
        assert_eq!(img.apparent, 1 << 30);
        assert!(img.alloc < 1 << 20, "sparse file alloc = {}", img.alloc);
        assert!(img.is_sparse());

        let root = &tree.nodes[Tree::ROOT];
        assert_eq!(root.files, 3);
        // The 1 MB file is counted once even though it has two names.
        assert!(root.alloc >= 1 << 20 && root.alloc < 3 << 20, "root alloc = {}", root.alloc);
        assert_eq!(scan.largest[0], tree.nodes.iter().position(|n| n.alloc >= 1 << 20 && !n.is_dir).unwrap());
    }

    #[test]
    fn removing_a_subtree_shrinks_ancestors_and_stats() {
        let dir = std::env::temp_dir().join(format!("duw-remove-{}", std::process::id()));
        fs::create_dir_all(dir.join("big/inner")).unwrap();
        fs::create_dir_all(dir.join("small")).unwrap();
        fs::write(dir.join("big/inner/a.bin"), vec![1u8; 3 << 20]).unwrap();
        fs::write(dir.join("big/b.bin"), vec![1u8; 1 << 20]).unwrap();
        fs::write(dir.join("small/c.txt"), vec![1u8; 2 << 20]).unwrap();

        let mut scan = scan(&dir, true, &Progress::default()).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        let id = |scan: &Scan, name: &str| scan.tree.nodes.iter().position(|n| n.name == name).unwrap();
        let (big, small, inner) = (id(&scan, "big"), id(&scan, "small"), id(&scan, "inner"));
        let root_before = scan.tree.nodes[Tree::ROOT].alloc;
        let inner_alloc = scan.tree.nodes[inner].alloc;
        assert_eq!(scan.tree.nodes[Tree::ROOT].children, vec![big, small]);

        scan.remove(inner);

        let nodes = &scan.tree.nodes;
        assert!(nodes[inner].removed);
        assert!(nodes[id(&scan, "a.bin")].removed);
        assert_eq!(nodes[Tree::ROOT].alloc, root_before - inner_alloc);
        assert_eq!(nodes[Tree::ROOT].files, 2);
        assert!(!nodes[big].children.contains(&inner));
        // "big" is now smaller than "small", so the root's children reorder.
        assert_eq!(nodes[Tree::ROOT].children, vec![small, big]);
        assert_eq!(scan.largest.len(), 2);
        assert!(!scan.largest.contains(&id(&scan, "a.bin")));
        assert_eq!(scan.types.iter().find(|t| t.ext == ".bin").unwrap().count, 1);
    }
}
