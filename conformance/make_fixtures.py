#!/usr/bin/env python3
"""Builds the synthetic workbooks in conformance/fixtures/.

The real workbooks GhostBuster was diagnosed against are private business
documents, so they never go in this repository. These stand-ins reproduce
every shape the repairs care about instead:

  ghosts.xlsx      an infested sheet: zero-area anchors of all three kinds,
                   spread down column A, a ghost-only image BUST must delete,
                   an image shared by a ghost and a real picture, a second
                   sheet that is nothing but ghosts, and an in-cell
                   (xl/richData) picture that must survive untouched
  emf-heavy.xlsm   bitmap-in-a-metafile EMFs of several sizes, bottom-up and
                   top-down, one genuine vector EMF and one WMF (both must be
                   skipped), one too small for JPEG to beat
  photos.xlsx      JPEGs that will and will not re-encode smaller, a palette
                   PNG, an uncompressed PNG and a BMP; no ghosts at all
  clean.xlsx       no pictures whatsoever: every action is a no-op
  not-excel.xlsx   plain text wearing an .xlsx extension
  no-workbook.xlsx a zip with no xl/workbook.xml inside

Output is deterministic (fixed zip timestamps, seeded pixels) so re-running
gives the same bytes. The committed fixtures are what the golden results were
produced from; regenerate both together or neither.

    python conformance/make_fixtures.py
"""

import io
import os
import random
import struct
import zipfile

import openpyxl
from PIL import Image, ImageChops, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")
EMU = 914400  # per inch
STAMP = (2026, 1, 1, 0, 0, 0)

NS_XDR = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"
NS_A = "http://schemas.openxmlformats.org/drawingml/2006/main"
NS_R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
REL_IMAGE = NS_R + "/image"
REL_DRAWING = NS_R + "/drawing"

CONTENT_TYPES = {
    "emf": "image/x-emf",
    "wmf": "image/x-wmf",
    "png": "image/png",
    "jpeg": "image/jpeg",
    "bmp": "image/bmp",
}


# ------------------------------------------------------------------ pixels

