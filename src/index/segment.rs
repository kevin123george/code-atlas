//! One immutable index segment on disk: a file table, the symbols of those files, and a
//! trigram → posting-list table (decision 0014).
//!
//! Layout (little endian): `header | path offsets + paths | file records | symbols | symbol
//! names | postings | trigram keys | trigram offsets`. Opening a segment reads everything but
//! the postings into memory; posting lists are read with `pread` when a query needs them, so a
//! big index costs page cache, not heap. Segments are never changed: updates write a new small
//! segment (the newest entry of a path wins, deletions are tombstone entries), and compaction
//! merges segments into a new one.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use super::trigram::{self, Tri};

const MAGIC: &[u8; 8] = b"CATLSEG1";
const VERSION: u32 = 1;
const HEADER: usize = 8 + 4 * 4 + 8 * 8;

/// The file was deleted (a tombstone: it hides older entries of the same path).
pub const F_DELETED: u8 = 1;
/// Only the name is indexed: binary, too big or unreadable.
pub const F_NO_CONTENT: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMeta {
    /// Modification time in nanoseconds since the epoch, and size: a changed pair = reindex.
    pub mtime: i64,
    pub size: u64,
    pub flags: u8,
}

/// A symbol as stored: its name is `names[name_off..][..name_len]`.
#[derive(Clone, Copy, Debug)]
pub struct Sym {
    pub name_off: u32,
    pub name_len: u16,
    pub kind: u8,
    pub file: u32,
    pub line: u32,
    pub col: u32,
    /// Letters (a–z folded, `_`, digits) anywhere in the name / at word starts, one bit each:
    /// a query is only scored against names that have all its letters.
    pub all: u32,
    pub starts: u32,
}

fn letter_bit(c: u8) -> u32 {
    match c.to_ascii_lowercase() {
        c @ b'a'..=b'z' => 1 << (c - b'a'),
        b'_' => 1 << 26,
        b'0'..=b'9' => 1 << 27,
        _ => 1 << 28,
    }
}

/// (all letters, word-start letters) of a name.
pub fn letter_masks(name: &[u8]) -> (u32, u32) {
    let (mut all, mut starts) = (0, 0);
    let mut prev = b'_';
    for (i, &c) in name.iter().enumerate() {
        all |= letter_bit(c);
        let start = i == 0 || ((prev == b'_' || prev == b'-' || prev == b'.' || prev == b'$') && c != b'_') || (prev.is_ascii_lowercase() && c.is_ascii_uppercase()) || (!prev.is_ascii_digit() && c.is_ascii_digit());
        if start {
            starts |= letter_bit(c);
        }
        prev = c;
    }
    (all, starts)
}

// ---- building --------------------------------------------------------------------------------

/// A segment being built in memory. Files get consecutive local numbers in the order they are
/// added, so every posting list is appended in increasing order.
#[derive(Default)]
pub struct Builder {
    path_offs: Vec<u32>,
    paths: Vec<u8>,
    files: Vec<FileMeta>,
    syms: Vec<Sym>,
    names: Vec<u8>,
    post: HashMap<Tri, (u32, Vec<u8>)>,
    /// Rough heap use, to flush before the memory budget is reached.
    pub mem: usize,
}

impl Builder {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Adds a file with its trigrams and symbols (name, kind, line, col).
    pub fn add(&mut self, path: &str, meta: FileMeta, tris: &[Tri], syms: &[super::symbols::Def]) -> u32 {
        let id = self.files.len() as u32;
        if self.path_offs.is_empty() {
            self.path_offs.push(0);
        }
        self.paths.extend_from_slice(path.as_bytes());
        self.path_offs.push(self.paths.len() as u32);
        self.files.push(meta);
        for &t in tris {
            let fresh = !self.post.contains_key(&t);
            let e = self.post.entry(t).or_insert((u32::MAX, Vec::new()));
            if fresh {
                self.mem += 48;
            }
            let before = e.1.capacity();
            trigram::push_posting(&mut e.1, (e.0 != u32::MAX).then_some(e.0), id);
            e.0 = id;
            self.mem += e.1.capacity() - before;
        }
        for d in syms {
            let bytes = d.name.as_bytes();
            let (all, starts) = letter_masks(bytes);
            self.syms.push(Sym { name_off: self.names.len() as u32, name_len: bytes.len().min(u16::MAX as usize) as u16, kind: d.kind as u8, file: id, line: d.line, col: d.col, all, starts });
            self.names.extend_from_slice(bytes);
        }
        self.mem += path.len() + 24 + syms.len() * 40;
        id
    }

