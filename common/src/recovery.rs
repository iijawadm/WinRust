//! Recovery, baseline backup, change history, and safe revert helpers shared across WinRust.
//!
//! Layout under `%ProgramData%\WinRust\recovery\`:
//! - `baseline.cfg`  Original Windows settings captured ONCE before WinRust changes them.
//! - `history.log`   Append-only change log (`<unix_ts>\t<key>\t<old>\t<new>`) for "Undo Last Changes".
//! - `snapshots/`    Named recovery points (`<unix_ts>-<label>.cfg`) capturing both WinRust and Windows state.
//! - `folders-{0,1,2}.reg`  Backups of original Explorer folder/drive shell handlers if customized before WinRust.
//!
//! Safety guarantees:
//! - Never overwrites an existing baseline with WinRust's modified values.
//! - Never deletes user personal files or assumes stock Windows defaults when a baseline exists.
//! - Every registry query/write honors `WINRUST_SID` (`crate::reg::user_args`) when run elevated under another admin account.

use crate::{gaming, proc, reg, style, t, tf};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    mem::{size_of, zeroed},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    ptr::{null, null_mut},
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    UI::{
        Shell::{
            ABM_SETSTATE, ABS_ALWAYSONTOP, APPBARDATA, IsUserAnAdmin, SHAppBarMessage,
            SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW,
        },
        WindowsAndMessaging::{
            EnumWindows, FindWindowExW, FindWindowW, GetShellWindow, GetSystemMetrics,
            GetWindowThreadProcessId, HWND_BROADCAST, IsWindowVisible, PostMessageW,
            RegisterWindowMessageW, SM_CXSCREEN, SM_CYSCREEN, SPI_SETMOUSE, SPI_SETWORKAREA,
            SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SW_HIDE, SW_SHOW, SW_SHOWNORMAL,
            SendNotifyMessageW, ShowWindow, SystemParametersInfoW, WM_CLOSE,
        },
    },
};

const NO_WINDOW: u32 = 0x0800_0000;
const EXPLORER_ADV: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
const HKCU_WINLOGON: &str = r"HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
const HKLM_WINLOGON: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
const RX_FOLDERS: [&str; 3] = [
    r"HKCU\Software\Classes\Directory\shell\open",
    r"HKCU\Software\Classes\Folder\shell\open",
    r"HKCU\Software\Classes\Drive\shell\open",
];

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn recovery_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("WinRust").join("recovery")
}

pub fn baseline_path() -> PathBuf {
    recovery_dir().join("baseline.cfg")
}

pub fn history_path() -> PathBuf {
    recovery_dir().join("history.log")
}

pub fn snapshots_dir() -> PathBuf {
    recovery_dir().join("snapshots")
}

pub fn recovery_exe_path() -> PathBuf {
    let pf = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    pf.join("WinRust").join("winrust-recovery.exe")
}

fn reg_out(args: &[&str]) -> Option<String> {
    let owned = reg::user_args(args);
    let o = Command::new("reg").args(&owned).creation_flags(NO_WINDOW).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn reg_ok(args: &[&str]) -> bool {
    reg_out(args).is_some()
}

fn reg_dword(key: &str, name: &str) -> Option<u32> {
    reg::dword(&reg_out(&["query", key, "/v", name])?)
}

fn reg_str(key: &str, name: &str) -> Option<String> {
    reg::value(&reg_out(&["query", key, "/v", name])?).map(|(_, d)| d.to_string())
}

fn parse_kv(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn format_kv(map: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (k, v) in map {
        out.push_str(k);
        out.push('=');
        out.push_str(&v.replace(['\r', '\n'], " "));
        out.push('\n');
    }
    out
}

pub fn read_baseline() -> BTreeMap<String, String> {
    fs::read_to_string(baseline_path()).map(|s| parse_kv(&s)).unwrap_or_default()
}

pub fn has_baseline() -> bool {
    baseline_path().is_file()
}

/// Captures the user's original Windows settings ONCE before WinRust alters them.
/// Safe to call repeatedly: never overwrites an existing baseline file.
pub fn capture_baseline_once() {
    let p = baseline_path();
    if p.is_file() {
        return;
    }
    let dir = recovery_dir();
    let _ = fs::create_dir_all(&dir);
    let mut map = BTreeMap::new();
    map.insert("captured_at".into(), now_secs().to_string());
    // Original desktop icon visibility (0 = shown, 1 = hidden). Preserve user's actual setting!
    let hide_icons = reg_dword(EXPLORER_ADV, "HideIcons").unwrap_or(0);
    map.insert("desktop_hide_icons".into(), if hide_icons != 0 { "1" } else { "0" }.into());
    let auto_restart = reg_dword(HKLM_WINLOGON, "AutoRestartShell").unwrap_or(1);
    map.insert("auto_restart_shell".into(), auto_restart.to_string());
    if let Some(shell) = reg_str(HKCU_WINLOGON, "Shell") {
        if !shell.to_lowercase().contains("rs-shell") {
            map.insert("hkcu_winlogon_shell".into(), shell);
        }
    }
    if let Some(v) = reg_dword(r"HKCU\Software\Microsoft\GameBar", "AutoGameModeEnabled") {
        map.insert("win_game_mode".into(), v.to_string());
    }
    if let Some(v) = reg_dword(r"HKCU\System\GameConfigStore", "GameDVR_Enabled") {
        map.insert("game_dvr".into(), v.to_string());
    }
    if let Some(v) = reg_str(r"HKCU\Control Panel\Mouse", "MouseSpeed") {
        map.insert("mouse_speed".into(), v);
    }
    if let Some(v) = reg_str(r"HKCU\Control Panel\Accessibility\StickyKeys", "Flags") {
        map.insert("sticky_flags".into(), v);
    }
    // Back up any pre-existing third-party folder handlers (only if they are NOT rust-explorer).
    for (i, k) in RX_FOLDERS.iter().enumerate() {
        let bak = dir.join(format!("folders-{i}.reg"));
        let cmd = reg_out(&["query", &format!(r"{k}\command"), "/ve"]).unwrap_or_default();
        if !bak.is_file() && !cmd.to_lowercase().contains("rust-explorer") && reg_ok(&["query", k]) {
            let _ = reg_ok(&["export", k, &bak.display().to_string(), "/y"]);
        }
    }
    let _ = fs::write(p, format_kv(&map));
}

// ---------- Change journal (Undo Last Changes) ----------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeEntry {
    pub ts: u64,
    pub key: String,
    pub old: String,
    pub new: String,
}

fn sanitize_field(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

fn parse_history_line(line: &str) -> Option<ChangeEntry> {
    let mut parts = line.trim_end_matches('\r').splitn(4, '\t');
    let ts = parts.next()?.trim().parse::<u64>().ok()?;
    let key = parts.next()?.trim().to_string();
    let old = parts.next()?.to_string();
    let new = parts.next()?.to_string();
    if key.is_empty() {
        return None;
    }
    Some(ChangeEntry { ts, key, old, new })
}

fn format_history_line(e: &ChangeEntry) -> String {
    format!("{}\t{}\t{}\t{}", e.ts, sanitize_field(&e.key), sanitize_field(&e.old), sanitize_field(&e.new))
}

fn record_change_in(path: &Path, key: &str, old: &str, new: &str) {
    if old == new {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let entry = ChangeEntry {
        ts: now_secs(),
        key: key.into(),
        old: old.into(),
        new: new.into(),
    };
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", format_history_line(&entry));
    }
}

fn pop_change_from(path: &Path) -> Option<ChangeEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut entries: Vec<ChangeEntry> = text.lines().filter_map(parse_history_line).collect();
    let last = entries.pop()?;
    let mut buf = String::new();
    for e in &entries {
        buf.push_str(&format_history_line(e));
        buf.push('\n');
    }
    let _ = fs::write(path, buf);
    Some(last)
}

fn peek_change_from(path: &Path) -> Option<ChangeEntry> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().rev().find_map(parse_history_line)
}

