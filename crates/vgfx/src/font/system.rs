//! The index of system fonts (vault/CONTRACT.md D-15): every OpenType
//! file under `/usr/share/fonts`, without fontconfig. Scanning reads only
//! the table directory and the `head`, `name`, `OS/2` and `post` tables of
//! each face; whole files load on first use. Families resolve by name or
//! by the generic families of D-15, faces by [`super::matching`], and
//! characters a face lacks fall back to the installed Noto fonts.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::matching::{Descriptor, Request, Style, StyleRange, best_faces};
use super::read::{Tag, u16_at, u32_at};
use super::{Face, Names, cmap, parse_names};

/// Where the system fonts are (D-15).
pub const SYSTEM_FONT_DIR: &str = "/usr/share/fonts";

/// Default font sizes of D-15, in CSS pixels.
pub const DEFAULT_FONT_SIZE: f32 = 16.0;
pub const DEFAULT_MONOSPACE_FONT_SIZE: f32 = 13.0;

/// A generic family of CSS Fonts 4 §4.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Generic {
    Serif,
    SansSerif,
    Monospace,
    Cursive,
    Fantasy,
    SystemUi,
}

impl Generic {
    pub fn from_keyword(s: &str) -> Option<Generic> {
        Some(match s.to_ascii_lowercase().as_str() {
            "serif" => Generic::Serif,
            "sans-serif" => Generic::SansSerif,
            "monospace" => Generic::Monospace,
            "cursive" => Generic::Cursive,
            "fantasy" => Generic::Fantasy,
            "system-ui" => Generic::SystemUi,
            _ => return None,
        })
    }

    /// The family D-15 maps the generic to.
    pub fn family(self) -> &'static str {
        match self {
            Generic::Serif => "Liberation Serif",
            Generic::Monospace => "Liberation Mono",
            Generic::SansSerif | Generic::Cursive | Generic::Fantasy | Generic::SystemUi => {
                "Liberation Sans"
            }
        }
    }
}

/// One entry of a `font-family` list.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Named(String),
    Generic(Generic),
}

/// One face of a font file on disk.
#[derive(Debug)]
pub struct SystemFace {
    pub path: PathBuf,
    pub index: u32,
    pub names: Names,
    pub weight: u16,
    pub stretch: f32,
    pub italic: bool,
    pub oblique: bool,
    pub monospace: bool,
    /// Offset and length of `cmap` in the file (for fallback checks
    /// without loading the face).
    cmap_at: (u64, u32),
    cmap: OnceLock<Option<(Vec<u8>, cmap::Cmap)>>,
    face: OnceLock<Option<Arc<Face>>>,
}

impl SystemFace {
    pub fn descriptor(&self) -> Descriptor {
        let w = self.weight as f32;
        Descriptor {
            weight: (w, w),
            stretch: (self.stretch, self.stretch),
            style: if self.italic {
                StyleRange::Italic
            } else if self.oblique {
                StyleRange::Oblique(14.0, 14.0)
            } else {
                StyleRange::Normal
            },
        }
    }

    /// The family names this face answers to, lowercase.
    fn family_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in [&self.names.typographic_family, &self.names.family]
            .into_iter()
            .flatten()
        {
            let k = n.to_lowercase();
            if !out.contains(&k) {
                out.push(k);
            }
        }
        out
    }

    /// Whether `cmap` maps `cp`, reading only that table.
    pub fn has_char(&self, cp: u32) -> bool {
        let entry = self.cmap.get_or_init(|| {
            let bytes = read_range(&self.path, self.cmap_at.0, self.cmap_at.1 as usize)?;
            let c = cmap::Cmap::parse(&bytes);
            Some((bytes, c))
        });
        match entry {
            Some((bytes, c)) => c.glyph(bytes, cp).is_some(),
            None => false,
        }
    }

    /// The parsed face (the whole file is read once and shared with the
    /// other faces of a collection only through this entry).
    pub fn load(&self) -> Option<Arc<Face>> {
        self.face
            .get_or_init(|| {
                let bytes = std::fs::read(&self.path).ok()?;
                Face::from_shared(Arc::new(bytes), self.index)
                    .ok()
                    .map(Arc::new)
            })
            .clone()
    }
}

