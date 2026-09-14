//! Reading, repairing and writing the .xlsx package.
//!
//! Two repairs live here. BUST removes the zero-area picture anchors Excel
//! accumulates in xl/drawings/drawingN.xml when images are pasted onto a sheet
//! over and over -- they are invisible, but Excel lays out every one of them.
//! SQUISH (in `squish.rs`) re-encodes the images themselves.
//!
//! Deliberately untouched by BUST: xl/richData/*, which holds Excel's newer
//! "Place in Cell" images. Those are real content and never appear in
//! drawingN.xml; the orphan sweep protects them because their own
//! relationships are never rewritten.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::Path;

use regex::Regex;

use crate::squish::Quality;

pub type Res<T> = Result<T, String>;

const EMU_PER_INCH: f64 = 914400.0;

pub struct Package {
    pub names: Vec<String>,
    pub blobs: HashMap<String, Vec<u8>>,
}

pub struct BustReport {
    pub ghosts: usize,
    pub kept: usize,
    pub drawing_before: usize,
    pub drawing_after: usize,
    pub media_removed: usize,
}

pub struct Scan {
    pub ghosts: usize,
    pub images: usize,
    pub media_bytes: u64,
}

pub struct RepairReport {
    pub changed: bool,
    pub bust: Option<BustReport>,
    pub squish: Option<crate::squish::SquishReport>,
    pub file_before: u64,
    pub file_after: u64,
}

pub struct Usage {
    pub inches: HashMap<String, f64>,
    pub ghost_only: HashSet<String>,
}

struct Rx {
    anchors: [Regex; 3],
    xdr_ext: Regex,
    from: Regex,
    to: Regex,
    a_ext: Regex,
    embed: Regex,
    relationship: Regex,
    rel_id: Regex,
    rel_target: Regex,
    rel_pair: Regex,
}

fn rx() -> &'static Rx {
    use std::sync::OnceLock;
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| {
        // Anchors never nest, so a lazy match to the closing tag is safe.
        let anchor =
            |t: &str| Regex::new(&format!(r"(?s)<xdr:{t}\b[^>]*>.*?</xdr:{t}>")).unwrap();
        let point = |t: &str| {
            Regex::new(&format!(
                r"(?s)<xdr:{t}>\s*<xdr:col>(\d+)</xdr:col>\s*<xdr:colOff>(-?\d+)</xdr:colOff>\s*<xdr:row>(\d+)</xdr:row>\s*<xdr:rowOff>(-?\d+)</xdr:rowOff>"
            ))
            .unwrap()
        };
        Rx {
            anchors: [
                anchor("oneCellAnchor"),
                anchor("twoCellAnchor"),
                anchor("absoluteAnchor"),
            ],
            xdr_ext: Regex::new("<xdr:ext\\s+cx=\"(\\d+)\"\\s+cy=\"(\\d+)\"").unwrap(),
            from: point("from"),
            to: point("to"),
            a_ext: Regex::new("<a:ext\\s+cx=\"(\\d+)\"\\s+cy=\"(\\d+)\"").unwrap(),
            embed: Regex::new("r:embed=\"([^\"]+)\"").unwrap(),
            relationship: Regex::new(r"(?s)<Relationship\b[^>]*?/>").unwrap(),
            rel_id: Regex::new("Id=\"([^\"]+)\"").unwrap(),
            rel_target: Regex::new("Target=\"([^\"]+)\"").unwrap(),
            rel_pair: Regex::new("Id=\"([^\"]+)\"[^>]*Target=\"([^\"]+)\"").unwrap(),
        }
    })
}

/// True when an anchor encloses zero area, i.e. it draws nothing.
///
/// `oneCellAnchor` and `absoluteAnchor` carry an explicit `<xdr:ext>` size.
/// `twoCellAnchor` has no size of its own -- it spans from one cell to another
/// -- so it is a ghost when its start and end points are identical.
fn is_ghost(anchor: &str) -> bool {
    let r = rx();
    if let Some(c) = r.xdr_ext.captures(anchor) {
        return &c[1] == "0" || &c[2] == "0";
    }
    match (r.from.captures(anchor), r.to.captures(anchor)) {
        (Some(f), Some(t)) => (1..=4).all(|i| f[i] == t[i]),
        _ => false,
    }
}

fn is_drawing(name: &str) -> bool {
    name.starts_with("xl/drawings/drawing")
        && name.ends_with(".xml")
        && !name.contains("_rels")
        && name["xl/drawings/drawing".len()..name.len() - 4]
            .chars()
            .all(|c| c.is_ascii_digit())
        && name.len() > "xl/drawings/drawing.xml".len()
}

