# Snuffler

GhostBuster, for the Mac. Two repairs for Excel workbooks that have grown
unusable.

**BUST** removes the invisible, zero-size pictures Excel accumulates in a
worksheet when images are pasted onto it over and over. They are never visible
and never deleted, but Excel lays out every one of them, so a sheet carrying
tens of thousands becomes unresponsive.

**SQUISH** re-encodes the workbook's images. Excel stores a pasted picture as
an "Enhanced Metafile", which in practice is usually a raw *uncompressed*
bitmap in a wrapper -- often megabytes for a thumbnail-sized image.

Same window, same rules and same wording as GhostBuster on Windows; see
GhostBuster's README for the diagnosis behind both repairs.

## Sending it to someone

Get `Snuffler.dmg` from the latest release, or from the `Snuffler.dmg`
artifact of any CI run. (GitHub wraps artifacts in a zip; unzip that anywhere,
Windows included, and send the `.dmg` inside.)

**Send the `.dmg` itself** -- Google Drive, email, AirDrop, anything. Don't
zip it. A Mac app is really a folder with an executable inside, and a disk
image carries both, executable bit included, through any transfer. A bare
binary downloaded from anywhere loses that bit and opens in TextEdit; a zip
made on Windows loses it too. Nor does a `.dmg` set off Google Drive's
"this archive contains an executable" warning.

On their Mac they double-click the `.dmg` and get a window holding Snuffler,
a shortcut to Applications, and `How to open Snuffler.txt`. They drag Snuffler
into Applications and double-click it.

### The one-time warning

Without a paid Apple Developer ID and notarisation, macOS stops *any* app
downloaded from the internet on its first launch. There is no free way around
that. Snuffler is ad-hoc signed, which matters: an unsigned app on Apple
Silicon is reported as "damaged" with no way past it, whereas an ad-hoc signed
one gets the ordinary warning, which can be approved once:

1. Double-click Snuffler. At the warning, click **Done**.
2. **System Settings -> Privacy & Security**, scroll down, **Open Anyway**.
3. Password, then **Open Anyway** again.

After that it opens like any other app. Before macOS 15, right-click -> Open
does the same in one step. This is spelled out in `How to open Snuffler.txt`
inside the disk image; it is worth pasting into the message that goes with it
too.

Only files that arrive through a browser, Mail, Messages or AirDrop are
flagged for this check. One copied off a USB stick is not, and opens
straight away.

The first time Snuffler saves into Desktop, Documents or Downloads, macOS also
asks whether it may use that folder. That is a normal privacy prompt: Allow.

## What was reused

| | GhostBuster | Snuffler |
|---|---|---|
| Runs on | Windows | macOS 11+, Apple Silicon and Intel |
| Form | one `.exe` | `Snuffler.app`, delivered in `Snuffler.dmg` |
| Window | Win32, owner-drawn | AppKit, owner-drawn |
| Imaging | WIC | pure Rust: `image`, `png`, `jpeg-encoder` |

- **`src/clean.rs` is GhostBuster's, byte for byte**: reading and writing the
  package, ghost detection, the relationship pruning and orphan sweep, the
  richData protection, scan-time rejection, output naming.
- **`src/headless.rs`** is GhostBuster's headless mode: same flags, same JSON.
- **`src/squish.rs`** keeps every decision: which EMFs are bitmap wrappers,
  Med's target size (the same `floor(x + 0.5)` arithmetic, so the same
  integer dimensions), "replace only if smaller", and the renaming. Only the
  encoders changed.
- **`src/app.rs`** is GhostBuster's window logic -- gating, latching,
  re-arming on a new quality, re-deriving from the original, every message --
  lifted out of the wndproc so it is plain, testable data.
- **`src/gui.rs`** is the only new code of substance: it paints `app.rs`'s
  state with the same rectangles, palette and font sizes as the Win32 window.
  Points on a Mac are GhostBuster's logical pixels, so the layout numbers are
  copied unchanged.

Why not Apple's ImageIO, which would be the true counterpart of WIC? Because
pure Rust produces the same bytes everywhere. The whole repair core builds and
tests on Windows and Linux, which matters when nobody working on it owns a Mac.

A few differences are deliberate:

- The repair runs on a background thread. GhostBuster works on the UI thread
  and forces repaints; macOS shows the spinning beach ball after a couple of
  seconds of that.
- Clicking the result panel selects every cleaned copy in Finder, not just
  the first.
- A workbook can also be dropped on the Dock icon, opened with *Open With*,
  or chosen with Cmd-O.
- Palette PNGs are re-packed as palette PNGs. WIC does that implicitly; a
  general decoder expands them to RGBA, after which re-encoding can never
  beat the original.

## Conformance suite

```bash
python conformance/run.py
```

GhostBuster's suite demands byte-identical media from its two builds, because
both drive the same WIC encoders. Snuffler cannot share those, so it is held
to what that identity stood for:

- every structural check from GhostBuster's suite, unchanged: zip integrity,
  every relationship resolves, media types declared, richData survives, no
  ghost left after a bust, every cell value unchanged
- identical decisions wherever the decision is logic rather than encoder:
  rejections, ghosts removed, pictures kept, media removed, layout-data sizes
- media GhostBuster leaves alone are byte-identical
- **every image GhostBuster shrinks, Snuffler shrinks too**, to the same name
  and the same pixel dimensions. It may shrink a few more, where its PNG
  encoder beats the original and WIC's did not
- the lossless tier is pixel-identical, both to GhostBuster and to the source;
  Med is within a mean difference of 3/255 of GhostBuster; an XSmall JPEG is
  no further from the source than GhostBuster's, give or take 1/255

The reference is GhostBuster's Rust build. On Windows the suite can run it
live (`--reference path/to/ghostbuster.exe`); in CI it compares against
`conformance/golden/`, recorded from it with `--write-golden`.

On the synthetic fixtures, against the golden results (this is what CI runs,
for both the Apple Silicon and the Intel slice, from inside the `.dmg`):

```
bust           OK        untouched 19
squish-high    OK        untouched 11, pixel-exact 10, bytes -2.1%
squish-med     OK        untouched 5, close 16, bytes -7.1%
squish-xsmall  OK        untouched 6, close 14, +1 more shrunk, bytes -7.1%
bust+xsmall    OK        untouched 4, close 14, +1 more shrunk, bytes -7.1%
14 outputs structurally checked; 0 problems
```

On the five real workbooks GhostBuster was diagnosed against, against live
GhostBuster, run locally on Windows:

```
bust           OK        untouched 113
squish-high    OK        untouched 64, pixel-exact 49, +8 more shrunk, bytes -1.3%
squish-med     OK        untouched 64, close 50, +7 more shrunk, bytes -2.4%
squish-xsmall  OK        untouched 56, close 65, bytes -4.0%
bust+xsmall    OK        untouched 48, close 65, bytes -4.0%
23 outputs structurally checked; 0 problems
```

### Fixtures

The real workbooks are private business documents and are **never
committed** -- `.gitignore` refuses every spreadsheet outside
`conformance/fixtures/` and `conformance/golden/`. To compare against them,
keep them outside this repository and pass `--fixtures DIR --reference ...`.

`conformance/make_fixtures.py` builds synthetic stand-ins that reproduce every
shape the repairs care about: all three zero-area anchor kinds spread down
column A, a ghost-only image, an image shared by a ghost and a real picture, a
sheet of nothing but ghosts, an in-cell richData picture, bitmap EMFs
bottom-up and top-down, a real vector EMF and a WMF (both left alone), one EMF
too small for JPEG to beat, JPEGs that will and will not shrink, a palette
PNG, an uncompressed PNG, a BMP, an `.xlsm`, a workbook with nothing to do,
and two fakes. Regenerate fixtures and golden results together or not at all.

## Building

Anywhere Rust runs:

```bash
cargo test
cargo build --release
```

Off macOS that builds a headless-only binary: the window is AppKit. On a Mac,
or in CI:

```bash
packaging/make_app.sh
```

builds a universal `dist/Snuffler.app` (arm64 + x86_64, macOS 11+), ad-hoc
signs it, and wraps it in `dist/Snuffler.dmg`.

CI (`.github/workflows/build.yml`, on GitHub's macOS runners) does all of
that on every push, then:

- runs the unit tests and the conformance suite against the binary inside the
  `.dmg`, natively and again under Rosetta
- opens the app the way Finder does, hands it two workbooks, and has it press
  BUST and SQUISH through its own self-test hook, then checks what the result
  panel said and that the cleaned files are valid
- uploads screenshots of the window and the `.dmg` as artifacts

Pushing a tag like `v1.0.1` also publishes the `.dmg` as a GitHub release.

Headless, for scripting:

```
Snuffler.app/Contents/MacOS/Snuffler --headless --bust --squish XSmall --json out.json <file>...
```

## Known scope limits

- Everything GhostBuster leaves out, Snuffler does too: SQUISH only handles
  EMFs that wrap a single bitmap; only `.xlsx` / `.xlsm`; no drag-out.
- The window has only been seen through CI screenshots, never on a Mac in
  front of anyone. The logic behind it is unit-tested, and CI drives it
  end to end, but a real click-through on a real Mac is still the one missing
  check.
- GIFs are expanded before re-encoding, unlike palette PNGs. Excel rarely
  stores them.
- Every build carries a fresh ad-hoc signature, so an updated Snuffler counts
  as a new app to macOS: first-launch approval and the folder prompts happen
  again.
- GhostBuster's "in use - close it in Excel first" cannot occur: Excel on a
  Mac does not lock the files it has open.
