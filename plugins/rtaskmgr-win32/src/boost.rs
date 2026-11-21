//! Страница «Ускорение»: профили «Игра»/«Работа», кэш памяти, лишние процессы оболочки и группы debloat
//! (апдейтеры, исключения Defender, NTFS, блокировка рекламы в hosts). Всё обратимо; долгие действия идут в фоновом потоке,
//! результат забирает такт окна (`take_done`).
use crate::sys;
use std::{
    fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    ptr::null_mut,
    sync::Mutex,
};
use winapi::shared::minwindef::HKEY;
use winapi::um::{winnt::{KEY_READ, REG_SZ}, winreg::*};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Id {
    Game,
    AutoGame,
    WinLock,
    Gaming,
    Work,
    MemNow,
    MemAuto,
    Shell,
    Updaters,
    Defender,
    Ntfs,
    Dns,
}

pub const ALL: [Id; 12] = [
    Id::Game,
    Id::AutoGame,
    Id::WinLock,
    Id::Gaming,
    Id::Work,
    Id::MemNow,
    Id::MemAuto,
    Id::Shell,
    Id::Updaters,
    Id::Defender,
    Id::Ntfs,
    Id::Dns,
];

impl Id {
    pub fn key(self) -> &'static str {
        match self {
            Id::Game => "game",
            Id::AutoGame => "autogame",
            Id::WinLock => "winlock",
            Id::Gaming => "gaming",
            Id::Work => "work",
            Id::MemNow => "memnow",
            Id::MemAuto => "memauto",
            Id::Shell => "shell",
            Id::Updaters => "updaters",
            Id::Defender => "defender",
            Id::Ntfs => "ntfs",
            Id::Dns => "dns",
        }
    }
    pub fn from_key(k: &str) -> Option<Id> {
        ALL.iter().copied().find(|i| i.key() == k)
    }
    pub fn title(self) -> &'static str {
        match self {
            Id::Game => t!("Профиль «Игра»", "'Game' profile"),
            Id::AutoGame => t!("Авторежим «Игра» (topbar)", "Auto Game Mode (topbar)"),
            Id::WinLock => t!("Блокировать Win в играх", "Lock Win key in fullscreen games"),
            Id::Gaming => t!("Игровые настройки Windows (HAGS, MMCSS, DVR)", "Windows gaming tweaks (HAGS, MMCSS, DVR)"),
            Id::Work => t!("Профиль «Работа»", "'Work' profile"),
            Id::MemNow => t!("Очистить кэш памяти сейчас", "Clear memory cache now"),
            Id::MemAuto => t!("Очищать кэш памяти при нехватке", "Clear memory cache when it runs short"),
            Id::Shell => t!("Не запускать лишние процессы оболочки", "Block unneeded shell processes"),
            Id::Updaters => t!("Один обновлятор через winget", "One updater through winget"),
            Id::Defender => t!("Исключения Defender для сборок и игр", "Defender exclusions for builds and games"),
            Id::Ntfs => t!("Быстрее NTFS", "Faster NTFS"),
            Id::Dns => t!("Блокировать рекламу (hosts)", "Block ads (hosts)"),
        }
    }
    pub fn desc(self) -> &'static str {
        match self {
            Id::Game => t!("Останавливает Windows Update, BITS, SysMain, оптимизацию доставки, печать, телеметрию; закрывает OneDrive, Teams, Dropbox, Google Drive, Phone Link, виджеты; включает максимальную производительность, очищает кэш памяти и блокирует Win в играх. «Выключить» всё возвращает.", "Stops Windows Update, BITS, SysMain, Delivery Optimization, printing, telemetry; closes OneDrive, Teams, Dropbox, Google Drive, Phone Link, widgets; switches to High/Ultimate Performance power plan, purges standby RAM and locks Win in games. 'Disable' restores everything."),
            Id::AutoGame => t!("Когда запущена полноэкранная игра (браузеры и плееры не трогаются): topbar поднимает приоритет игры, включает таймер 0,5 мс, режим максимальной производительности от сети и блокирует клавишу Win. При выходе из игры всё возвращается.", "When a fullscreen game runs (browsers and media players are excluded): topbar boosts the game's priority, engages 0.5 ms timer resolution, switches to Best Performance on AC and locks the Win key. Restores everything on exit."),
            Id::WinLock => t!("Игнорирует случайные нажатия Win и Win+D/E/I/1-3 в полноэкранных играх (Alt+Tab и Ctrl+Shift+Esc работают). Также переключается сочетанием Win+Shift+G.", "Suppresses accidental Win and Win+D/E/I/1-3 presses inside fullscreen games (Alt+Tab and Ctrl+Shift+Esc keep working). Also toggles with Win+Shift+G."),
            Id::Gaming => t!("Включает Игровой режим Windows и HAGS (аппаратное планирование GPU), отключает фоновую запись Game DVR, задаёт играм высокий приоритет GPU/ЦП в MMCSS и убирает ограничение сетевых пакетов. Полностью обратимо (часть изменений после перезагрузки).", "Enables Windows Game Mode and HAGS (Hardware-Accelerated GPU Scheduling), disables Game DVR background recording, sets high MMCSS GPU/CPU priority for games and disables network throttling. Fully reversible (some changes need a reboot)."),
            Id::Work => t!("Закрывает Steam, Epic, Battle.net, EA, Ubisoft, GOG, Overwolf, Xbox и Game Bar, останавливает их службы. «Выключить» запускает лаунчеры обратно.", "Closes Steam, Epic, Battle.net, EA, Ubisoft, GOG, Overwolf, Xbox and Game Bar, stops their services. 'Disable' starts the launchers again."),
            Id::MemNow => t!("Сбрасывает список standby (кэш файлов, который никто не держит), как ISLC. Помогает от рывков в играх, когда свободной памяти нет.", "Drops the standby list (cached file data nobody holds), like ISLC. Helps against stutter in games when no memory is free."),
            Id::MemAuto => t!("Панель topbar каждые 10 с проверяет память и сбрасывает кэш, если свободно меньше 5% (не меньше 512 МБ), а кэш больше 1 ГБ; не чаще раза в минуту.", "The topbar checks memory every 10 s and drops the cache when less than 5% (at least 512 MB) is free and the cache holds over 1 GB; at most once a minute."),
            Id::Shell => t!("Меню «Пуск» Windows, виджеты и Связь с телефоном не запускаются (их заменяет topbar). Уведомления и ввод текста не трогаются.", "The Windows Start menu, widgets and Phone Link do not start (topbar replaces them). Notifications and text input are not touched."),
            Id::Updaters => t!("Отключает службы и задачи обновления Adobe, Mozilla, Dropbox, CCleaner, Opera, Apple, Zoom, OneDrive; раз в неделю winget обновляет все программы. Браузеры обновляются сами, как раньше.", "Disables updater services and tasks of Adobe, Mozilla, Dropbox, CCleaner, Opera, Apple, Zoom, OneDrive; winget updates every app once a week. Browsers keep updating themselves."),
            Id::Defender => t!("Defender перестаёт проверять кэши cargo, rustup, npm, pip, NuGet, Gradle и библиотеки Steam, Epic, Xbox, GOG, EA, Ubisoft, Riot, Battle.net, а также cargo.exe и rustc.exe. Сборка и загрузка игр быстрее, но эти папки не сканируются.", "Defender stops scanning the caches of cargo, rustup, npm, pip, NuGet, Gradle and Steam/Epic/Xbox/GOG/EA/Ubisoft/Riot/Battle.net game libraries, plus cargo.exe and rustc.exe. Builds and game loading get faster, but those folders are not scanned."),
            Id::Ntfs => t!("NTFS не пишет время последнего доступа при каждом чтении и не создаёт короткие имена 8.3. Действует после перезагрузки.", "NTFS stops writing the last-access time on every read and creating 8.3 short names. Takes effect after a reboot."),
            Id::Dns => t!("Около 170 рекламных и следящих доменов ведут в никуда через файл hosts (домены Microsoft не трогаются).", "About 170 ad and tracker domains lead nowhere through the hosts file (Microsoft domains are left alone)."),
        }
    }
    fn debloat_group(self) -> Option<&'static str> {
        matches!(self, Id::Gaming | Id::Updaters | Id::Defender | Id::Ntfs | Id::Dns).then(|| self.key())
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum State {
    On,
    Off,
    Busy,
    Info(String),        // разовое действие: что сейчас
    Unavailable(String), // почему нельзя
}

