//! The window's state machine, without the window.
//!
//! Everything GhostBuster's wndproc decided -- which buttons are live, what
//! latches, what each panel says -- is plain data here, so the AppKit view
//! only has to paint it, and all of it can be unit-tested with no display.

use std::path::{Path, PathBuf};

use crate::clean;
use crate::squish::{self, Quality};

pub const ORDER: [Quality; 3] = [Quality::High, Quality::Med, Quality::XSmall];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Empty,
    Loaded,
    Working,
    Done,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Cleaned,
    NoChange,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Bust,
    Squish,
}

/// What the pointer is over, for hover highlighting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hot {
    None,
    Bust,
    Squish,
    Result,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub output: PathBuf,
    pub status: Status,
    pub ghosts: usize,
    pub img_before: usize,
    pub img_after: usize,
    pub before: u64,
    pub after: u64,
    pub error: String,
}

/// One run of one action over every loaded file, detached from `State` so a
/// worker thread can own it while the window stays responsive.
pub struct Job {
    pub sources: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    pub do_bust: bool,
    pub quality: Option<Quality>,
}

impl Job {
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn run_one(&self, i: usize) -> Outcome {
        let dst = self.outputs[i].clone();
        match clean::repair(&self.sources[i], &dst, self.do_bust, self.quality) {
            Ok(r) if !r.changed => Outcome {
                output: dst, status: Status::NoChange, ghosts: 0,
                img_before: 0, img_after: 0, before: r.file_before, after: 0,
                error: String::new(),
            },
            Ok(r) => Outcome {
                output: dst,
                status: Status::Cleaned,
                ghosts: r.bust.as_ref().map(|b| b.ghosts).unwrap_or(0),
                img_before: r.squish.as_ref().map(|s| s.before).unwrap_or(0),
                img_after: r.squish.as_ref().map(|s| s.after).unwrap_or(0),
                before: r.file_before,
                after: r.file_after,
                error: String::new(),
            },
            Err(e) => Outcome {
                output: dst, status: Status::Failed, ghosts: 0,
                img_before: 0, img_after: 0, before: 0, after: 0, error: e,
            },
        }
    }

    #[cfg(test)]
    pub fn run_all(&self) -> Vec<Outcome> {
        (0..self.len()).map(|i| self.run_one(i)).collect()
    }
}

pub struct State {
    pub stage: Stage,
    pub sources: Vec<PathBuf>,
    pub scans: Vec<clean::Scan>,
    pub skipped: Vec<String>,
    pub outputs: Vec<PathBuf>,
    pub results: Vec<Outcome>,
    pub busted: bool,
    pub squished_as: Option<Quality>,
    pub format: Quality,
    pub progress: usize,
    pub action: Option<Action>,
    pub headline: String,
    pub detail: String,
    pub error: String,
    pub hot: Hot,
}

impl State {
    pub fn new() -> State {
        State {
            stage: Stage::Empty,
            sources: Vec::new(),
            scans: Vec::new(),
            skipped: Vec::new(),
            outputs: Vec::new(),
            results: Vec::new(),
            busted: false,
            squished_as: None,
            format: Quality::High,
            progress: 0,
            action: None,
            headline: String::new(),
            detail: String::new(),
            error: String::new(),
            hot: Hot::None,
        }
    }

    pub fn ghosts(&self) -> usize {
        self.scans.iter().map(|s| s.ghosts).sum()
    }
    pub fn images(&self) -> usize {
        self.scans.iter().map(|s| s.images).sum()
    }
    pub fn ready(&self) -> bool {
        !self.sources.is_empty() && self.stage != Stage::Working
    }
    pub fn busy(&self) -> bool {
        self.stage == Stage::Working
    }
    // Each action is gated only on its own precondition and its own latch,
    // never on whether the other one has run.
    pub fn can_bust(&self) -> bool {
        self.ready() && !self.busted && self.ghosts() > 0
    }
    pub fn can_squish(&self) -> bool {
        self.ready()
            && squish::available()
            && self.images() > 0
            && self.squished_as != Some(self.format)
    }
    pub fn cleaned(&self) -> Vec<&Outcome> {
        self.results.iter().filter(|r| r.status == Status::Cleaned).collect()
    }

