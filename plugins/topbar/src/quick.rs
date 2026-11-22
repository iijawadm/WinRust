//! The quick-settings tiles that have a state of their own: Bluetooth and airplane mode (Windows.Devices.Radios), energy saver
//! (the "best power efficiency" power mode) / gaming performance overlay, accessibility (sticky keys, filter keys, and whether Narrator / Magnifier run).
//! Radio calls wait, so they run in a thread; the flyout reads the cached answer when it repaints.
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, AtomicI8, Ordering::Relaxed},
};
use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows_sys::{
    Win32::{
        System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
        UI::{Accessibility::*, WindowsAndMessaging::*},
    },
    core::GUID,
};

// -1: no such radio, 0 off, 1 on, -2 not read yet
static BT: AtomicI8 = AtomicI8::new(-2);
static AIR: AtomicI8 = AtomicI8::new(-2);
static BUSY: AtomicBool = AtomicBool::new(false);

fn radios() -> Vec<Radio> {
    // the worker threads run WinRT calls: without COM on the thread they fail at once and no radio is ever found
    unsafe { let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED); }
    let _ = Radio::RequestAccessAsync().and_then(|a| a.join());
    Radio::GetRadiosAsync().and_then(|a| a.join()).map(|v| v.into_iter().collect()).unwrap_or_default()
}

/// Reads the radios again (one thread at a time).
pub fn refresh() {
    if BUSY.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        let rs = radios();
        let on = |r: &Radio| r.State().is_ok_and(|s| s == RadioState::On);
        let bt: Vec<_> = rs.iter().filter(|r| r.Kind().is_ok_and(|k| k == RadioKind::Bluetooth)).collect();
        BT.store(if bt.is_empty() { -1 } else { bt.iter().any(|r| on(r)) as i8 }, Relaxed);
        AIR.store(if rs.is_empty() { -1 } else { rs.iter().all(|r| !on(r)) as i8 }, Relaxed);
        BUSY.store(false, Relaxed);
    });
}

pub fn bluetooth() -> Option<bool> {
    match BT.load(Relaxed) {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// True only when the PC actually has a Bluetooth radio adapter.
pub fn has_bluetooth() -> bool {
    BT.load(Relaxed) >= 0
}

pub fn airplane() -> Option<bool> {
    match AIR.load(Relaxed) {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// True when the PC has at least one wireless radio (Wi-Fi, Bluetooth, cellular).
pub fn has_radios() -> bool {
    AIR.load(Relaxed) >= 0
}

/// Switches Bluetooth, or airplane mode (everything off; back from it Wi-Fi and Bluetooth come on).
pub fn set(air: bool, on: bool) {
    if air {
        AIR.store(on as i8, Relaxed); // the tile follows the click at once; refresh() corrects it
    } else {
        BT.store(on as i8, Relaxed);
    }
    std::thread::spawn(move || {
        for r in radios() {
            let kind = r.Kind().ok();
            let (hit, state) = if air { (on || matches!(kind, Some(RadioKind::WiFi | RadioKind::Bluetooth)), if on { RadioState::Off } else { RadioState::On }) } else { (kind == Some(RadioKind::Bluetooth), if on { RadioState::On } else { RadioState::Off }) };
            if hit {
                let _ = r.SetStateAsync(state).and_then(|a| a.join());
            }
        }
        BUSY.store(false, Relaxed);
        refresh();
    });
}

/// Switches Wi-Fi through Windows' radio manager, like Settings does: the WLAN API alone cannot turn on a radio that airplane
/// mode or Settings switched off ("Wi-Fi is locked"). `fallback` runs when there is no Wi-Fi radio to ask or it refuses.
pub fn set_wifi(on: bool, fallback: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let state = if on { RadioState::On } else { RadioState::Off };
        let done = radios().iter().filter(|r| r.Kind().is_ok_and(|k| k == RadioKind::WiFi)).fold(None, |acc: Option<bool>, r| {
            let ok = r.SetStateAsync(state).and_then(|a| a.join()).is_ok_and(|s| s == windows::Devices::Radios::RadioAccessStatus::Allowed);
            Some(acc.unwrap_or(true) && ok)
        });
        if done != Some(true) {
            fallback();
        }
        refresh();
    });
}

// ---- energy saver & gaming performance overlay ----

const EFFICIENCY: GUID = GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a);
const BALANCED: GUID = GUID::from_u128(0);
const BEST_PERFORMANCE: GUID = GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238);

static PREV_OVERLAY: Mutex<Option<GUID>> = Mutex::new(None);

fn same_guid(a: &GUID, b: &GUID) -> bool {
    (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
}

/// An export of powrprof.dll that its import library does not list, so it is looked up at run time (module handle cached once).
fn powrprof(name: &[u8]) -> Option<usize> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    static MOD: OnceLock<usize> = OnceLock::new();
    let h = *MOD.get_or_init(|| {
        let dll: Vec<u16> = "powrprof.dll".encode_utf16().chain([0]).collect();
        unsafe { LoadLibraryW(dll.as_ptr()) as usize }
    });
    if h == 0 {
        return None;
    }
    unsafe { GetProcAddress(h as _, name.as_ptr()).map(|f| f as usize) }
}

