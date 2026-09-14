//! Image re-encoding, in pure Rust.
//!
//! GhostBuster reaches WIC through the OS. There is no WIC on a Mac, and rather
//! than trade it for ImageIO -- just as native, but only runnable on a Mac --
//! this uses pure-Rust codecs, which produce the same bytes on every platform.
//! That is what lets the whole repair core be built and tested on Windows or
//! Linux as well as in macOS CI.
//!
//! Every *decision* is GhostBuster's: which images qualify, the Med target
//! size (same floor(x + 0.5) arithmetic, so the same integer dimensions), the
//! "replace only when genuinely smaller" rule, and the renaming. Only the
//! encoders differ, so High is still pixel-exact while the encoded bytes are
//! not WIC's.

use std::io::Cursor;
use std::path::Path;

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// Lossless PNG at native resolution: pixel-exact.
    High,
    /// PNG downsampled so the image lands at ~150 dpi for its displayed size.
    Med,
    /// JPEG q92.
    XSmall,
}

impl Quality {
    pub fn parse(s: &str) -> Option<Quality> {
        match s.to_ascii_lowercase().as_str() {
            "high" => Some(Quality::High),
            "med" => Some(Quality::Med),
            "xsmall" => Some(Quality::XSmall),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "png  (high)",
            Quality::Med => "png  (med)",
            Quality::XSmall => "jpg  (xsmall)",
        }
    }
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            Quality::High => "High",
            Quality::Med => "Med",
            Quality::XSmall => "XSmall",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Quality::XSmall => "jpeg",
            _ => "png",
        }
    }
}

const MED_DPI: f64 = 150.0;
const JPEG_QUALITY: u8 = 92;

/// Written into every JPEG. GhostBuster carries the source's resolution
/// through and falls back to 96 when there is none; the bitmaps inside Excel's
/// EMFs never carry one, so 96 is what it writes for them too. Either way it is
/// never the invalid density of 0.
const JPEG_DPI: u16 = 96;

/// Whether image re-encoding can run at all. Always, here: nothing is borrowed
/// from the OS. Kept so the window's gating reads the same as GhostBuster's.
pub fn available() -> bool {
    true
}

/// Lifts the pixels out of an EMF that is just a bitmap in a metafile wrapper.
///
/// Excel writes a pasted picture as an EMF that is usually exactly
/// HEADER + one STRETCHDIBITS + EOF, with no vector content at all. When that
/// is the shape, the pixels can be taken exactly. Anything else returns None
/// and is left alone rather than rasterised, because rasterising real vector
/// art is a quality decision this tool should not make silently.
fn emf_bitmap(data: &[u8]) -> Option<DynamicImage> {
    if data.len() < 88 {
        return None;
    }
    let u32_at = |o: usize| -> u32 {
        u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };
    let i32_at = |o: usize| -> i32 {
        i32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };

    // Walk the record list; we want exactly three records.
    let mut recs: Vec<(u32, usize)> = Vec::new();
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let t = u32_at(off);
        let sz = u32_at(off + 4) as usize;
        if sz < 8 || off + sz > data.len() {
            return None;
        }
        recs.push((t, off));
        if recs.len() > 3 {
            return None;
        }
        off += sz;
    }
    if recs.len() != 3 || recs[0].0 != 1 || recs[1].0 != 81 || recs[2].0 != 14 {
        return None; // EMR_HEADER, EMR_STRETCHDIBITS, EMR_EOF
    }

    let ro = recs[1].1;
    if ro + 64 > data.len() {
        return None;
    }
    let off_bmi = u32_at(ro + 48) as usize;
    let off_bits = u32_at(ro + 56) as usize;
    let cb_bits = u32_at(ro + 60) as usize;
    let bmi = ro + off_bmi;
    if bmi + 20 > data.len() {
        return None;
    }
    let bw = i32_at(bmi + 4);
    let bh = i32_at(bmi + 8);
    let bpp = u16::from_le_bytes([data[bmi + 14], data[bmi + 15]]);
    let comp = u32_at(bmi + 16);
    if comp != 0 || bpp != 32 || bw <= 0 || bh == 0 {
        return None; // BI_RGB, 32bpp only
    }
    let h = bh.unsigned_abs() as usize;
    let w = bw as usize;
    let stride = w * 4;
    if cb_bits < stride * h || ro + off_bits + stride * h > data.len() {
        return None;
    }

    // Rows are stored bottom-up when biHeight is positive.
    //
    // BGR plus a padding byte, not BGRA: for BI_RGB the fourth byte is
    // undefined, not alpha. It comes back uniformly 0 on many images, and
    // treating it as alpha would render them fully transparent.
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let src = if bh > 0 { h - 1 - y } else { y };
        let s = ro + off_bits + src * stride;
        for px in data[s..s + stride].chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    RgbImage::from_raw(w as u32, h as u32, rgb).map(DynamicImage::ImageRgb8)
}

