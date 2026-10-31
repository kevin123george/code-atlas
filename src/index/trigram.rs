//! Trigrams and posting lists, the core of the content index (decision 0014).
//!
//! A file's trigrams are the distinct 3-byte windows of its text with ASCII letters folded to
//! lower case; windows that cross a line break are left out (searches are single-line). A
//! query's trigrams must all be present in a file for it to match, so the index only narrows
//! the files to read: every candidate is checked against its real contents afterwards.
//! Posting lists are sorted file numbers, stored as varint deltas.

/// A trigram: three folded bytes packed into the low 24 bits.
pub type Tri = u32;

#[inline]
fn pack(a: u8, b: u8, c: u8) -> Tri {
    (a.to_ascii_lowercase() as u32) << 16 | (b.to_ascii_lowercase() as u32) << 8 | c.to_ascii_lowercase() as u32
}

/// Collects distinct trigrams with a 2^24-bit scratch set (2 MB), reused between files.
pub struct Extractor {
    bits: Vec<u64>,
    list: Vec<Tri>,
}

impl Default for Extractor {
    fn default() -> Self {
        Extractor { bits: vec![0; 1 << 18], list: Vec::new() }
    }
}

impl Extractor {
    /// The distinct trigrams of `data` (in first-seen order).
    pub fn extract(&mut self, data: &[u8]) -> &[Tri] {
        for &t in &self.list {
            self.bits[(t >> 6) as usize] = 0;
        }
        self.list.clear();
        if data.len() >= 3 {
            for w in data.windows(3) {
                if w.contains(&b'\n') {
                    continue;
                }
                let t = pack(w[0], w[1], w[2]);
                let (word, bit) = ((t >> 6) as usize, 1u64 << (t & 63));
                if self.bits[word] & bit == 0 {
                    self.bits[word] |= bit;
                    self.list.push(t);
                }
            }
        }
        &self.list
    }
}

/// The distinct trigrams a match of `query` must contain (none for queries under 3 bytes).
pub fn query_trigrams(query: &[u8]) -> Vec<Tri> {
    let mut v: Vec<Tri> = query.windows(3).filter(|w| !w.contains(&b'\n')).map(|w| pack(w[0], w[1], w[2])).collect();
    v.sort_unstable();
    v.dedup();
    v
}

pub fn put_varint(buf: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

/// Appends file `id` to a posting list whose last id was `last` (`None` = empty list).
pub fn push_posting(buf: &mut Vec<u8>, last: Option<u32>, id: u32) {
    put_varint(buf, last.map_or(id, |l| id - l));
}

/// Decodes a delta-varint posting list.
pub fn decode(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    let (mut cur, mut shift, mut acc, mut first) = (0u32, 0u32, 0u32, true);
    for &b in bytes {
        acc |= ((b & 0x7f) as u32) << shift;
        if b & 0x80 != 0 {
            shift += 7;
            continue;
        }
        cur = if first { acc } else { cur + acc };
        first = false;
        out.push(cur);
        acc = 0;
        shift = 0;
    }
    out
}

/// Sorted intersection of two sorted lists.
pub fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(small.len());
    if small.len() * 16 < big.len() {
        // Very different sizes: binary search the big one.
        let mut lo = 0;
        for &x in small {
            match big[lo..].binary_search(&x) {
                Ok(i) => {
                    out.push(x);
                    lo += i + 1;
                }
                Err(i) => lo += i,
            }
            if lo >= big.len() {
                break;
            }
        }
        return out;
    }
    let (mut i, mut j) = (0, 0);
    while i < small.len() && j < big.len() {
        match small[i].cmp(&big[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(small[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// FNV-1a, 64 bit: stable across runs and Rust versions (cache folder names, path keys).
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigrams_fold_case_and_skip_line_breaks() {
        let mut x = Extractor::default();
        let t: Vec<Tri> = x.extract(b"AbC\nabc").to_vec();
        assert_eq!(t, vec![pack(b'a', b'b', b'c')], "same trigram twice, none across the break");
        assert_eq!(query_trigrams(b"ABCD"), {
            let mut v = vec![pack(b'a', b'b', b'c'), pack(b'b', b'c', b'd')];
            v.sort();
            v
        });
        assert!(query_trigrams(b"ab").is_empty());
        // The scratch set is cleared between files.
        assert_eq!(x.extract(b"xyz").len(), 1);
        assert_eq!(x.extract(b"abc").to_vec(), vec![pack(b'a', b'b', b'c')]);
    }

    #[test]
    fn postings_round_trip() {
        let ids = [0u32, 1, 5, 127, 128, 300, 70_000, 1 << 30];
        let mut buf = Vec::new();
        let mut last = None;
        for &id in &ids {
            push_posting(&mut buf, last, id);
            last = Some(id);
        }
        assert_eq!(decode(&buf), ids);
        assert!(decode(&[]).is_empty());
    }

    #[test]
    fn intersection_both_ways() {
        let a: Vec<u32> = (0..1000).step_by(3).collect();
        let b: Vec<u32> = (0..1000).step_by(5).collect();
        let want: Vec<u32> = (0..1000).step_by(15).collect();
        assert_eq!(intersect(&a, &b), want);
        assert_eq!(intersect(&[15, 30, 999], &a), vec![15, 30, 999]);
        assert_eq!(intersect(&[2, 4], &a), Vec::<u32>::new());
    }
}
