//! Translated shaders kept on disk, so an area entered again, or the same area in a later run, does not translate its shaders again.
//!
//! The folder is `<cache>/translations/<SHADER_SOURCE>`: the hash of the translator's and the graphics code's sources (`build.rs`), so a
//! build whose translation could differ never reads another build's. `translations.txt` has one line per translation: its key (a hash of
//! the program bytes and of everything else the translation depends on), the hash of its WGSL (`shaders/<id>.wgsl`, the same names as
//! the pipeline cache's), and its interface, with a check hash at the end. A line that does not check out, or whose WGSL is missing or
//! does not hash to its id, is not used (the shader is translated as if it were not there).

use mh3u_latte::translate::{Interface, TexKind};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The most translations read back at a start.
const LIMIT: usize = 50_000;

fn kind_number(k: TexKind) -> u32 {
    match k {
        TexKind::D2 => 0,
        TexKind::D3 => 1,
        TexKind::Cube => 2,
        TexKind::Depth2d => 3,
    }
}

fn kind_of(n: u32) -> Option<TexKind> {
    Some(match n {
        0 => TexKind::D2,
        1 => TexKind::D3,
        2 => TexKind::Cube,
        3 => TexKind::Depth2d,
        _ => return None,
    })
}

/// An interface as numbers: each set or map as its length and its items, then the single values.
pub(crate) fn encode_interface(i: &Interface) -> Vec<u32> {
    let mut v = Vec::new();
    v.push(i.banks.len() as u32);
    for (b, n) in &i.banks {
        v.extend([*b, *n]);
    }
    v.push(i.textures.len() as u32);
    for (r, k) in &i.textures {
        v.extend([*r, kind_number(*k)]);
    }
    for set in [&i.samplers, &i.compare_samplers, &i.colour_outputs, &i.params] {
        v.push(set.len() as u32);
        v.extend(set.iter().copied());
    }
    v.extend([
        i.const_file,
        u32::from(i.depth_output),
        u32::from(i.kills),
        u32::from(i.position),
        i.ring_dwords,
        u32::from(i.reads_ring),
    ]);
    v
}

pub(crate) fn decode_interface(v: &[u32]) -> Option<Interface> {
    let mut it = v.iter().copied();
    let mut next = || it.next();
    let mut banks = BTreeMap::new();
    for _ in 0..next()?.min(4096) {
        banks.insert(next()?, next()?);
    }
    let mut textures = BTreeMap::new();
    for _ in 0..next()?.min(4096) {
        textures.insert(next()?, kind_of(next()?)?);
    }
    let mut sets: [BTreeSet<u32>; 4] = Default::default();
    for set in &mut sets {
        for _ in 0..next()?.min(4096) {
            set.insert(next()?);
        }
    }
    let [samplers, compare_samplers, colour_outputs, params] = sets;
    let const_file = next()?;
    let flag = |x: u32| (x <= 1).then_some(x == 1);
    let depth_output = flag(next()?)?;
    let kills = flag(next()?)?;
    let position = flag(next()?)?;
    let ring_dwords = next()?;
    let reads_ring = flag(next()?)?;
    if next().is_some() {
        return None;
    }
    Some(Interface {
        banks,
        const_file,
        textures,
        samplers,
        compare_samplers,
        colour_outputs,
        depth_output,
        kills,
        position,
        params,
        ring_dwords,
        reads_ring,
    })
}

/// One line of `translations.txt`: (key, WGSL id, interface).
pub(crate) fn parse_line(line: &str) -> Option<(u64, u64, Interface)> {
    let (body, check) = line.rsplit_once(' ')?;
    if u64::from_str_radix(check, 16).ok()? != xxhash_rust::xxh3::xxh3_64(body.as_bytes()) {
        return None;
    }
    let mut it = body.split_ascii_whitespace();
    if it.next()? != "t1" {
        return None;
    }
    let key = u64::from_str_radix(it.next()?, 16).ok()?;
    let id = u64::from_str_radix(it.next()?, 16).ok()?;
    let words: Option<Vec<u32>> = it.map(|w| u32::from_str_radix(w, 16).ok()).collect();
    Some((key, id, decode_interface(&words?)?))
}

pub(crate) fn line(key: u64, id: u64, interface: &Interface) -> String {
    let mut body = format!("t1 {key:016x} {id:016x}");
    for w in encode_interface(interface) {
        body.push_str(&format!(" {w:x}"));
    }
    let check = xxhash_rust::xxh3::xxh3_64(body.as_bytes());
    format!("{body} {check:016x}\n")
}

/// What a translation found on disk gives: its WGSL, the WGSL's hash, and its interface.
pub(crate) struct Found {
    pub wgsl: Arc<str>,
    pub id: u64,
    pub interface: Interface,
}

#[derive(Default)]
pub(crate) struct Translations {
    dir: Option<PathBuf>,
    list: Option<std::fs::File>,
    known: HashMap<u64, (u64, Interface)>,
    /// Looked up and found, looked up and not there (then translated and written down), and read at the start.
    pub hits: u64,
    pub misses: u64,
    pub loaded: usize,
}

