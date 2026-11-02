# Peekel

[Русская версия](README.ru.md)

![Far Manager: a picture in the quick view, drawn by the Peekel plugin](docs/images/far-quickview-v2.png)

**Peekel is a quick-view plugin for Far Manager**: press Ctrl+Q and see what a file is, in the panel: a picture, SVG, a PDF, a DjVu book, the
start of a Word, PowerPoint or Excel file, and a card for fonts, programs and archives.

A console viewer (for F3, Linux and Midnight Commander) works and is being prepared; it will be published a little later.

## Why

- **Ctrl+Q works in Windows Terminal.** Far's quick view shows the picture, the document or a card in the panel, as you move
  over the files.
- **No second window.** Peekel draws with Sixel graphics inside the terminal's own cells, so nothing floats over the console
  and has to be kept in place, and it works over SSH too.
- **More than pictures.** A PDF, a DjVu book, the start of a Word, PowerPoint or Excel file, a card for a font (in its own letters), a
  program, a ZIP: the kind, size, dates, what is inside. When the name says nothing (a recovered `.chk` fragment) it tells what a
  file is by its content, and it warns when a name and the content disagree.
- **Damaged files are safe.** The size, the time and the depth of reading are limited; a broken file gives a message, not a
  crash.
- **One DLL, no runtime.** Written in Rust. Open source, MIT or Apache-2.0.

## What it looks like

Far Manager, the quick view (Ctrl+Q) with the plugin. The PDF cover is public domain ([credits](docs/images/CREDITS.md)).

**A Word document**

![A Word document in the quick view of Far](docs/images/far-word-v2.png)

**An Excel sheet**

![An Excel sheet in the quick view of Far](docs/images/far-excel-v2.png)

**A PowerPoint slide**

![A PowerPoint slide in the quick view of Far](docs/images/far-powerpoint-v2.png)

**A PDF**

![The cover of a PDF brochure in the quick view of Far](docs/images/far-pdf-v2.png)

**A ZIP archive: what is inside**

![The card of a ZIP archive in the quick view of Far](docs/images/far-zip-v2.png)

**A font, in its own letters**

![The card of a font in the quick view of Far](docs/images/far-font-v2.png)

**A photo: Ctrl+Alt+I shows the card with its EXIF**

![The card of a photo in the quick view of Far: camera, lens, date, exposure](docs/images/far-photo-card-v2.png)

**A file whose name says nothing, recognised by its content**

![A file called 61000000file.chk, recognised as a Word document: size, dates, title and author](docs/images/far-recovered-v2.png)

## What it shows

| What | In the quick view |
|---|---|
| PNG, JPEG, GIF (first frame), BMP, WebP, TIFF, SVG | the picture |
| RAW photographs (NEF, CR2, ARW, DNG, ORF, PEF and others) | the camera's own JPEG preview (the full size in most files), in a few tenths of a second |
| HEIC, AVIF, JPEG XL, JPEG XR, DDS and whatever else **your Windows** has a codec for | the picture, only where the codec is installed (Windows' own codecs; the menu Commands, Plugin commands, "Peekel: formats" lists what yours has) |
| PDF (text pages and scans; not password-protected) | the first page |
| DjVu (scanned books) | the first page; the card gives the number of pages and the size of the page (read by a reader of our own, written from the format's open specification; not read yet: the text layer and documents whose pages are separate files) |
| Word (.docx), PowerPoint (.pptx), Excel (.xlsx) | the start, the first slide, the top of the sheet |
| Fonts, programs (exe, dll), ZIP, gzip, any other binary file | a **card**: the kind, size, dates; fonts in their own letters, programs with icon, version and company, ZIP with its files |
| Files inside an archive shown by ArcLite, 7-Zip and the like | the same as for any file |

A picture with a colour profile (Display P3, Adobe RGB) is converted to sRGB, and EXIF orientation is honoured. **Documents are
previews, not copies**: words are wrapped by a guess, and charts, text boxes, headers and footers and much else are not drawn.
Text in SVGs and documents is drawn with the fonts installed on the machine; a document that names a font that is not installed
gets another one.

### Formats from Windows (WIC)

HEIC, AVIF, JPEG XR, DDS, icons and similar files are opened by the codecs of **your Windows** (the Windows Imaging Component,
WIC), not by Peekel's own code. So what opens depends on the PC: Windows has some codecs itself (JPEG XR, DDS, icons and others),
and others are extensions from the Microsoft Store (HEIF, JPEG XL and others) that may not be installed.

- **What your PC opens:** in Far, the menu Commands, Plugin commands, "Peekel: formats" lists it. (F11 opens the same menu, but
  Windows Terminal takes F11 for full screen.)
- **When there is no codec:** the card of a HEIC, AVIF or JPEG XL file says so and names the Microsoft Store extension that may
  add one. While that card is shown, **Ctrl+Alt+S** opens the Store on the page of that extension; whether to install it is
  yours to decide, Peekel installs nothing.
- What Peekel reads itself (PNG, JPEG, RAW by its preview and the rest of the table above) does not depend on any of this.

## Install

Take the archive of the [latest release](../../releases/latest): `Peekel-<version>-windows-x64.zip`. It has a `.sha256` file:
check it. You need Far 3.0 (64-bit) in a terminal that shows Sixel: **Windows Terminal 1.22 or newer**, or xterm, mlterm, foot and
others. **ConEmu is not supported**: it does not show Sixel (the newest release, 230724 of July 2023, draws no picture) and is no
longer developed.

Close Far and run `install.ps1` from the archive. It finds your Far; if it finds none or several, say which:
`.\install.ps1 -Far 'C:\Tools\Far'`. `-Uninstall` removes the plugin, `-WhatIf` shows what would be done. By hand: copy the folder
`Peekel` into the `Plugins` folder next to `Far.exe` (or into `%APPDATA%\Far Manager\Profile\Plugins`). Start Far, press Ctrl+Q
and move over files. Peekel may conflict with other plugins that handle quick view (Ctrl+Q).

From source: install Rust (the version is in `rust-toolchain.toml`) and run `cargo build --release -p peekel-far`; the plugin is
`target/release/peekel_far.dll`.

## Using it

Press Ctrl+Q in a panel to make the other panel a quick view, and move the cursor: a picture, a document or a card is drawn in
the frame, over the text Far would print there; text files are shown by Far as usual. **Ctrl+Alt+I** shows the card of the file (kind, size, dates, the EXIF of a photo)
instead of the picture; press it again to go back. If something is wrong, set an environment
variable before starting Far:

| Variable | Does |
|---|---|
| `PEEKEL_FAR_LOG=<file>` | the plugin writes what it does to this file (send it with a bug report) |
| `PEEKEL_OFF=hook` | switches the plugin off without removing it |
| `PEEKEL_CELL=10x20` | the size of a terminal cell in pixels, when the terminal cannot be asked |

There is no settings dialog yet; if you want to turn something off, say which.

## Licence

Peekel is under **either** of the [MIT licence](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
Copyright (c) 2026 Vladislav Sorokin. The licences of the libraries inside the DLL are in `THIRD-PARTY-LICENSES.txt` in the
archive; all are permissive (MIT, Apache-2.0, BSD, Zlib, Unicode).