def picture(w, h, seed, noise=4):
    """A deterministic stand-in for a product photo: gradient, shapes, grain."""
    rnd = random.Random(seed)
    img = Image.new("RGB", (w, h))
    top = [rnd.randrange(40, 230) for _ in range(3)]
    bot = [rnd.randrange(40, 230) for _ in range(3)]
    grad = Image.linear_gradient("L").resize((w, h))
    img = Image.composite(Image.new("RGB", (w, h), tuple(bot)),
                          Image.new("RGB", (w, h), tuple(top)), grad)
    d = ImageDraw.Draw(img)
    for _ in range(6):
        x0, y0 = rnd.randrange(w), rnd.randrange(h)
        x1, y1 = x0 + rnd.randrange(w // 6, w // 2), y0 + rnd.randrange(h // 6, h // 2)
        colour = tuple(rnd.randrange(256) for _ in range(3))
        if rnd.random() < 0.5:
            d.ellipse([x0, y0, x1, y1], fill=colour)
        else:
            d.rectangle([x0, y0, x1, y1], fill=colour)
    for i in range(3):
        d.text((8, 8 + 14 * i), "SKU %05d  $%d.%02d" % (rnd.randrange(99999),
               rnd.randrange(99), rnd.randrange(100)), fill=(20, 20, 20))
    if noise:
        grain = Image.frombytes("L", (w, h), rnd.randbytes(w * h))
        grain = grain.point(lambda v: v * (2 * noise + 1) // 256).convert("RGB")
        img = ImageChops.add(img, grain, 1.0, -noise)
    return img


def png(img, **kw):
    b = io.BytesIO()
    img.save(b, "PNG", **kw)
    return b.getvalue()


def jpeg(img, quality, subsampling=0):
    b = io.BytesIO()
    img.save(b, "JPEG", quality=quality, subsampling=subsampling)
    return b.getvalue()


def bmp(img):
    b = io.BytesIO()
    img.save(b, "BMP")
    return b.getvalue()


def emf(img, top_down=False, vector=False):
    """An EMF exactly the way Excel writes a pasted picture:
    EMR_HEADER + EMR_STRETCHDIBITS (32bpp BI_RGB) + EMR_EOF.

    `vector` inserts one extra drawing record, which makes it a real metafile
    rather than a bitmap wrapper -- SQUISH must leave that alone.
    """
    w, h = img.size
    rows = []
    b, g, r = [c.tobytes() for c in reversed(img.convert("RGB").split())]
    # BGRX with a zero padding byte, as Excel leaves it.
    for y in range(h):
        row = bytearray(w * 4)
        for x in range(w):
            i = y * w + x
            row[4 * x] = b[i]
            row[4 * x + 1] = g[i]
            row[4 * x + 2] = r[i]
        rows.append(bytes(row))
    if not top_down:
        rows.reverse()
    bits = b"".join(rows)

    bmi = struct.pack("<IiiHHIIiiII", 40, w, -h if top_down else h, 1, 32, 0,
                      len(bits), 3780, 3780, 0, 0)
    sdib_head = 80
    sdib = struct.pack(
        "<II4i6i4I2I2i",
        81, sdib_head + len(bmi) + len(bits),
        0, 0, w - 1, h - 1,                 # rclBounds
        0, 0, 0, 0, w, h,                    # xDest yDest xSrc ySrc cxSrc cySrc
        sdib_head, len(bmi), sdib_head + len(bmi), len(bits),
        0, 0x00CC0020,                       # DIB_RGB_COLORS, SRCCOPY
        w, h,                                # cxDest cyDest
    ) + bmi + bits
    extra = struct.pack("<III", 18, 12, 1) if vector else b""  # EMR_SETBKMODE
    eof = struct.pack("<IIIII", 14, 20, 0, 16, 20)
    records = 4 if vector else 3
    total = 88 + len(extra) + len(sdib) + len(eof)
    header = struct.pack(
        "<II4i4iIIIIHHIIIii ii".replace(" ", ""),
        1, 88,
        0, 0, w - 1, h - 1,                  # rclBounds
        0, 0, w * 26, h * 26,                # rclFrame, .01 mm
        0x464D4520, 0x10000, total, records,
        1, 0, 0, 0, 0,
        1920, 1080, 508, 286,                # szlDevice, szlMillimeters
    )
    assert len(header) == 88
    return header + extra + sdib + eof


# ----------------------------------------------------------------- drawings

def _pic(rid, cx, cy, n):
    return (
        '<xdr:pic><xdr:nvPicPr><xdr:cNvPr id="%d" name="Picture %d"/>'
        '<xdr:cNvPicPr><a:picLocks noChangeAspect="1"/></xdr:cNvPicPr></xdr:nvPicPr>'
        '<xdr:blipFill><a:blip xmlns:r="%s" r:embed="%s"/><a:stretch><a:fillRect/>'
        '</a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x="0" y="0"/>'
        '<a:ext cx="%d" cy="%d"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/>'
        '</a:prstGeom></xdr:spPr></xdr:pic><xdr:clientData/>'
    ) % (n + 1, n, NS_R, rid, cx, cy)


def _point(tag, col, row, coff=0, roff=0):
    return ("<xdr:%s><xdr:col>%d</xdr:col><xdr:colOff>%d</xdr:colOff>"
            "<xdr:row>%d</xdr:row><xdr:rowOff>%d</xdr:rowOff></xdr:%s>"
            % (tag, col, coff, row, roff, tag))


class Drawing:
    def __init__(self):
        self.anchors = []

    def one_cell(self, rid, col, row, cx, cy):
        n = len(self.anchors) + 1
        self.anchors.append(
            '<xdr:oneCellAnchor>%s<xdr:ext cx="%d" cy="%d"/>%s</xdr:oneCellAnchor>'
            % (_point("from", col, row), cx, cy, _pic(rid, cx, cy, n)))

    def two_cell(self, rid, frm, to, cx, cy):
        n = len(self.anchors) + 1
        self.anchors.append(
            '<xdr:twoCellAnchor editAs="oneCell">%s%s%s</xdr:twoCellAnchor>'
            % (_point("from", *frm), _point("to", *to), _pic(rid, cx, cy, n)))

    def absolute(self, rid, x, y, cx, cy):
        n = len(self.anchors) + 1
        self.anchors.append(
            '<xdr:absoluteAnchor><xdr:pos x="%d" y="%d"/><xdr:ext cx="%d" cy="%d"/>%s'
            '</xdr:absoluteAnchor>' % (x, y, cx, cy, _pic(rid, cx, cy, n)))

    def textbox(self, frm, to, words):
        n = len(self.anchors) + 1
        self.anchors.append(
            '<xdr:twoCellAnchor>%s%s<xdr:sp macro="" textlink=""><xdr:nvSpPr>'
            '<xdr:cNvPr id="%d" name="TextBox %d"/><xdr:cNvSpPr txBox="1"/></xdr:nvSpPr>'
            '<xdr:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr>'
            '<xdr:txBody><a:bodyPr/><a:p><a:r><a:t>%s</a:t></a:r></a:p></xdr:txBody>'
            '</xdr:sp><xdr:clientData/></xdr:twoCellAnchor>'
            % (_point("from", *frm), _point("to", *to), n + 1, n, words))

    def xml(self):
        return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n'
                '<xdr:wsDr xmlns:xdr="%s" xmlns:a="%s">%s</xdr:wsDr>'
                % (NS_XDR, NS_A, "".join(self.anchors)))


def rels(entries):
    body = "".join('<Relationship Id="%s" Type="%s" Target="%s"/>' % e for e in entries)
    return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/'
            'relationships">%s</Relationships>' % body)


