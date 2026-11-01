//! The real size of a cell of Windows Terminal, worked out from what Windows says about its window.
//!
//! Why: the terminal answers `CSI 16 t` (the size of a cell in pixels) with the size that the console host behind it (ConPTY)
//! assumes, 10 x 20, whatever the font is. A Sixel picture is then stretched by the terminal over as many real cells as it has
//! 10 x 20 cells, so with a font whose real cell is not 1 : 2 (Consolas at 150 %: 13 x 28 pixels) the picture is stretched more
//! down than across. A font whose cell is 1 : 2 (Cascadia Code) is not affected. The question cannot be put to the terminal, but
//! the window of the terminal can be measured: its client area is made of the text (`columns * cell width` and
//! `rows * cell height`, whole cells) and of what is round it, which is known well enough (the title bar that holds the tabs, the
//! padding, the scroll bar); only whole sizes of the cell that fit are looked for, and if there is not exactly one, nothing is
//! claimed (a split pane, another terminal, a window that is being moved).
//!
//! The numbers of the author's PC, from `tools/diag.ps1`, are in the tests: the client area 1730 x 925 at 144 dpi, a title bar of
//! 60 pixels, 120 columns and 30 rows with Cascadia Code (a 14 x 28 cell), 129 columns with Consolas (13 x 28).

/// What Windows says about the window of the terminal, and the size of the text in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// The client area of the window of the terminal in pixels (it includes the title bar with the tabs).
    pub client: (i32, i32),
    pub dpi: u32,
    /// The height of the title bar that holds the tabs (the drag bar child window), 0 if there is none.
    pub bar: i32,
    pub columns: usize,
    pub rows: usize,
}

/// The padding of the text area, in device-independent pixels each side: the default of Windows Terminal.
const PADDING_DIP: f64 = 8.0;
/// The widest scroll bar, in device-independent pixels (it is 12 in the versions looked at): the width is known only to be this
/// or less (it may be hidden).
const SCROLLBAR_DIP: f64 = 16.0;
/// The proportions of a cell of a font that is used in a terminal (height over width): outside of these a result is not taken.
const ASPECT: std::ops::RangeInclusive<f64> = 1.4..=3.0;

/// The size of a cell (width, height) in pixels, if exactly one whole size fits the window; None when none does, or more than one.
pub fn solve(facts: &Facts) -> Option<(usize, usize)> {
    if facts.columns == 0 || facts.rows == 0 || facts.dpi == 0 {
        return None;
    }
    let scale = f64::from(facts.dpi) / 96.0;
    let padding = (PADDING_DIP * scale).round() as i64;
    let scrollbar = (SCROLLBAR_DIP * scale).round() as i64;
    let (columns, rows) = (facts.columns as i64, facts.rows as i64);
    // what is left of the width when the padding and the cells are taken off must be less than a cell and a scroll bar, and not
    // negative; what is left of the height when the bar, the padding and the cells are taken off must be less than a cell
    let widths: Vec<i64> = (4..=64)
        .filter(|&w| {
            let left = i64::from(facts.client.0) - 2 * padding - columns * w;
            (0..w + scrollbar).contains(&left)
        })
        .collect();
    let heights: Vec<i64> = (8..=128)
        .filter(|&h| {
            let left = i64::from(facts.client.1) - i64::from(facts.bar) - 2 * padding - rows * h;
            (0..h).contains(&left)
        })
        .collect();
    let ([width], [height]) = (widths.as_slice(), heights.as_slice()) else {
        return None;
    };
    ASPECT
        .contains(&(*height as f64 / *width as f64))
        .then_some((*width as usize, *height as usize))
}

/// Whether the cell `real` is not the same shape as `nominal` (the one the terminal tells): by more than a hundredth. When it is
/// the same shape the picture is drawn as it always was.
pub fn differs(nominal: (usize, usize), real: (usize, usize)) -> bool {
    let a = nominal.1 as f64 / nominal.0 as f64;
    let b = real.1 as f64 / real.0 as f64;
    (a / b - 1.0).abs() > 0.01
}

