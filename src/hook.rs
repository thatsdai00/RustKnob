// Global low-level mouse + keyboard hooks. They run on the main thread and only post messages.
//  mouse:    double middle click -> open the capsule
//            wheel while the capsule is captured -> volume (swallowed)
//            buttons over the capsule / knob -> media control; left click in the settings panel (swallowed)
//  keyboard: volume keys -> volume in the capsule instead of Windows' own popup (swallowed)

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::Relaxed};

use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_OPEN: u32 = WM_APP + 1; // double middle click
pub const WM_APP_WHEEL: u32 = WM_APP + 2; // wparam: wheel delta
pub const WM_APP_PLAYSTATE: u32 = WM_APP + 3; // play/pause finished
pub const WM_APP_MEDIA: u32 = WM_APP + 4; // wparam: 1 prev, 2 next, 3 play/pause
pub const WM_APP_SONG: u32 = WM_APP + 5; // a new song started
pub const WM_APP_DEVICE: u32 = WM_APP + 6; // default output device changed
pub const WM_APP_VOLKEY: u32 = WM_APP + 7; // wparam: 1 up, 2 down, 3 mute
pub const WM_APP_CLICK: u32 = WM_APP + 8; // settings click, lparam: packed screen x | y << 16
pub const WM_APP_MOVE: u32 = WM_APP + 9; // pointer moved over the settings panel
pub const WM_APP_DISMISS: u32 = WM_APP + 11; // clicked outside the settings panel
pub const WM_APP_NTFY: u32 = WM_APP + 12; // a push notification arrived
pub const WM_APP_UPLOAD: u32 = WM_APP + 14; // the uploader started, finished or switched file
pub const WM_APP_HOVER: u32 = WM_APP + 15; // wparam: 1 the pointer entered the capsule, 0 it left
pub const WM_APP_DRAGCHECK: u32 = WM_APP + 18; // the left button is held and the pointer moved: a drag may have started; lparam: where the press was (packed x | y << 16)
pub const WM_APP_DRAGEND: u32 = WM_APP + 19; // the button was let go after such a move
pub const WM_APP_SNIP: u32 = WM_APP + 20; // Win+Shift+S
pub const WM_APP_SNIP_CANCEL: u32 = WM_APP + 21; // Esc while the screenshot overlay is up
pub const WM_APP_SNIP_DONE: u32 = WM_APP + 22; // a screenshot was copied; wparam / lparam: its width / height
pub const WM_APP_PASTED: u32 = WM_APP + 23; // Ctrl+V while the clipboard still holds the screenshot RustKnob took
pub const WM_APP_FADE: u32 = WM_APP + 27; // the pointer is near the pinned capsule; wparam 0..FADE_STEPS: how near
pub const WM_APP_KEY: u32 = WM_APP + 13; // typed character while a settings field is edited (8 backspace, 13 enter, 27 esc, 22 paste)

/// What the hooks should capture, kept up to date by the capsule.
pub static CAPTURE_WHEEL: AtomicBool = AtomicBool::new(false);
pub static CAPTURE_BUTTONS: AtomicBool = AtomicBool::new(false);
pub static SETTINGS_OPEN: AtomicBool = AtomicBool::new(false);
pub static VOLUME_KEYS: AtomicBool = AtomicBool::new(true);
pub static SNIP_KEY: AtomicBool = AtomicBool::new(true); // Win+Shift+S belongs to the screenshot overlay
pub static SNIP_SEQ: AtomicU32 = AtomicU32::new(0); // clipboard sequence number of the last screenshot we put there (0 = none)
pub static EDITING: AtomicBool = AtomicBool::new(false); // a settings text field takes typed characters

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static MID_TIME: AtomicU32 = AtomicU32::new(0);
static MID_X: AtomicIsize = AtomicIsize::new(0);
static MID_Y: AtomicIsize = AtomicIsize::new(0);
static AREA: [AtomicIsize; 4] = [const { AtomicIsize::new(0) }; 4]; // left, top, right, bottom (screen px)
static AREA_ROUND: AtomicBool = AtomicBool::new(false); // the area is the ellipse inscribed in the rectangle
static NEAR_PAD: AtomicIsize = AtomicIsize::new(0); // side buttons also work this many px around the area
static LMB_DOWN: AtomicBool = AtomicBool::new(false);
static DRAG_FROM: [AtomicIsize; 2] = [const { AtomicIsize::new(0) }; 2];
static DRAG_SENT: AtomicBool = AtomicBool::new(false);
/// The pinned capsule fades as the pointer comes near. The rectangle is `AREA`; the reach (device pixels) is
/// worked out once, when the capsule is placed, so a move costs a few comparisons.
pub const FADE_STEPS: u32 = 8; // coarse on purpose: the capsule eases between steps, and a move that stays in a step posts nothing
static FADE_TRACK: AtomicBool = AtomicBool::new(false);
static FADE_REACH: AtomicIsize = AtomicIsize::new(1);
static FADE_LEVEL: AtomicU32 = AtomicU32::new(0);
static HOVER_TRACK: AtomicBool = AtomicBool::new(false); // report the pointer entering / leaving the area
static HOVER_NOW: AtomicBool = AtomicBool::new(false);
static MOVE_PENDING: AtomicBool = AtomicBool::new(false);