fn rels_for(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn leaf(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

// ------------------------------------------------------------------ package

pub fn read_package(path: &Path) -> Res<Package> {
    let raw = std::fs::read(path).map_err(|e| format!("cannot read the file: {e}"))?;
    let mut zip = zip::ZipArchive::new(Cursor::new(&raw[..]))
        .map_err(|_| "this is not a readable .xlsx file".to_string())?;

    // The whole package in memory. Even a badly infested workbook is only tens
    // of megabytes, and it lets the two repairs compose without repeated zip
    // round-trips.
    let mut names = Vec::with_capacity(zip.len());
    let mut blobs = HashMap::with_capacity(zip.len());
    for i in 0..zip.len() {
        let mut e = zip.by_index(i).map_err(|e| format!("damaged entry: {e}"))?;
        if e.is_dir() {
            continue;
        }
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf).map_err(|e| format!("damaged entry: {e}"))?;
        let n = e.name().to_string();
        names.push(n.clone());
        blobs.insert(n, buf);
    }
    Ok(Package { names, blobs })
}

pub fn write_package(pkg: &Package, dst: &Path) -> Res<u64> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for name in &pkg.names {
        let Some(data) = pkg.blobs.get(name) else { continue };
        out.start_file(name, opts).map_err(|e| format!("cannot write output: {e}"))?;
        out.write_all(data).map_err(|e| format!("cannot write output: {e}"))?;
    }
    let bytes = out.finish().map_err(|e| format!("cannot write output: {e}"))?.into_inner();
    std::fs::write(dst, &bytes).map_err(|e| format!("cannot save the cleaned file: {e}"))?;
    Ok(bytes.len() as u64)
}

// --------------------------------------------------------------------- bust

pub fn bust(pkg: &mut Package) -> BustReport {
    let r = rx();
    let mut rep = BustReport {
        ghosts: 0,
        kept: 0,
        drawing_before: 0,
        drawing_after: 0,
        media_removed: 0,
    };
    let mut rels_updates: Vec<(String, String)> = Vec::new();

    for name in pkg.names.clone() {
        if !is_drawing(&name) {
            continue;
        }
        let Some(bytes) = pkg.blobs.get(&name) else { continue };
        let Ok(xml) = String::from_utf8(bytes.clone()) else { continue };

        let mut removed = 0usize;
        let mut kept = 0usize;
        let mut out = xml.clone();
        for pattern in &r.anchors {
            out = pattern
                .replace_all(&out, |c: &regex::Captures| {
                    if is_ghost(&c[0]) {
                        removed += 1;
                        String::new()
                    } else {
                        kept += 1;
                        c[0].to_string()
                    }
                })
                .into_owned();
        }
        if removed == 0 {
            continue;
        }

        rep.ghosts += removed;
        rep.kept += kept;
        rep.drawing_before += xml.len();
        rep.drawing_after += out.len();

        // Drop relationships for images no surviving anchor references.
        let survivors: HashSet<String> =
            r.embed.captures_iter(&out).map(|c| c[1].to_string()).collect();
        let rels_name = rels_for(&name);
        if let Some(rb) = pkg.blobs.get(&rels_name) {
            if let Ok(rels) = std::str::from_utf8(rb) {
                let pruned = r
                    .relationship
                    .replace_all(rels, |c: &regex::Captures| {
                        let tag = &c[0];
                        let is_image = tag.contains("/image");
                        match r.rel_id.captures(tag) {
                            Some(m) if is_image && !survivors.contains(&m[1]) => String::new(),
                            _ => tag.to_string(),
                        }
                    })
                    .into_owned();
                rels_updates.push((rels_name, pruned));
            }
        }
        pkg.blobs.insert(name, out.into_bytes());
    }

    for (n, x) in rels_updates {
        pkg.blobs.insert(n, x.into_bytes());
    }
    rep.media_removed = remove_orphan_media(pkg);
    rep
}

/// Keeps any media still named by *any* .rels part. This is what protects the
/// xl/richData images: their own relationships are never touched, so their
/// media stays referenced and survives.
pub fn remove_orphan_media(pkg: &mut Package) -> usize {
    let r = rx();
    let mut referenced: HashSet<String> = HashSet::new();
    for name in &pkg.names {
        if !name.ends_with(".rels") {
            continue;
        }
        let Some(b) = pkg.blobs.get(name) else { continue };
        if let Ok(text) = std::str::from_utf8(b) {
            for c in r.rel_target.captures_iter(text) {
                referenced.insert(leaf(&c[1]).to_string());
            }
        }
    }
    let before = pkg.names.len();
    let mut keep = Vec::with_capacity(before);
    for name in pkg.names.drain(..) {
        if name.starts_with("xl/media/") && !referenced.contains(leaf(&name)) {
            pkg.blobs.remove(&name);
            continue;
        }
        keep.push(name);
    }
    pkg.names = keep;
    before - pkg.names.len()
}

