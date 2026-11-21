//! Shared gaming helpers used by topbar, ReShell, rtaskmgr-win32 and rust-settings:
//! - Persistent switches under %ProgramData%\WinRust\ (re-read at most once per second, so hot hooks do zero disk I/O):
//!   `winlock.txt` (Win-key lock in fullscreen games), `autogame.txt` (Auto Game Mode), `timerboost.txt` (0.5 ms timer).
//! - Accurate fullscreen detection that distinguishes true/borderless fullscreen games from normal maximized windows
//!   (which fill the screen when Floating Dock or auto-hide is enabled).
//! - 0.5 ms high-precision timer resolution (`NtSetTimerResolution`), balanced and automatically released when not gaming.
//! - Safe foreground game priority + power-throttling boost (never fails on anti-cheat protected processes).
use std::{
    ffi::c_void,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, GetPriorityClass, NORMAL_PRIORITY_CLASS, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, SetPriorityClass,
    },
    UI::WindowsAndMessaging::{
        GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, IsIconic, IsWindowVisible,
        IsZoomed, WS_CAPTION,
    },
};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSetTimerResolution(desired_resolution: u32, set_resolution: u8, current_resolution: *mut u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetProcessInformation(h: HANDLE, class: i32, info: *const c_void, size: u32) -> i32;
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn state_file(name: &str) -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("WinRust").join(name))
}

fn read_flag(name: &str, val: &AtomicU8, at: &AtomicU64) -> bool {
    let now = now_ms();
    if now.saturating_sub(at.load(Relaxed)) > 1000 {
        let on = state_file(name)
            .and_then(|f| std::fs::read_to_string(f).ok())
            .is_some_and(|s| s.trim() == "1");
        val.store(on as u8, Relaxed);
        at.store(now, Relaxed);
    }
    val.load(Relaxed) == 1
}

fn write_flag(name: &str, val: &AtomicU8, at: &AtomicU64, on: bool) {
    val.store(on as u8, Relaxed);
    at.store(now_ms(), Relaxed);
    if let Some(f) = state_file(name) {
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(f, if on { "1" } else { "0" });
    }
}

static WIN_LOCK: AtomicU8 = AtomicU8::new(0);
static WIN_LOCK_AT: AtomicU64 = AtomicU64::new(0);

static AUTO_GAME: AtomicU8 = AtomicU8::new(0);
static AUTO_GAME_AT: AtomicU64 = AtomicU64::new(0);

static TIMER_BOOST: AtomicU8 = AtomicU8::new(0);
static TIMER_BOOST_AT: AtomicU64 = AtomicU64::new(0);

static GAME_PROF: AtomicU8 = AtomicU8::new(0);
static GAME_PROF_AT: AtomicU64 = AtomicU64::new(0);

/// Suppress accidental Win-key / Win+desktop shortcuts while a fullscreen game is in front.
pub fn win_lock() -> bool {
    read_flag("winlock.txt", &WIN_LOCK, &WIN_LOCK_AT)
}

pub fn set_win_lock(on: bool) {
    write_flag("winlock.txt", &WIN_LOCK, &WIN_LOCK_AT, on);
}

/// Automatic Game Mode: when a fullscreen game runs, boost its priority, engage 0.5 ms timer and Best Performance power mode.
pub fn auto_game() -> bool {
    read_flag("autogame.txt", &AUTO_GAME, &AUTO_GAME_AT)
}

pub fn set_auto_game(on: bool) {
    write_flag("autogame.txt", &AUTO_GAME, &AUTO_GAME_AT, on);
}

/// Request 0.5 ms system timer resolution while fullscreen games are active (improves frame pacing / Sleep precision).
pub fn timer_boost() -> bool {
    read_flag("timerboost.txt", &TIMER_BOOST, &TIMER_BOOST_AT)
}

pub fn set_timer_boost(on: bool) {
    write_flag("timerboost.txt", &TIMER_BOOST, &TIMER_BOOST_AT, on);
}

/// True while rtaskmgr's 'Game' profile is active (%ProgramData%\WinRust\profile.txt starts with "game").
pub fn game_profile_active() -> bool {
    let now = now_ms();
    if now.saturating_sub(GAME_PROF_AT.load(Relaxed)) > 1000 {
        let on = state_file("profile.txt")
            .and_then(|f| std::fs::read_to_string(f).ok())
            .is_some_and(|s| s.lines().next().is_some_and(|l| l.trim() == "game"));
        GAME_PROF.store(on as u8, Relaxed);
        GAME_PROF_AT.store(now, Relaxed);
    }
    GAME_PROF.load(Relaxed) == 1
}

/// True when Win-key suppression should apply during fullscreen games.
pub fn should_lock_win_key() -> bool {
    win_lock() || auto_game() || game_profile_active()
}

/// Distinguishes true/borderless fullscreen windows from normal maximized desktop windows.
/// When Floating Dock or auto-hide sets the work area to the whole monitor, maximized apps cover the full monitor
/// rect too, but they still have `IsZoomed` AND `WS_CAPTION`. Borderless fullscreen games and F11 video drop `WS_CAPTION`
/// (or are not zoomed).
pub fn is_fullscreen_window(style: u32, zoomed: bool, covers_monitor: bool) -> bool {
    if !covers_monitor {
        return false;
    }
    !(zoomed && (style & WS_CAPTION) == WS_CAPTION)
}