fn read_range(path: &Path, off: u64, len: usize) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut buf = vec![0; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn table(f: &mut File, dir: &[(u32, u32, u32)], tag: Tag) -> Option<Vec<u8>> {
    let &(_, off, len) = dir.iter().find(|t| t.0 == tag.0)?;
    if len > 1 << 20 {
        return None;
    }
    let mut buf = vec![0; len as usize];
    f.seek(SeekFrom::Start(off as u64)).ok()?;
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Reads the faces of one file from its headers.
fn scan_file(path: &Path) -> Vec<SystemFace> {
    let mut out = Vec::new();
    let Ok(mut f) = File::open(path) else {
        return out;
    };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() {
        return out;
    }
    let starts: Vec<u32> = match u32_at(&head, 0) {
        Some(0x7474_6366) => {
            let n = u32_at(&head, 8).unwrap_or(0).min(256);
            let mut offs = vec![0u8; 4 * n as usize];
            if f.read_exact(&mut offs).is_err() {
                return out;
            }
            offs.chunks_exact(4)
                .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }
        Some(0x0001_0000) | Some(0x4f54_544f) | Some(0x7472_7565) => vec![0],
        _ => return out,
    };
    for (index, start) in starts.into_iter().enumerate() {
        let mut offset_table = [0u8; 12];
        if f.seek(SeekFrom::Start(start as u64)).is_err()
            || f.read_exact(&mut offset_table).is_err()
        {
            continue;
        }
        let n = u16_at(&offset_table, 4).unwrap_or(0) as usize;
        let mut records = vec![0u8; 16 * n];
        if f.read_exact(&mut records).is_err() {
            continue;
        }
        let dir: Vec<(u32, u32, u32)> = records
            .chunks_exact(16)
            .map(|r| {
                (
                    u32::from_be_bytes([r[0], r[1], r[2], r[3]]),
                    u32::from_be_bytes([r[8], r[9], r[10], r[11]]),
                    u32::from_be_bytes([r[12], r[13], r[14], r[15]]),
                )
            })
            .collect();
        let Some(&(_, cmap_off, cmap_len)) = dir.iter().find(|t| t.0 == Tag::new(b"cmap").0) else {
            continue;
        };
        let names = table(&mut f, &dir, Tag::new(b"name"))
            .map(|n| parse_names(&n))
            .unwrap_or_default();
        if names.css_family().is_none() {
            continue;
        }
        let mac_style = table(&mut f, &dir, Tag::new(b"head"))
            .and_then(|h| u16_at(&h, 44))
            .unwrap_or(0);
        let os2 = table(&mut f, &dir, Tag::new(b"OS/2"));
        let monospace = table(&mut f, &dir, Tag::new(b"post"))
            .and_then(|p| u32_at(&p, 12))
            .unwrap_or(0)
            != 0;
        let fs_selection = os2.as_deref().and_then(|o| u16_at(o, 62)).unwrap_or(0);
        let weight = os2
            .as_deref()
            .and_then(|o| u16_at(o, 4))
            .filter(|w| (1..=1000).contains(w))
            .unwrap_or(if mac_style & 1 != 0 { 700 } else { 400 });
        let width = os2
            .as_deref()
            .and_then(|o| u16_at(o, 6))
            .filter(|w| (1..=9).contains(w))
            .unwrap_or(5);
        let italic = if os2.is_some() {
            fs_selection & 1 != 0
        } else {
            mac_style & 2 != 0
        };
        out.push(SystemFace {
            path: path.to_path_buf(),
            index: index as u32,
            names,
            weight,
            stretch: STRETCH[(width - 1) as usize],
            italic,
            oblique: fs_selection & (1 << 9) != 0,
            monospace,
            cmap_at: (cmap_off as u64, cmap_len),
            cmap: OnceLock::new(),
            face: OnceLock::new(),
        });
    }
    out
}

/// `font-stretch` percentages of `usWidthClass` 1–9.
const STRETCH: [f32; 9] = [50.0, 62.5, 75.0, 87.5, 100.0, 112.5, 125.0, 150.0, 200.0];

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if ft.is_dir() || (ft.is_symlink() && p.is_dir()) {
            collect_files(&p, out);
        } else {
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
                out.push(p);
            }
        }
    }
}