pub fn record_change(key: &str, old: &str, new: &str) {
    capture_baseline_once();
    record_change_in(&history_path(), key, old, new);
}

pub fn history_count() -> usize {
    fs::read_to_string(history_path())
        .map(|s| s.lines().filter_map(parse_history_line).count())
        .unwrap_or(0)
}

fn friendly_key(key: &str) -> &'static str {
    match key {
        "style" => t!("Стиль", "Style"),
        "align_left" => t!("Панель слева", "Left-aligned taskbar"),
        "autohide" => t!("Автоскрытие панели", "Auto-hide taskbar"),
        "bar_size" => t!("Размер панели", "Taskbar size"),
        "side" => t!("Положение панели", "Taskbar edge"),
        "alttab_per_monitor" => t!("Alt+Tab по монитору", "Alt+Tab per monitor"),
        "bar_opacity" => t!("Прозрачность панели", "Bar opacity"),
        "hide_desktop_icons" => t!("Скрытие значков рабочего стола", "Hide desktop icons"),
        "auto_game" => t!("Авто-режим игры", "Auto Game Mode"),
        "win_lock" => t!("Блокировка клавиши Win", "Win-key lock in games"),
        "timer_boost" => t!("Таймер 0,5 мс", "0.5 ms gaming timer"),
        "raw_mouse" => t!("Отключение ускорения мыши", "Mouse acceleration"),
        "sticky_hotkey" => t!("Залипание клавиш (5x Shift)", "Sticky Keys shortcut"),
        "win_game_mode" => t!("Игровой режим Windows", "Windows Game Mode"),
        "game_dvr" => t!("Запись Game Bar DVR", "Game Bar DVR"),
        _ => t!("Настройка WinRust", "WinRust setting"),
    }
}

pub fn last_change_summary() -> Option<String> {
    let e = peek_change_from(&history_path())?;
    Some(format!("{}: {} -> {}", friendly_key(&e.key), e.old, e.new))
}

fn apply_mouse_speed(speed_str: &str) {
    let speed: i32 = speed_str.trim().parse().unwrap_or(1);
    let _ = reg_ok(&["add", r"HKCU\Control Panel\Mouse", "/v", "MouseSpeed", "/t", "REG_SZ", "/d", &speed.to_string(), "/f"]);
    let mut params = if speed == 0 { [0i32, 0, 0] } else { [6i32, 10, 1] };
    unsafe { SystemParametersInfoW(SPI_SETMOUSE, 0, params.as_mut_ptr() as _, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE) };
}

fn apply_sticky_flags(flags: &str) {
    let _ = reg_ok(&["add", r"HKCU\Control Panel\Accessibility\StickyKeys", "/v", "Flags", "/t", "REG_SZ", "/d", flags, "/f"]);
}

fn apply_change_value(key: &str, val: &str) -> Result<(), String> {
    match key {
        "style" => style::set_style(style::Style::parse(val)),
        "align_left" => style::set_align_left(val == "1"),
        "autohide" => style::set_autohide(val == "1"),
        "bar_size" => style::set_bar_size(val.parse::<u8>().unwrap_or(1)),
        "side" => style::set_side(style::Side::parse(val)),
        "alttab_per_monitor" => style::set_alttab_per_monitor(val == "1"),
        "bar_opacity" => style::set_bar_opacity(val.parse::<u8>().unwrap_or(100)),
        "hide_desktop_icons" => apply_desktop_icons_hidden(val == "1"),
        "auto_game" => gaming::set_auto_game(val == "1"),
        "win_lock" => gaming::set_win_lock(val == "1"),
        "timer_boost" => gaming::set_timer_boost(val == "1"),
        "raw_mouse" => apply_mouse_speed(if val == "1" { "0" } else { "1" }),
        "sticky_hotkey" => apply_sticky_flags(val),
        "win_game_mode" => {
            let _ = reg_ok(&["add", r"HKCU\Software\Microsoft\GameBar", "/v", "AutoGameModeEnabled", "/t", "REG_DWORD", "/d", val, "/f"]);
        }
        "game_dvr" => {
            let _ = reg_ok(&["add", r"HKCU\System\GameConfigStore", "/v", "GameDVR_Enabled", "/t", "REG_DWORD", "/d", val, "/f"]);
            let _ = reg_ok(&["add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\GameDVR", "/v", "AppCaptureEnabled", "/t", "REG_DWORD", "/d", val, "/f"]);
        }
        other => return Err(tf!("Неизвестная запись журнала: {other}", "Unknown change key: {other}")),
    }
    Ok(())
}