static BUSY: Mutex<Vec<Id>> = Mutex::new(vec![]);
static DONE: Mutex<Vec<String>> = Mutex::new(vec![]);

pub fn is_busy() -> bool {
    !BUSY.lock().unwrap().is_empty()
}

pub fn take_done() -> Vec<String> {
    std::mem::take(&mut *DONE.lock().unwrap())
}

fn program_files() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into()))
}

fn program_data() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into()))
}

fn topbar_installed() -> bool {
    program_files().join(r"topbar\topbar.exe").is_file()
}

/// debloat.exe: установленный плагин или копия рядом с диспетчером (её кладёт установщик).
fn debloat_exe() -> Option<PathBuf> {
    let near = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("debloat.exe")));
    [Some(program_files().join(r"debloat\debloat.exe")), near].into_iter().flatten().find(|p| p.is_file())
}

fn debloat_applied(group: &str) -> bool {
    program_data().join("winrust-debloat").join(format!("undo-{group}.txt")).is_file()
}

pub fn state(id: Id, is_admin: bool) -> State {
    if BUSY.lock().unwrap().contains(&id) {
        return State::Busy;
    }
    if !is_admin {
        return State::Unavailable(t!("нужен администратор", "administrator needed").into());
    }
    let on = |b: bool| if b { State::On } else { State::Off };
    match id {
        Id::Game | Id::Work => on(active_profile() == Some(id)),
        Id::AutoGame if !topbar_installed() => State::Unavailable(t!("нужна панель topbar", "needs the topbar").into()),
        Id::AutoGame => on(winrust_common::gaming::auto_game()),
        Id::WinLock => on(winrust_common::gaming::win_lock()),
        Id::MemNow => match winrust_common::mem::lists() {
            Some(l) => State::Info(tf!("свободно {}, кэш {}", "free {}, cache {}", crate::ui::fmt_mem(l.free), crate::ui::fmt_mem(l.standby))),
            None => State::Unavailable(t!("нет права SeProfileSingleProcess", "no SeProfileSingleProcess right").into()),
        },
        Id::MemAuto if !topbar_installed() => State::Unavailable(t!("нужна панель topbar", "needs the topbar").into()),
        Id::MemAuto => on(winrust_common::mem::auto_on()),
        Id::Shell if !topbar_installed() => State::Unavailable(t!("нужна панель topbar (без неё не будет меню «Пуск»)", "needs the topbar (no Start menu without it)").into()),
        Id::Shell => on(shell_blocked()),
        _ => match (id.debloat_group(), debloat_exe()) {
            (Some(g), Some(_)) => on(debloat_applied(g)),
            _ => State::Unavailable(t!("нет debloat.exe (переустановите диспетчер задач)", "no debloat.exe (reinstall the task manager)").into()),
        },
    }
}

