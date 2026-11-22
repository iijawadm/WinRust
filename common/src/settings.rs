//! Opens a Settings page (`ms-settings:...`). Settings is a Store app: it opens only while Explorer is the shell (activation
//! fails with 0x80040900 otherwise, also with a shell window of our own registered). So with rust-explorer in place of Explorer:
//! start Explorer for as long as Settings is open, then end it again, which keeps the RAM saving. Done on a background thread:
//! it waits for processes, a hook or UI thread must not. Shared by Win+I (reshell) and the topbar's menus.
use crate::proc;
use std::{os::windows::process::CommandExt, process::{Command, Stdio}, thread::sleep, time::Duration};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM},
    UI::{
        Input::KeyboardAndMouse::keybd_event,
        WindowsAndMessaging::{EnumWindows, FindWindowExW, GetShellWindow, GetWindow, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, SetForegroundWindow, ShowWindow},
    },
};

const SETTINGS: &str = "SystemSettings.exe";
const NO_WINDOW: u32 = 0x0800_0000;

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn pid_of(h: HWND) -> u32 {
    let mut p = 0;
    unsafe { GetWindowThreadProcessId(h, &mut p) };
    p
}

fn shell_pid() -> Option<u32> {
    let h = unsafe { GetShellWindow() };
    (!h.is_null()).then(|| pid_of(h))
}

fn explorer_shell() -> bool {
    shell_pid().is_some_and(|p| proc::list().iter().any(|(i, n)| *i == p && n.eq_ignore_ascii_case("explorer.exe")))
}

/// The shell's own taskbar comes last (the topbar has a Shell_TrayWnd of its own, so only one that belongs to the shell process counts).
fn explorer_tray() -> bool {
    let Some(sp) = shell_pid() else { return false };
    let cls = w("Shell_TrayWnd");
    let mut t = std::ptr::null_mut();
    loop {
        t = unsafe { FindWindowExW(std::ptr::null_mut(), t, cls.as_ptr(), std::ptr::null()) };
        if t.is_null() {
            return false;
        }
        if pid_of(t) == sp {
            return true;
        }
    }
}

/// A fullscreen game keeps the screen: Settings opens behind it unless it is brought to the front.
fn raise() {
    let Some(sp) = proc::pids(SETTINGS).first().copied() else { return };
    let cls = w("Windows.UI.Core.CoreWindow");
    let mut c = std::ptr::null_mut();
    loop {
        c = unsafe { FindWindowExW(std::ptr::null_mut(), c, cls.as_ptr(), std::ptr::null()) };
        if c.is_null() {
            return;
        }
        if pid_of(c) == sp {
            unsafe {
                let mut o = GetWindow(c, 4); // GW_OWNER
                if o.is_null() {
                    o = c;
                }
                keybd_event(0x12, 0, 0, 0); // an Alt tap lets SetForegroundWindow through
                keybd_event(0x12, 0, 2, 0);
                ShowWindow(o, 9); // SW_RESTORE
                SetForegroundWindow(o);
            }
            return;
        }
    }
}

unsafe extern "system" fn hung_cb(h: HWND, l: LPARAM) -> i32 {
    let d = unsafe { &mut *(l as *mut (u32, bool)) };
    if pid_of(h) == d.0 && unsafe { IsWindowVisible(h) != 0 && IsHungAppWindow(h) != 0 } {
        d.1 = true;
    }
    1
}

fn hung(pid: u32) -> bool {
    let mut d = (pid, false);
    unsafe { EnumWindows(Some(hung_cb), &mut d as *mut _ as LPARAM) };
    d.1
}

/// A Settings process that hangs (opened before the shell was ready) swallows every later launch: it must go.
fn any_hung() -> bool {
    proc::pids(SETTINGS).into_iter().any(hung)
}

fn kill_stale() {
    proc::pids(SETTINGS).into_iter().filter(|&p| hung(p)).for_each(proc::kill);
}

fn wait(n: u32, mut done: impl FnMut() -> bool) {
    for _ in 0..n {
        if done() {
            return;
        }
        sleep(Duration::from_millis(100));
    }
}

fn has_desktop(p: u32) -> bool {
    proc::cmdline(p).is_some_and(|c| c.contains("--desktop"))
}

