//! Far Manager plugin, an experiment: when the other panel is the quick view panel (Ctrl+Q) and the current file of
//! this one is a picture, draw it there as Sixel. Far has no call for drawing in its quick view, so the plugin writes
//! the Sixel to the console itself and has to find out when Far has painted over it; the code below keeps what
//! was learned about Far's repainting in its comments.
//!
//! The Far API is C (`plugin.hpp`, Far 3, x64): the structures below are laid out as there. Windows only.

#![cfg(windows)]
// Far's C API: raw pointers in and out of the exported functions.
#![allow(unsafe_code, clippy::missing_safety_doc, missing_debug_implementations)]

mod api;
mod cache;
mod cell;
mod draw;
mod log;

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use api::{
    GlobalInfo, PluginInfo, PluginStartupInfo, ProcessConsoleInputInfo, ProcessSynchroEventInfo,
    ProcessViewerEventInfo, VersionInfo,
};

/// The plugin's identity for Far (a made-up GUID).
const GUID: [u8; 16] = *b"\x53\x49\x58\x56\x49\x45\x57\x00\xa1\xb2\xc3\xd4\xe5\xf6\x07\x18";

static ADV_CONTROL: AtomicUsize = AtomicUsize::new(0);
static PANEL_CONTROL: AtomicUsize = AtomicUsize::new(0);
static TEXT: AtomicUsize = AtomicUsize::new(0);
static VIEWER_CONTROL: AtomicUsize = AtomicUsize::new(0);
/// A request for `ProcessSynchroEventW` is on its way and has not run yet.
static SYNCHRO_PENDING: AtomicBool = AtomicBool::new(false);

/// A panic happened in the plugin: it draws nothing from then on.
/// Far is closing: the thread that asks for the ticks ends.
static CLOSING: AtomicBool = AtomicBool::new(false);
static BROKEN: AtomicBool = AtomicBool::new(false);

/// Runs `work` for one of Far's calls. The plugin lives in Far's process, and a panic must not leave the exported
/// function (that would end Far): it is logged (the default message would go to the console and spoil Far's screen),
/// the colour of the viewer's text is put back, and the plugin does nothing more.
fn guarded(what: &str, work: impl FnOnce()) {
    if BROKEN.load(Ordering::SeqCst) {
        return;
    }
    if catch_unwind(AssertUnwindSafe(work)).is_err() {
        BROKEN.store(true, Ordering::SeqCst);
        log::line(&format!("{what} panicked: the plugin draws nothing more"));
        // a second panic here is caught too: nothing more can be done
        let _ = catch_unwind(draw::show_text);
    }
}

fn utf16(s: &str) -> &'static [u16] {
    // a few strings for the life of the process
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    Box::leak(v.into_boxed_slice())
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetGlobalInfoW(info: *mut GlobalInfo) {
    let info = unsafe { &mut *info };
    info.struct_size = size_of::<GlobalInfo>();
    info.min_far_version = VersionInfo::new(3, 0, 0, 0);
    info.version = VersionInfo::new(0, 2, 0, 0);
    info.guid = GUID;
    info.title = utf16("Peekel").as_ptr();
    info.description = utf16("A picture in the quick view panel as Sixel").as_ptr();
    info.author = utf16("Peekel").as_ptr();
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SetStartupInfoW(info: *const PluginStartupInfo) {
    let info = unsafe { &*info };
    ADV_CONTROL.store(info.adv_control, Ordering::SeqCst);
    PANEL_CONTROL.store(info.panel_control, Ordering::SeqCst);
    TEXT.store(info.text, Ordering::SeqCst);
    VIEWER_CONTROL.store(info.viewer_control, Ordering::SeqCst);
    // nothing that can panic runs outside a guard in a call from Far: a panic must not unwind into it
    let _ = catch_unwind(|| {
        std::panic::set_hook(Box::new(|info| log::line(&format!("panic: {info}"))));
        log::line(&format!("plugin started ({})", draw::describe()));
    });
    // Far runs plugin code on its own thread only; a thread of ours asks it to call `ProcessSynchroEventW`
    // (every 20 ms, but never a second request while the last one has not run: a busy Far would queue them up)
    std::thread::spawn(|| {
        while !CLOSING.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
            if !SYNCHRO_PENDING.swap(true, Ordering::SeqCst) {
                api::request_synchro();
            }
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetPluginInfoW(info: *mut PluginInfo) {
    let info = unsafe { &mut *info };
    *info = PluginInfo::default();
    info.struct_size = size_of::<PluginInfo>();
    info.flags = api::PF_PRELOAD;
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessSynchroEventW(info: *const ProcessSynchroEventInfo) -> isize {
    let info = unsafe { &*info };
    SYNCHRO_PENDING.store(false, Ordering::SeqCst);
    if info.event == 0 {
        guarded("the tick", draw::tick);
    }
    0
}

/// Far's viewer opens or closes a file (the quick view's too): for a picture in the quick view the text is hidden before
/// Far paints it, and shown again when the viewer closes.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessViewerEventW(info: *const ProcessViewerEventInfo) -> isize {
    let info = unsafe { &*info };
    match info.event {
        api::VE_READ => guarded("the viewer's opening", || draw::viewer_opened(info.viewer_id)),
        api::VE_CLOSE => guarded("the viewer's closing", || draw::viewer_closed(info.viewer_id)),
        _ => {}
    }
    0
}

/// Every input record, before Far has it: the answer of the terminal about the size of a cell is taken out here.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessConsoleInputW(info: *const ProcessConsoleInputInfo) -> isize {
    let info = unsafe { &*info };
    let mut taken = false;
    guarded("the input", || {
        taken = draw::takes_info_key(&info.record) || cell::takes(&info.record)
    });
    isize::from(taken)
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ExitFARW(_info: *const c_void) {
    CLOSING.store(true, Ordering::SeqCst); // the ticker stops asking: Far may unload the library after this call
    let _ = catch_unwind(draw::show_text);
    log::line("Far is closing");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_stays_inside_and_stops_the_plugin() {
        guarded("a test", || panic!("on purpose"));
        assert!(BROKEN.load(Ordering::SeqCst));
        // later calls do nothing, not even run
        guarded("a test", || unreachable!("the plugin is stopped"));
        BROKEN.store(false, Ordering::SeqCst);
    }
}