/// Reverts the most recent recorded WinRust change.
pub fn undo_last_change() -> Result<String, String> {
    let Some(entry) = pop_change_from(&history_path()) else {
        return Err(t!("Нет недавних изменений для отмены.", "No recent WinRust changes to undo.").into());
    };
    apply_change_value(&entry.key, &entry.old)?;
    Ok(tf!(
        "Отменено: {} ({} -> {})",
        "Reverted: {} ({} -> {})",
        friendly_key(&entry.key),
        entry.new,
        entry.old
    ))
}

// ---------- Desktop icons visibility (synced across rust-explorer AND Windows Explorer) ----------

unsafe extern "system" fn find_defview_cb(h: HWND, l: LPARAM) -> i32 {
    let out = unsafe { &mut *(l as *mut HWND) };
    let def = unsafe { FindWindowExW(h, null_mut(), wide("SHELLDLL_DefView").as_ptr(), null()) };
    if !def.is_null() {
        *out = def;
        return 0;
    }
    1
}

fn find_explorer_desktop_listview() -> HWND {
    unsafe {
        let mut def = null_mut();
        let progman = FindWindowW(wide("Progman").as_ptr(), wide("Program Manager").as_ptr());
        if !progman.is_null() {
            def = FindWindowExW(progman, null_mut(), wide("SHELLDLL_DefView").as_ptr(), null());
        }
        if def.is_null() {
            EnumWindows(Some(find_defview_cb), &mut def as *mut HWND as LPARAM);
        }
        if def.is_null() {
            return null_mut();
        }
        FindWindowExW(def, null_mut(), wide("SysListView32").as_ptr(), null())
    }
}

/// Applies desktop icon hide/show to both WinRust (`style::set_hide_desktop_icons`) and Windows Explorer
/// (`HideIcons` registry value + live `SysListView32` window visibility) without recording history.
pub fn apply_desktop_icons_hidden(hide: bool) {
    style::set_hide_desktop_icons(hide);
    let _ = reg_ok(&["add", EXPLORER_ADV, "/v", "HideIcons", "/t", "REG_DWORD", "/d", if hide { "1" } else { "0" }, "/f"]);
    let lv = find_explorer_desktop_listview();
    if !lv.is_null() {
        unsafe { ShowWindow(lv, if hide { SW_HIDE } else { SW_SHOW }) };
    }
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
}

/// Sets desktop icons hidden/visible, capturing the pre-WinRust baseline first and recording the change for Undo.
pub fn set_desktop_icons_hidden(hide: bool) {
    capture_baseline_once();
    let was = style::hide_desktop_icons();
    if was != hide {
        record_change("hide_desktop_icons", if was { "1" } else { "0" }, if hide { "1" } else { "0" });
    }
    apply_desktop_icons_hidden(hide);
}

/// Restores desktop icon visibility to the user's original pre-WinRust setting saved in `baseline.cfg`.
pub fn restore_original_desktop_icons() {
    let base = read_baseline();
    let orig_hidden = base.get("desktop_hide_icons").is_some_and(|v| v == "1");
    apply_desktop_icons_hidden(orig_hidden);
}

// ---------- Recovery Points ----------

fn sanitize_label(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let s = s.trim_matches('-');
    if s.is_empty() { "manual".into() } else { s.chars().take(32).collect() }
}

fn create_snapshot_in(dir: &Path, label: &str, kv: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = format!("{}-{}.cfg", now_secs(), sanitize_label(label));
    let path = dir.join(name);
    fs::write(&path, format_kv(kv)).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Saves a timestamped snapshot of current WinRust and Windows shell configuration, plus best-effort Windows Restore Point.
pub fn create_recovery_point(label: &str) -> Result<String, String> {
    capture_baseline_once();
    let mut kv = BTreeMap::new();
    kv.insert("created_at".into(), now_secs().to_string());
    kv.insert("label".into(), label.into());
    kv.insert("style".into(), style::style().name().into());
    kv.insert("align_left".into(), if style::align_left() { "1" } else { "0" }.into());
    kv.insert("autohide".into(), if style::autohide() { "1" } else { "0" }.into());
    kv.insert("bar_size".into(), style::bar_size().to_string());
    kv.insert("side".into(), style::side().name().into());
    kv.insert("bar_opacity".into(), style::bar_opacity().to_string());
    kv.insert("hide_desktop_icons".into(), if style::hide_desktop_icons() { "1" } else { "0" }.into());
    kv.insert("auto_game".into(), if gaming::auto_game() { "1" } else { "0" }.into());
    kv.insert("win_lock".into(), if gaming::win_lock() { "1" } else { "0" }.into());
    kv.insert("timer_boost".into(), if gaming::timer_boost() { "1" } else { "0" }.into());
    kv.insert("reg_hide_icons".into(), reg_dword(EXPLORER_ADV, "HideIcons").unwrap_or(0).to_string());
    kv.insert("reg_auto_restart_shell".into(), reg_dword(HKLM_WINLOGON, "AutoRestartShell").unwrap_or(1).to_string());
    if let Some(sh) = reg_str(HKCU_WINLOGON, "Shell") {
        kv.insert("reg_hkcu_shell".into(), sh);
    }
    let path = create_snapshot_in(&snapshots_dir(), label, &kv)?;
    // Best-effort Windows System Restore point if running elevated (Windows rate-limits to 1 per 24 h).
    let mut sys_rp = false;
    if unsafe { IsUserAnAdmin() != 0 } {
        let status = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Checkpoint-Computer -Description 'WinRust Recovery Point' -RestorePointType MODIFY_SETTINGS"])
            .creation_flags(NO_WINDOW)
            .status();
        sys_rp = status.is_ok_and(|s| s.success());
    }
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("snapshot.cfg");
    Ok(if sys_rp {
        tf!("Точка восстановления создана: {fname} (+ точка Windows)", "Recovery point saved: {fname} (+ Windows restore point)")
    } else {
        tf!("Точка восстановления создана: {fname}", "Recovery point saved: {fname}")
    })
}

pub fn list_recovery_points() -> Vec<String> {
    let mut list: Vec<String> = fs::read_dir(snapshots_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".cfg").then_some(n)
        })
        .collect();
    list.sort();
    list.reverse();
    list
}