/// Запускает включение/выключение в фоне. Err — сразу понятно, что нельзя (текст для подсказки).
pub fn start(id: Id, on: bool, donors: Vec<u32>) -> Result<(), String> {
    match state(id, true) {
        State::Busy => return Err(t!("уже выполняется", "already running").into()),
        State::Unavailable(why) => return Err(why),
        _ => {}
    }
    if id == Id::MemNow && !on {
        return Err(t!("это разовое действие: выключать нечего", "a one-time action: nothing to disable").into());
    }
    BUSY.lock().unwrap().push(id);
    std::thread::spawn(move || {
        let r = run(id, on, &donors);
        BUSY.lock().unwrap().retain(|&x| x != id);
        let head = id.title();
        DONE.lock().unwrap().push(match r {
            Ok(msg) => format!("{head}: {msg}"),
            Err(e) => tf!("{head}: ошибка — {e}", "{head}: error - {e}"),
        });
    });
    Ok(())
}

fn run(id: Id, on: bool, donors: &[u32]) -> Result<String, String> {
    match id {
        Id::Game | Id::Work if on => profile_on(id, donors),
        Id::Game | Id::Work => profile_off(donors),
        Id::AutoGame => {
            winrust_common::gaming::set_auto_game(on);
            Ok(if on { t!("включено (работает, пока запущена панель)", "on (works while the bar runs)") } else { t!("выключено", "off") }.into())
        }
        Id::WinLock => {
            winrust_common::gaming::set_win_lock(on);
            Ok(if on { t!("включено (Win+Shift+G для быстрого переключения)", "on (press Win+Shift+G anytime to toggle)") } else { t!("выключено", "off") }.into())
        }
        Id::MemNow => {
            let before = winrust_common::mem::lists();
            winrust_common::mem::purge_standby().map_err(|st| tf!("NTSTATUS 0x{:08X}", "NTSTATUS 0x{:08X}", st as u32))?;
            let freed = match (before, winrust_common::mem::lists()) {
                (Some(a), Some(b)) => b.free.saturating_sub(a.free),
                _ => 0,
            };
            Ok(tf!("освобождено {}", "{} freed", crate::ui::fmt_mem(freed)))
        }
        Id::MemAuto => {
            let f = winrust_common::mem::auto_file().ok_or("ProgramData?")?;
            if on {
                fs::create_dir_all(f.parent().unwrap()).and_then(|_| fs::write(&f, "1")).map_err(|e| e.to_string())?;
                Ok(t!("включено (работает, пока запущена панель)", "on (works while the bar runs)").into())
            } else {
                let _ = fs::remove_file(&f);
                Ok(t!("выключено", "off").into())
            }
        }
        Id::Shell => shell_set(on),
        _ => {
            let exe = debloat_exe().ok_or("debloat.exe")?;
            let g = id.debloat_group().unwrap();
            let o = Command::new(&exe).args([if on { "--apply" } else { "--undo" }, g]).creation_flags(0x0800_0000).output().map_err(|e| e.to_string())?;
            if o.status.success() {
                Ok(if on { t!("включено", "on") } else { t!("возвращено как было", "restored") }.into())
            } else {
                let err = String::from_utf8_lossy(&o.stderr);
                Err(err.lines().last().unwrap_or("debloat").to_string())
            }
        }
    }
}

