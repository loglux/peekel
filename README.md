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
| JPEG XL (the first frame) and DDS (DXT1 to BC7 and the common uncompressed kinds) | the picture; Peekel reads them itself, no codec of the system is needed |
| HEIC, AVIF, JPEG XR and whatever else **your Windows** has a codec for | the picture, only where the codec is installed (Windows' own codecs; the menu Commands, Plugin commands, "Peekel: formats" lists what yours has) |
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

HEIC, AVIF, JPEG XR, icons and similar files are opened by the codecs of **your Windows** (the Windows Imaging Component,
WIC), not by Peekel's own code. So what opens depends on the PC: Windows has some codecs itself (JPEG XR, icons and others),
and others are extensions from the Microsoft Store (HEIF and others) that may not be installed. JPEG XL and DDS are read by Peekel itself.

- **What your PC opens:** in Far, the menu Commands, Plugin commands, "Peekel: formats" lists it. (F11 opens the same menu, but
  Windows Terminal takes F11 for full screen.)
- **When there is no codec:** the card of a HEIC or AVIF file says so and names the Microsoft Store extension that may
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
| `PEEKEL_CELL=10x20` | the size of a terminal cell the plugin takes instead of asking the terminal; it also stops every question the plugin puts to the terminal (users whose Far macro on Esc misbehaved found that this helped), so the colour of the panels is not asked either: give it with `PEEKEL_BG` |
| `PEEKEL_REAL_CELL=13x28` | the real size of a cell by hand, for a case where the plugin cannot work it out from the window (a split pane); in Windows Terminal it works it out by itself, with `PEEKEL_CELL` too |
| `PEEKEL_BG=#0037DA` | the colour the pictures are laid on (the background of the panels) by hand; the colour of the panels as you see them (the "Blue" of your colour scheme in the settings of Windows Terminal is a good first guess: `#0037DA` in Campbell); a colour that is not the panels' shows as a strip at the picture's right and bottom edge |

There is no settings dialog yet; if you want to turn something off, say which.

## Questions

**Why does the plugin put questions to the terminal?** To draw a picture the right size it needs two things that only the terminal knows: how
many pixels a character cell has, and what colour the panels are (the picture is laid on that colour, so no strip of another colour shows round
it). It asks Windows Terminal with escape sequences (what it is, the size of a cell, the 16 colours), and the terminal answers by typing escape
sequences into the input; the plugin takes them out before Far sees them. It asks at the start, when the window or the font changes, and
every 15 seconds while you are not typing.

**Windows Terminal says a cell is 10 x 20. Is that wrong?** It says that always, with any font (the console host behind the terminal assumes
it). The real cell depends on the font and on the scale of the screen: on the author's PC Consolas at 150 % has 13 x 28 pixels and Cascadia Code
14 x 28. The terminal stretches a picture over the real cells, so with a font whose cell is not 1 : 2 a picture made for 10 x 20 comes out too
tall (8 % with Consolas). So the plugin works the real cell out from what Windows says about the window of the terminal: the size of its
client area, the scale of the screen, the height of the title bar, and how many columns and rows of text there are. It looks for the whole cell
sizes that fit (allowing for the padding and the scroll bar), takes one only when there is exactly one and its shape is believable (between
1 : 1.4 and 1 : 3), and in another terminal, in a split pane or when unsure it takes nothing and draws as it would without it. It is not a plain
division of the width by the columns.

**A Far macro on Esc misbehaves with the plugin.** A macro that does something at every Esc on an empty command line (one that hides and shows
the panels with Ctrl+O, for example) conflicts with the plugin: reported by two users and seen by the author. **The cause is a guess, not
proved:** every answer of the terminal begins with an Escape, and the macro may take it for a key press. The way round is `PEEKEL_CELL=10x20`
set before Far starts (one user set it in `_macroinit.lua`): the plugin then asks the terminal nothing. Since 0.5.0 it still works the real
cell out from the window, so the pictures keep their proportions with any font; what it cannot know is the colour of the panels: it takes
the console's default blue (`#000080`), and if your terminal shows Far's blue another way a strip of the other blue may show round a
picture. Give the colour with `PEEKEL_BG`.

**Where do I find the colour for `PEEKEL_BG`?** Ask the terminal once: start Far in the usual way (without `PEEKEL_CELL`, and with the macro
off for this one start) with `PEEKEL_FAR_LOG=<file>` set, open a picture, wait twenty seconds and close Far. The line "the terminal says its
colours are [...]" of the log lists the colours of the terminal's palette as red, green and blue; Far's blue is the fifth one (the number 4,
counted from 0). On the author's PC the terminal said `[0, 0, 128]` in one start, and the panels were `#000080`, while in other starts they were
`#0037DA` (`[0, 55, 218]`, the Blue of the Campbell scheme): the colour can change, so if a strip of another colour shows at the picture's right
and bottom edge, look again. It can be written `#0037DA`, `0037DA` or `0,55,218`. If you changed the colours of the panels in Far, it is the colour
you gave them.

## Licence

Peekel is under **either** of the [MIT licence](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
Copyright (c) 2026 Vladislav Sorokin. The licences of the libraries inside the DLL are in `THIRD-PARTY-LICENSES.txt` in the
archive; all are permissive (MIT, Apache-2.0, BSD, Zlib, Unicode).