/// Re-compresses a palette PNG as a palette PNG.
///
/// Excel keeps some pictures as palette PNGs, and WIC re-encodes those as
/// palette PNGs. A general decoder expands them to RGBA first, which makes
/// every one bigger than the original, so none would ever be replaced. When
/// the pixels are not being resampled, the indices, palette and transparency
/// are carried over untouched and only the compression is redone. None means
/// "not a palette PNG, or it needs scaling": use the ordinary path.
fn repack_indexed_png(data: &[u8], quality: Quality, display_inches: f64) -> Option<Vec<u8>> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::IDENTITY);
    let mut reader = dec.read_info().ok()?;
    let info = reader.info();
    if info.color_type != png::ColorType::Indexed {
        return None;
    }
    let (w, h, depth) = (info.width, info.height, info.bit_depth);
    if quality == Quality::Med && med_size(w, h, display_inches).is_some() {
        return None;
    }
    let palette = info.palette.as_ref()?.to_vec();
    let trns = info.trns.as_ref().map(|t| t.to_vec());
    let mut rows = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut rows).ok()?;
    rows.truncate(frame.buffer_size());

    // Filtering rarely helps palette data, but not never; try both.
    [png::Filter::NoFilter, png::Filter::Adaptive]
        .into_iter()
        .filter_map(|filter| {
            let mut out = Vec::new();
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(depth);
            enc.set_palette(palette.clone());
            if let Some(t) = &trns {
                enc.set_trns(t.clone());
            }
            enc.set_compression(png::Compression::High);
            enc.set_filter(filter);
            let mut writer = enc.write_header().ok()?;
            writer.write_image_data(&rows).ok()?;
            writer.finish().ok()?;
            Some(out)
        })
        .min_by_key(|v| v.len())
}

/// Decodes any raster format the build understands (PNG, JPEG, GIF, BMP,
/// TIFF -- the same list GhostBuster hands to WIC).
fn decode(data: &[u8]) -> Option<DynamicImage> {
    image::load_from_memory(data).ok()
}

/// The size Med scales to, or None when it should not scale.
///
/// floor(x + 0.5) rather than f64::round, and the same h * nw / w expression
/// both GhostBuster builds use, so all three decide on identical integer
/// dimensions instead of landing a pixel apart.
fn med_size(w: u32, h: u32, display_inches: f64) -> Option<(u32, u32)> {
    if display_inches <= 0.0 {
        return None;
    }
    let nw = (display_inches * MED_DPI + 0.5).floor();
    if nw < 1.0 || (nw as u32) >= w {
        return None; // never upscale
    }
    let nh = (((h as f64) * nw / (w as f64)) + 0.5).floor().max(1.0) as u32;
    Some((nw as u32, nh))
}