fn run(uri: &str) {
    kill_stale();
    let mut started = false;
    if !explorer_shell() {
        // our desktop is a window of class Progman too: with it up Explorer thinks a shell exists and starts as a plain folder host
        proc::pids("rust-explorer.exe").into_iter().filter(|&p| has_desktop(p)).for_each(proc::kill);
        let _ = Command::new("runas.exe").args(["/trustlevel:0x20000", r"C:\Windows\explorer.exe"]).creation_flags(NO_WINDOW).status();
        wait(200, explorer_shell);
        // the shell window appears before the immersive shell services behind it are up
        wait(150, explorer_tray);
        sleep(Duration::from_secs(1));
        started = true;
    }
    // through explorer.exe: the page opens with the user's normal token even when this process is elevated
    for _ in 0..2 {
        let _ = Command::new(r"C:\Windows\explorer.exe").arg(uri).status();
        wait(80, || !proc::pids(SETTINGS).is_empty());
        sleep(Duration::from_secs(1));
        raise();
        sleep(Duration::from_secs(3));
        raise();
        if !any_hung() {
            break;
        }
        kill_stale();
    }
    if started {
        // ends when Settings is closed, or when it hangs for good
        let mut bad = 0;
        while bad < 5 && !proc::pids(SETTINGS).is_empty() {
            sleep(Duration::from_secs(2));
            bad = if any_hung() { bad + 1 } else { 0 };
        }
        kill_stale();
        proc::kill_shell_explorers();
        let rx = std::path::PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_default()).join(r"rust-explorer\rust-explorer.exe");
        if rx.is_file() && !proc::pids("rust-explorer.exe").into_iter().any(has_desktop) {
            let _ = Command::new(rx).arg("--desktop").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
}

/// The rust-settings page that stands for an exact `ms-settings:` address (a bare `ms-settings:` is Win+I: the first page).
/// Only these: any other address keeps going to Windows' own Settings.
fn own_page(uri: &str) -> Option<&'static str> {
    match uri.trim().to_ascii_lowercase().as_str() {
        "ms-settings:" | "ms-settings:display" | "ms-settings:nightlight" => Some("display"),
        "ms-settings:sound" => Some("sound"),
        "ms-settings:personalization" | "ms-settings:colors" | "ms-settings:personalization-colors" | "ms-settings:themes" => Some("personalization"),
        "ms-settings:powersleep" | "ms-settings:storagesense" | "ms-settings:batterysaver" => Some("power"),
        "ms-settings:network-status" | "ms-settings:network-wifi" | "ms-settings:network-wifisettings" | "ms-settings:network-vpn" | "ms-settings:network-proxy" => Some("network"),
        "ms-settings:bluetooth" | "ms-settings:network-airplanemode" => Some("bluetooth"),
        "ms-settings:appsfeatures" => Some("apps"),
        "ms-settings:windowsupdate" => Some("update"),
        "ms-settings:recovery" => Some("recovery"),
        "ms-settings:about" => Some("about"),
        "ms-settings:gaming-gamemode" | "ms-settings:gaming-gamedvr" => Some("gaming"),
        _ => None,
    }
}

/// rust-settings.exe where the installer puts it: Program Files is writable by administrators only (a user-writable
/// folder would let any program swap the exe that Win+I starts).
fn own_exe() -> Option<std::path::PathBuf> {
    let exe = std::path::PathBuf::from(std::env::var_os("ProgramFiles")?).join("rust-settings").join("rust-settings.exe");
    exe.is_file().then_some(exe)
}

/// A second call while the first still waits is harmless.
pub fn open(uri: &str) {
    if let (Some(page), Some(exe)) = (own_page(uri), own_exe()) {
        if Command::new(exe).arg(page).stdin(Stdio::null()).creation_flags(0x0800_0000).spawn().is_ok() {
            return;
        }
    }
    let uri = uri.to_string();
    std::thread::spawn(move || run(&uri)); // shortcut: dies with the host process, which is long-lived (reshell, topbar)
}

#[cfg(test)]
mod tests {
    use super::own_page;

    #[test]
    fn only_exact_addresses_go_to_rust_settings() {
        assert_eq!(own_page("ms-settings:"), Some("display"));
        assert_eq!(own_page("MS-Settings:Sound"), Some("sound"));
        assert_eq!(own_page("ms-settings:themes"), Some("personalization"));
        assert_eq!(own_page("ms-settings:powersleep"), Some("power"));
        assert_eq!(own_page("ms-settings:network-status"), Some("network"));
        assert_eq!(own_page("ms-settings:personalization-background"), None);
        assert_eq!(own_page("ms-settings:network-wifi"), Some("network"));
        assert_eq!(own_page("ms-settings:bluetooth"), Some("bluetooth"));
        assert_eq!(own_page("ms-settings:windowsupdate"), Some("update"));
        assert_eq!(own_page("ms-settings:recovery"), Some("recovery"));
        assert_eq!(own_page("ms-settings:gaming-gamemode"), Some("gaming"));
        assert_eq!(own_page("ms-settings:easeofaccess"), None);
        assert_eq!(own_page("ms-settings:sound; calc"), None);
    }
}