impl Translations {
    /// Opens (or makes) the folder for this build's translator under `root`; returns it and how many translations it has.
    pub(crate) fn open(&mut self, root: &Path) -> std::io::Result<(PathBuf, usize)> {
        let dir = root.join("translations").join(crate::SHADER_SOURCE);
        std::fs::create_dir_all(dir.join("shaders"))?;
        let path = dir.join("translations.txt");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for l in text.lines() {
            if self.known.len() >= LIMIT {
                break;
            }
            if let Some((key, id, interface)) = parse_line(l) {
                self.known.insert(key, (id, interface));
            }
        }
        self.list = Some(std::fs::OpenOptions::new().create(true).append(true).open(&path)?);
        self.dir = Some(dir.clone());
        self.loaded = self.known.len();
        Ok((dir, self.loaded))
    }

    pub(crate) fn is_open(&self) -> bool {
        self.dir.is_some()
    }

    /// The translation kept under `key`, if there is one and its WGSL is there and whole.
    pub(crate) fn get(&mut self, key: u64) -> Option<Found> {
        let dir = self.dir.as_ref()?;
        let Some((id, interface)) = self.known.get(&key) else {
            self.misses += 1;
            return None;
        };
        let wgsl = std::fs::read_to_string(dir.join("shaders").join(format!("{id:016x}.wgsl"))).ok();
        match wgsl {
            Some(w) if xxhash_rust::xxh3::xxh3_64(w.as_bytes()) == *id => {
                self.hits += 1;
                Some(Found {
                    wgsl: w.into(),
                    id: *id,
                    interface: interface.clone(),
                })
            }
            _ => {
                self.known.remove(&key);
                self.misses += 1;
                None
            }
        }
    }

    /// Writes down a translation made now (its WGSL first, whole under another name and renamed, then its line).
    pub(crate) fn record(&mut self, key: u64, id: u64, wgsl: &str, interface: &Interface) {
        let (Some(dir), Some(list)) = (&self.dir, &mut self.list) else {
            return;
        };
        if self.known.contains_key(&key) {
            return;
        }
        let path = dir.join("shaders").join(format!("{id:016x}.wgsl"));
        if !path.exists() {
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            if std::fs::write(&tmp, wgsl).is_err() || std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
                return;
            }
        }
        if list.write_all(line(key, id, interface).as_bytes()).is_ok() {
            self.known.insert(key, (id, interface.clone()));
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.known.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface() -> Interface {
        let mut i = Interface::default();
        i.banks.insert(0, 16);
        i.banks.insert(3, 4096);
        i.textures.insert(1, TexKind::Cube);
        i.textures.insert(4, TexKind::Depth2d);
        i.samplers.extend([1, 4]);
        i.compare_samplers.insert(4);
        i.colour_outputs.extend([0, 1]);
        i.params.extend([0, 2, 5]);
        i.const_file = 7;
        i.kills = true;
        i.position = true;
        i.ring_dwords = 12;
        i
    }

    #[test]
    fn an_interface_goes_through_numbers_and_back() {
        let i = interface();
        let back = decode_interface(&encode_interface(&i)).expect("decoded");
        assert_eq!(format!("{back:?}"), format!("{i:?}"));
        // cut short, a word too many, a flag that is not 0 or 1
        let words = encode_interface(&i);
        assert!(decode_interface(&words[..words.len() - 1]).is_none());
        assert!(decode_interface(&[words.clone(), vec![0]].concat()).is_none());
        let mut bad = words.clone();
        let n = bad.len();
        bad[n - 1] = 2;
        assert!(decode_interface(&bad).is_none());
    }

    #[test]
    fn a_line_is_used_only_when_it_checks_out() {
        let i = interface();
        let l = line(0x1234, 0xabcd, &i);
        let (key, id, back) = parse_line(l.trim_end()).expect("parsed");
        assert_eq!((key, id), (0x1234, 0xabcd));
        assert_eq!(format!("{back:?}"), format!("{i:?}"));
        assert!(parse_line(&l.trim_end().replacen("1234 ", "1235 ", 1)).is_none(), "an edited key");
        assert!(parse_line(&l[..l.len() - 5]).is_none(), "cut short");
    }

    #[test]
    fn a_translation_written_down_is_found_at_the_next_start() {
        let root = std::env::temp_dir().join(format!("mh3u-translations-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let wgsl = "@fragment fn main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        let id = xxhash_rust::xxh3::xxh3_64(wgsl.as_bytes());
        {
            let mut t = Translations::default();
            assert_eq!(t.open(&root).unwrap().1, 0);
            assert!(t.get(7).is_none());
            t.record(7, id, wgsl, &interface());
            t.record(7, id, wgsl, &interface());
        }
        let mut t = Translations::default();
        let (dir, n) = t.open(&root).unwrap();
        assert_eq!(n, 1, "written once");
        let found = t.get(7).expect("found");
        assert_eq!((found.id, &*found.wgsl), (id, wgsl));
        assert_eq!(format!("{:?}", found.interface), format!("{:?}", interface()));
        assert_eq!((t.hits, t.misses), (1, 0));
        // a WGSL file changed on disk is not used
        std::fs::write(dir.join("shaders").join(format!("{id:016x}.wgsl")), "changed").unwrap();
        let mut t = Translations::default();
        t.open(&root).unwrap();
        assert!(t.get(7).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
