//! Replacement for desktop-hotkeys.ps1: Win+1/2/3 -> desktop 1/2/3 (Win+Ctrl+1..3 while topbar is running,
//! so Win+1..9 open taskbar apps), Win+` -> previous desktop, Win+E -> Windows File Explorer,
//! Win+D -> show desktop, Win+I -> Settings. Win+E/D/I are done here, not left to Explorer: with topbar (and rs-shell)
//! Explorer's taskbar is hidden or gone and it stops answering them although it still holds the hotkeys.
//! No undocumented COM: the current desktop comes from the registry, switching sends Ctrl+Win+Left/Right
//! (Win is already held by the user). shortcut: relies on the system shortcuts being enabled.
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicUsize, Ordering::Relaxed};
use windows_sys::Win32::{Foundation::*, System::{LibraryLoader::GetModuleHandleW, Registry::*}, UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*}};

static PREV: AtomicUsize = AtomicUsize::new(usize::MAX);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static HELD: AtomicU32 = AtomicU32::new(0); // the Win+letter key still down: auto-repeat must not toggle the desktop again and again

/// (current index, desktop count) from the registry.
fn desktops() -> Option<(usize, usize)> {
    let key: Vec<u16> = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops\0".encode_utf16().collect();
    let get = |name: &str| -> Option<Vec<u8>> {
        let n: Vec<u16> = format!("{name}\0").encode_utf16().collect();
        let mut buf = vec![0u8; 1024];
        let mut len = buf.len() as u32;
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), n.as_ptr(), RRF_RT_REG_BINARY, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        (r == 0).then(|| { buf.truncate(len as usize); buf })
    };
    index_in(&get("VirtualDesktopIDs")?, &get("CurrentVirtualDesktop")?)
}

fn index_in(ids: &[u8], cur: &[u8]) -> Option<(usize, usize)> {
    let i = ids.chunks_exact(16).position(|c| c == cur)?;
    Some((i, ids.len() / 16))
}

fn tap(vk: u16, up: bool) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    i.r#type = INPUT_KEYBOARD;
    i.Anonymous.ki = KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
    i
}

fn send(keys: &[INPUT]) {
    unsafe { SendInput(keys.len() as u32, keys.as_ptr(), size_of::<INPUT>() as i32) };
}

/// Win+E: launch Windows File Explorer independently (never hijacked by rust-explorer).
/// Null std handles: this process has no console (FreeConsole) and a child that inherits the invalid ones fails to start.
fn open_files() {
    use std::process::{Command, Stdio};
    let _ = Command::new("explorer.exe").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

fn open_settings() {
    winrust_common::settings::open("ms-settings:");
}

/// What Win+<key> does here instead of in Explorer.
fn win_action(vk: u32) -> Option<fn()> {
    match vk {
        0x45 => Some(open_files),                        // E
        0x44 => Some(winrust_common::desktop::toggle),   // D
        0x49 => Some(open_settings),                     // I
        _ => None,
    }
}

fn go(target: usize) {
    let Some((cur, count)) = desktops() else { return };
    if target == cur || target >= count { return }
    PREV.store(cur, Relaxed);
    let key = if target > cur { VK_RIGHT } else { VK_LEFT };
    for _ in 0..cur.abs_diff(target) {
        send(&[tap(VK_CONTROL, false), tap(key, false), tap(key, true), tap(VK_CONTROL, true)]);
    }
}

unsafe extern "system" fn hook(n: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if n >= 0 {
        let k = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        if (w == WM_KEYUP as usize || w == WM_SYSKEYUP as usize) && HELD.load(Relaxed) == k.vkCode {
            HELD.store(0, Relaxed); // also when Win was let go first
        }
        let act = win_action(k.vkCode);
        let maybe_desk = matches!(k.vkCode, 0x31..=0x33 | 0xC0);
        // Check the key code BEFORE GetAsyncKeyState / FindWindowW: avoids syscalls on every normal/game keystroke.
        if (act.is_some() || maybe_desk) && k.flags & LLKHF_INJECTED == 0 {
            let win = unsafe { (GetAsyncKeyState(VK_LWIN as i32) | GetAsyncKeyState(VK_RWIN as i32)) as u16 & 0x8000 != 0 };
            if win {
                // During a fullscreen game with Win-key lock active, swallow Win+D/E/I/1-3/` without leaving the game.
                if winrust_common::gaming::should_lock_win_key() && winrust_common::gaming::is_foreground_fullscreen() {
                    if w == WM_KEYDOWN as usize {
                        send(&[tap(0xE8, false), tap(0xE8, true)]);
                    }
                    return 1;
                }
                if let Some(act) = act {
                    // swallow it so Explorer's own handler (if it still answers) does not act as well
                    if w == WM_KEYDOWN as usize && HELD.swap(k.vkCode, Relaxed) != k.vkCode {
                        send(&[tap(0xE8, false), tap(0xE8, true)]); // Win release must not open Start
                        std::thread::spawn(act);
                    }
                    return 1;
                }
                if maybe_desk {
                    // with the topbar running Win+1..9 belong to its app buttons: desktops are Win+Ctrl+1..3 then
                    let bar = unsafe { !FindWindowW("topbar\0".encode_utf16().collect::<Vec<u16>>().as_ptr(), std::ptr::null()).is_null() };
                    let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000 != 0 };
                    let id = match k.vkCode {
                        0x31..=0x33 if !bar || ctrl => Some(k.vkCode as usize - 0x31),
                        0xC0 => Some(usize::MAX),
                        _ => None,
                    };
                    if let Some(id) = id {
                        if w == WM_KEYDOWN as usize && HELD.swap(k.vkCode, Relaxed) != k.vkCode {
                            // unassigned key tap stops Win release from opening Start; switching runs off the hook thread
                            send(&[tap(0xE8, false), tap(0xE8, true)]);
                            std::thread::spawn(move || go(if id == usize::MAX { PREV.load(Relaxed) } else { id }));
                        }
                        return 1;
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(HOOK.load(Relaxed) as _, n, w, l) }
}

pub fn run() -> i32 {
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(std::ptr::null()), 0) };
    if h.is_null() { eprintln!("hook failed"); return 1 }
    HOOK.store(h as isize, Relaxed);
    let mut m: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut m, std::ptr::null_mut(), 0, 0) } > 0 {}
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_index() {
        let ids = [[1u8; 16], [2; 16], [3; 16]].concat();
        assert_eq!(super::index_in(&ids, &[2; 16]), Some((1, 3)));
        assert_eq!(super::index_in(&ids, &[9; 16]), None);
    }
}