// slots: left, right, back, forward. `SWALLOWED` remembers a swallowed press so its release is swallowed too.
static SWALLOWED: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
const SLOT_CMD: [usize; 4] = [3, 2, 1, 2]; // play/pause, next, previous, next

/// Screen-space rectangle of the shape; `round` makes it an ellipse (the knob).
pub fn set_area(left: i32, top: i32, right: i32, bottom: i32, round: bool, near_pad: i32) {
    NEAR_PAD.store(near_pad as isize, Relaxed);
    for (a, v) in AREA.iter().zip([left, top, right, bottom]) {
        a.store(v as isize, Relaxed);
    }
    AREA_ROUND.store(round, Relaxed);
}

pub fn set_fade_track(on: bool, reach_px: i32) {
    FADE_REACH.store(reach_px.max(1) as isize, Relaxed);
    if FADE_TRACK.swap(on, Relaxed) != on && !on {
        FADE_LEVEL.store(0, Relaxed);
    }
}

pub fn set_hover_track(on: bool) {
    HOVER_TRACK.store(on, Relaxed);
    if !on {
        HOVER_NOW.store(false, Relaxed);
    }
}

pub fn move_handled() {
    MOVE_PENDING.store(false, Relaxed);
}

fn in_area(pt: POINT) -> bool {
    let a = |i: usize| AREA[i].load(Relaxed) as f32;
    let (x, y) = (pt.x as f32, pt.y as f32);
    if AREA_ROUND.load(Relaxed) {
        let (cx, cy) = ((a(0) + a(2)) / 2.0, (a(1) + a(3)) / 2.0);
        let (rx, ry) = ((a(2) - a(0)) / 2.0, (a(3) - a(1)) / 2.0);
        return rx > 0.0 && ry > 0.0 && ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0;
    }
    x >= a(0) && x < a(2) && y >= a(1) && y < a(3)
}

/// The area grown by the near padding on every side (the screen edge cuts it at the top).
fn near_area(pt: POINT) -> bool {
    let pad = NEAR_PAD.load(Relaxed);
    pt.x as isize >= AREA[0].load(Relaxed) - pad
        && (pt.x as isize) < AREA[2].load(Relaxed) + pad
        && pt.y as isize >= AREA[1].load(Relaxed) - pad
        && (pt.y as isize) < AREA[3].load(Relaxed) + pad
}

/// (slot, is_press) for the buttons the capsule listens to.
fn button(msg: u32, mouse_data: u32) -> Option<(usize, bool)> {
    match msg {
        WM_LBUTTONDOWN => Some((0, true)),
        WM_LBUTTONUP => Some((0, false)),
        WM_RBUTTONDOWN => Some((1, true)),
        WM_RBUTTONUP => Some((1, false)),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let slot = match (mouse_data >> 16) as u16 {
                1 => 2,
                2 => 3,
                _ => return None,
            };
            Some((slot, msg == WM_XBUTTONDOWN))
        }
        _ => None,
    }
}

pub unsafe fn install(hinst: HINSTANCE, hwnd: HWND) -> [Option<HHOOK>; 2] {
    MAIN_HWND.store(hwnd.0 as isize, Relaxed);
    [
        SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hinst, 0).ok(),
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), hinst, 0).ok(),
    ]
}