pub fn latest_recovery_point() -> Option<String> {
    list_recovery_points().into_iter().next()
}

// ---------- Restore Explorer Associations & Windows Defaults ----------

/// Removes any `rust-explorer` overrides from HKCU Directory/Folder/Drive shell verbs so Windows File Explorer
/// launches independently again, restoring any previously backed-up third-party handler if one existed.
pub fn restore_explorer_associations() -> Result<String, String> {
    let pf = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let rx_dir = pf.join("rust-explorer");
    let rec_dir = recovery_dir();
    let mut cleaned = 0usize;
    for (i, k) in RX_FOLDERS.iter().enumerate() {
        let cmd = reg_out(&["query", &format!(r"{k}\command"), "/ve"]).unwrap_or_default();
        if cmd.to_lowercase().contains("rust-explorer") {
            let _ = reg_ok(&["delete", k, "/f"]);
            cleaned += 1;
            let bak = [rx_dir.join(format!("folders-{i}.reg")), rec_dir.join(format!("folders-{i}.reg"))]
                .into_iter()
                .find(|p| p.is_file());
            if let Some(bak) = bak {
                let _ = reg_ok(&["import", &bak.display().to_string()]);
            }
        }
    }
    if cleaned > 0 {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
    }
    Ok(tf!(
        "Ассоциации Проводника проверены (очищено переопределений: {cleaned})",
        "Explorer associations verified (overrides removed: {cleaned})"
    ))
}

fn rec_log(msg: &str) {
    let dir = recovery_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("recovery.log")) {
        let _ = writeln!(f, "[{}] {msg}", now_secs());
    }
}

pub fn explorer_exe_path() -> PathBuf {
    let win = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    win.join("explorer.exe")
}

fn hwnd_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    pid
}

fn explorer_pids() -> Vec<u32> {
    proc::pids("explorer.exe")
}

fn explorer_shell_running() -> bool {
    let h = unsafe { GetShellWindow() };
    if h.is_null() {
        return false;
    }
    let pid = hwnd_pid(h);
    pid != 0 && explorer_pids().contains(&pid)
}

fn find_windows_by_class(class: &str) -> Vec<HWND> {
    let cls = wide(class);
    let mut out = Vec::new();
    let mut cur = null_mut();
    loop {
        cur = unsafe { FindWindowExW(null_mut(), cur, cls.as_ptr(), null()) };
        if cur.is_null() {
            break;
        }
        out.push(cur);
    }
    out
}

fn explorer_tray_windows() -> Vec<HWND> {
    let epids = explorer_pids();
    let mut out = Vec::new();
    for cls in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
        for h in find_windows_by_class(cls) {
            if epids.contains(&hwnd_pid(h)) {
                out.push(h);
            }
        }
    }
    out
}

/// True when a non-Explorer process (e.g. topbar.exe) currently owns a `Shell_TrayWnd` window.
/// While a foreign `Shell_TrayWnd` exists, Windows `explorer.exe` refuses to initialize as the desktop shell.
fn has_foreign_shell_tray() -> bool {
    let epids = explorer_pids();
    find_windows_by_class("Shell_TrayWnd")
        .into_iter()
        .any(|h| !epids.contains(&hwnd_pid(h)))
}

fn has_rs_desktop() -> bool {
    unsafe { !FindWindowW(wide("Progman").as_ptr(), wide("rs-desktop").as_ptr()).is_null() }
}

/// Live health report for the Windows Explorer desktop shell & taskbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellStatus {
    pub explorer_running: bool,
    pub shell_window_ok: bool,
    pub taskbar_visible: bool,
    pub foreign_tray_present: bool,
    pub rs_desktop_present: bool,
}

impl ShellStatus {
    pub fn is_healthy(&self) -> bool {
        self.explorer_running
            && self.shell_window_ok
            && self.taskbar_visible
            && !self.foreign_tray_present
            && !self.rs_desktop_present
    }

    pub fn summary(&self) -> String {
        format!(
            "explorer={}, shell_window={}, taskbar_visible={}, foreign_tray={}, rs_desktop={}",
            self.explorer_running,
            self.shell_window_ok,
            self.taskbar_visible,
            self.foreign_tray_present,
            self.rs_desktop_present
        )
    }
}