/// Non-game apps that commonly go fullscreen (browsers on YouTube F11, media players, slides, IDEs, WinRust itself).
/// Auto Game Mode skips priority / power boosts for these so watching a fullscreen video does not spin up performance mode.
const NON_GAME_EXES: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "vivaldi.exe",
    "brave.exe",
    "opera.exe",
    "zen.exe",
    "waterfox.exe",
    "vlc.exe",
    "mpv.exe",
    "mpc-hc64.exe",
    "mpc-hc.exe",
    "mpc-be64.exe",
    "potplayermini64.exe",
    "potplayermini.exe",
    "wmplayer.exe",
    "video.ui.exe",
    "powerpnt.exe",
    "mstsc.exe",
    "rust-explorer.exe",
    "topbar.exe",
    "photoview.exe",
    "rtaskmgr-win32.exe",
    "rust-settings.exe",
    "rs-launcher.exe",
    "obs64.exe",
    "obs32.exe",
    "code.exe",
    "devenv.exe",
    "windowsterminal.exe",
    "wt.exe",
];

pub fn is_non_game_fullscreen_exe(path_or_name: &str) -> bool {
    let base = path_or_name
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path_or_name)
        .trim();
    base.is_empty() || NON_GAME_EXES.iter().any(|&n| base.eq_ignore_ascii_case(n))
}

/// True when the foreground window is a fullscreen window (game, F11 video, presentation) on any monitor.
/// Excludes desktop/shell windows and normal maximized windows.
pub fn is_foreground_fullscreen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() || IsWindowVisible(fg) == 0 || IsIconic(fg) != 0 {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, cls.as_mut_ptr(), 64).max(0) as usize;
        if matches!(
            String::from_utf16_lossy(&cls[..n]).as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "RustExplorer" | "topbar"
        ) {
            return false;
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(fg, &mut r) == 0 {
            return false;
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST), &mut mi) == 0 {
            return false;
        }
        let m = mi.rcMonitor;
        let covers = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
        let style = GetWindowLongW(fg, GWL_STYLE) as u32;
        is_fullscreen_window(style, IsZoomed(fg) != 0, covers)
    }
}

static TIMER_HELD: AtomicBool = AtomicBool::new(false);

/// Acquires or releases 0.5 ms (5000 x 100 ns) system timer resolution for this process.
/// Redundant calls are no-ops; releasing balances an earlier acquire so idle battery life is unaffected.
pub fn set_timer_res(enable: bool) -> bool {
    let was = TIMER_HELD.load(Relaxed);
    if enable == was {
        return true;
    }
    let mut cur = 0u32;
    let st = unsafe { NtSetTimerResolution(5000, enable as u8, &mut cur) };
    if st >= 0 {
        TIMER_HELD.store(enable, Relaxed);
        true
    } else {
        false
    }
}

#[repr(C)]
struct PowerThrottlingState {
    version: u32,
    control_mask: u32,
    state_mask: u32,
}

/// Promotes a game process from Normal to Above Normal priority and disables EcoQoS power throttling.
/// Returns the previous priority class if changed so `restore_process` can put it back cleanly.
/// Silently returns None if access is denied (e.g. kernel anti-cheat) or priority was already customized.
pub fn boost_process(pid: u32) -> Option<u32> {
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let cur = GetPriorityClass(h);
        let mut changed = None;
        if cur == NORMAL_PRIORITY_CLASS && SetPriorityClass(h, ABOVE_NORMAL_PRIORITY_CLASS) != 0 {
            // Explicitly disable EcoQoS / execution-speed throttling for the active game
            let throttle = PowerThrottlingState {
                version: 1,
                control_mask: 1, // PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                state_mask: 0,   // off = full performance
            };
            let _ = SetProcessInformation(
                h,
                4, // ProcessPowerThrottling
                &throttle as *const _ as *const c_void,
                size_of::<PowerThrottlingState>() as u32,
            );
            changed = Some(cur);
        }
        CloseHandle(h);
        changed
    }
}

/// Restores a process boosted by `boost_process` back to its previous priority and default OS power throttling.
pub fn restore_process(pid: u32, old_class: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, pid);
        if h.is_null() {
            return;
        }
        if GetPriorityClass(h) == ABOVE_NORMAL_PRIORITY_CLASS {
            let _ = SetPriorityClass(h, old_class);
        }
        let throttle = PowerThrottlingState {
            version: 1,
            control_mask: 0, // hand control back to Windows scheduler
            state_mask: 0,
        };
        let _ = SetProcessInformation(
            h,
            4,
            &throttle as *const _ as *const c_void,
            size_of::<PowerThrottlingState>() as u32,
        );
        CloseHandle(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximized_titled_window_is_not_fullscreen() {
        const WS_POPUP: u32 = 0x8000_0000;
        const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
        // Normal maximized Chrome / Discord / VS Code (under floating dock, work area == monitor)
        assert!(!is_fullscreen_window(WS_OVERLAPPEDWINDOW, true, true));
        // Borderless fullscreen game (WS_POPUP, even if marked zoomed)
        assert!(is_fullscreen_window(WS_POPUP, false, true));
        assert!(is_fullscreen_window(WS_POPUP, true, true));
        // F11 browser / exclusive fullscreen game (no WS_CAPTION)
        assert!(is_fullscreen_window(0, false, true));
        // Non-covering window is never fullscreen
        assert!(!is_fullscreen_window(WS_POPUP, false, false));
    }

    #[test]
    fn non_game_fullscreen_apps_are_excluded() {
        assert!(is_non_game_fullscreen_exe(r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        assert!(is_non_game_fullscreen_exe("VLC.EXE"));
        assert!(is_non_game_fullscreen_exe(""));
        assert!(!is_non_game_fullscreen_exe(r"D:\SteamLibrary\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe"));
        assert!(!is_non_game_fullscreen_exe("Cyberpunk2077.exe"));
        assert!(!is_non_game_fullscreen_exe("VALORANT-Win64-Shipping.exe"));
    }
}
