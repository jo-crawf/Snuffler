//! The window, in AppKit.
//!
//! A port of GhostBuster's owner-drawn Win32 window: one custom view paints
//! everything, with the same layout, palette and wording, while `app::State`
//! decides everything the wndproc used to. Coordinates are GhostBuster's
//! logical pixels; on a Mac those are points, so Retina scaling comes free and
//! the DPI arithmetic disappears. The view is flipped so y grows downward, as
//! it does in GDI, and every rectangle below is copied straight from there.
//!
//! One deliberate difference: GhostBuster repairs on the UI thread and forces
//! a repaint between files. AppKit shows the spinning beach ball after a couple
//! of seconds of that, so here the work runs on a background thread and posts
//! progress back to the main queue.

use std::cell::RefCell;
use std::path::PathBuf;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBezierPath,
    NSColor, NSCursor, NSDragOperation, NSDraggingInfo, NSEvent, NSFont, NSFontAttributeName,
    NSFontWeightBold, NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSLineBreakMode, NSMenu, NSMenuItem, NSModalResponseOK, NSMutableParagraphStyle, NSOpenPanel,
    NSParagraphStyleAttributeName, NSPasteboardTypeFileURL, NSResponder, NSStringDrawing,
    NSTextAlignment, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{
    ns_string, NSArray, NSAttributedStringKey, NSDictionary, NSNotification, NSObject,
    NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use crate::app::{Action, Hot, Stage, State, ORDER};
use crate::squish::Quality;

// ---------------------------------------------------------------- palette
// GhostBuster's COLORREFs, un-reversed from 0x00BBGGRR into ordinary RGB.
const BG: u32 = 0x1E2126;
const PANEL: u32 = 0x2B2E36;
const PANEL_HOT: u32 = 0x363B45;
const SEG: u32 = 0x24272E;
const EDGE: u32 = 0x565E6E;
const TEXT: u32 = 0xECEEF2;
const MUTED: u32 = 0x929AA8;
const ACCENT: u32 = 0x5865F2;
const SQUISH_C: u32 = 0xD88A3A;
const GOOD: u32 = 0x7EE787;
const BAD: u32 = 0xF25858;
const WHITE: u32 = 0xFFFFFF;

// --------------------------------------------------------------- geometry
const W: f64 = 760.0;
const H: f64 = 500.0;

fn rect(l: i32, t: i32, r: i32, b: i32) -> NSRect {
    NSRect::new(
        NSPoint::new(l as f64, t as f64),
        NSSize::new((r - l) as f64, (b - t) as f64),
    )
}
fn left_panel() -> NSRect {
    rect(24, 78, 286, 404)
}
fn right_panel() -> NSRect {
    rect(474, 78, 736, 404)
}
fn bust_rect() -> NSRect {
    rect(310, 160, 450, 202)
}
fn squish_rect() -> NSRect {
    rect(310, 344, 450, 386)
}
fn seg_rect(i: usize) -> NSRect {
    let top = 240 + (i as i32) * 29;
    rect(310, top, 450, top + 28)
}
fn hit(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x
        && p.x < r.origin.x + r.size.width
        && p.y >= r.origin.y
        && p.y < r.origin.y + r.size.height
}

// ------------------------------------------------------------------ state
thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
    static VIEW: RefCell<Option<Retained<Canvas>>> = const { RefCell::new(None) };
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    /// Files that arrived before the window existed.
    static PENDING: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// Remaining self-test steps, last first. None when not self-testing.
    static SELFTEST: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

fn busy() -> bool {
    STATE.with(|st| st.borrow().busy())
}

fn redraw() {
    VIEW.with(|v| {
        if let Some(v) = v.borrow().as_ref() {
            v.setNeedsDisplay(true);
        }
    });
}

// ----------------------------------------------------------------- drawing
fn color(rgb: u32) -> Retained<NSColor> {
    let c = |shift: u32| ((rgb >> shift) & 0xFF) as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), 1.0)
}