/// Encodes a source at the requested quality. `display_inches` is the width the
/// image occupies on the sheet, used only by Med; zero means "unknown, do not
/// downsample".
fn encode(source: &DynamicImage, quality: Quality, display_inches: f64) -> Result<Vec<u8>, String> {
    let scaled;
    let mut src = source;
    if quality == Quality::Med {
        if let Some((w, h)) = med_size(source.width(), source.height(), display_inches) {
            // Triangle is an area-weighted filter when shrinking -- the same
            // job WIC's Fant interpolation does for GhostBuster.
            scaled = source.resize_exact(w, h, FilterType::Triangle);
            src = &scaled;
        }
    }

    let mut out = Vec::new();
    if quality == Quality::XSmall {
        // JPEG cannot carry an alpha channel. It is dropped, not composited,
        // which is what WIC's format converter does too.
        let rgb = src.to_rgb8();
        let (w, h) = (
            u16::try_from(rgb.width()).map_err(|_| "too wide for JPEG")?,
            u16::try_from(rgb.height()).map_err(|_| "too tall for JPEG")?,
        );
        // 4:2:0, as WIC's encoder defaults to. The `image` crate's own JPEG
        // encoder only does 4:4:4, which would make every file needlessly
        // larger than GhostBuster's.
        let mut enc = jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY);
        enc.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
        enc.set_optimized_huffman_tables(true);
        enc.set_density(jpeg_encoder::Density::Inch { x: JPEG_DPI, y: JPEG_DPI });
        enc.encode(rgb.as_raw(), w, h, jpeg_encoder::ColorType::Rgb)
            .map_err(|e| format!("jpeg: {e}"))?;
    } else {
        let enc = PngEncoder::new_with_quality(
            Cursor::new(&mut out),
            CompressionType::Best,
            PngFilter::Adaptive,
        );
        src.write_with_encoder(enc).map_err(|e| format!("png: {e}"))?;
    }
    Ok(out)
}

pub struct SquishReport {
    pub converted: usize,
    pub skipped: usize,
    pub before: usize,
    pub after: usize,
}

