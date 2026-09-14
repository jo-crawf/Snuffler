//! Headless mode, for the conformance suite. Same flags and same JSON as
//! GhostBuster, so one runner can compare the builds field by field.
//!
//!   Snuffler --headless [--bust] [--squish High|Med|XSmall] --json out.json <file>...

use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::app::file_label;
use crate::clean;
use crate::squish::Quality;

/// FNV-1a, 64-bit. Every build implements this identically so the conformance
/// suite can compare media bytes across languages without a hash crate.
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn media_json(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else { return "[]".into() };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return "[]".into() };
    let mut names: Vec<String> = zip
        .file_names()
        .filter(|n| n.starts_with("xl/media/"))
        .map(|s| s.to_string())
        .collect();
    names.sort();
    let mut items: Vec<String> = Vec::new();
    for n in names {
        let mut buf = Vec::new();
        if let Ok(mut e) = zip.by_name(&n) {
            let _ = e.read_to_end(&mut buf);
        }
        items.push(format!(
            "{{\"name\":\"{}\",\"bytes\":{},\"fnv\":\"{:016x}\"}}",
            json_escape(&n), buf.len(), fnv1a(&buf)
        ));
    }
    format!("[{}]", items.join(","))
}

pub fn run(args: &[String]) -> i32 {
    let mut do_bust = false;
    let mut quality: Option<Quality> = None;
    let mut json_out: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--headless" => {}
            "--bust" => do_bust = true,
            "--squish" => {
                i += 1;
                quality = args.get(i).and_then(|s| Quality::parse(s));
            }
            "--json" => {
                i += 1;
                json_out = args.get(i).map(PathBuf::from);
            }
            other => files.push(PathBuf::from(other)),
        }
        i += 1;
    }
    if !do_bust && quality.is_none() {
        do_bust = true;
    }

    let mut records: Vec<String> = Vec::new();
    let mut exit = 0;
    for f in &files {
        let name = json_escape(&file_label(f));
        match clean::scan(f) {
            Err(reason) => {
                exit = 1;
                records.push(format!(
                    "{{\"source\":\"{name}\",\"rejected\":\"{}\"}}",
                    json_escape(&reason)
                ));
            }
            Ok(sc) => {
                let dst = clean::output_path(f);
                match clean::repair(f, &dst, do_bust, quality) {
                    Err(e) => {
                        exit = 1;
                        records.push(format!(
                            "{{\"source\":\"{name}\",\"rejected\":null,\"error\":\"{}\"}}",
                            json_escape(&e)
                        ));
                    }
                    Ok(r) => {
                        let b = r.bust.as_ref();
                        let s = r.squish.as_ref();
                        records.push(format!(
                            "{{\"source\":\"{name}\",\"rejected\":null,\"error\":null,\
                             \"scan\":{{\"ghosts\":{},\"images\":{}}},\
                             \"changed\":{},\"ghosts_removed\":{},\"pictures_kept\":{},\
                             \"media_removed\":{},\"drawing_before\":{},\"drawing_after\":{},\
                             \"images_converted\":{},\"images_before\":{},\"images_after\":{},\
                             \"file_before\":{},\"file_after\":{},\"media\":{}}}",
                            sc.ghosts, sc.images,
                            r.changed,
                            b.map(|x| x.ghosts).unwrap_or(0),
                            b.map(|x| x.kept).unwrap_or(0),
                            b.map(|x| x.media_removed).unwrap_or(0),
                            b.map(|x| x.drawing_before).unwrap_or(0),
                            b.map(|x| x.drawing_after).unwrap_or(0),
                            s.map(|x| x.converted).unwrap_or(0),
                            s.map(|x| x.before).unwrap_or(0),
                            s.map(|x| x.after).unwrap_or(0),
                            r.file_before, r.file_after,
                            if r.changed { media_json(&dst) } else { media_json(f) }
                        ));
                    }
                }
            }
        }
    }

    let doc = format!("{{\"tool\":\"snuffler\",\"results\":[{}]}}", records.join(","));
    if let Some(p) = json_out {
        let _ = std::fs::write(p, doc);
    }
    exit
}