/// Walks the drawing anchors once and reports, per media file, the widest size
/// it is actually displayed at, plus whether every anchor pointing at it
/// encloses zero area.
///
/// Zero-area anchors contribute no display size -- a ghost has none to speak of.
/// A file reachable only through ghosts is one BUST would delete as an orphan,
/// so re-encoding it is pure churn. Media that no drawing references at all
/// (the xl/richData in-cell images) appear in neither and squish normally.
pub fn media_usage(pkg: &Package) -> Usage {
    let r = rx();
    let mut inches: HashMap<String, f64> = HashMap::new();
    let mut real: HashSet<String> = HashSet::new();
    let mut ghost: HashSet<String> = HashSet::new();

    for name in &pkg.names {
        if !is_drawing(name) {
            continue;
        }
        let rels_name = rels_for(name);
        let Some(rb) = pkg.blobs.get(&rels_name) else { continue };
        let Ok(rels) = std::str::from_utf8(rb) else { continue };
        let idmap: HashMap<&str, &str> = r
            .rel_pair
            .captures_iter(rels)
            .map(|c| {
                (
                    c.get(1).unwrap().as_str(),
                    leaf(c.get(2).unwrap().as_str()),
                )
            })
            .collect();

        let Some(b) = pkg.blobs.get(name) else { continue };
        let Ok(xml) = std::str::from_utf8(b) else { continue };
        for pattern in &r.anchors {
            for m in pattern.find_iter(xml) {
                let a = m.as_str();
                let Some(e) = r.embed.captures(a) else { continue };
                let Some(file) = idmap.get(&e[1]) else { continue };
                let file = file.to_string();
                if is_ghost(a) {
                    ghost.insert(file);
                    continue;
                }
                real.insert(file.clone());
                if let Some(x) = r.a_ext.captures(a) {
                    let inch = x[1].parse::<f64>().unwrap_or(0.0) / EMU_PER_INCH;
                    let slot = inches.entry(file).or_insert(0.0);
                    if *slot < inch {
                        *slot = inch;
                    }
                }
            }
        }
    }

    let ghost_only = ghost.difference(&real).cloned().collect();
    Usage { inches, ghost_only }
}

// --------------------------------------------------------------- scan / run

/// Validates the file and reports what each action would have to work on, so
/// the buttons can be enabled only when their precondition is met.
///
/// The extension is only the first check: an .xlsx is a zip container, so
/// anything that is not a zip holding xl/workbook.xml is rejected here rather
/// than failing part-way through a run.
pub fn scan(path: &Path) -> Result<Scan, String> {
    if path.is_dir() {
        return Err("folders are not accepted".into());
    }
    if !path.is_file() {
        return Err("file not found".into());
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "xls" {
        return Err("legacy .xls is a different format".into());
    }
    if ext != "xlsx" && ext != "xlsm" {
        return Err("not an Excel workbook".into());
    }

    let file = std::fs::File::open(path).map_err(|_| "in use - close it in Excel first")?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "not a real Excel file")?;
    if zip.by_name("xl/workbook.xml").is_err() {
        return Err("not a real Excel file".into());
    }

    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    let mut ghosts = 0usize;
    let mut images = 0usize;
    let mut media_bytes = 0u64;
    let r = rx();
    for name in names {
        if name.starts_with("xl/media/") {
            images += 1;
            if let Ok(e) = zip.by_name(&name) {
                media_bytes += e.size();
            }
            continue;
        }
        if !is_drawing(&name) {
            continue;
        }
        let mut buf = Vec::new();
        if let Ok(mut e) = zip.by_name(&name) {
            if e.read_to_end(&mut buf).is_err() {
                continue;
            }
        }
        let Ok(xml) = std::str::from_utf8(&buf) else { continue };
        for pattern in &r.anchors {
            for m in pattern.find_iter(xml) {
                if is_ghost(m.as_str()) {
                    ghosts += 1;
                }
            }
        }
    }
    Ok(Scan { ghosts, images, media_bytes })
}

/// Applies the requested operations to one workbook and writes the result.
///
/// Both always start from the original, so switching squish quality re-derives
/// from source rather than re-compressing a previous result -- otherwise
/// picking "high" after "xsmall" would bake JPEG artefacts into a supposedly
/// lossless PNG. BUST runs first, so its orphan sweep clears ghost-only media
/// before SQUISH looks at anything.
pub fn repair(
    src: &Path,
    dst: &Path,
    do_bust: bool,
    quality: Option<Quality>,
) -> Res<RepairReport> {
    let mut pkg = read_package(src)?;
    if !pkg.blobs.contains_key("xl/workbook.xml") {
        return Err("that does not look like an Excel workbook".into());
    }
    let file_before = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);

    let bust_rep = if do_bust { Some(bust(&mut pkg)) } else { None };
    let squish_rep = quality.map(|q| crate::squish::squish(&mut pkg, q));

    let changed = bust_rep.as_ref().is_some_and(|b| b.ghosts > 0)
        || squish_rep.as_ref().is_some_and(|s| s.converted > 0);

    let file_after = if changed { write_package(&pkg, dst)? } else { 0 };

    Ok(RepairReport {
        changed,
        bust: bust_rep,
        squish: squish_rep,
        file_before,
        file_after,
    })
}

/// `Book.xlsx` -> `Book (cleaned).xlsx`, never overwriting an existing file.
pub fn output_path(src: &Path) -> std::path::PathBuf {
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workbook".into());
    let ext = src
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "xlsx".into());
    let mut candidate = dir.join(format!("{stem} (cleaned).{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} (cleaned {n}).{ext}"));
        n += 1;
    }
    candidate
}