# --------------------------------------------------------------- packaging

class Book:
    """An openpyxl workbook, then extra parts spliced into its zip."""

    def __init__(self, sheets):
        wb = openpyxl.Workbook()
        first = True
        for title, rows in sheets:
            ws = wb.active if first else wb.create_sheet()
            ws.title = title
            first = False
            for row in rows:
                ws.append(row)
        buf = io.BytesIO()
        wb.save(buf)
        z = zipfile.ZipFile(io.BytesIO(buf.getvalue()))
        self.order = z.namelist()
        self.parts = {n: z.read(n) for n in self.order}
        # openpyxl writes package-absolute targets ("/xl/worksheets/..."); Excel
        # writes them relative to the part, and so do these fixtures.
        self.parts["xl/_rels/workbook.xml.rels"] = self.parts[
            "xl/_rels/workbook.xml.rels"].replace(b'Target="/xl/', b'Target="')
        self.parts["docProps/core.xml"] = (
            b'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            b'<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/'
            b'metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" '
            b'xmlns:dcterms="http://purl.org/dc/terms/" '
            b'xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">'
            b'<dc:creator>snuffler fixtures</dc:creator>'
            b'<dcterms:created xsi:type="dcterms:W3CDTF">2026-01-01T00:00:00Z</dcterms:created>'
            b'<dcterms:modified xsi:type="dcterms:W3CDTF">2026-01-01T00:00:00Z</dcterms:modified>'
            b'</cp:coreProperties>')
        self.defaults = set()
        self.overrides = []
        self.workbook_rels = []

    def add(self, name, data):
        if isinstance(data, str):
            data = data.encode("utf-8")
        if name not in self.parts:
            self.order.append(name)
        self.parts[name] = data

    def media(self, leaf, data):
        self.defaults.add(leaf.rsplit(".", 1)[1])
        self.add("xl/media/" + leaf, data)
        return "../media/" + leaf

    def drawing(self, sheet_no, drawing_no, drawing, targets):
        """`targets` maps rId -> media target for this drawing's rels."""
        dname = "xl/drawings/drawing%d.xml" % drawing_no
        self.add(dname, drawing.xml())
        self.add("xl/drawings/_rels/drawing%d.xml.rels" % drawing_no,
                 rels([(rid, REL_IMAGE, t) for rid, t in targets.items()]))
        self.overrides.append(("/" + dname,
                               "application/vnd.openxmlformats-officedocument.drawing+xml"))
        sheet = "xl/worksheets/sheet%d.xml" % sheet_no
        srels = "xl/worksheets/_rels/sheet%d.xml.rels" % sheet_no
        assert srels not in self.parts, "openpyxl already wrote sheet rels"
        self.add(srels, rels([("rIdDr1", REL_DRAWING,
                               "../drawings/drawing%d.xml" % drawing_no)]))
        xml = self.parts[sheet].decode("utf-8")
        tag = '<drawing xmlns:r="%s" r:id="rIdDr1"/>' % NS_R
        self.parts[sheet] = xml.replace("</worksheet>", tag + "</worksheet>").encode("utf-8")

    def rich_data_image(self, leaf, data):
        """An in-cell ("Place in Cell") picture: xl/richData parts with their
        own relationships, never referenced from any drawing."""
        target = self.media(leaf, data)
        self.add("xl/richData/rdrichvalue.xml",
                 '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n'
                 '<rvData xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/'
                 'richdata" count="1"><rv s="0"><v>0</v><v>5</v></rv></rvData>')
        self.add("xl/richData/richValueRel.xml",
                 '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n'
                 '<richValueRels xmlns="http://schemas.microsoft.com/office/spreadsheetml/'
                 '2022/richvaluerel" xmlns:r="%s"><rel r:id="rId1"/></richValueRels>' % NS_R)
        self.add("xl/richData/_rels/richValueRel.xml.rels",
                 rels([("rId1", REL_IMAGE, target)]))
        self.overrides += [
            ("/xl/richData/rdrichvalue.xml", "application/vnd.ms-excel.rdrichvalue+xml"),
            ("/xl/richData/richValueRel.xml", "application/vnd.ms-excel.richvaluerel+xml"),
        ]
        self.workbook_rels += [
            ("rIdRd1", "http://schemas.microsoft.com/office/2017/06/relationships/rdRichValue",
             "richData/rdrichvalue.xml"),
            ("rIdRd2", "http://schemas.microsoft.com/office/2022/10/relationships/richValueRel",
             "richData/richValueRel.xml"),
        ]

    def macro_enabled(self):
        ct = self.parts["[Content_Types].xml"].decode("utf-8")
        ct = ct.replace("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
                        "application/vnd.ms-excel.sheet.macroEnabled.main+xml")
        self.parts["[Content_Types].xml"] = ct.encode("utf-8")

    def save(self, filename):
        ct = self.parts["[Content_Types].xml"].decode("utf-8")
        head = "".join('<Default Extension="%s" ContentType="%s"/>' % (e, CONTENT_TYPES[e])
                       for e in sorted(self.defaults) if 'Extension="%s"' % e not in ct)
        head += "".join('<Override PartName="%s" ContentType="%s"/>' % o for o in self.overrides)
        i = ct.index(">", ct.index("<Types")) + 1
        self.parts["[Content_Types].xml"] = (ct[:i] + head + ct[i:]).encode("utf-8")
        if self.workbook_rels:
            wr = self.parts["xl/_rels/workbook.xml.rels"].decode("utf-8")
            extra = "".join('<Relationship Id="%s" Type="%s" Target="%s"/>' % r
                            for r in self.workbook_rels)
            self.parts["xl/_rels/workbook.xml.rels"] = wr.replace(
                "</Relationships>", extra + "</Relationships>").encode("utf-8")
        write_zip(os.path.join(OUT, filename), [(n, self.parts[n]) for n in self.order])