struct Fonts {
    title: Retained<NSFont>,
    body: Retained<NSFont>,
    small: Retained<NSFont>,
    tiny: Retained<NSFont>,
    button: Retained<NSFont>,
    big: Retained<NSFont>,
}

impl Fonts {
    /// GhostBuster's Segoe UI sizes are em heights in logical pixels, which is
    /// exactly what a point size is here.
    fn new() -> Fonts {
        let (regular, semibold, bold) =
            unsafe { (NSFontWeightRegular, NSFontWeightSemibold, NSFontWeightBold) };
        let f = NSFont::systemFontOfSize_weight;
        Fonts {
            title: f(15.0, semibold),
            body: f(13.0, regular),
            small: f(11.0, regular),
            tiny: f(10.0, regular),
            button: f(13.0, bold),
            big: f(22.0, bold),
        }
    }
}

fn fill(r: NSRect, c: u32) {
    color(c).setFill();
    NSBezierPath::fillRect(r);
}

/// GDI's RoundRect takes the corner ellipse's width, so its radius is half.
fn rounded(r: NSRect, radius: f64, f: Option<u32>, e: Option<u32>, dashed: bool) {
    let inset = NSRect::new(
        NSPoint::new(r.origin.x + 0.5, r.origin.y + 0.5),
        NSSize::new(r.size.width - 1.0, r.size.height - 1.0),
    );
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(inset, radius / 2.0, radius / 2.0);
    if let Some(c) = f {
        color(c).setFill();
        path.fill();
    }
    if let Some(c) = e {
        color(c).setStroke();
        path.setLineWidth(1.0);
        if dashed {
            let pattern: [f64; 2] = [6.0, 3.0];
            unsafe { path.setLineDash_count_phase(pattern.as_ptr(), 2, 0.0) };
        }
        path.stroke();
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
}

/// The DrawText flag combinations GhostBuster uses.
#[derive(Clone, Copy)]
enum Flow {
    /// DT_SINGLELINE: one line from the top.
    Line,
    /// DT_SINGLELINE | DT_VCENTER.
    Middle,
    /// DT_WORDBREAK: wrapped, from the top.
    Wrap,
    /// DT_SINGLELINE | DT_PATH_ELLIPSIS: elided in the middle.
    Path,
}

fn text(msg: &str, r: NSRect, font: &NSFont, c: u32, align: Align, flow: Flow) {
    let para = NSMutableParagraphStyle::new();
    para.setAlignment(match align {
        Align::Left => NSTextAlignment::Left,
        Align::Center => NSTextAlignment::Center,
    });
    para.setLineBreakMode(match flow {
        Flow::Wrap => NSLineBreakMode::ByWordWrapping,
        Flow::Path => NSLineBreakMode::ByTruncatingMiddle,
        Flow::Line | Flow::Middle => NSLineBreakMode::ByClipping,
    });
    let col = color(c);
    let keys: [&NSAttributedStringKey; 3] = unsafe {
        [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName]
    };
    let vals: [&AnyObject; 3] = [font, &col, &para];
    let attrs = NSDictionary::from_slices(&keys, &vals);
    let s = NSString::from_str(msg);
    let mut at = r;
    if let Flow::Middle = flow {
        let size = unsafe { s.sizeWithAttributes(Some(&attrs)) };
        at.origin.y += ((r.size.height - size.height) / 2.0).max(0.0);
        at.size.height = size.height;
    }
    unsafe { s.drawInRect_withAttributes(at, Some(&attrs)) };
}

/// A ghost: domed head, straight sides, scalloped hem. The same integer
/// arithmetic as GhostBuster, so the outline lands on the same points.
fn draw_ghost(cx: i32, cy: i32, r: i32, c: u32) {
    let pt = |x: i32, y: i32| NSPoint::new(x as f64, y as f64);
    let mut pts: Vec<NSPoint> = Vec::new();
    let top = cy - r / 3;
    let bottom = cy + r;
    for i in 0..=24 {
        let a = std::f64::consts::PI + std::f64::consts::PI * (i as f64 / 24.0);
        pts.push(pt(cx + (r as f64 * a.cos()) as i32, top + (r as f64 * a.sin()) as i32));
    }
    pts.push(pt(cx + r, bottom));
    let lw = (2 * r) / 4;
    for i in 0..4 {
        let xr = cx + r - i * lw;
        let xl = xr - lw;
        pts.push(pt((xl + xr) / 2, bottom - lw / 2));
        pts.push(pt(xl, bottom));
    }
    pts.push(pt(cx - r, top));

    let body = NSBezierPath::bezierPath();
    body.moveToPoint(pts[0]);
    for p in &pts[1..] {
        body.lineToPoint(*p);
    }
    body.closePath();
    color(c).setFill();
    body.fill();

    color(BG).setFill();
    let (ex, ew) = (r / 3, r / 5);
    for dx in [-ex, ex] {
        NSBezierPath::bezierPathWithOvalInRect(rect(cx + dx - ew, top - ew, cx + dx + ew, top + ew)).fill();
    }
}

fn paint() {
    let f = Fonts::new();
    fill(rect(0, 0, W as i32, H as i32), BG);
    text("Snuffler", rect(24, 16, 400, 42), &f.title, TEXT, Align::Left, Flow::Middle);
    text("Strips invisible pasted images, and shrinks the ones that are real",
        rect(24, 44, 560, 64), &f.small, MUTED, Align::Left, Flow::Middle);

    STATE.with(|st| {
        let st = st.borrow();
        let n = st.sources.len();
        let done = st.stage == Stage::Done;
        let c = Align::Center;

        // ---- left: the input
        rounded(left_panel(), 10.0, Some(PANEL), Some(EDGE), n == 0);
        text("1  ORIGINAL", rect(24, 88, 286, 108), &f.small, MUTED, c, Flow::Line);
        if n > 0 {
            match st.source_names() {
                Some(names) => text(&names, rect(36, 130, 274, 230), &f.body, TEXT, c, Flow::Wrap),
                None => text(&format!("{n} workbooks"), rect(36, 150, 274, 210), &f.big, TEXT, c, Flow::Line),
            }
            text(&crate::app::human(st.total_bytes()), rect(36, 236, 274, 258), &f.small, MUTED, c, Flow::Line);
            text(&st.facts(), rect(36, 262, 274, 306), &f.small, MUTED, c, Flow::Wrap);
            if let Some(note) = st.rejected_note() {
                text(&note, rect(36, 312, 274, 352), &f.small, BAD, c, Flow::Wrap);
            }
        } else if st.stage == Stage::Failed {
            text(&st.error, rect(36, 180, 274, 270), &f.body, BAD, c, Flow::Wrap);
            text("Drop another file, or click to browse", rect(36, 286, 274, 326), &f.small, MUTED, c, Flow::Wrap);
        } else {
            text("Drop Excel files here", rect(36, 210, 274, 236), &f.body, TEXT, c, Flow::Line);
            text("one or many, or click to browse", rect(36, 238, 274, 260), &f.small, MUTED, c, Flow::Line);
        }

        // ---- middle: the two actions
        draw_ghost(380, 118, 26, if n > 0 { TEXT } else { EDGE });

        let cb = st.can_bust();
        rounded(bust_rect(), 8.0, Some(if cb { ACCENT } else { PANEL }), None, false);
        text(&st.button_label(Action::Bust), bust_rect(), &f.button,
            if cb { WHITE } else { MUTED }, c, Flow::Middle);

        text("SQUISH IMAGES AS", rect(310, 214, 450, 232), &f.tiny, MUTED, c, Flow::Line);
        for (i, q) in ORDER.iter().enumerate() {
            let r = seg_rect(i);
            let sel = st.format == *q;
            let applied = st.squished_as == Some(*q);
            rounded(r, 6.0, Some(if sel { SQUISH_C } else { SEG }), None, false);
            let fg = if sel { WHITE } else if applied { GOOD } else { MUTED };
            text(q.label(), r, &f.small, fg, c, Flow::Middle);
        }

        let cs = st.can_squish();
        rounded(squish_rect(), 8.0, Some(if cs { SQUISH_C } else { PANEL }), None, false);
        text(&st.button_label(Action::Squish), squish_rect(), &f.button,
            if cs { WHITE } else { MUTED }, c, Flow::Middle);

        // ---- right: the result
        let rp_bg = if done && st.hot == Hot::Result { PANEL_HOT } else { PANEL };
        rounded(right_panel(), 10.0, Some(rp_bg), Some(if done { GOOD } else { EDGE }), !done);
        text("2  CLEANED", rect(474, 88, 736, 108), &f.small, MUTED, c, Flow::Line);
        if done {
            text(&st.headline, rect(486, 124, 724, 158), &f.big, GOOD, c, Flow::Line);
            text(&st.detail, rect(486, 162, 724, 258), &f.small, MUTED, c, Flow::Wrap);
            text(&st.shown_outputs(), rect(486, 262, 724, 338), &f.small, TEXT, c, Flow::Wrap);
            text(st.reveal_hint(), rect(486, 344, 724, 366), &f.small, MUTED, c, Flow::Line);
        } else if st.stage == Stage::Failed && n > 0 {
            text(&st.error, rect(486, 180, 724, 270), &f.body, BAD, c, Flow::Wrap);
        } else {
            text("The repaired copies will appear here,\nsaved beside each original.",
                rect(486, 200, 724, 260), &f.small, MUTED, c, Flow::Wrap);
        }

        // ---- footer
        let (footer, is_path) = st.footer();
        text(&footer, rect(24, 430, 736, 460), &f.small, MUTED, c,
            if is_path { Flow::Path } else { Flow::Wrap });
    });
}

// ------------------------------------------------------------------ logic

fn hot_at(st: &State, p: NSPoint) -> Hot {
    if hit(bust_rect(), p) && st.can_bust() {
        Hot::Bust
    } else if hit(squish_rect(), p) && st.can_squish() {
        Hot::Squish
    } else if hit(right_panel(), p) && st.stage == Stage::Done {
        Hot::Result
    } else {
        Hot::None
    }
}

fn clickable(st: &State, p: NSPoint) -> bool {
    (hit(bust_rect(), p) && st.can_bust())
        || (hit(squish_rect(), p) && st.can_squish())
        || (0..ORDER.len()).any(|i| hit(seg_rect(i), p))
        || hit(left_panel(), p)
        || (hit(right_panel(), p) && st.stage == Stage::Done)
}

fn set_cursor(hand: bool) {
    if hand {
        NSCursor::pointingHandCursor().set();
    } else {
        NSCursor::arrowCursor().set();
    }
}

fn load(paths: Vec<PathBuf>) {
    if paths.is_empty() || busy() {
        return;
    }
    STATE.with(|st| st.borrow_mut().load(paths));
    redraw();
    selftest_next();
}

fn start(which: Action) {
    let job = STATE.with(|st| st.borrow_mut().begin(which));
    redraw();
    std::thread::spawn(move || {
        let mut done = Vec::with_capacity(job.len());
        for i in 0..job.len() {
            let so_far = done.clone();
            DispatchQueue::main().exec_async(move || {
                STATE.with(|st| st.borrow_mut().progress(i, so_far));
                redraw();
            });
            done.push(job.run_one(i));
        }
        DispatchQueue::main().exec_async(move || {
            STATE.with(|st| st.borrow_mut().finish(which, done));
            redraw();
            selftest_next();
        });
    });
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    // A Finder drag hands over file-reference URLs (file:///.file/id=...);
    // those resolve to an ordinary path URL first.
    let url = url.filePathURL().unwrap_or_else(|| url.retain());
    url.path().map(|p| PathBuf::from(p.to_string()))
}

fn browse(mtm: MainThreadMarker) {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setAllowsMultipleSelection(true);
    panel.setCanChooseDirectories(false);
    panel.setCanChooseFiles(true);
    if panel.runModal() != NSModalResponseOK {
        return;
    }
    let paths: Vec<PathBuf> = panel.URLs().iter().filter_map(|u| url_path(&u)).collect();
    load(paths);
}

/// Shows every cleaned copy, selected, in Finder.
fn reveal() {
    let urls: Vec<Retained<NSURL>> = STATE.with(|st| {
        st.borrow()
            .cleaned()
            .iter()
            .map(|o| NSURL::fileURLWithPath(&NSString::from_str(&o.output.to_string_lossy())))
            .collect()
    });
    if !urls.is_empty() {
        NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
    }
}

fn on_click(mtm: MainThreadMarker, p: NSPoint) {
    for (i, q) in ORDER.iter().enumerate() {
        if hit(seg_rect(i), p) {
            if STATE.with(|st| st.borrow_mut().select(*q)) {
                redraw();
            }
            return;
        }
    }
    if hit(bust_rect(), p) {
        if STATE.with(|st| st.borrow().can_bust()) {
            start(Action::Bust);
        }
        return;
    }
    if hit(squish_rect(), p) {
        if STATE.with(|st| st.borrow().can_squish()) {
            start(Action::Squish);
        }
        return;
    }
    if hit(left_panel(), p) {
        if !busy() {
            browse(mtm);
        }
        return;
    }
    if hit(right_panel(), p) && STATE.with(|st| st.borrow().stage == Stage::Done) {
        reveal();
    }
}

// --------------------------------------------------------------- selftest

/// CI drives the real window through this, clicking as a person would:
/// SNUFFLER_SELFTEST="bust,xsmall" presses SNUFF, then picks xsmall and presses
/// SQUISH. SNUFFLER_SELFTEST_REPORT names a file that receives what the result
/// panel says once the last step finishes.
fn selftest_next() {
    if busy() {
        return;
    }
    let step = SELFTEST.with(|s| s.borrow_mut().as_mut().map(|steps| steps.pop()));
    match step {
        None => {}
        Some(None) => {
            SELFTEST.with(|s| *s.borrow_mut() = None);
            selftest_report();
        }
        Some(Some(step)) => {
            if step == "bust" {
                if STATE.with(|st| st.borrow().can_bust()) {
                    return start(Action::Bust);
                }
            } else if let Some(q) = Quality::parse(&step) {
                STATE.with(|st| st.borrow_mut().select(q));
                redraw();
                if STATE.with(|st| st.borrow().can_squish()) {
                    return start(Action::Squish);
                }
            }
            selftest_next();
        }
    }
}

fn selftest_report() {
    let Ok(path) = std::env::var("SNUFFLER_SELFTEST_REPORT") else { return };
    let report = STATE.with(|st| {
        let st = st.borrow();
        format!(
            "stage={:?}\nheadline={}\ndetail={}\nerror={}\noutputs={}\n",
            st.stage,
            st.headline,
            st.detail.replace('\n', " | "),
            st.error,
            st.cleaned().iter().map(|o| o.output.display().to_string()).collect::<Vec<_>>().join(" | "),
        )
    });
    let _ = std::fs::write(path, report);
}

// ------------------------------------------------------------------- view

define_class!(
    // SAFETY: NSView has no subclassing requirements, and Canvas has no Drop.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnufflerCanvas"]
    struct Canvas;

    impl Canvas {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            paint();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            on_click(self.mtm(), p);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let (changed, hand) = STATE.with(|st| {
                let mut st = st.borrow_mut();
                let h = hot_at(&st, p);
                let c = st.hot != h;
                st.hot = h;
                (c, clickable(&st, p))
            });
            set_cursor(hand);
            if changed {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            set_cursor(STATE.with(|st| clickable(&st.borrow(), p)));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            let changed = STATE.with(|st| {
                let mut st = st.borrow_mut();
                let c = st.hot != Hot::None;
                st.hot = Hot::None;
                c
            });
            set_cursor(false);
            if changed {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if busy() { NSDragOperation::None } else { NSDragOperation::Copy }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let mut paths = Vec::new();
            if let Some(items) = sender.draggingPasteboard().pasteboardItems() {
                for item in items.iter() {
                    let Some(s) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else { continue };
                    if let Some(p) = NSURL::URLWithString(&s).and_then(|u| url_path(&u)) {
                        paths.push(p);
                    }
                }
            }
            load(paths);
            true
        }
    }

    unsafe impl NSObjectProtocol for Canvas {}
);

impl Canvas {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H));
        let this = Self::alloc(mtm).set_ivars(());
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };

        view.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));

        let owner: &AnyObject = &view;
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::CursorUpdate
                    | NSTrackingAreaOptions::ActiveAlways
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(owner),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }
}