/// Возвращает всё включённое здесь (удаление диспетчера). Синхронно; список ошибок.
pub fn undo_all() -> Vec<String> {
    let mut errs = vec![];
    for id in ALL {
        let applied = match id {
            Id::Game => active_profile().is_some(),
            Id::AutoGame => winrust_common::gaming::auto_game(),
            Id::WinLock => winrust_common::gaming::win_lock(),
            Id::Work | Id::MemNow => false, // один файл профиля на оба; очистка — разовое действие
            Id::MemAuto => winrust_common::mem::auto_on(),
            Id::Shell => SHELL_EXES.iter().any(|e| debugger(e).is_some_and(|d| ours(&d))),
            _ => id.debloat_group().is_some_and(debloat_applied),
        };
        if applied && let Err(e) = run(id, false, &[]) {
            errs.push(format!("{}: {e}", id.title()));
        }
    }
    errs
}

// ---------- профили ----------

/// (имя процесса, аргументы для запуска обратно; None — не запускать: помощник или пакетное приложение, оно встанет само)
type ProcSpec = (&'static str, Option<&'static str>);

const GAME_SVCS: &[&str] = &["wuauserv", "BITS", "DoSvc", "UsoSvc", "Spooler", "WSearch", "DiagTrack", "MapsBroker", "WerSvc", "SysMain", "TabletInputService", "RemoteRegistry"];
const GAME_PROCS: &[ProcSpec] = &[
    ("onedrive.exe", Some("/background")),
    ("teams.exe", None),
    ("ms-teams.exe", None),
    ("dropbox.exe", Some("/systemstartup")),
    ("googledrivefs.exe", Some("")),
    ("phoneexperiencehost.exe", None),
    ("widgets.exe", None),
    ("adobecollabsync.exe", None),
    ("creative cloud.exe", None),
    ("adobeipcbroker.exe", None),
    ("ccxprocess.exe", None),
    ("m365copilot.exe", None),
    ("icloudservices.exe", None),
];
const WORK_SVCS: &[&str] = &["Steam Client Service", "XblAuthManager", "XblGameSave", "XboxNetApiSvc", "EABackgroundService", "EpicOnlineServices"];
const WORK_PROCS: &[ProcSpec] = &[
    ("steam.exe", Some("-silent")),
    ("steamwebhelper.exe", None),
    ("epicgameslauncher.exe", Some("-silent")),
    ("epicwebhelper.exe", None),
    ("battle.net.exe", Some("")),
    ("eadesktop.exe", Some("")),
    ("upc.exe", Some("")),
    ("galaxyclient.exe", Some("")),
    ("overwolf.exe", None),
    ("xboxpcapp.exe", None),
    ("gamebar.exe", None),
    ("gamebarftserver.exe", None),
];