/// The power mode in force (None where Windows has no such setting).
fn overlay() -> Option<GUID> {
    let f = powrprof(b"PowerGetActualOverlayScheme\0")?;
    let mut g = BALANCED;
    (unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(*mut GUID) -> u32>(f)(&mut g) } == 0).then_some(g)
}

fn set_overlay(g: GUID) {
    if let Some(f) = powrprof(b"PowerSetActiveOverlayScheme\0") {
        unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(GUID) -> u32>(f)(g) };
    }
}

/// True on desktop PCs (no battery) or laptops plugged into AC power.
pub fn on_ac_power() -> bool {
    let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    unsafe { GetSystemPowerStatus(&mut s) == 0 || s.ACLineStatus == 1 || s.BatteryFlag == 128 }
}

/// On while the power mode is "best power efficiency" or Windows' battery saver is on.
pub fn energy_saver() -> bool {
    let eco = overlay().is_some_and(|g| same_guid(&g, &EFFICIENCY));
    let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    eco || (unsafe { GetSystemPowerStatus(&mut s) } != 0 && s.SystemStatusFlag == 1)
}

pub fn set_energy_saver(on: bool) {
    set_overlay(if on { EFFICIENCY } else { BALANCED });
}

/// Temporarily switches Windows power mode to Best Performance while a fullscreen game runs on AC power,
/// restoring the exact previous overlay when the game closes.
pub fn set_game_power(on: bool) {
    let mut prev = PREV_OVERLAY.lock().unwrap();
    if on {
        if prev.is_none() && on_ac_power() {
            if let Some(cur) = overlay() {
                if !same_guid(&cur, &BEST_PERFORMANCE) {
                    *prev = Some(cur);
                    set_overlay(BEST_PERFORMANCE);
                }
            }
        }
    } else if let Some(old) = prev.take() {
        set_overlay(old);
    }
}

// ---- accessibility ----

const SKF_ON: u32 = 1; // STICKYKEYS / FILTERKEYS: the feature is on

pub fn sticky_keys() -> bool {
    let mut k = STICKYKEYS { cbSize: size_of::<STICKYKEYS>() as u32, dwFlags: 0 };
    unsafe { SystemParametersInfoW(SPI_GETSTICKYKEYS, k.cbSize, &mut k as *mut _ as _, 0) != 0 && k.dwFlags & SKF_ON != 0 }
}

pub fn filter_keys() -> bool {
    let mut k = FILTERKEYS { cbSize: size_of::<FILTERKEYS>() as u32, dwFlags: 0, iWaitMSec: 0, iDelayMSec: 0, iRepeatMSec: 0, iBounceMSec: 0 };
    unsafe { SystemParametersInfoW(SPI_GETFILTERKEYS, k.cbSize, &mut k as *mut _ as _, 0) != 0 && k.dwFlags & SKF_ON != 0 }
}

/// Switches sticky keys (`filter` false) or filter keys; the setting is kept for the user.
pub fn set_keys(filter: bool, on: bool) {
    unsafe {
        if filter {
            let mut k = FILTERKEYS { cbSize: size_of::<FILTERKEYS>() as u32, dwFlags: 0, iWaitMSec: 0, iDelayMSec: 0, iRepeatMSec: 0, iBounceMSec: 0 };
            SystemParametersInfoW(SPI_GETFILTERKEYS, k.cbSize, &mut k as *mut _ as _, 0);
            k.dwFlags = if on { k.dwFlags | SKF_ON } else { k.dwFlags & !SKF_ON };
            SystemParametersInfoW(SPI_SETFILTERKEYS, k.cbSize, &mut k as *mut _ as _, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
        } else {
            let mut k = STICKYKEYS { cbSize: size_of::<STICKYKEYS>() as u32, dwFlags: 0 };
            SystemParametersInfoW(SPI_GETSTICKYKEYS, k.cbSize, &mut k as *mut _ as _, 0);
            k.dwFlags = if on { k.dwFlags | SKF_ON } else { k.dwFlags & !SKF_ON };
            SystemParametersInfoW(SPI_SETSTICKYKEYS, k.cbSize, &mut k as *mut _ as _, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
        }
    }
}

/// Whether a program of that exe name is running (Narrator.exe, Magnify.exe).
pub fn running(exe: &str) -> bool {
    use windows_sys::Win32::{Foundation::{CloseHandle, INVALID_HANDLE_VALUE}, System::Diagnostics::ToolHelp::*};
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok && !found {
            let n = &e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len())];
            found = String::from_utf16_lossy(n).eq_ignore_ascii_case(exe);
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        found
    }
}

/// Anything of the accessibility group is on (lights the tile).
pub fn accessibility_on() -> bool {
    sticky_keys() || filter_keys() || running("Narrator.exe") || running("Magnify.exe")
}