// ------------------------------------------------------------ application

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and Delegate has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnufflerDelegate"]
    struct Delegate;

    impl Delegate {
        /// File > Open (Cmd-O). A menu item with no target walks the responder
        /// chain, which ends at the application delegate.
        #[unsafe(method(openDocument:))]
        fn open_document(&self, _sender: Option<&AnyObject>) {
            if !busy() {
                browse(self.mtm());
            }
        }
    }

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _note: &NSNotification) {
            build_window(self.mtm());
            let pending = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
            load(pending);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        /// Files dropped on the Dock icon or the app in Finder, or chosen with
        /// "Open With". This can arrive before the window exists.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            let paths: Vec<PathBuf> = urls.iter().filter_map(|u| url_path(&u)).collect();
            if VIEW.with(|v| v.borrow().is_some()) {
                load(paths);
            } else {
                PENDING.with(|p| p.borrow_mut().extend(paths));
            }
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: Sel, key: &str) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    }
}

/// Just enough menu bar for the keys a Mac user expects to work.
fn build_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let menus = [
        ("Snuffler", vec![
            menu_item(mtm, "Hide Snuffler", sel!(hide:), "h"),
            menu_item(mtm, "Quit Snuffler", sel!(terminate:), "q"),
        ]),
        ("File", vec![
            menu_item(mtm, "Open\u{2026}", sel!(openDocument:), "o"),
            menu_item(mtm, "Close Window", sel!(performClose:), "w"),
        ]),
        ("Window", vec![
            menu_item(mtm, "Minimize", sel!(performMiniaturize:), "m"),
        ]),
    ];
    for (title, items) in menus {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for item in &items {
            menu.addItem(item);
        }
        let top = NSMenuItem::new(mtm);
        top.setSubmenu(Some(&menu));
        bar.addItem(&top);
    }
    bar
}