/// Measures the window of the terminal that this console belongs to, and works the cell out. None when the console is not that of
/// Windows Terminal, or the window cannot be read, or the cell cannot be told.
pub fn measure() -> Option<(usize, usize)> {
    let facts = facts()?;
    solve(&facts)
}

#[cfg(windows)]
fn facts() -> Option<Facts> {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetConsoleWindow, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GA_ROOTOWNER, GetAncestor, GetClassNameW, GetClientRect,
    };
    use windows::core::BOOL;

    /// The name of the class of a window.
    fn class_of(window: HWND) -> String {
        let mut name = [0u16; 128];
        // SAFETY: a buffer of its own, and its length is what the call is told.
        let n = unsafe { GetClassNameW(window, &mut name) };
        String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0).min(name.len())])
    }

    // SAFETY: calls with no pointers of ours, except the rectangles and the structure below, which live through them.
    unsafe {
        let console = GetConsoleWindow();
        if console.0.is_null() {
            return None;
        }
        // the window that the terminal made for the console is owned by the terminal's own window
        let window = GetAncestor(console, GA_ROOTOWNER);
        if window.0.is_null() || class_of(window) != "CASCADIA_HOSTING_WINDOW_CLASS" {
            return None;
        }
        let mut client = RECT::default();
        GetClientRect(window, &mut client).ok()?;
        let dpi = GetDpiForWindow(window);
        // the title bar with the tabs is a child window of its own
        extern "system" fn bar_height(child: HWND, found: LPARAM) -> BOOL {
            if class_of(child) == "DRAG_BAR_WINDOW_CLASS" {
                let mut rect = RECT::default();
                // SAFETY: `found` is the address of an i32 of the caller, which is there while the enumeration runs.
                unsafe {
                    if GetClientRect(child, &mut rect).is_ok() {
                        *(found.0 as *mut i32) = rect.bottom - rect.top;
                    }
                }
            }
            BOOL(1)
        }
        let mut bar = 0i32;
        let _ = EnumChildWindows(Some(window), Some(bar_height), LPARAM(&mut bar as *mut i32 as isize));
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE).ok()?, &mut info).ok()?;
        let columns = usize::try_from(info.srWindow.Right - info.srWindow.Left + 1).ok()?;
        let rows = usize::try_from(info.srWindow.Bottom - info.srWindow.Top + 1).ok()?;
        Some(Facts {
            client: (client.right - client.left, client.bottom - client.top),
            dpi,
            bar,
            columns,
            rows,
        })
    }
}

#[cfg(not(windows))]
fn facts() -> Option<Facts> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window of the author's PC (tools/diag.ps1): 1730 x 925, 144 dpi, a title bar of 60 pixels, 30 rows.
    fn author(columns: usize) -> Facts {
        Facts {
            client: (1730, 925),
            dpi: 144,
            bar: 60,
            columns,
            rows: 30,
        }
    }

    #[test]
    fn cascadia_code_is_14_by_28_and_consolas_13_by_28_in_the_window_of_the_author() {
        assert_eq!(solve(&author(120)), Some((14, 28)));
        assert_eq!(solve(&author(129)), Some((13, 28)));
    }

    #[test]
    fn a_split_pane_or_a_window_that_does_not_fit_gives_nothing() {
        // half the columns: a cell twice as wide fits, but it is no shape of a font
        assert_eq!(solve(&author(60)), None);
        // half the rows: more than one height fits
        let half = Facts {
            rows: 15,
            ..author(120)
        };
        assert_eq!(solve(&half), None);
        // no cells, no scale
        assert_eq!(
            solve(&Facts {
                columns: 0,
                ..author(120)
            }),
            None
        );
        assert_eq!(solve(&Facts { dpi: 0, ..author(120) }), None);
        // a window that is not that of a terminal of this kind (the sizes are far from any whole cell of a font)
        let odd = Facts {
            client: (1700, 400),
            ..author(120)
        };
        assert_eq!(solve(&odd), None);
    }

    #[test]
    fn a_cell_that_is_not_the_shape_of_the_terminals_is_told_from_one_that_is() {
        assert!(differs((10, 20), (13, 28)));
        assert!(!differs((10, 20), (14, 28)));
        assert!(!differs((10, 20), (10, 20)));
        assert!(!differs((10, 20), (20, 40)));
    }
}