def write_zip(path, entries):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in entries:
            info = zipfile.ZipInfo(name, STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, data)


def costing_rows(seed, n=40):
    rnd = random.Random(seed)
    rows = [["Style", "Description", "Qty", "FOB", "Duty %", "Landed", "Ships"]]
    for i in range(n):
        qty = rnd.randrange(12, 5000)
        fob = round(rnd.uniform(0.4, 38.0), 2)
        rows.append(["ST-%04d" % rnd.randrange(10000),
                     rnd.choice(["Tee", "Hoodie", "Tag", "Pallet", "Leash", "Cap"]),
                     qty, fob, rnd.choice([0.0, 0.12, 0.165, 0.32]),
                     "=C%d*D%d*(1+E%d)" % (i + 2, i + 2, i + 2),
                     rnd.choice([True, False])])
    rows.append(["", "Total", "=SUM(C2:C%d)" % (n + 1), None, None,
                 "=SUM(F2:F%d)" % (n + 1), None])
    return rows


# ----------------------------------------------------------------- fixtures

def ghosts():
    b = Book([("Costing", costing_rows(1)), ("Pallets", costing_rows(2, 12))])
    d = Drawing()
    t = {
        "rId1": b.media("image1.emf", emf(picture(480, 360, 11))),
        "rId2": b.media("image2.png", png(picture(400, 300, 12, noise=0), compress_level=0)),
        "rId3": b.media("image3.jpeg", jpeg(picture(320, 240, 13, noise=6), 100)),
        "rId4": b.media("image4.jpeg", jpeg(picture(64, 48, 14), 90)),
    }
    # The real pictures. Med targets 150 dpi at the displayed width: image1 at
    # 2 in -> 300 px (downsampled), image2 at 4 in -> 600 px (never upscaled),
    # image3 at 1 in -> 150 px.
    d.one_cell("rId1", 3, 2, 2 * EMU, int(1.5 * EMU))
    d.two_cell("rId2", (8, 2), (12, 14), 4 * EMU, 3 * EMU)
    d.absolute("rId3", 6 * EMU, 5 * EMU, EMU, int(0.75 * EMU))
    d.textbox((3, 20), (7, 24), "Approved for costing")
    # The infestation. Column A, but spread over 28 rows -- a rule keyed on
    # row 0 would catch none of them.
    for i in range(240):
        d.one_cell("rId1" if i % 40 == 0 else "rId4", 0, i % 28, 0, 0)
    for i in range(12):
        d.one_cell("rId4", 0, i, EMU, 0)                 # zero height only
    for i in range(20):
        d.two_cell("rId4", (0, i, 0, 0), (0, i, 0, 0), 0, 0)
    for i in range(4):
        d.absolute("rId4", 0, i * EMU, int(0.5 * EMU), 0)
    b.drawing(1, 1, d, t)

    g = Drawing()
    for i in range(30):
        g.one_cell("rId1", 0, i % 9, 0, 0)
    b.drawing(2, 2, g, {"rId1": b.media("image5.png", png(picture(90, 90, 15)))})

    rgba = picture(200, 200, 16, noise=0).convert("RGBA")
    mask = Image.new("L", (200, 200), 0)
    ImageDraw.Draw(mask).ellipse([10, 10, 190, 190], fill=255)
    rgba.putalpha(mask)
    b.rich_data_image("image6.png", png(rgba, compress_level=1))
    b.save("ghosts.xlsx")


