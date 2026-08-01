#!/usr/bin/env python3
"""Generate N distinct minimal EPUBs for a library-seeding campaign.

They are generated rather than downloaded so every run sees the same bytes with
no network, and each carries a distinct title and identifier so a library that
deduplicates by content still shows N books.

usage: make-epubs.py OUTPUT_DIR COUNT
"""

import sys
import zipfile
from pathlib import Path

CONTAINER = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""

OPF = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:uuid:reproit-field-book-{index:02d}</dc:identifier>
    <dc:title>Reproit Field Book {index:02d}</dc:title>
    <dc:language>en</dc:language>
    <dc:creator>Reproit Validation</dc:creator>
    <meta property="dcterms:modified">2026-01-01T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="chapter"/>
  </spine>
</package>
"""

NAV = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
  <head><title>Contents</title></head>
  <body>
    <nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter One</a></li></ol></nav>
  </body>
</html>
"""

CHAPTER = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>Reproit Field Book {index:02d}</title></head>
  <body>
    <h1>Reproit Field Book {index:02d}</h1>
    {paragraphs}
  </body>
</html>
"""


def write_epub(path: Path, index: int) -> None:
    paragraphs = "\n    ".join(
        f"<p>Paragraph {number} of field book {index:02d}. This text exists only to "
        f"give the reader a stable body of prose.</p>"
        for number in range(1, 21)
    )
    with zipfile.ZipFile(path, "w") as archive:
        # The mimetype entry must be first and stored, not deflated.
        archive.writestr(
            zipfile.ZipInfo("mimetype"), "application/epub+zip",
            compress_type=zipfile.ZIP_STORED,
        )
        archive.writestr("META-INF/container.xml", CONTAINER)
        archive.writestr("OEBPS/content.opf", OPF.format(index=index))
        archive.writestr("OEBPS/nav.xhtml", NAV)
        archive.writestr("OEBPS/chapter.xhtml",
                         CHAPTER.format(index=index, paragraphs=paragraphs))


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    directory = Path(sys.argv[1])
    count = int(sys.argv[2])
    directory.mkdir(parents=True, exist_ok=True)
    for index in range(1, count + 1):
        write_epub(directory / f"field-book-{index:02d}.epub", index)
    print(f"generated {count} epubs in {directory}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