    /// Writes the segment to `path` (via a temporary file and a rename).
    pub fn write(self, path: &Path) -> io::Result<()> {
        let mut post: Vec<(Tri, Vec<u8>)> = self.post.into_iter().map(|(k, (_, l))| (k, l)).collect();
        post.sort_unstable_by_key(|(k, _)| *k);
        let parts = Parts {
            path_offs: if self.path_offs.is_empty() { vec![0] } else { self.path_offs },
            paths: self.paths,
            files: self.files,
            syms: self.syms,
            names: self.names,
        };
        write_segment(path, &parts, post.into_iter().map(Ok))
    }
}

struct Parts {
    path_offs: Vec<u32>,
    paths: Vec<u8>,
    files: Vec<FileMeta>,
    syms: Vec<Sym>,
    names: Vec<u8>,
}

/// Writes a segment; `lists` gives (trigram, posting list) in increasing trigram order and is
/// streamed to disk (a merge never holds all lists at once).
fn write_segment(path: &Path, p: &Parts, lists: impl Iterator<Item = io::Result<(Tri, Vec<u8>)>>) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
    let nfiles = p.files.len() as u32;
    // Header placeholder: filled in once the offsets are known.
    w.write_all(&[0u8; HEADER])?;
    let mut pos = HEADER as u64;
    let mut offs = [0u64; 8];
    let put = |w: &mut BufWriter<File>, b: &[u8], pos: &mut u64| -> io::Result<()> {
        w.write_all(b)?;
        *pos += b.len() as u64;
        Ok(())
    };
    // paths
    offs[0] = pos;
    for o in &p.path_offs {
        put(&mut w, &o.to_le_bytes(), &mut pos)?;
    }
    put(&mut w, &p.paths, &mut pos)?;
    // files
    offs[1] = pos;
    for f in &p.files {
        let mut b = [0u8; 17];
        b[..8].copy_from_slice(&f.mtime.to_le_bytes());
        b[8..16].copy_from_slice(&f.size.to_le_bytes());
        b[16] = f.flags;
        put(&mut w, &b, &mut pos)?;
    }
    // symbols
    offs[2] = pos;
    for s in &p.syms {
        let mut b = [0u8; 20];
        b[..4].copy_from_slice(&s.name_off.to_le_bytes());
        b[4..6].copy_from_slice(&s.name_len.to_le_bytes());
        b[6] = s.kind;
        b[8..12].copy_from_slice(&s.file.to_le_bytes());
        b[12..16].copy_from_slice(&s.line.to_le_bytes());
        b[16..20].copy_from_slice(&s.col.to_le_bytes());
        put(&mut w, &b, &mut pos)?;
    }
    offs[3] = pos;
    put(&mut w, &p.names, &mut pos)?;
    // postings, streamed
    offs[4] = pos;
    let mut list_offs = Vec::new();
    let mut keys = Vec::new();
    let mut rel = 0u64;
    for l in lists {
        let (k, l) = l?;
        keys.push(k);
        list_offs.push(rel);
        put(&mut w, &l, &mut pos)?;
        rel += l.len() as u64;
    }
    list_offs.push(rel);
    offs[5] = pos;
    for k in &keys {
        put(&mut w, &k.to_le_bytes(), &mut pos)?;
    }
    offs[6] = pos;
    for o in &list_offs {
        put(&mut w, &o.to_le_bytes(), &mut pos)?;
    }
    offs[7] = pos;
    let mut f = w.into_inner().map_err(|e| e.into_error())?;
    let mut h = Vec::with_capacity(HEADER);
    h.extend_from_slice(MAGIC);
    h.extend_from_slice(&VERSION.to_le_bytes());
    h.extend_from_slice(&nfiles.to_le_bytes());
    h.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    h.extend_from_slice(&(p.syms.len() as u32).to_le_bytes());
    for o in offs {
        h.extend_from_slice(&o.to_le_bytes());
    }
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&h)?;
    f.sync_all().ok();
    drop(f);
    std::fs::rename(&tmp, path)
}

// ---- reading ---------------------------------------------------------------------------------