    /// A new set of files replaces everything except the chosen quality.
    pub fn load(&mut self, paths: Vec<PathBuf>) {
        *self = State { format: self.format, hot: self.hot, ..State::new() };
        for p in paths {
            match clean::scan(&p) {
                Ok(sc) => {
                    self.sources.push(p);
                    self.scans.push(sc);
                }
                Err(reason) => self.skipped.push(reason),
            }
        }
        if self.sources.is_empty() {
            if self.skipped.is_empty() {
                self.stage = Stage::Empty;
            } else {
                self.stage = Stage::Failed;
                self.error = if self.skipped.len() == 1 {
                    self.skipped[0].clone()
                } else {
                    format!("None of those {} files are Excel workbooks.", self.skipped.len())
                };
            }
        } else {
            self.stage = Stage::Loaded;
        }
    }

    /// Picking a different quality re-arms SQUISH. The previous result stays
    /// on screen and on disk until a new run replaces it.
    pub fn select(&mut self, q: Quality) -> bool {
        if self.format != q && self.stage != Stage::Working {
            self.format = q;
            true
        } else {
            false
        }
    }

    /// Both operations always start from the original, so the job carries
    /// whatever has already been applied plus the one just requested.
    pub fn begin(&mut self, which: Action) -> Job {
        self.action = Some(which);
        self.stage = Stage::Working;
        let do_bust = self.busted || which == Action::Bust;
        let quality = if which == Action::Squish { Some(self.format) } else { self.squished_as };
        if self.outputs.is_empty() {
            self.outputs = self.sources.iter().map(|p| clean::output_path(p)).collect();
        }
        Job { sources: self.sources.clone(), outputs: self.outputs.clone(), do_bust, quality }
    }

    pub fn progress(&mut self, i: usize, so_far: Vec<Outcome>) {
        self.progress = i + 1;
        self.results = so_far;
    }

    pub fn finish(&mut self, which: Action, results: Vec<Outcome>) {
        self.results = results;
        let failed = self.results.iter().filter(|r| r.status == Status::Failed).count();
        let cleaned: Vec<Outcome> = self.cleaned().into_iter().cloned().collect();

        // Latch the action that just ran, even when it changed nothing:
        // repeating it with the same settings would do the same nothing. A hard
        // error is the exception -- that stays retryable.
        if failed == 0 {
            if which == Action::Bust {
                self.busted = true;
            } else {
                self.squished_as = Some(self.format);
            }
        }

        if cleaned.is_empty() {
            self.stage = Stage::Failed;
            self.error = if failed > 0 {
                self.results.iter().find(|r| r.status == Status::Failed)
                    .map(|r| r.error.clone()).unwrap_or_default()
            } else if which == Action::Bust {
                "No ghost images found -- already clean.".into()
            } else {
                "Those images are already as small as this setting can make them.".into()
            };
            self.action = None;
            return;
        }

        let ghosts: usize = cleaned.iter().map(|c| c.ghosts).sum();
        let ib: usize = cleaned.iter().map(|c| c.img_before).sum();
        let ia: usize = cleaned.iter().map(|c| c.img_after).sum();
        let fb: u64 = cleaned.iter().map(|c| c.before).sum();
        let fa: u64 = cleaned.iter().map(|c| c.after).sum();

        self.headline = if which == Action::Bust {
            if cleaned.len() == 1 { format!("{} snuffed", thousands(ghosts)) }
            else { format!("{} cleaned", cleaned.len()) }
        } else {
            let pct = 100.0 * (1.0 - (fa as f64 / fb.max(1) as f64));
            format!("{pct:.0}% smaller")
        };

        let mut lines: Vec<String> = Vec::new();
        if self.busted && ghosts > 0 {
            lines.push(format!("{} ghost images removed.", thousands(ghosts)));
        }
        if let Some(q) = self.squished_as {
            if ib > 0 {
                lines.push(format!("Images {} -> {} ({}).",
                    human(ib as u64), human(ia as u64),
                    q.label().split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        }
        lines.push(format!("File {} -> {}.", human(fb), human(fa)));
        if failed > 0 {
            lines.push(format!("{failed} failed."));
        }
        self.detail = lines.join("\n");
        self.stage = Stage::Done;
        self.action = None;
    }

    // ------------------------------------------------------- panel wording

    /// SNUFF / SQUISH, or progress while that action runs. (SNUFF is what
    /// GhostBuster calls BUST; internally it is still Action::Bust, and the
    /// headless flag is still --bust so the two builds stay comparable.)
    pub fn button_label(&self, which: Action) -> String {
        if self.stage == Stage::Working && self.action == Some(which) {
            let n = self.sources.len();
            if n > 1 { format!("{} / {}", self.progress, n) } else { "WORKING".into() }
        } else {
            match which {
                Action::Bust => "SNUFF".into(),
                Action::Squish => "SQUISH".into(),
            }
        }
    }

    /// Up to three names, one per line; beyond that the panel shows a count.
    pub fn source_names(&self) -> Option<String> {
        if self.sources.is_empty() || self.sources.len() > 3 {
            return None;
        }
        Some(self.sources.iter().map(|p| file_label(p)).collect::<Vec<_>>().join("\n"))
    }

    pub fn total_bytes(&self) -> u64 {
        self.sources.iter().filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len())).sum()
    }

    pub fn facts(&self) -> String {
        let g = self.ghosts();
        let i = self.images();
        format!(
            "{}\n{}",
            if g > 0 { format!("{} ghost images", thousands(g)) } else { "no ghost images".into() },
            if i > 0 { plural(i, "image", "images") } else { "no images".into() })
    }

    pub fn rejected_note(&self) -> Option<String> {
        match self.skipped.len() {
            0 => None,
            1 => Some(format!("1 file rejected: {}", self.skipped[0])),
            n => Some(format!("{n} files rejected")),
        }
    }

    pub fn shown_outputs(&self) -> String {
        let cleaned = self.cleaned();
        let mut shown: Vec<String> = cleaned.iter().take(3).map(|o| file_label(&o.output)).collect();
        if cleaned.len() > 3 {
            shown.push(format!("and {} more", cleaned.len() - 3));
        }
        shown.join("\n")
    }

    pub fn reveal_hint(&self) -> &'static str {
        if self.cleaned().len() == 1 {
            "Click to show it in the folder"
        } else {
            "Click to show them in the folder"
        }
    }