fn profile_file() -> PathBuf {
    program_data().join(r"WinRust\profile.txt")
}

/// Файл профиля: первая строка game/work, дальше `svc|<имя>` (остановлено нами) и `run|<команда>` (закрыто нами).
pub fn parse_profile(text: &str) -> Option<(Id, Vec<String>, Vec<String>)> {
    let mut lines = text.lines();
    let id = match lines.next()?.trim() {
        "game" => Id::Game,
        "work" => Id::Work,
        _ => return None,
    };
    let (mut svcs, mut runs) = (vec![], vec![]);
    for l in lines {
        if let Some(s) = l.strip_prefix("svc|") {
            svcs.push(s.to_string());
        } else if let Some(r) = l.strip_prefix("run|") {
            runs.push(r.to_string());
        }
    }
    Some((id, svcs, runs))
}

/// Извлекает сохранённый GUID схемы питания (`pwr|<guid>`) из файла профиля.
pub fn parse_profile_power(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("pwr|").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// Извлекает первый GUID вида `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` из вывода `powercfg /getactivescheme`.
pub fn parse_power_guid(out: &str) -> Option<String> {
    out.split_whitespace().find_map(|tok| {
        let s = tok.trim_matches(|c: char| !c.is_ascii_hexdigit() && c != '-');
        let parts: Vec<&str> = s.split('-').collect();
        (parts.len() == 5
            && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(&n, p)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit())))
        .then(|| s.to_ascii_lowercase())
    })
}

fn active_power_guid() -> Option<String> {
    let o = Command::new("powercfg.exe").args(["/getactivescheme"]).creation_flags(0x0800_0000).output().ok()?;
    o.status.success().then(|| parse_power_guid(&String::from_utf8_lossy(&o.stdout))).flatten()
}

fn set_power_guid(guid: &str) -> bool {
    Command::new("powercfg.exe").args(["/setactive", guid]).creation_flags(0x0800_0000).status().is_ok_and(|s| s.success())
}

/// Переключает на Ultimate Performance или High Performance (если одна из схем есть в системе). Возвращает прежний GUID.
fn boost_power_plan() -> Option<String> {
    const ULTIMATE: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";
    const HIGH: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
    let old = active_power_guid()?;
    if old == ULTIMATE || old == HIGH {
        return None;
    }
    if set_power_guid(ULTIMATE) || set_power_guid(HIGH) {
        Some(old)
    } else {
        None
    }
}

fn active_profile() -> Option<Id> {
    fs::read_to_string(profile_file()).ok().and_then(|t| parse_profile(&t)).map(|p| p.0)
}