unsafe fn post(msg: u32, wparam: usize, lparam: isize) {
    let _ = PostMessageW(HWND(MAIN_HWND.load(Relaxed) as _), msg, WPARAM(wparam), LPARAM(lparam));
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let ms = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let msg = wp.0 as u32;

        // by far the most common event: a plain move while nothing wants to know about it
        if msg == WM_MOUSEMOVE && FADE_TRACK.load(Relaxed) {
            let (l, t, r, b) = (AREA[0].load(Relaxed), AREA[1].load(Relaxed), AREA[2].load(Relaxed), AREA[3].load(Relaxed));
            let (x, y) = (ms.pt.x as isize, ms.pt.y as isize);
            let (dx, dy) = ((l - x).max(x - r).max(0) as f32, (t - y).max(y - b).max(0) as f32);
            let near = 1.0 - (dx.hypot(dy) / FADE_REACH.load(Relaxed) as f32).min(1.0); // 0 far .. 1 over it
            let level = (near * FADE_STEPS as f32).round() as u32;
            if FADE_LEVEL.swap(level, Relaxed) != level {
                post(WM_APP_FADE, level as usize, 0);
            }
        }

        if msg == WM_MOUSEMOVE && !HOVER_TRACK.load(Relaxed) && !SETTINGS_OPEN.load(Relaxed) && !LMB_DOWN.load(Relaxed) {
            return CallNextHookEx(None, code, wp, lp);
        }

        if msg == WM_MOUSEWHEEL && CAPTURE_WHEEL.load(Relaxed) {
            post(WM_APP_WHEEL, (ms.mouseData >> 16) as i16 as isize as usize, 0);
            return LRESULT(1);
        }

        if msg == WM_MOUSEMOVE && HOVER_TRACK.load(Relaxed) {
            let inside = in_area(ms.pt);
            if HOVER_NOW.swap(inside, Relaxed) != inside {
                post(WM_APP_HOVER, usize::from(inside), 0);
            }
        }

        if msg == WM_MOUSEMOVE && SETTINGS_OPEN.load(Relaxed) && in_area(ms.pt) && !MOVE_PENDING.swap(true, Relaxed) {
            post(WM_APP_MOVE, 0, 0);
        }

        // a drag (the capsule offers itself as a drop target when a file is being dragged)
        match msg {
            WM_LBUTTONDOWN => {
                LMB_DOWN.store(true, Relaxed);
                DRAG_SENT.store(false, Relaxed);
                DRAG_FROM[0].store(ms.pt.x as isize, Relaxed);
                DRAG_FROM[1].store(ms.pt.y as isize, Relaxed);
            }
            WM_MOUSEMOVE if LMB_DOWN.load(Relaxed) && !DRAG_SENT.load(Relaxed) => {
                let (dx, dy) = (ms.pt.x as isize - DRAG_FROM[0].load(Relaxed), ms.pt.y as isize - DRAG_FROM[1].load(Relaxed));
                if dx * dx + dy * dy > 20 * 20 {
                    DRAG_SENT.store(true, Relaxed);
                    let (x, y) = (DRAG_FROM[0].load(Relaxed), DRAG_FROM[1].load(Relaxed));
                    post(WM_APP_DRAGCHECK, 0, (x as i16 as u16 as isize) | ((y as i16 as u16 as isize) << 16));
                }
            }
            WM_LBUTTONUP => {
                LMB_DOWN.store(false, Relaxed);
                if DRAG_SENT.swap(false, Relaxed) {
                    post(WM_APP_DRAGEND, 0, 0);
                }
            }
            _ => {}
        }

        if let Some((slot, press)) = button(msg, ms.mouseData) {
            if press {
                if SETTINGS_OPEN.load(Relaxed) && slot == 0 {
                    if in_area(ms.pt) {
                        SWALLOWED[0].store(true, Relaxed);
                        post(WM_APP_CLICK, 0, (ms.pt.x as i16 as u16 as isize) | ((ms.pt.y as i16 as u16 as isize) << 16));
                        return LRESULT(1);
                    }
                    post(WM_APP_DISMISS, 0, 0); // the click itself still goes through
                } else if CAPTURE_BUTTONS.load(Relaxed) && if slot >= 2 { near_area(ms.pt) } else { in_area(ms.pt) } {
                    SWALLOWED[slot].store(true, Relaxed);
                    post(WM_APP_MEDIA, SLOT_CMD[slot], 0);
                    return LRESULT(1);
                }
            } else if SWALLOWED[slot].swap(false, Relaxed) {
                return LRESULT(1);
            }
        }

        // the first click passes through (a browser's autoscroll toggles back off on the second)
        if msg == WM_MBUTTONDOWN {
            let last = MID_TIME.load(Relaxed);
            let near = |a: &AtomicIsize, v: i32| (v as isize - a.load(Relaxed)).abs() <= GetSystemMetrics(SM_CXDOUBLECLK) as isize;
            if last != 0 && ms.time.wrapping_sub(last) <= GetDoubleClickTime() && near(&MID_X, ms.pt.x) && near(&MID_Y, ms.pt.y) {
                MID_TIME.store(0, Relaxed);
                post(WM_APP_OPEN, 0, 0);
            } else {
                MID_TIME.store(ms.time.max(1), Relaxed);
            }
            MID_X.store(ms.pt.x as isize, Relaxed);
            MID_Y.store(ms.pt.y as isize, Relaxed);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// What a key press means for a text field: a character, or 8 / 13 / 27 / 22 (backspace, enter, esc, paste).
/// Shortcuts (Ctrl / Alt / Win combos, except AltGr and Ctrl+V) are left alone.
unsafe fn typed(kb: &KBDLLHOOKSTRUCT) -> Option<u32> {
    let down = |k: VIRTUAL_KEY| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0;
    let (ctrl, alt, shift) = (down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT));
    if down(VK_LWIN) || down(VK_RWIN) {
        return None;
    }
    let vk = VIRTUAL_KEY(kb.vkCode as u16);
    match vk {
        VK_BACK => return Some(8),
        VK_RETURN => return Some(13),
        VK_ESCAPE => return Some(27),
        VK_V if ctrl && !alt => return Some(22),
        _ => {}
    }
    let altgr = ctrl && alt; // AltGr reports as Ctrl+Alt and types characters like @
    if (ctrl || alt) && !altgr {
        return None;
    }
    let mut state = [0u8; 256];
    if shift {
        state[VK_SHIFT.0 as usize] = 0x80;
    }
    if altgr {
        state[VK_CONTROL.0 as usize] = 0x80;
        state[VK_MENU.0 as usize] = 0x80;
    }
    if GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 {
        state[VK_CAPITAL.0 as usize] = 1;
    }
    let mut buf = [0u16; 4];
    // flag 4: translating must not disturb the dead-key state of the real keyboard
    let n = ToUnicode(kb.vkCode, kb.scanCode, Some(&state), &mut buf, 4);
    (n == 1 && buf[0] >= 32).then_some(buf[0] as u32)
}

/// A harmless key press: it keeps Windows from opening the Start menu when Win is let go after a swallowed combination.
unsafe fn mask_start_menu() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_F24, dwFlags: flags, ..Default::default() } },
    };
    SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], std::mem::size_of::<INPUT>() as i32);
}

unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if kb.flags.0 & LLKHF_INJECTED.0 == 0 {
            let down = |k: VIRTUAL_KEY| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0;
            if kb.vkCode == VK_ESCAPE.0 as u32 && crate::snip::ACTIVE.load(Relaxed) {
                post(WM_APP_SNIP_CANCEL, 0, 0);
                return LRESULT(1);
            }
            if kb.vkCode == 'V' as u32 && down(VK_CONTROL) && !down(VK_SHIFT) && !down(VK_MENU) {
                let seq = SNIP_SEQ.load(Relaxed);
                if seq != 0 && windows::Win32::System::DataExchange::GetClipboardSequenceNumber() == seq {
                    post(WM_APP_PASTED, 0, 0); // the key press itself goes through
                }
            }
            if kb.vkCode == 'S' as u32 && SNIP_KEY.load(Relaxed) && (down(VK_LWIN) || down(VK_RWIN)) && down(VK_SHIFT) && !down(VK_CONTROL) && !down(VK_MENU) {
                mask_start_menu();
                post(WM_APP_SNIP, 0, 0);
                return LRESULT(1);
            }
        }
    }
    if code == HC_ACTION as i32 && EDITING.load(Relaxed) {
        let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if let Some(ch) = typed(kb) {
            if matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
                post(WM_APP_KEY, ch as usize, 0);
            }
            return LRESULT(1);
        }
    }
    if code == HC_ACTION as i32 && VOLUME_KEYS.load(Relaxed) {
        let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let which = match VIRTUAL_KEY(kb.vkCode as u16) {
            VK_VOLUME_UP => 1,
            VK_VOLUME_DOWN => 2,
            VK_VOLUME_MUTE => 3,
            _ => 0,
        };
        if which != 0 {
            if matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
                post(WM_APP_VOLKEY, which, 0);
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}