    /// The footer, and whether it is a path -- a long path has no spaces to
    /// wrap on, so it is elided in the middle rather than clipped at both ends.
    pub fn footer(&self) -> (String, bool) {
        let cleaned = self.cleaned();
        if self.stage == Stage::Done && !cleaned.is_empty() {
            let mut dirs: Vec<&Path> = cleaned.iter().filter_map(|o| o.output.parent()).collect();
            dirs.dedup();
            if dirs.len() == 1 {
                return (format!("Saved in   {}", dirs[0].display()), true);
            }
            return ("Saved beside each original.".into(), false);
        }
        ("Your original files are never modified.".into(), false)
    }
}

// ------------------------------------------------------------------ utils

pub fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn human(bytes: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", U[i]) }
}

pub fn thousands(n: usize) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("{n} {one}") } else { format!("{n} {many}") }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Read;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance").join("fixtures")
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("snuffler-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn copy(dir: &Path, name: &str) -> PathBuf {
        let dst = dir.join(name);
        std::fs::copy(fixtures().join(name), &dst).unwrap();
        dst
    }

    fn run(st: &mut State, which: Action) {
        let job = st.begin(which);
        assert!(st.busy());
        let results = job.run_all();
        st.finish(which, results);
    }

    fn entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut out = BTreeMap::new();
        for i in 0..z.len() {
            let mut e = z.by_index(i).unwrap();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            out.insert(e.name().to_string(), buf);
        }
        out
    }

    #[test]
    fn rejections_use_ghostbusters_wording() {
        let dir = scratch("reject");
        let fake = copy(&dir, "not-excel.xlsx");
        let empty = copy(&dir, "no-workbook.xlsx");
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "hello").unwrap();
        let xls = dir.join("book.xls");
        std::fs::write(&xls, "legacy").unwrap();
        let folder = dir.join("folder.xlsx");
        std::fs::create_dir_all(&folder).unwrap();

        let mut st = State::new();
        st.load(vec![fake.clone(), empty, txt, xls, folder, dir.join("missing.xlsx")]);
        assert_eq!(st.skipped, [
            "not a real Excel file",
            "not a real Excel file",
            "not an Excel workbook",
            "legacy .xls is a different format",
            "folders are not accepted",
            "file not found",
        ]);
        assert_eq!(st.stage, Stage::Failed);
        assert_eq!(st.error, "None of those 6 files are Excel workbooks.");

        st.load(vec![fake]);
        assert_eq!(st.error, "not a real Excel file");
        assert!(!st.can_bust() && !st.can_squish());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn buttons_gate_latch_and_rearm() {
        let dir = scratch("latch");
        let mut st = State::new();
        st.load(vec![copy(&dir, "ghosts.xlsx")]);
        assert_eq!(st.stage, Stage::Loaded);
        assert_eq!(st.ghosts(), 306);
        assert_eq!(st.images(), 6);
        assert_eq!(st.facts(), "306 ghost images\n6 images");
        assert!(st.can_bust() && st.can_squish());

        run(&mut st, Action::Bust);
        assert_eq!(st.stage, Stage::Done);
        assert_eq!(st.headline, "306 snuffed");
        assert!(!st.can_bust(), "SNUFF latches after running");
        assert!(st.can_squish(), "SQUISH is not gated on SNUFF");

        assert!(st.select(Quality::XSmall));
        run(&mut st, Action::Squish);
        assert_eq!(st.stage, Stage::Done);
        assert!(st.headline.ends_with("% smaller"), "{}", st.headline);
        assert!(st.detail.starts_with("306 ghost images removed.\nImages "), "{}", st.detail);
        assert!(!st.can_squish(), "SQUISH latches at the quality it ran at");
        assert!(!st.select(Quality::XSmall));
        assert!(st.select(Quality::High));
        assert!(st.can_squish(), "a different quality re-arms SQUISH");
        assert_eq!(st.shown_outputs(), "ghosts (cleaned).xlsx");
        assert_eq!(st.footer(), (format!("Saved in   {}", dir.display()), true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_to_do_means_nothing_to_click() {
        let dir = scratch("clean");
        let mut st = State::new();
        st.load(vec![copy(&dir, "clean.xlsx")]);
        assert_eq!(st.stage, Stage::Loaded);
        assert_eq!(st.facts(), "no ghost images\nno images");
        assert!(!st.can_bust() && !st.can_squish());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_hard_error_stays_retryable() {
        let dir = scratch("retry");
        let file = copy(&dir, "ghosts.xlsx");
        let mut st = State::new();
        st.load(vec![file.clone()]);
        std::fs::remove_file(&file).unwrap();
        run(&mut st, Action::Bust);
        assert_eq!(st.stage, Stage::Failed);
        assert!(st.error.starts_with("cannot read the file"), "{}", st.error);
        assert!(st.can_bust());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bust_then_squish_equals_squish_then_bust() {
        let (a, b) = (scratch("order-a"), scratch("order-b"));
        let mut first = State::new();
        first.load(vec![copy(&a, "ghosts.xlsx")]);
        first.select(Quality::XSmall);
        run(&mut first, Action::Bust);
        run(&mut first, Action::Squish);

        let mut second = State::new();
        second.load(vec![copy(&b, "ghosts.xlsx")]);
        second.select(Quality::XSmall);
        run(&mut second, Action::Squish);
        run(&mut second, Action::Bust);

        assert_eq!(entries(&first.outputs[0]), entries(&second.outputs[0]));
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn a_new_quality_rederives_from_the_original() {
        let (a, b) = (scratch("rederive-a"), scratch("rederive-b"));
        let mut st = State::new();
        st.load(vec![copy(&a, "emf-heavy.xlsm")]);
        st.select(Quality::XSmall);
        run(&mut st, Action::Squish);
        st.select(Quality::High);
        run(&mut st, Action::Squish);

        let direct = b.join("direct.xlsm");
        clean::repair(&copy(&b, "emf-heavy.xlsm"), &direct, false, Some(Quality::High)).unwrap();
        assert_eq!(entries(&st.outputs[0]), entries(&direct),
            "High after XSmall must not bake JPEG artefacts into the PNGs");
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn output_never_overwrites() {
        let dir = scratch("names");
        let src = copy(&dir, "clean.xlsx");
        assert_eq!(clean::output_path(&src), dir.join("clean (cleaned).xlsx"));
        std::fs::write(dir.join("clean (cleaned).xlsx"), "taken").unwrap();
        assert_eq!(clean::output_path(&src), dir.join("clean (cleaned 2).xlsx"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn progress_label_counts_files() {
        let dir = scratch("progress");
        let mut st = State::new();
        st.load(vec![copy(&dir, "ghosts.xlsx"), copy(&dir, "photos.xlsx")]);
        assert_eq!(st.button_label(Action::Bust), "SNUFF");
        let _job = st.begin(Action::Bust);
        st.progress(1, Vec::new());
        assert_eq!(st.button_label(Action::Bust), "2 / 2");
        assert_eq!(st.button_label(Action::Squish), "SQUISH");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn number_formatting() {
        assert_eq!(thousands(18456), "18,456");
        assert_eq!(thousands(999), "999");
        assert_eq!(human(963), "963 B");
        assert_eq!(human(1_770_423), "1.7 MB");
        assert_eq!(plural(1, "image", "images"), "1 image");
    }
}