pub struct Segment {
    /// Sequence number (newer segments have higher ones).
    pub seq: u64,
    file: File,
    post_off: u64,
    path_offs: Vec<u32>,
    paths: Vec<u8>,
    pub meta: Vec<FileMeta>,
    /// Sorted by file.
    pub syms: Vec<Sym>,
    names: Vec<u8>,
    keys: Vec<Tri>,
    offs: Vec<u64>,
    pub disk_bytes: u64,
}

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, off)
}

#[cfg(not(unix))]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_read(&mut buf[done..], off + done as u64)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        done += n;
    }
    Ok(())
}

impl Segment {
    pub fn open(path: &Path, seq: u64) -> io::Result<Segment> {
        let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {what}", path.display()));
        let mut file = File::open(path)?;
        let disk_bytes = file.metadata()?.len();
        let mut h = [0u8; HEADER];
        file.read_exact(&mut h)?;
        if &h[..8] != MAGIC || rd_u32(&h, 8) != VERSION {
            return Err(bad("not a catlas index segment of this version"));
        }
        let (nfiles, ntri, nsym) = (rd_u32(&h, 12) as usize, rd_u32(&h, 16) as usize, rd_u32(&h, 20) as usize);
        let o: Vec<u64> = (0..8).map(|i| rd_u64(&h, 24 + i * 8)).collect();
        if o.windows(2).any(|w| w[0] > w[1]) || o[7] != disk_bytes || o[0] != HEADER as u64 {
            return Err(bad("broken offsets"));
        }
        // Everything before the postings, then the trigram table after them.
        let mut front = vec![0u8; (o[4] - o[0]) as usize];
        read_at(&file, &mut front, o[0])?;
        let mut back = vec![0u8; (o[7] - o[5]) as usize];
        read_at(&file, &mut back, o[5])?;
        let base = o[0] as usize;
        let at = |x: u64| (x as usize) - base;
        let po = at(o[0]);
        if (at(o[1]) - po) < (nfiles + 1) * 4 || at(o[2]) - at(o[1]) != nfiles * 17 || at(o[3]) - at(o[2]) != nsym * 20 {
            return Err(bad("table sizes"));
        }
        let path_offs: Vec<u32> = (0..=nfiles).map(|i| rd_u32(&front, po + i * 4)).collect();
        let paths = front[po + (nfiles + 1) * 4..at(o[1])].to_vec();
        if path_offs.windows(2).any(|w| w[0] > w[1]) || *path_offs.last().unwrap_or(&0) as usize != paths.len() {
            return Err(bad("paths"));
        }
        let meta = (0..nfiles)
            .map(|i| {
                let b = at(o[1]) + i * 17;
                FileMeta { mtime: rd_u64(&front, b) as i64, size: rd_u64(&front, b + 8), flags: front[b + 16] }
            })
            .collect();
        let names = front[at(o[3])..].to_vec();
        let mut syms = Vec::with_capacity(nsym);
        for i in 0..nsym {
            let b = at(o[2]) + i * 20;
            let name_off = rd_u32(&front, b);
            let name_len = u16::from_le_bytes([front[b + 4], front[b + 5]]);
            let end = name_off as usize + name_len as usize;
            if end > names.len() {
                return Err(bad("symbol names"));
            }
            let (all, starts) = letter_masks(&names[name_off as usize..end]);
            let file = rd_u32(&front, b + 8);
            if file as usize >= nfiles {
                return Err(bad("symbol file"));
            }
            syms.push(Sym { name_off, name_len, kind: front[b + 6], file, line: rd_u32(&front, b + 12), col: rd_u32(&front, b + 16), all, starts });
        }
        let keys: Vec<Tri> = (0..ntri).map(|i| rd_u32(&back, i * 4)).collect();
        let ob = (o[6] - o[5]) as usize;
        if ob != ntri * 4 || back.len() - ob != (ntri + 1) * 8 {
            return Err(bad("trigram table"));
        }
        let offs: Vec<u64> = (0..=ntri).map(|i| rd_u64(&back, ob + i * 8)).collect();
        if offs.windows(2).any(|w| w[0] > w[1]) || offs.last().copied().unwrap_or(0) != o[5] - o[4] {
            return Err(bad("posting offsets"));
        }
        Ok(Segment { seq, file, post_off: o[4], path_offs, paths, meta, syms, names, keys, offs, disk_bytes })
    }