fn build_window(mtm: MainThreadMarker) {
    let view = Canvas::new(mtm);
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // The window is owned from Rust, so AppKit must not also release it.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Snuffler"));
    // Blend the title bar into the dark window, as GhostBuster does with DWM.
    window.setTitlebarAppearsTransparent(true);
    window.setBackgroundColor(Some(&color(BG)));
    window.setContentView(Some(&view));
    window.center();
    window.makeKeyAndOrderFront(None);
    VIEW.with(|v| *v.borrow_mut() = Some(view));
    WINDOW.with(|w| *w.borrow_mut() = Some(window));
}

pub fn run(args: Vec<String>) {
    let mtm = MainThreadMarker::new().expect("the window must be created on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // Dark everywhere, including the open panel.
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        app.setAppearance(Some(&dark));
    }
    app.setMainMenu(Some(&build_menu(mtm)));
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    // Paths on the command line, when started from a terminal. Finder never
    // does this -- it sends application:openURLs: instead -- and old systems
    // add a -psn_ process-serial argument, which is not a file.
    let files = args.iter().filter(|a| !a.starts_with("-psn_")).map(PathBuf::from);
    PENDING.with(|p| p.borrow_mut().extend(files));

    if let Ok(steps) = std::env::var("SNUFFLER_SELFTEST") {
        let mut steps: Vec<String> = steps.split(',').map(|s| s.trim().to_ascii_lowercase()).collect();
        steps.reverse();
        SELFTEST.with(|s| *s.borrow_mut() = Some(steps));
    }

    // activate() is macOS 14+; this one exists on every version we support.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
    drop(delegate);
}