/// Re-encodes every image the platform can read. An image is replaced only when
/// the new encoding is genuinely smaller, so an over-compressed or useless
/// re-encode leaves the original bytes untouched.
pub fn squish(pkg: &mut crate::clean::Package, quality: Quality) -> SquishReport {
    let usage = crate::clean::media_usage(pkg);
    let mut report = SquishReport { converted: 0, skipped: 0, before: 0, after: 0 };
    let mut renames: Vec<(String, String)> = Vec::new();

    for name in pkg.names.clone() {
        if !name.starts_with("xl/media/") {
            continue;
        }
        let leaf = name.rsplit('/').next().unwrap_or(&name).to_string();

        // Reachable only through zero-area anchors: BUST deletes it as an
        // orphan, so re-encoding it would be work spent on a doomed file.
        if usage.ghost_only.contains(&leaf) {
            report.skipped += 1;
            continue;
        }

        let data = match pkg.blobs.get(&name) {
            Some(d) => d.clone(),
            None => continue,
        };
        let ext = Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let inches = usage.inches.get(&leaf).copied().unwrap_or(0.0);
        let repacked = if ext == "png" && quality != Quality::XSmall {
            repack_indexed_png(&data, quality, inches)
        } else {
            None
        };
        let encoded = match repacked {
            Some(bytes) => bytes,
            None => {
                let source = if ext == "emf" || ext == "wmf" {
                    emf_bitmap(&data)
                } else if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff") {
                    decode(&data)
                } else {
                    None
                };
                let Some(source) = source else {
                    report.skipped += 1;
                    continue;
                };
                let Ok(bytes) = encode(&source, quality, inches) else {
                    report.skipped += 1;
                    continue;
                };
                bytes
            }
        };
        if encoded.len() >= data.len() {
            report.skipped += 1;
            continue;
        }

        report.before += data.len();
        report.after += encoded.len();
        report.converted += 1;

        let stem = leaf.rsplit_once('.').map(|(s, _)| s).unwrap_or(&leaf).to_string();
        let mut new_name = format!("xl/media/{stem}.{}", quality.extension());
        let mut n = 2;
        while new_name != name && pkg.blobs.contains_key(&new_name) {
            new_name = format!("xl/media/{stem}_{n}.{}", quality.extension());
            n += 1;
        }
        if new_name != name {
            pkg.blobs.remove(&name);
            renames.push((name.clone(), new_name.clone()));
        }
        pkg.blobs.insert(new_name, encoded);
    }

    if report.converted == 0 {
        return report;
    }

    // Point the part list, every relationship, and [Content_Types] at the new
    // names. Extensions delimit the match, so "image2.png" cannot be confused
    // with "image21.png".
    let map: std::collections::HashMap<String, String> = renames
        .iter()
        .map(|(o, n)| {
            (
                o.rsplit('/').next().unwrap().to_string(),
                n.rsplit('/').next().unwrap().to_string(),
            )
        })
        .collect();

    for slot in pkg.names.iter_mut() {
        if let Some((_, n)) = renames.iter().find(|(o, _)| o == slot) {
            *slot = n.clone();
        }
    }

    for name in pkg.names.clone() {
        if !name.ends_with(".rels") {
            continue;
        }
        let Some(bytes) = pkg.blobs.get(&name) else { continue };
        let Ok(text) = std::str::from_utf8(bytes) else { continue };
        let mut out = text.to_string();
        for (old, new) in &map {
            if out.contains(old.as_str()) {
                out = out.replace(old.as_str(), new.as_str());
            }
        }
        if out != text {
            pkg.blobs.insert(name, out.into_bytes());
        }
    }

    let ct = "[Content_Types].xml".to_string();
    if let Some(bytes) = pkg.blobs.get(&ct) {
        if let Ok(text) = std::str::from_utf8(bytes) {
            let mut out = text.to_string();
            for (ext, mime) in [("png", "image/png"), ("jpeg", "image/jpeg")] {
                if !out.contains(&format!("Extension=\"{ext}\"")) {
                    if let Some(i) = out.find('>') {
                        out.insert_str(
                            i + 1,
                            &format!("<Default Extension=\"{ext}\" ContentType=\"{mime}\"/>"),
                        );
                    }
                }
            }
            pkg.blobs.insert(ct, out.into_bytes());
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HEADER + STRETCHDIBITS (32bpp BI_RGB) + EOF, as Excel writes a pasted
    /// picture. `extra` inserts one more record, making it a real metafile.
    fn emf(w: u32, h: u32, top_down: bool, extra: bool, px: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut bits = Vec::new();
        for row in 0..h {
            let y = if top_down { row } else { h - 1 - row };
            for x in 0..w {
                let [r, g, b] = px(x, y);
                bits.extend_from_slice(&[b, g, r, 0]);
            }
        }
        let le = |v: &mut Vec<u8>, n: u32| v.extend_from_slice(&n.to_le_bytes());
        let mut bmi = Vec::new();
        le(&mut bmi, 40);
        le(&mut bmi, w);
        le(&mut bmi, if top_down { (h as i32).wrapping_neg() as u32 } else { h });
        bmi.extend_from_slice(&1u16.to_le_bytes());
        bmi.extend_from_slice(&32u16.to_le_bytes());
        for v in [0, bits.len() as u32, 0, 0, 0, 0] {
            le(&mut bmi, v);
        }
        let mut sdib = Vec::new();
        for v in [81, 80 + bmi.len() as u32 + bits.len() as u32, 0, 0, w, h, 0, 0, 0, 0, w, h] {
            le(&mut sdib, v);
        }
        for v in [80, bmi.len() as u32, 80 + bmi.len() as u32, bits.len() as u32, 0, 0x00CC_0020, w, h] {
            le(&mut sdib, v);
        }
        assert_eq!(sdib.len(), 80);
        sdib.extend_from_slice(&bmi);
        sdib.extend_from_slice(&bits);

        let mut out = Vec::new();
        le(&mut out, 1);
        le(&mut out, 88);
        out.resize(88, 0);
        if extra {
            for v in [18, 12, 1] {
                le(&mut out, v);
            }
        }
        out.extend_from_slice(&sdib);
        for v in [14, 20, 0, 16, 20] {
            le(&mut out, v);
        }
        out
    }

    fn pattern(x: u32, y: u32) -> [u8; 3] {
        [(x * 7 % 256) as u8, (y * 13 % 256) as u8, ((x ^ y) % 256) as u8]
    }

    #[test]
    fn high_is_pixel_exact_bottom_up_and_top_down() {
        for top_down in [false, true] {
            let data = emf(37, 23, top_down, false, pattern);
            let src = emf_bitmap(&data).expect("bitmap EMF must be recognised");
            let png = encode(&src, Quality::High, 0.0).unwrap();
            let back = image::load_from_memory(&png).unwrap().to_rgb8();
            assert_eq!((back.width(), back.height()), (37, 23));
            for (x, y, p) in back.enumerate_pixels() {
                assert_eq!(p.0, pattern(x, y), "pixel {x},{y} top_down={top_down}");
            }
        }
    }

    #[test]
    fn vector_emf_is_left_alone() {
        assert!(emf_bitmap(&emf(8, 8, false, true, pattern)).is_none());
        assert!(emf_bitmap(b"not a metafile at all, not even close to 88 bytes").is_none());
    }

    #[test]
    fn med_dimensions_match_ghostbuster_arithmetic() {
        assert_eq!(med_size(480, 360, 2.0), Some((300, 225)));
        assert_eq!(med_size(640, 480, 3.0), Some((450, 338))); // 337.5 rounds up
        assert_eq!(med_size(320, 240, 1.0), Some((150, 113))); // 112.5 rounds up
        assert_eq!(med_size(400, 300, 4.0), None); // never upscale
        assert_eq!(med_size(400, 300, 0.0), None); // unknown display size
    }

    #[test]
    fn palette_pngs_stay_palette_pngs() {
        // A 4-bit, 16-colour PNG with transparency, stored with no compression.
        let (w, h) = (50u32, 30u32);
        let palette: Vec<u8> = (0..16u8).flat_map(|i| [i * 16, 255 - i * 16, i * 7]).collect();
        let trns: Vec<u8> = (0..16u8).map(|i| if i == 0 { 0 } else { 255 }).collect();
        let stride = (w as usize + 1) / 2;
        let rows: Vec<u8> = (0..h as usize)
            .flat_map(|y| (0..stride).map(move |x| (((x + y) % 16) << 4 | (x * 3 + y) % 16) as u8))
            .collect();
        let mut original = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut original, w, h);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Four);
            enc.set_palette(palette);
            enc.set_trns(trns);
            enc.set_compression(png::Compression::NoCompression);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&rows).unwrap();
            wr.finish().unwrap();
        }

        let repacked = repack_indexed_png(&original, Quality::High, 0.0).unwrap();
        assert!(repacked.len() < original.len());
        assert_eq!(repacked[25], 3, "colour type must stay indexed");
        let a = image::load_from_memory(&original).unwrap().to_rgba8();
        let b = image::load_from_memory(&repacked).unwrap().to_rgba8();
        assert_eq!(a, b);

        // Med that needs scaling goes the ordinary way instead.
        assert!(repack_indexed_png(&original, Quality::Med, 0.1).is_none());
        assert!(repack_indexed_png(&original, Quality::Med, 1.0).is_some());
    }

    #[test]
    fn xsmall_is_a_jpeg_of_the_same_size() {
        let src = emf_bitmap(&emf(64, 40, false, false, pattern)).unwrap();
        let jpg = encode(&src, Quality::XSmall, 0.0).unwrap();
        assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
        let back = image::load_from_memory(&jpg).unwrap();
        assert_eq!((back.width(), back.height()), (64, 40));
    }
}