pub fn verify_windows_shell() -> ShellStatus {
    let epids = explorer_pids();
    let shell_h = unsafe { GetShellWindow() };
    let shell_window_ok = !shell_h.is_null() && epids.contains(&hwnd_pid(shell_h));
    let trays = explorer_tray_windows();
    let taskbar_visible = trays.iter().any(|&h| unsafe { IsWindowVisible(h) != 0 });
    ShellStatus {
        explorer_running: !epids.is_empty(),
        shell_window_ok,
        taskbar_visible,
        foreign_tray_present: has_foreign_shell_tray(),
        rs_desktop_present: has_rs_desktop(),
    }
}

/// True when the current process is NOT elevated, yet restoring Windows defaults requires admin rights
/// (e.g., `topbar.exe` runs elevated via its scheduled task and cannot be closed unelevated, or HKLM AutoRestartShell is 0).
pub fn needs_elevation_for_restore() -> bool {
    if unsafe { IsUserAnAdmin() != 0 } {
        return false;
    }
    !proc::pids("topbar.exe").is_empty()
        || has_foreign_shell_tray()
        || reg_dword(HKLM_WINLOGON, "AutoRestartShell") == Some(0)
}

/// Launches the elevated recovery command (`winrust-recovery.exe restore-defaults --yes`, or fallback
/// `rust-settings.exe --restore-defaults --yes`) when called from a standard-user UI process.
pub fn launch_elevated_restore_defaults() -> Result<String, String> {
    let _ = create_recovery_point("pre-restore");
    let rec = recovery_exe_path();
    let (exe, args) = if rec.is_file() {
        (rec, "restore-defaults --yes")
    } else if let Ok(cur) = std::env::current_exe() {
        (cur, "--restore-defaults --yes")
    } else {
        return Err(t!("Не найден исполняемый файл восстановления.", "Recovery executable not found.").into());
    };
    let r = unsafe {
        ShellExecuteW(
            null_mut(),
            wide("runas").as_ptr(),
            wide(&exe.display().to_string()).as_ptr(),
            wide(args).as_ptr(),
            null(),
            SW_SHOWNORMAL,
        ) as usize
    };
    if r > 32 {
        Ok(t!(
            "Запущено восстановление с правами администратора (подтвердите UAC).",
            "Elevated Windows restore launched (confirm UAC)."
        )
        .into())
    } else {
        Err(t!(
            "Не удалось запустить восстановление с правами администратора (UAC отменён).",
            "Administrator confirmation (UAC) is required to stop elevated topbar and restore Windows Explorer."
        )
        .into())
    }
}

#[derive(Clone, Copy, Default)]
struct WinRustShellSnapshot {
    had_topbar: bool,
    had_desktop: bool,
    had_reshell: bool,
}

fn capture_shell_snapshot() -> WinRustShellSnapshot {
    WinRustShellSnapshot {
        had_topbar: !proc::pids("topbar.exe").is_empty(),
        had_desktop: has_rs_desktop(),
        had_reshell: !proc::pids("reshell.exe").is_empty(),
    }
}

fn wait_until(timeout_ms: u64, step_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let steps = (timeout_ms / step_ms.max(1)).max(1);
    for _ in 0..steps {
        if cond() {
            return true;
        }
        sleep(Duration::from_millis(step_ms));
    }
    cond()
}

/// Stops WinRust shell processes (`rs-shell`, `reshell`, `topbar`, `rust-explorer --desktop`) in strict safe order
/// and waits until all foreign `Shell_TrayWnd` and `rs-desktop` `Progman` windows are completely gone.
pub fn stop_winrust_shell_and_wait() -> Result<(), String> {
    // 1) Supervisors & keyboard hooks first so nothing respawns topbar or blocks keyboard input.
    for name in ["rs-shell.exe", "reshell.exe"] {
        for pid in proc::pids(name) {
            proc::kill(pid);
        }
        let _ = Command::new("taskkill").args(["/f", "/im", name]).creation_flags(NO_WINDOW).status();
    }

    // 2) Ask topbar politely to close (its WM_DESTROY restores taskbar state & destroys its Shell_TrayWnd), then force if needed.
    let tb_win = unsafe { FindWindowW(wide("topbar").as_ptr(), null()) };
    if !tb_win.is_null() {
        unsafe { PostMessageW(tb_win, WM_CLOSE, 0, 0) };
    }
    if !wait_until(1500, 100, || proc::pids("topbar.exe").is_empty() && !has_foreign_shell_tray()) {
        for pid in proc::pids("topbar.exe") {
            proc::kill(pid);
        }
        let _ = Command::new("taskkill").args(["/f", "/im", "topbar.exe"]).creation_flags(NO_WINDOW).status();
    }

    // 3) Close custom desktop host (only `--desktop` instances; keep normal folder windows untouched if any).
    let desk_win = unsafe { FindWindowW(wide("Progman").as_ptr(), wide("rs-desktop").as_ptr()) };
    if !desk_win.is_null() {
        unsafe { PostMessageW(desk_win, WM_CLOSE, 0, 0) };
    }
    if !wait_until(1500, 100, || !has_rs_desktop()) {
        for pid in proc::pids("rust-explorer.exe") {
            if proc::cmdline(pid).is_some_and(|c| c.contains("--desktop")) {
                proc::kill(pid);
            }
        }
    }

    // 4) Wait until all shell-conflicting windows are completely released before launching explorer.exe.
    let clear = wait_until(4000, 100, || {
        proc::pids("rs-shell.exe").is_empty()
            && proc::pids("topbar.exe").is_empty()
            && !has_foreign_shell_tray()
            && !has_rs_desktop()
    });
    if !clear {
        return Err(t!(
            "Не удалось остановить topbar или рабочий стол WinRust (нужны права администратора).",
            "Could not stop elevated topbar or WinRust desktop before starting Explorer (administrator rights required)."
        )
        .into());
    }
    Ok(())
}