/// A loaded `@font-face` (or `FontFace`) of a document.
#[derive(Clone, Debug)]
pub struct WebFace {
    pub family: String,
    pub descriptor: Descriptor,
    /// `unicode-range` as inclusive ranges (empty = all of Unicode).
    pub unicode_range: Vec<(u32, u32)>,
    pub face: Arc<Face>,
}

impl WebFace {
    pub fn covers(&self, cp: u32) -> bool {
        self.unicode_range.is_empty() || self.unicode_range.iter().any(|&(a, b)| cp >= a && cp <= b)
    }
}

/// The face chosen for a character.
#[derive(Clone, Debug)]
pub enum Selected {
    Web(Arc<Face>),
    /// An index into [`FontIndex::faces`].
    System(usize),
}

impl Selected {
    pub fn face(&self, index: &FontIndex) -> Option<Arc<Face>> {
        match self {
            Selected::Web(f) => Some(f.clone()),
            Selected::System(i) => index.load(*i),
        }
    }
}

/// The index: faces by family, plus a cache of fallback answers.
pub struct FontIndex {
    pub faces: Vec<SystemFace>,
    families: HashMap<String, Vec<usize>>,
    /// Noto families in fallback order, each with its faces.
    noto: Vec<Vec<usize>>,
    fallback_cache: Mutex<HashMap<(u32, u8), Option<usize>>>,
}

impl FontIndex {
    /// Scans `dir` recursively. Files are sorted by path so the index
    /// does not depend on directory order.
    pub fn scan(dir: &Path) -> FontIndex {
        let mut files = Vec::new();
        collect_files(dir, &mut files);
        files.sort();
        let faces: Vec<SystemFace> = files.iter().flat_map(|p| scan_file(p)).collect();
        FontIndex::from_faces(faces)
    }

    fn from_faces(faces: Vec<SystemFace>) -> FontIndex {
        let mut families: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in faces.iter().enumerate() {
            for k in f.family_keys() {
                families.entry(k).or_default().push(i);
            }
        }
        let mut noto: Vec<(String, Vec<usize>)> = families
            .iter()
            .filter(|(k, _)| k.starts_with("noto "))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Fallback order: Noto Sans, the other Noto Sans families, Noto
        // Serif and its families, then the rest; by name inside a rank.
        let rank = |k: &str| match k {
            "noto sans" => 0,
            _ if k.starts_with("noto sans ") && !k.contains("mono") => 1,
            "noto serif" => 2,
            _ if k.starts_with("noto serif ") => 3,
            _ if k.contains("emoji") => 5,
            _ => 4,
        };
        noto.sort_by(|a, b| (rank(&a.0), &a.0).cmp(&(rank(&b.0), &b.0)));
        FontIndex {
            faces,
            families,
            noto: noto.into_iter().map(|e| e.1).collect(),
            fallback_cache: Mutex::new(HashMap::new()),
        }
    }