    pub fn len(&self) -> usize {
        self.meta.len()
    }

    pub fn path(&self, i: usize) -> &str {
        let (s, e) = (self.path_offs[i] as usize, self.path_offs[i + 1] as usize);
        std::str::from_utf8(&self.paths[s..e]).unwrap_or("")
    }

    pub fn trigrams(&self) -> usize {
        self.keys.len()
    }

    /// Byte length of trigram `t`'s posting list (`None`: no file has it).
    pub fn posting_bytes(&self, t: Tri) -> Option<u64> {
        let i = self.keys.binary_search(&t).ok()?;
        Some(self.offs[i + 1] - self.offs[i])
    }

    /// The files holding trigram `t` (empty when none does).
    pub fn postings(&self, t: Tri) -> io::Result<Vec<u32>> {
        let Ok(i) = self.keys.binary_search(&t) else { return Ok(Vec::new()) };
        self.list_at(i)
    }

    fn list_at(&self, i: usize) -> io::Result<Vec<u32>> {
        let (s, e) = (self.offs[i], self.offs[i + 1]);
        let mut buf = vec![0u8; (e - s) as usize];
        read_at(&self.file, &mut buf, self.post_off + s)?;
        Ok(trigram::decode(&buf))
    }

    pub fn sym_name(&self, s: &Sym) -> &str {
        std::str::from_utf8(&self.names[s.name_off as usize..s.name_off as usize + s.name_len as usize]).unwrap_or("")
    }

    /// The symbols of local file `file` (symbols are stored in file order).
    pub fn file_syms(&self, file: u32) -> &[Sym] {
        let lo = self.syms.partition_point(|s| s.file < file);
        let hi = self.syms.partition_point(|s| s.file <= file);
        &self.syms[lo..hi]
    }

    /// Rough heap use of the loaded tables.
    pub fn heap_bytes(&self) -> usize {
        self.path_offs.len() * 4 + self.paths.len() + self.meta.len() * 24 + self.syms.len() * std::mem::size_of::<Sym>() + self.names.len() + self.keys.len() * 4 + self.offs.len() * 8
    }
}