/// Restores primary work area, unhides Explorer's stock taskbar(s) directly via Win32, restores saved auto-hide
/// preference if present, and notifies tray applications (`TaskbarCreated`).
pub fn unhide_explorer_taskbars() {
    unsafe {
        let mut wa = RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        };
        SystemParametersInfoW(SPI_SETWORKAREA, 0, &mut wa as *mut RECT as _, SPIF_SENDCHANGE);
    }
    let state_file = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("topbar-taskbar-state.txt"));
    let saved_state = state_file
        .as_ref()
        .and_then(|f| fs::read_to_string(f).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(ABS_ALWAYSONTOP as usize);

    for tb in explorer_tray_windows() {
        unsafe {
            let mut ab: APPBARDATA = zeroed();
            ab.cbSize = size_of::<APPBARDATA>() as u32;
            ab.hWnd = tb;
            ab.lParam = saved_state as isize;
            SHAppBarMessage(ABM_SETSTATE, &mut ab);
            ShowWindow(tb, SW_SHOW);
        }
    }
    if let Some(f) = state_file {
        let _ = fs::remove_file(f);
    }
    unsafe {
        let msg = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        if msg != 0 {
            SendNotifyMessageW(HWND_BROADCAST, msg, 0, 0);
        }
    }
}

fn start_explorer_via_limited_task(explorer: &Path) {
    let ps = format!(
        "$u=(Get-CimInstance Win32_ComputerSystem).UserName; \
         if (-not $u) {{ $u=\"$env:USERDOMAIN\\$env:USERNAME\" }}; \
         Register-ScheduledTask -TaskName WinRust-StartExplorer -Force \
           -Action (New-ScheduledTaskAction -Execute '{}') \
           -Principal (New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited) | Out-Null; \
         Start-ScheduledTask -TaskName WinRust-StartExplorer; \
         Start-Sleep -Milliseconds 1200; \
         Unregister-ScheduledTask -TaskName WinRust-StartExplorer -Confirm:$false -ErrorAction SilentlyContinue",
        explorer.display()
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .creation_flags(NO_WINDOW)
        .status();
}

/// Ensures Windows `explorer.exe` is running as the desktop shell with a visible taskbar, using a multi-tier
/// verified launch strategy. Never returns `Ok` until `verify_windows_shell().is_healthy()` is true.
pub fn ensure_explorer_shell_verified() -> Result<ShellStatus, String> {
    let explorer = explorer_exe_path();
    if !explorer.is_file() {
        return Err(tf!(
            "Не найден системный файл Проводника: {}",
            "Windows Explorer executable not found: {}",
            explorer.display()
        ));
    }

    if !explorer_shell_running() {
        let elevated = unsafe { IsUserAnAdmin() != 0 };
        let exp_str = explorer.display().to_string();
        if !elevated {
            rec_log("Tier 1: launching explorer.exe directly (unelevated)");
            let _ = Command::new(&explorer).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            wait_until(6000, 150, || explorer_shell_running() && !explorer_tray_windows().is_empty());
        } else {
            rec_log("Tier 2: launching explorer.exe via runas /trustlevel:0x20000");
            let _ = Command::new("runas.exe")
                .args(["/trustlevel:0x20000", &exp_str])
                .creation_flags(NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if !wait_until(6000, 150, || explorer_shell_running() && !explorer_tray_windows().is_empty()) {
                rec_log("Tier 3: runas did not initialize shell, trying limited interactive task");
                start_explorer_via_limited_task(&explorer);
                wait_until(6000, 150, || explorer_shell_running() && !explorer_tray_windows().is_empty());
            }
            if !explorer_shell_running() {
                rec_log("Tier 4: last-resort direct explorer.exe spawn");
                let _ = Command::new(&explorer).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
                wait_until(5000, 150, || explorer_shell_running() && !explorer_tray_windows().is_empty());
            }
        }
    }

    unhide_explorer_taskbars();
    wait_until(2500, 150, || verify_windows_shell().is_healthy());
    let status = verify_windows_shell();
    rec_log(&format!("Post-restore shell verification: {}", status.summary()));
    if status.is_healthy() {
        Ok(status)
    } else {
        Err(tf!(
            "Проверка оболочки Windows не пройдена ({}). Попробуйте перезагрузить компьютер или запустить explorer.exe в Диспетчере задач.",
            "Windows shell verification failed ({}). Try rebooting or running explorer.exe from Task Manager.",
            status.summary()
        ))
    }
}

/// Emergency automatic rollback if Explorer failed to start: brings WinRust's shell back so the user is never left on a black screen.
fn rollback_to_winrust_shell(snap: WinRustShellSnapshot) {
    let pf = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    if snap.had_topbar {
        let tb = pf.join(r"topbar\topbar.exe");
        if tb.is_file() {
            let _ = Command::new(&tb).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
    if snap.had_desktop {
        let rx = pf.join(r"rust-explorer\rust-explorer.exe");
        if rx.is_file() {
            let _ = Command::new(&rx).arg("--desktop").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
    if snap.had_reshell {
        let rs = pf.join(r"topbar\reshell.exe");
        if rs.is_file() {
            let _ = Command::new(&rs).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
}

/// Restores Windows shell, File Explorer, taskbar, desktop icons (from baseline), and resets WinRust overrides.
/// Never reports success unless Windows Explorer and the taskbar are verified running; automatically rolls back
/// to WinRust's shell if Explorer cannot initialize so a black screen cannot occur. Never deletes personal files.
pub fn restore_windows_defaults() -> Result<String, String> {
    rec_log("restore_windows_defaults: start");
    if !explorer_exe_path().is_file() {
        return Err(t!(
            "Отмена восстановления: C:\\Windows\\explorer.exe не найден.",
            "Restore aborted: Windows explorer.exe was not found."
        )
        .into());
    }
    if needs_elevation_for_restore() {
        return launch_elevated_restore_defaults();
    }

    let snap = capture_shell_snapshot();
    let _ = create_recovery_point("pre-restore");
    let _ = restore_explorer_associations();
    let base = read_baseline();

    // 1) Restore shell/Winlogon registry keys BEFORE stopping WinRust shell.
    if let Some(cur_shell) = reg_str(HKCU_WINLOGON, "Shell") {
        if cur_shell.to_lowercase().contains("rs-shell") {
            if let Some(orig) = base.get("hkcu_winlogon_shell").filter(|s| !s.is_empty()) {
                let _ = reg_ok(&["add", HKCU_WINLOGON, "/v", "Shell", "/t", "REG_SZ", "/d", orig, "/f"]);
            } else {
                let _ = reg_ok(&["delete", HKCU_WINLOGON, "/v", "Shell", "/f"]);
            }
        }
    }
    let _ = reg_ok(&["delete", r"HKCU\Software\rs-shell", "/f"]);
    let orig_restart = base.get("auto_restart_shell").map(String::as_str).unwrap_or("1");
    let _ = reg_ok(&["add", HKLM_WINLOGON, "/v", "AutoRestartShell", "/t", "REG_DWORD", "/d", orig_restart, "/f"]);

    // Disable WinRust shell autostart entries so Windows stays on stock Explorer after a reboot.
    for tn in ["rs-overlay", "topbar", "reshell"] {
        let _ = Command::new("schtasks").args(["/change", "/tn", tn, "/disable"]).creation_flags(NO_WINDOW).status();
    }
    let run_key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    for val in ["rs-overlay", "topbar", "reshell"] {
        let _ = reg_ok(&["delete", run_key, "/v", val, "/f"]);
    }

    // Restore Windows gaming/accessibility registry keys only when captured in baseline.
    if let Some(v) = base.get("win_game_mode") {
        let _ = reg_ok(&["add", r"HKCU\Software\Microsoft\GameBar", "/v", "AutoGameModeEnabled", "/t", "REG_DWORD", "/d", v, "/f"]);
    }
    if let Some(v) = base.get("game_dvr") {
        let _ = reg_ok(&["add", r"HKCU\System\GameConfigStore", "/v", "GameDVR_Enabled", "/t", "REG_DWORD", "/d", v, "/f"]);
        let _ = reg_ok(&["add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\GameDVR", "/v", "AppCaptureEnabled", "/t", "REG_DWORD", "/d", v, "/f"]);
    }
    if let Some(v) = base.get("mouse_speed") {
        apply_mouse_speed(v);
    }
    if let Some(v) = base.get("sticky_flags") {
        apply_sticky_flags(v);
    }

    // 2) Stop ALL WinRust shell components (rs-shell, reshell, topbar, rust-explorer --desktop) and wait until
    //    their Shell_TrayWnd and Progman windows are completely gone so explorer.exe can become the real shell.
    if let Err(e) = stop_winrust_shell_and_wait() {
        rec_log(&format!("stop_winrust_shell_and_wait failed: {e}"));
        rollback_to_winrust_shell(snap);
        return Err(e);
    }

    // 3) Reset WinRust UI & gaming switches cleanly.
    style::reset_all();
    gaming::set_auto_game(false);
    gaming::set_win_lock(false);
    gaming::set_timer_boost(false);

    // 4) Start Windows Explorer safely and VERIFY shell + visible taskbar before declaring success.
    if let Err(e) = ensure_explorer_shell_verified() {
        rec_log(&format!("Explorer verification failed, rolling back WinRust shell: {e}"));
        rollback_to_winrust_shell(snap);
        return Err(tf!(
            "{e} Оболочка WinRust временно возвращена, чтобы избежать чёрного экрана.",
            "{e} WinRust shell was temporarily restored to prevent a black screen."
        ));
    }

    // 5) Restore user's original pre-WinRust desktop icon visibility now that Explorer desktop is live.
    restore_original_desktop_icons();
    rec_log("restore_windows_defaults: verified OK");

    Ok(t!(
        "Настройки Windows восстановлены и проверены: Проводник и панель задач работают (личные файлы не затронуты).",
        "Windows defaults restored and verified: Explorer and taskbar are active (no personal files were touched)."
    )
    .into())
}

// ---------- Full WinRust Uninstall ----------

/// Launches the full elevated uninstaller (`winrust-recovery.exe rollback --yes` if installed,
/// otherwise `rust-settings.exe --uninstall-all --yes`).
pub fn launch_full_uninstall() -> Result<String, String> {
    let _ = create_recovery_point("pre-uninstall");
    let rec = recovery_exe_path();
    let (exe, args) = if rec.is_file() {
        (rec, "rollback --yes")
    } else if let Ok(cur) = std::env::current_exe() {
        (cur, "--uninstall-all --yes")
    } else {
        return Err(t!("Не найден исполняемый файл восстановления.", "Recovery executable not found.").into());
    };
    let r = unsafe {
        ShellExecuteW(
            null_mut(),
            wide("runas").as_ptr(),
            wide(&exe.display().to_string()).as_ptr(),
            wide(args).as_ptr(),
            null(),
            SW_SHOWNORMAL,
        ) as usize
    };
    if r > 32 {
        Ok(t!(
            "Запущено полное удаление WinRust (подтвердите UAC).",
            "Full WinRust uninstall launched (confirm UAC)."
        )
        .into())
    } else {
        Err(t!(
            "Не удалось запустить удаление с правами администратора.",
            "Failed to launch elevated uninstall."
        )
        .into())
    }
}

/// Fallback full uninstall executed when `rust-settings.exe --uninstall-all --yes` runs elevated
/// (if `winrust-recovery.exe` was not present). Never touches user personal files.
pub fn uninstall_all_in_process() -> Result<String, String> {
    rec_log("uninstall_all_in_process: start");
    let restore_res = restore_windows_defaults();

    // Stop remaining WinRust background processes (shell processes were already cleanly stopped above).
    for name in [
        "rs-shell.exe",
        "rust-explorer.exe",
        "topbar.exe",
        "reshell.exe",
        "rs.exe",
        "rs-launcher.exe",
        "rtaskmgr-win32.exe",
        "wiztree.exe",
        "photoview.exe",
    ] {
        for pid in proc::pids(name) {
            proc::kill(pid);
        }
    }

    // Remove scheduled tasks created by WinRust.
    for tn in ["rs-overlay", "topbar", "reshell", "rust-search", "rs-launcher", "WinRustSearch"] {
        let _ = Command::new("schtasks").args(["/delete", "/tn", tn, "/f"]).creation_flags(NO_WINDOW).status();
    }

    // Remove HKCU Run entries created by WinRust.
    let run_key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    for val in ["rs-overlay", "topbar", "reshell", "rs-launcher", "WinRustNightLight"] {
        let _ = reg_ok(&["delete", run_key, "/v", val, "/f"]);
    }

    // Remove Task Manager IFEO override only if it points to rtaskmgr-win32.
    let tm_ifeo = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";
    if reg_str(tm_ifeo, "Debugger").is_some_and(|d| d.to_lowercase().contains("rtaskmgr")) {
        let _ = reg_ok(&["delete", tm_ifeo, "/v", "Debugger", "/f"]);
    }
    // Unblock SearchHost IFEO if WinRust blocked it.
    let sh_ifeo = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\SearchHost.exe";
    if reg_str(sh_ifeo, "Debugger").is_some_and(|d| d.to_lowercase().contains("systray")) {
        let _ = reg_ok(&["delete", sh_ifeo, "/v", "Debugger", "/f"]);
    }
    // Re-enable Windows Search service if disabled.
    let _ = Command::new("sc.exe").args(["config", "WSearch", "start=", "delayed-auto"]).creation_flags(NO_WINDOW).status();
    let _ = Command::new("sc.exe").args(["start", "WSearch"]).creation_flags(NO_WINDOW).status();

    // Remove Start Menu shortcuts created by WinRust.
    let sm = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(r"Microsoft\Windows\Start Menu\Programs");
    for lnk in [
        "Rust Explorer.lnk",
        "WinRust Settings.lnk",
        "WinRust Recovery.lnk",
        "WinRust Disk.lnk",
        "Photo viewer.lnk",
    ] {
        let _ = fs::remove_file(sm.join(lnk));
    }

    // Remove plugin directories under Program Files (leaving rust-settings for self-cleanup if currently executing).
    let pf = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    for dir in ["rust-explorer", "topbar", "rust-search", "rtaskmgr-win32", "wiztree", "photoview", "debloat", "purge", "WinRust"] {
        let _ = fs::remove_dir_all(pf.join(dir));
    }

    // Ensure Windows Explorer shell is verified alive after removing plugin directories.
    let shell_ok = ensure_explorer_shell_verified();
    // Restore original desktop icons one final time after Explorer is active.
    restore_original_desktop_icons();

    if let Err(e) = shell_ok {
        return Err(e);
    }
    if let Err(e) = restore_res {
        rec_log(&format!("uninstall_all_in_process initial restore note: {e}"));
    }

    Ok(t!(
        "Компоненты WinRust удалены, Проводник и панель задач Windows проверены.",
        "WinRust components removed; Windows Explorer and taskbar verified."
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_round_trips_cleanly() {
        let mut m = BTreeMap::new();
        m.insert("desktop_hide_icons".into(), "1".into());
        m.insert("auto_restart_shell".into(), "1".into());
        m.insert("hkcu_winlogon_shell".into(), "explorer.exe".into());
        let text = format_kv(&m);
        let back = parse_kv(&text);
        assert_eq!(back, m);
    }

    #[test]
    fn history_journal_push_peek_and_pop() {
        let path = std::env::temp_dir().join(format!("winrust-hist-{}.log", std::process::id()));
        let _ = fs::remove_file(&path);
        record_change_in(&path, "align_left", "0", "1");
        record_change_in(&path, "align_left", "1", "1"); // no-op when equal
        record_change_in(&path, "hide_desktop_icons", "0", "1");
        let top = peek_change_from(&path).unwrap();
        assert_eq!(top.key, "hide_desktop_icons");
        assert_eq!(top.old, "0");
        assert_eq!(top.new, "1");
        let popped = pop_change_from(&path).unwrap();
        assert_eq!(popped.key, "hide_desktop_icons");
        let second = pop_change_from(&path).unwrap();
        assert_eq!(second.key, "align_left");
        assert!(pop_change_from(&path).is_none());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn snapshot_label_sanitization_and_save() {
        assert_eq!(sanitize_label("before / test!"), "before---test");
        assert_eq!(sanitize_label("   "), "manual");
        let dir = std::env::temp_dir().join(format!("winrust-snap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut kv = BTreeMap::new();
        kv.insert("style".into(), "normal".into());
        let f = create_snapshot_in(&dir, "unit_test", &kv).unwrap();
        assert!(f.is_file());
        let back = parse_kv(&fs::read_to_string(&f).unwrap());
        assert_eq!(back.get("style").map(String::as_str), Some("normal"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_status_health_requires_explorer_and_clean_tray() {
        let healthy = ShellStatus {
            explorer_running: true,
            shell_window_ok: true,
            taskbar_visible: true,
            foreign_tray_present: false,
            rs_desktop_present: false,
        };
        assert!(healthy.is_healthy());
        assert!(!ShellStatus { taskbar_visible: false, ..healthy }.is_healthy());
        assert!(!ShellStatus { shell_window_ok: false, ..healthy }.is_healthy());
        assert!(!ShellStatus { foreign_tray_present: true, ..healthy }.is_healthy());
        assert!(!ShellStatus { rs_desktop_present: true, ..healthy }.is_healthy());
        assert!(!ShellStatus { explorer_running: false, ..healthy }.is_healthy());
    }
}