    /// The index of [`SYSTEM_FONT_DIR`], scanned once per process.
    pub fn system() -> &'static FontIndex {
        static INDEX: OnceLock<FontIndex> = OnceLock::new();
        INDEX.get_or_init(|| FontIndex::scan(Path::new(SYSTEM_FONT_DIR)))
    }

    /// The faces of a family name (ASCII case-insensitive, CSS Fonts 4
    /// §5.1), or of the family a generic maps to.
    pub fn family(&self, family: &Family) -> &[usize] {
        let name = match family {
            Family::Named(n) => n.to_lowercase(),
            Family::Generic(g) => g.family().to_lowercase(),
        };
        self.families.get(&name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Family names, as the fonts spell them, sorted.
    pub fn family_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .faces
            .iter()
            .filter_map(|f| f.names.css_family().map(str::to_string))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The best face of `family` for `req` (§5.2).
    pub fn match_family(&self, family: &Family, req: &Request) -> Option<usize> {
        let ids = self.family(family);
        let descs: Vec<Descriptor> = ids.iter().map(|&i| self.faces[i].descriptor()).collect();
        best_faces(&descs, req).first().map(|&k| ids[k])
    }

    /// The face of `local(name)`: full name or PostScript name, ASCII
    /// case-insensitive (CSS Fonts 4 §4.3.1).
    pub fn local(&self, name: &str) -> Option<usize> {
        let want = name.to_lowercase();
        self.faces.iter().position(|f| {
            [&f.names.full, &f.names.postscript]
                .into_iter()
                .flatten()
                .any(|n| n.to_lowercase() == want)
        })
    }

    /// The first family of `families` with a face, matched for `req`, and
    /// whose face maps `cp`; then the Noto fallback for `cp`.
    pub fn select(&self, families: &[Family], req: &Request, cp: u32) -> Option<usize> {
        for fam in families {
            if let Some(i) = self.match_family(fam, req) {
                if self.faces[i].has_char(cp) {
                    return Some(i);
                }
            }
        }
        self.fallback(cp, req)
    }

    /// [`FontIndex::select`] with the document's web fonts first: a
    /// family named by `@font-face` rules uses only those faces (CSS
    /// Fonts 4 §4.1), matched for `req` and then, among the ties, the
    /// last defined whose `unicode-range` covers `cp` and that maps it.
    pub fn select_with(
        &self,
        web: &[WebFace],
        families: &[Family],
        req: &Request,
        cp: u32,
    ) -> Option<Selected> {
        for fam in families {
            if let Family::Named(name) = fam {
                let ids: Vec<usize> = (0..web.len())
                    .filter(|&i| web[i].family.eq_ignore_ascii_case(name))
                    .collect();
                if !ids.is_empty() {
                    let descs: Vec<Descriptor> =
                        ids.iter().map(|&i| web[i].descriptor.clone()).collect();
                    let best = best_faces(&descs, req);
                    if let Some(&k) = best
                        .iter()
                        .rev()
                        .find(|&&k| web[ids[k]].covers(cp) && web[ids[k]].face.has_char(cp))
                    {
                        return Some(Selected::Web(web[ids[k]].face.clone()));
                    }
                    continue;
                }
            }
            if let Some(i) = self.match_family(fam, req) {
                if self.faces[i].has_char(cp) {
                    return Some(Selected::System(i));
                }
            }
        }
        self.fallback(cp, req).map(Selected::System)
    }

    /// The first face (font order of `from_faces`) among the Noto
    /// families, matched for `req`, that maps `cp`.
    pub fn fallback(&self, cp: u32, req: &Request) -> Option<usize> {
        let style_key =
            (req.weight >= 600.0) as u8 | ((!matches!(req.style, Style::Normal) as u8) << 1);
        if let Some(&hit) = self.fallback_cache.lock().ok()?.get(&(cp, style_key)) {
            return hit;
        }
        let mut found = None;
        for fam in &self.noto {
            let descs: Vec<Descriptor> = fam.iter().map(|&i| self.faces[i].descriptor()).collect();
            if let Some(&k) = best_faces(&descs, req).first() {
                let i = fam[k];
                if self.faces[i].has_char(cp) {
                    found = Some(i);
                    break;
                }
            }
        }
        if let Ok(mut c) = self.fallback_cache.lock() {
            c.insert((cp, style_key), found);
        }
        found
    }

    pub fn load(&self, i: usize) -> Option<Arc<Face>> {
        self.faces.get(i)?.load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generics_follow_d15() {
        assert_eq!(
            Generic::from_keyword("SERIF").map(Generic::family),
            Some("Liberation Serif")
        );
        assert_eq!(
            Generic::from_keyword("fantasy").map(Generic::family),
            Some("Liberation Sans")
        );
        assert_eq!(
            Generic::from_keyword("monospace").map(Generic::family),
            Some("Liberation Mono")
        );
        assert_eq!(Generic::from_keyword("emoji"), None);
    }

    #[test]
    fn scanning_a_missing_directory_is_empty() {
        let idx = FontIndex::scan(Path::new("/nonexistent/vidrio-fonts"));
        assert!(idx.faces.is_empty());
        assert_eq!(
            idx.match_family(&Family::Generic(Generic::Serif), &Request::default()),
            None
        );
    }
}