/// Все процессы: (PID, имя в нижнем регистре, путь к exe).
fn processes() -> Vec<(u32, String, Option<String>)> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let Ok(mut s) = System::new() else { return vec![] };
    s.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_exe(UpdateKind::Always));
    s.processes().iter().map(|(pid, p)| (pid.as_u32(), p.name().to_string_lossy().to_lowercase(), p.exe().map(|e| e.display().to_string()))).collect()
}

fn profile_on(id: Id, donors: &[u32]) -> Result<String, String> {
    if active_profile().is_some() {
        profile_off(donors)?;
    }
    let (svcs, procs) = if id == Id::Game { (GAME_SVCS, GAME_PROCS) } else { (WORK_SVCS, WORK_PROCS) };
    let mut text = format!("{}\n", id.key());
    let running: Vec<String> = sys::services().into_iter().filter(|s| s.running).map(|s| s.name).collect();
    let mut stopped = 0;
    for s in svcs {
        if running.iter().any(|r| r.eq_ignore_ascii_case(s)) && sys::service_ctl(s, false).is_ok() {
            text += &format!("svc|{s}\n");
            stopped += 1;
        }
    }
    let mut closed = 0;
    let mut runs: Vec<String> = vec![];
    for (pid, name, exe) in processes() {
        let Some((_, again)) = procs.iter().find(|p| p.0 == name) else { continue };
        if !sys::kill(pid) {
            continue;
        }
        closed += 1;
        if let (Some(args), Some(exe)) = (again, exe) {
            let cmd = if args.is_empty() { format!("\"{exe}\"") } else { format!("\"{exe}\" {args}") };
            if !runs.contains(&cmd) {
                runs.push(cmd);
            }
        }
    }
    for r in &runs {
        text += &format!("run|{r}\n");
    }
    if id == Id::Game {
        if let Some(old_pwr) = boost_power_plan() {
            text += &format!("pwr|{old_pwr}\n");
        }
        let _ = winrust_common::mem::purge_standby();
    }
    let f = profile_file();
    fs::create_dir_all(f.parent().unwrap()).and_then(|_| fs::write(&f, text)).map_err(|e| e.to_string())?;
    Ok(tf!("остановлено служб: {stopped}, закрыто процессов: {closed}", "{stopped} services stopped, {closed} processes closed"))
}

fn profile_off(donors: &[u32]) -> Result<String, String> {
    let f = profile_file();
    let raw = fs::read_to_string(&f).ok();
    let Some((_, svcs, runs)) = raw.as_deref().and_then(parse_profile) else {
        return Ok(t!("профиль не был включён", "no profile was on").into());
    };
    if let Some(old_pwr) = raw.as_deref().and_then(parse_profile_power) {
        let _ = set_power_guid(&old_pwr);
    }
    let started = svcs.iter().filter(|s| sys::service_ctl(s, true).is_ok()).count();
    let mut launched = 0;
    for r in &runs {
        if sys::run_unelevated(donors, r) {
            launched += 1;
        } else if let Some(exe) = r.strip_prefix('"').and_then(|x| x.split('"').next()) {
            // без донора: explorer.exe запускает программу с обычными правами, но без аргументов
            launched += Command::new("explorer.exe").arg(exe).spawn().is_ok() as usize;
        }
    }
    fs::remove_file(&f).map_err(|e| e.to_string())?;
    Ok(tf!("запущено служб: {started}, программ: {launched}", "{started} services and {launched} programs started again"))
}

// ---------- процессы оболочки ----------

const SHELL_EXES: [&str; 4] = ["StartMenuExperienceHost.exe", "Widgets.exe", "WidgetService.exe", "PhoneExperienceHost.exe"];
const IFEO: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options";