def emf_heavy():
    b = Book([("Tags", costing_rows(3, 25))])
    d = Drawing()
    t = {
        "rId1": b.media("image1.emf", emf(picture(640, 480, 21))),
        "rId2": b.media("image2.emf", emf(picture(300, 200, 22), top_down=True)),
        "rId3": b.media("image3.emf", emf(picture(256, 256, 23, noise=2))),
        "rId4": b.media("image4.emf", emf(picture(120, 90, 24), vector=True)),
        "rId5": b.media("image5.wmf", b"\xd7\xcd\xc6\x9a" + bytes(range(256)) * 4),
        "rId6": b.media("image6.emf", emf(picture(8, 8, 26, noise=0))),
    }
    d.one_cell("rId1", 1, 1, 3 * EMU, int(2.25 * EMU))        # -> 450 x 338
    d.one_cell("rId2", 1, 18, 4 * EMU, int(2.67 * EMU))       # wider than native
    d.one_cell("rId3", 7, 1, int(1.25 * EMU), int(1.25 * EMU))  # -> 188 x 188
    d.one_cell("rId4", 7, 12, EMU, int(0.75 * EMU))
    d.one_cell("rId5", 7, 18, EMU, EMU)
    d.one_cell("rId6", 10, 1, int(0.1 * EMU), int(0.1 * EMU))
    d.two_cell("rId1", (0, 3, 0, 0), (0, 3, 0, 0), 0, 0)
    d.two_cell("rId1", (0, 9, 0, 0), (0, 9, 0, 0), 0, 0)
    b.drawing(1, 1, d, t)
    b.macro_enabled()
    b.save("emf-heavy.xlsm")


def photos():
    b = Book([("Catalogue", costing_rows(4, 30))])
    d = Drawing()
    t = {}
    for i, (w, h) in enumerate([(360, 270), (420, 315), (300, 300), (480, 320), (256, 384)]):
        rid = "rId%d" % (i + 1)
        t[rid] = b.media("image%d.jpeg" % (i + 1), jpeg(picture(w, h, 31 + i, noise=5), 100))
        d.one_cell(rid, 2 + 3 * i, 1, int(1.5 * EMU), int(1.5 * EMU * h / w))
    t["rId6"] = b.media("image6.jpeg", jpeg(picture(240, 180, 36, noise=5), 55, 2))
    d.one_cell("rId6", 2, 12, 2 * EMU, int(1.5 * EMU))
    pal = picture(120, 120, 37, noise=0).quantize(16)
    t["rId7"] = b.media("image7.png", png(pal, optimize=True))
    d.one_cell("rId7", 6, 12, EMU, EMU)
    t["rId8"] = b.media("image8.png", png(picture(220, 160, 38, noise=0), compress_level=0))
    d.one_cell("rId8", 9, 12, 2 * EMU, int(1.45 * EMU))
    t["rId9"] = b.media("image9.bmp", bmp(picture(200, 150, 39, noise=3)))
    d.one_cell("rId9", 12, 12, EMU, int(0.75 * EMU))
    b.drawing(1, 1, d, t)
    b.save("photos.xlsx")


def clean():
    Book([("Summary", costing_rows(5, 15))]).save("clean.xlsx")


def rejects():
    with open(os.path.join(OUT, "not-excel.xlsx"), "wb") as fh:
        fh.write(b"Style,Qty,FOB\nST-0001,12,4.10\n")
    write_zip(os.path.join(OUT, "no-workbook.xlsx"), [("readme.txt", b"no workbook here\n")])


def main():
    os.makedirs(OUT, exist_ok=True)
    ghosts()
    emf_heavy()
    photos()
    clean()
    rejects()
    for f in sorted(os.listdir(OUT)):
        print("%-18s %9d bytes" % (f, os.path.getsize(os.path.join(OUT, f))))


if __name__ == "__main__":
    main()