/// Merges `segs` (oldest first) into one new segment at `out`, keeping the entries `keep`
/// says yes to (per segment, per local file). Returns the old → new file numbers per segment.
pub fn merge(segs: &[&Segment], keep: &[Vec<bool>], out: &Path) -> io::Result<Vec<Vec<u32>>> {
    let mut remap: Vec<Vec<u32>> = Vec::with_capacity(segs.len());
    let mut parts = Parts { path_offs: vec![0], paths: Vec::new(), files: Vec::new(), syms: Vec::new(), names: Vec::new() };
    for (si, s) in segs.iter().enumerate() {
        let mut m = vec![u32::MAX; s.len()];
        for (i, slot) in m.iter_mut().enumerate() {
            if keep[si][i] {
                *slot = parts.files.len() as u32;
                parts.paths.extend_from_slice(s.path(i).as_bytes());
                parts.path_offs.push(parts.paths.len() as u32);
                parts.files.push(s.meta[i]);
            }
        }
        for sym in &s.syms {
            let nf = m[sym.file as usize];
            if nf == u32::MAX {
                continue;
            }
            let name = s.sym_name(sym).as_bytes();
            parts.syms.push(Sym { name_off: parts.names.len() as u32, file: nf, ..*sym });
            parts.names.extend_from_slice(name);
        }
        remap.push(m);
    }
    // k-way walk over the sorted trigram keys, one merged list at a time.
    let mut keys: Vec<Tri> = segs.iter().flat_map(|s| s.keys.iter().copied()).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut cursor = vec![0usize; segs.len()];
    let remap_ref = &remap;
    let lists = keys.into_iter().filter_map(move |k| {
        let mut buf = Vec::new();
        let mut last = None;
        for (si, s) in segs.iter().enumerate() {
            let c = &mut cursor[si];
            if *c < s.keys.len() && s.keys[*c] == k {
                match s.list_at(*c) {
                    Ok(ids) => {
                        for id in ids {
                            let nid = remap_ref[si][id as usize];
                            if nid != u32::MAX {
                                trigram::push_posting(&mut buf, last, nid);
                                last = Some(nid);
                            }
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
                *c += 1;
            }
        }
        (!buf.is_empty()).then_some(Ok((k, buf)))
    });
    write_segment(out, &parts, lists)?;
    Ok(remap)
}

#[cfg(test)]
mod tests {
    use super::super::symbols::{Def, Kind};
    use super::super::trigram::{query_trigrams, Extractor};
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("catlas-seg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn build(files: &[(&str, &str)]) -> Builder {
        let mut b = Builder::default();
        let mut x = Extractor::default();
        for (p, text) in files {
            let defs = vec![Def { name: format!("def_{p}"), kind: Kind::Function, line: 1, col: 2 }];
            b.add(p, FileMeta { mtime: 7, size: text.len() as u64, flags: 0 }, x.extract(text.as_bytes()), &defs);
        }
        b
    }

    fn files_with(s: &Segment, q: &str) -> Vec<String> {
        let mut ids: Option<Vec<u32>> = None;
        for t in query_trigrams(q.as_bytes()) {
            let p = s.postings(t).unwrap();
            ids = Some(match ids {
                None => p,
                Some(i) => trigram::intersect(&i, &p),
            });
        }
        ids.unwrap_or_default().into_iter().map(|i| s.path(i as usize).to_string()).collect()
    }

    #[test]
    fn write_open_query() {
        let d = scratch("rw");
        let p = d.join("a.seg");
        build(&[("src/a.rs", "fn alpha() {}"), ("src/b.rs", "fn beta() { alpha() }"), ("c.md", "Gamma")]).write(&p).unwrap();
        let s = Segment::open(&p, 1).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s.path(1), "src/b.rs");
        assert_eq!(s.meta[2], FileMeta { mtime: 7, size: 5, flags: 0 });
        assert_eq!(files_with(&s, "alpha"), vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(files_with(&s, "GAMMA"), vec!["c.md"], "folded case");
        assert!(files_with(&s, "zzz").is_empty());
        assert_eq!(s.file_syms(1).len(), 1);
        assert_eq!(s.sym_name(&s.file_syms(1)[0]), "def_src/b.rs");
        // A truncated file is refused, not misread.
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(d.join("bad.seg"), &bytes[..bytes.len() - 3]).unwrap();
        assert!(Segment::open(&d.join("bad.seg"), 2).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn empty_segment_round_trips() {
        let d = scratch("empty");
        Builder::default().write(&d.join("e.seg")).unwrap();
        let s = Segment::open(&d.join("e.seg"), 1).unwrap();
        assert_eq!((s.len(), s.trigrams()), (0, 0));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn merge_keeps_selected_entries_and_postings() {
        let d = scratch("merge");
        build(&[("a", "shared one"), ("b", "only in b")]).write(&d.join("1.seg")).unwrap();
        build(&[("c", "shared two"), ("a", "replaced a")]).write(&d.join("2.seg")).unwrap();
        let (s1, s2) = (Segment::open(&d.join("1.seg"), 1).unwrap(), Segment::open(&d.join("2.seg"), 2).unwrap());
        // The old "a" is replaced by the newer one.
        let keep = vec![vec![false, true], vec![true, true]];
        let remap = merge(&[&s1, &s2], &keep, &d.join("3.seg")).unwrap();
        assert_eq!(remap, vec![vec![u32::MAX, 0], vec![1, 2]]);
        let m = Segment::open(&d.join("3.seg"), 3).unwrap();
        assert_eq!((0..m.len()).map(|i| m.path(i)).collect::<Vec<_>>(), vec!["b", "c", "a"]);
        assert_eq!(files_with(&m, "shared"), vec!["c"]);
        assert_eq!(files_with(&m, "replaced"), vec!["a"]);
        assert_eq!(files_with(&m, "only in"), vec!["b"]);
        assert_eq!(m.syms.len(), 3);
        assert_eq!(m.sym_name(&m.file_syms(2)[0]), "def_a");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn letter_masks_mark_word_starts() {
        let (all, starts) = letter_masks(b"getUserName");
        assert_eq!(starts, letter_bit(b'g') | letter_bit(b'u') | letter_bit(b'n'));
        assert!(all & letter_bit(b'r') != 0 && all & letter_bit(b'z') == 0);
        let (_, s) = letter_masks(b"max_len");
        assert_eq!(s, letter_bit(b'm') | letter_bit(b'l'));
    }
}