/// Вместо процесса Windows запускает systray.exe: он ничего не показывает и сразу завершается.
fn noop_debugger() -> String {
    format!(r"{}\System32\systray.exe", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn debugger(exe: &str) -> Option<String> {
    let mut k: HKEY = null_mut();
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, w(&format!(r"{IFEO}\{exe}")).as_ptr(), 0, KEY_READ, &mut k) } != 0 {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let mut ty = 0u32;
    let rc = unsafe { RegQueryValueExW(k, w("Debugger").as_ptr(), null_mut(), &mut ty, buf.as_mut_ptr() as *mut u8, &mut len) };
    unsafe { RegCloseKey(k) };
    (rc == 0 && ty == REG_SZ).then(|| String::from_utf16_lossy(&buf[..len as usize / 2]).trim_end_matches('\0').to_string())
}

fn ours(v: &str) -> bool {
    v.to_lowercase().ends_with(r"\system32\systray.exe")
}

fn shell_blocked() -> bool {
    SHELL_EXES.iter().all(|e| debugger(e).is_some_and(|d| ours(&d)))
}

fn shell_set(on: bool) -> Result<String, String> {
    let mut n = 0;
    for exe in SHELL_EXES {
        let key = w(&format!(r"{IFEO}\{exe}"));
        let cur = debugger(exe);
        let rc = if on {
            if cur.as_deref().is_some_and(|d| !ours(d)) {
                continue; // чужой Debugger (отладчик, другая программа): не наш
            }
            let v = w(&noop_debugger());
            let rc = unsafe { RegSetKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), w("Debugger").as_ptr(), REG_SZ, v.as_ptr() as *const _, (v.len() * 2) as u32) };
            let _ = Command::new("taskkill.exe").args(["/f", "/im", exe]).creation_flags(0x0800_0000).output();
            rc
        } else if cur.as_deref().is_some_and(ours) {
            unsafe { RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), w("Debugger").as_ptr()) }
        } else {
            continue;
        };
        if rc != 0 {
            return Err(tf!("{exe}: ошибка реестра {rc}", "{exe}: registry error {rc}"));
        }
        n += 1;
    }
    Ok(if on { tf!("заблокировано процессов: {n}", "{n} processes blocked") } else { tf!("разблокировано процессов: {n}", "{n} processes unblocked") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_file_round_trip() {
        let t = "game\nsvc|wuauserv\nrun|\"C:\\Users\\a\\OneDrive.exe\" /background\nsvc|BITS\n";
        let (id, svcs, runs) = parse_profile(t).unwrap();
        assert_eq!(id, Id::Game);
        assert_eq!(svcs, ["wuauserv", "BITS"]);
        assert_eq!(runs, ["\"C:\\Users\\a\\OneDrive.exe\" /background"]);
        assert_eq!(parse_profile("junk\n"), None);
        assert_eq!(parse_profile(""), None);
    }

    #[test]
    fn keys_round_trip_and_lists_are_lower_case() {
        for id in ALL {
            assert_eq!(Id::from_key(id.key()), Some(id));
            assert!(!id.title().is_empty() && !id.desc().is_empty());
        }
        for (p, _) in GAME_PROCS.iter().chain(WORK_PROCS) {
            assert_eq!(*p, p.to_lowercase(), "имена процессов сравниваются в нижнем регистре");
        }
        assert!(ours(r"C:\WINDOWS\System32\systray.exe") && !ours("cmd.exe /c exit"));
    }

    #[test]
    fn needs_admin_without_it() {
        assert!(matches!(state(Id::Game, false), State::Unavailable(_)));
    }

    #[test]
    fn parses_power_guid_and_profile_power_line() {
        let out = "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)\r\n";
        assert_eq!(parse_power_guid(out).as_deref(), Some("381b4222-f694-41f0-9685-ff5bb260df2e"));
        assert_eq!(parse_power_guid("no guid here"), None);
        let t = "game\nsvc|wuauserv\npwr|381b4222-f694-41f0-9685-ff5bb260df2e\n";
        assert_eq!(parse_profile_power(t).as_deref(), Some("381b4222-f694-41f0-9685-ff5bb260df2e"));
        assert_eq!(parse_profile_power("work\n"), None);
    }
}
