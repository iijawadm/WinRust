//! The main WinRust window: DPI-aware double-buffered GDI+/GDI rendering, responsive landscape layout,
//! confirmation & plugin settings dialogs, and message loop handler.
use crate::{layout::*, *};

// ---------- drawing ----------
//
// One back buffer per paint: GDI+ for anti-aliased shapes (creating a graphics context per shape so it obeys
// the DC's IntersectClipRect clip region), and GDI (ClearType) for crisp text.

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// COLORREF (0x00BBGGRR) + alpha -> GDI+ ARGB.
pub(crate) const fn argb(c: u32, a: u8) -> u32 {
    (a as u32) << 24 | (c & 255) << 16 | (c >> 8 & 255) << 8 | (c >> 16 & 255)
}

pub(crate) fn lerp(a: u32, b: u32, t: f32) -> u32 {
    let m = |sh: u32| (((a >> sh & 255) as f32) * (1.0 - t) + ((b >> sh & 255) as f32) * t) as u32;
    m(0) | m(8) << 8 | m(16) << 16
}

pub(crate) struct Ui {
    dc: HDC,
    s: f32,
}

impl Ui {
    fn px(&self, v: f32) -> i32 {
        (v * self.s).round() as i32
    }
    unsafe fn shape(&self, b: Box4, rad: f32, color: u32) {
        let mut g = null_mut::<GpGraphics>();
        if GdipCreateFromHDC(self.dc, &mut g) != 0 || g.is_null() {
            return;
        }
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        let (s, mut path) = (self.s, null_mut::<GpPath>());
        let (x, y, w, h) = (b[0] * s, b[1] * s, (b[2] - b[0]) * s, (b[3] - b[1]) * s);
        if w > 0.1 && h > 0.1 {
            let d = (rad * 2.0 * s).min(h).min(w).max(0.01);
            GdipCreatePath(FillModeAlternate, &mut path);
            GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
            GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
            GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
            GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
            GdipClosePathFigure(path);
            let mut br = null_mut::<GpSolidFill>();
            GdipCreateSolidFill(color, &mut br);
            GdipFillPath(g, br as *mut GpBrush, path);
            GdipDeleteBrush(br as *mut GpBrush);
            GdipDeletePath(path);
        }
        GdipDeleteGraphics(g);
    }
    /// Rounded box with a 1 px outline (`line` == `fill` for none).
    unsafe fn round(&self, b: Box4, rad: f32, fill: u32, line: u32) {
        if line != fill {
            self.shape(b, rad, argb(line, 255));
            let i = 1.0 / self.s;
            self.shape([b[0] + i, b[1] + i, b[2] - i, b[3] - i], (rad - i).max(0.0), argb(fill, 255));
        } else {
            self.shape(b, rad, argb(fill, 255));
        }
    }
    unsafe fn font(&self, size: f32, weight: i32, face: &str) -> HFONT {
        CreateFontW(-self.px(size), 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, wide(face).as_ptr())
    }
    unsafe fn text_in(&self, s: &str, b: Box4, size: f32, weight: i32, color: u32, flags: u32, face: &str) {
        let f = self.font(size, weight, face);
        let of = SelectObject(self.dc, f);
        SetTextColor(self.dc, color);
        SetBkMode(self.dc, TRANSPARENT as i32);
        let mut r = RECT { left: self.px(b[0]), top: self.px(b[1]), right: self.px(b[2]), bottom: self.px(b[3]) };
        DrawTextW(self.dc, wide(s).as_ptr(), -1, &mut r, flags | DT_NOPREFIX);
        SelectObject(self.dc, of);
        DeleteObject(f);
    }
    unsafe fn text(&self, s: &str, b: Box4, size: f32, weight: i32, color: u32, flags: u32) {
        self.text_in(s, b, size, weight, color, flags, winrust_common::style::sans_face());
    }
    /// Width of a single line of text, logical px.
    unsafe fn width(&self, s: &str, size: f32, weight: i32) -> f32 {
        let f = self.font(size, weight, winrust_common::style::sans_face());
        let of = SelectObject(self.dc, f);
        let mut r: RECT = zeroed();
        DrawTextW(self.dc, wide(s).as_ptr(), -1, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(self.dc, of);
        DeleteObject(f);
        r.right as f32 / self.s
    }
}

pub(crate) const N_READY: usize = {
    let (mut n, mut i) = (0, 0);
    while i < PLUGINS.len() {
        if PLUGINS[i].actions.is_some() && PLUGINS[i].bulk {
            n += 1;
        }
        i += 1;
    }
    n
};

// clickable element IDs
pub(crate) const ID_MAIN: usize = 0;
pub(crate) const ID_ROLLBACK: usize = 1;
pub(crate) const ID_OK: usize = 2;
pub(crate) const ID_CANCEL: usize = 3;
pub(crate) const ID_SWITCH: usize = 4;
pub(crate) const ID_GEAR: usize = 5;
pub(crate) const ID_LANG: usize = 6; // EN / RU switch
pub(crate) const ID_SHELL: usize = 7; // settings panel: replace Explorer with rust-explorer
pub(crate) const ID_UNDO: usize = 8; // revert last recorded WinRust change
pub(crate) const ID_RESTORE: usize = 9; // safely restore Windows defaults + verify shell
pub(crate) const ID_CHECK: usize = 100; // + plugin index: card / checkbox toggle
pub(crate) const STARTING: usize = usize::MAX - 1; // JOB before install_all picks its first component
pub(crate) const JOB_RESTORE: usize = PLUGINS.len() + 1;

/// Installable components, in install order (topbar comes before rust-explorer, which needs it).
pub(crate) fn ready() -> impl Iterator<Item = usize> {
    (0..PLUGINS.len()).filter(|&i| PLUGINS[i].actions.is_some() && PLUGINS[i].bulk)
}

/// The components the checkboxes have ticked, in install order.
pub(crate) fn chosen() -> impl Iterator<Item = usize> {
    ready().filter(|&i| SELECTED[i].load(Relaxed))
}

/// Index of rust-explorer in PLUGINS.
fn explorer_idx() -> Option<usize> {
    PLUGINS.iter().position(|p| p.name == crate::selection::RUST_EXPLORER)
}

unsafe fn dpi_scale(hwnd: HWND) -> f32 {
    let dc = GetDC(hwnd);
    let s = (GetDeviceCaps(dc, LOGPIXELSX as i32) as f32 / 96.0).max(1.0);
    ReleaseDC(hwnd, dc);
    s
}

/// Button styles: 0 = primary accent, 1 = secondary outline, 2 = danger outline, 3 = disabled, 4 = warning/restore outline.
pub(crate) unsafe fn button(ui: &Ui, b: Box4, label: &str, kind: u8, hot: bool) {
    let (fill, line, fg) = match kind {
        0 => (ACCENT, ACCENT, rgb(255, 255, 255)),
        1 => (PANEL, BORDER, TEXT),
        2 => (PANEL, BORDER, BAD),
        4 => (PANEL, lerp(BORDER, WARN_COL, 0.35), WARN_COL),
        _ => (PANEL, lerp(PANEL, BORDER, 0.55), lerp(DIM, PANEL, 0.35)),
    };
    let fill = if hot && kind != 3 { lerp(fill, TEXT, if kind == 0 { 0.14 } else { 0.08 }) } else { fill };
    let line = if hot && kind == 2 {
        BAD
    } else if hot && kind == 4 {
        WARN_COL
    } else if kind == 0 {
        fill
    } else {
        line
    };
    ui.round(b, 8.0, fill, line);
    ui.text(label, b, 13.0, 600, fg, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
}

pub(crate) unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let (cw, ch) = (rc.right.max(1), rc.bottom.max(1));
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, cw, ch);
    let ob = SelectObject(mem, bmp);
    let s = (GetDeviceCaps(hdc, LOGPIXELSX as i32) as f32 / 96.0).max(1.0);
    let bgb = CreateSolidBrush(BG);
    FillRect(mem, &rc, bgb);
    DeleteObject(bgb);
    let ui = Ui { dc: mem, s };
    let (w, h) = (cw as f32 / s, ch as f32 / s);
    let lay = layout::compute(w, h, N_READY, SCROLL.get());
    SCROLL.set(lay.scroll);

    let hover = HOVER.get();
    let dialog = DIALOG.with_borrow(|d| d.as_ref().map(|d| (d.title.clone(), d.text.clone(), d.ok.clone(), d.danger)));
    let hot = |id: usize| hover == Some(id);
    let busy = BUSY.load(Relaxed);
    let job = JOB.load(Relaxed);
    let n = PLUGINS.len();
    let n_inst = ready().filter(|&i| INSTALLED[i].load(Relaxed)).count();
    let n_sel = chosen().count();
    let n_sel_inst = chosen().filter(|&i| INSTALLED[i].load(Relaxed)).count();
    let can_undo = !busy && i18n::recovery::history_count() > 0;

    // 1) Header: top accent line, compact badge, title & subtitle, language toggle
    ui.shape([0.0, 0.0, w, 3.0], 0.0, argb(ACCENT, 255));
    let logo = [M, 18.0, M + 44.0, 62.0];
    ui.round(logo, 12.0, ACCENT, ACCENT);
    let inner = lerp(ACCENT, BG, 0.14);
    ui.round([logo[0] + 3.0, logo[1] + 3.0, logo[2] - 3.0, logo[3] - 3.0], 9.0, inner, inner);
    ui.text("W", [logo[0], logo[1] - 1.0, logo[2], logo[3]], 24.0, 900, rgb(255, 255, 255), DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    let tx = logo[2] + 14.0;
    ui.text("WinRust", [tx, 15.0, lay.lang_box[0] - 12.0, 42.0], 22.0, 700, TEXT, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    ui.text(
        &tf!("Оболочка Windows на Rust  \u{00B7}  установлено {n_inst} из {N_READY}", "A Windows shell in Rust  \u{00B7}  {n_inst} of {N_READY} installed"),
        [tx, 41.0, lay.lang_box[0] - 12.0, 62.0],
        12.5,
        400,
        DIM,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
    );
    button(&ui, lay.lang_box, if en!() { "Русский" } else { "English" }, 1, hot(ID_LANG));

    // Section heading above the responsive card grid
    ui.text(t!("КОМПОНЕНТЫ", "COMPONENTS"), [M + 2.0, HEADER_H, w * 0.5, LIST_TOP - 2.0], 11.0, 700, DIM, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    ui.text(
        &tf!("Выбрано: {n_sel} из {N_READY}", "Selected: {n_sel} of {N_READY}"),
        [w * 0.5, HEADER_H, w - M - 2.0, LIST_TOP - 2.0],
        11.5,
        400,
        DIM,
        DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
    );

    // 2) Responsive Component Grid (clipped strictly to lay.list_box so scrolled cards never bleed)
    let well_bg = lerp(BG, PANEL, 0.35);
    ui.round(lay.list_box, 12.0, well_bg, BORDER);

    let saved = SaveDC(mem);
    IntersectClipRect(
        mem,
        ui.px(lay.list_box[0] + 1.0),
        ui.px(lay.list_box[1] + 1.0),
        ui.px(lay.list_box[2] - 1.0),
        ui.px(lay.list_box[3] - 1.0),
    );

    let pos = chosen().position(|i| i == job);
    for (k, i) in ready().enumerate() {
        let Some(&card) = lay.cards.get(k) else { break };
        if card[3] < lay.list_box[1] - 4.0 || card[1] > lay.list_box[3] + 4.0 {
            continue;
        }
        let p = &PLUGINS[i];
        let installed = INSTALLED[i].load(Relaxed);
        let working = busy && (job == i || (job == n && installed));
        let queued = busy && !installed && SELECTED[i].load(Relaxed) && (job == STARTING || ready().position(|j| j == job).is_some_and(|q| k > q));
        let sel = SELECTED[i].load(Relaxed);
        let card_hot = hot(ID_CHECK + i) && !busy;

        let c_fill = if working {
            lerp(PANEL, ACCENT, 0.14)
        } else if card_hot {
            lerp(PANEL, TEXT, 0.04)
        } else {
            PANEL
        };
        let c_line = if working {
            ACCENT
        } else if sel {
            lerp(BORDER, ACCENT, 0.45)
        } else {
            BORDER
        };
        ui.round(card, 10.0, c_fill, c_line);

        let (cb, _) = layout::card_check(card);
        ui.round(cb, 5.0, if sel { ACCENT } else { BG }, if sel { ACCENT } else { lerp(BORDER, TEXT, if card_hot { 0.45 } else { 0.0 }) });
        if sel {
            ui.text_in("\u{E73E}", cb, 11.0, 700, rgb(255, 255, 255), DT_CENTER | DT_VCENTER | DT_SINGLELINE, winrust_common::style::icon_face());
        }

        let mid = (card[1] + card[3]) / 2.0;
        let tile = [card[0] + 40.0, mid - 17.0, card[0] + 74.0, mid + 17.0];
        let lit = installed || working;
        let tint = if lit { lerp(PANEL, ACCENT, 0.22) } else { lerp(PANEL, TEXT, 0.06) };
        ui.round(tile, 8.0, tint, tint);
        ui.text_in(p.glyph, tile, 15.0, 400, if lit { ACCENT } else { TEXT }, DT_CENTER | DT_VCENTER | DT_SINGLELINE, winrust_common::style::icon_face());

        let (pill, pc) = if working {
            (if job == n { t!("Удаление…", "Removing…") } else { t!("Установка…", "Installing…") }, ACCENT)
        } else if installed {
            (t!("Установлен", "Installed"), GOOD)
        } else if queued {
            (t!("В очереди", "Queued"), DIM)
        } else {
            (t!("Не установлен", "Not installed"), DIM)
        };
        let gear_space = if p.settings { 38.0 } else { 0.0 };
        let pr = card[2] - 10.0 - gear_space;
        let pw = (ui.width(pill, 11.0, 600) + 14.0).min(110.0);
        let pb = [pr - pw, card[1] + 10.0, pr, card[1] + 29.0];
        let ptint = lerp(PANEL, pc, 0.16);
        ui.round(pb, 9.0, ptint, ptint);
        ui.text(pill, pb, 11.0, 600, pc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

        let nx = tile[2] + 12.0;
        ui.text(p.name, [nx, card[1] + 9.0, pb[0] - 8.0, card[1] + 30.0], 14.0, 600, TEXT, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        let desc = p.desc.get();
        let short = desc.split(". ").next().unwrap_or(desc).trim_end_matches('.');
        ui.text(short, [nx, card[1] + 31.0, pr, card[3] - 8.0], 11.5, 400, DIM, DT_LEFT | DT_WORDBREAK | DT_END_ELLIPSIS);

        if p.settings {
            let gb = layout::card_gear(card);
            let gh = hot(ID_GEAR) && !busy;
            ui.round(gb, 7.0, if gh { lerp(PANEL, TEXT, 0.10) } else { BG }, BORDER);
            ui.text_in("\u{E713}", gb, 13.5, 400, if busy { DIM } else { TEXT }, DT_CENTER | DT_VCENTER | DT_SINGLELINE, winrust_common::style::icon_face());
        }
    }

    if let Some(sb) = lay.scrollbar {
        ui.round(sb, 2.5, lerp(BORDER, TEXT, 0.22), lerp(BORDER, TEXT, 0.22));
    }
    RestoreDC(mem, saved);

    // 3) Pinned Bottom Footer: Action Bar (Install + Undo + Restore Defaults + Remove All) + Status Strip
    ui.shape([M, lay.footer_top, w - M, lay.footer_top + 1.0 / s], 0.0, argb(BORDER, 255));

    let mb = lay.main_box;
    if busy {
        let base = lerp(PANEL, ACCENT, 0.22);
        ui.round(mb, 9.0, base, base);
        let done = if job == n || job == JOB_RESTORE {
            0.0
        } else {
            pos.map_or(0.0, |q| (q as f32 + 0.5) / n_sel.max(1) as f32)
        };
        if done > 0.0 {
            ui.shape([mb[0], mb[1], mb[0] + (mb[2] - mb[0]) * done, mb[3]], 9.0, argb(ACCENT, 255));
        }
        let label = if job == n {
            t!("Удаление WinRust…", "Removing WinRust…").to_string()
        } else if job == JOB_RESTORE {
            t!("Восстановление и проверка Windows…", "Restoring & verifying Windows…").to_string()
        } else {
            pos.map_or(t!("Подготовка…", "Preparing…").into(), |q| tf!("Установка {} ({}/{})", "Installing {} ({}/{})", PLUGINS[job].name, q + 1, n_sel))
        };
        ui.text(&label, mb, 14.0, 700, TEXT, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    } else {
        let all = n_sel_inst == n_sel && n_sel > 0;
        let label = if n_sel == 0 {
            t!("Выберите компоненты", "Select components").to_string()
        } else if n_sel < N_READY {
            if all { tf!("Обновить выбранное ({n_sel})", "Update selected ({n_sel})") } else { tf!("Установить выбранное ({n_sel})", "Install selected ({n_sel})") }
        } else if all {
            t!("Обновить WinRust", "Update WinRust").to_string()
        } else if n_inst == 0 {
            t!("Установить WinRust", "Install WinRust").into()
        } else {
            tf!("Установить и обновить ({})", "Install & update ({} new)", N_READY - n_inst)
        };
        let (fill, fg) = if all || n_sel == 0 { (PANEL, if n_sel == 0 { DIM } else { TEXT }) } else { (ACCENT, rgb(255, 255, 255)) };
        let fill = if hot(ID_MAIN) && n_sel > 0 { lerp(fill, TEXT, if all { 0.08 } else { 0.14 }) } else { fill };
        ui.round(mb, 9.0, fill, if all || n_sel == 0 { BORDER } else { fill });
        let tw = ui.width(&label, 14.0, 700).min(mb[2] - mb[0] - 44.0);
        let cx = (mb[0] + mb[2]) / 2.0 + 10.0;
        ui.text_in(if all { "\u{E72C}" } else { "\u{E896}" }, [cx - tw / 2.0 - 26.0, mb[1], cx - tw / 2.0 - 4.0, mb[3]], 14.0, 400, fg, DT_CENTER | DT_VCENTER | DT_SINGLELINE, winrust_common::style::icon_face());
        ui.text(&label, [cx - tw / 2.0, mb[1], (cx + tw / 2.0 + 4.0).min(mb[2] - 8.0), mb[3]], 14.0, 700, fg, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    }

    button(&ui, lay.undo_box, t!("Отменить", "Undo"), if can_undo { 1 } else { 3 }, hot(ID_UNDO));
    button(&ui, lay.restore_box, t!("Восстановить", "Restore Defaults"), if !busy { 4 } else { 3 }, hot(ID_RESTORE));
    button(&ui, lay.remove_box, t!("Удалить всё", "Remove All"), if n_inst > 0 && !busy { 2 } else { 3 }, hot(ID_ROLLBACK));

    // Status strip
    let (m, ok) = MSG.lock().unwrap().clone();
    let (m, dot, fg) = if busy {
        (t!("Выполняется, не закрывайте окно…", "Working, please wait…").to_string(), ACCENT, DIM)
    } else if m.is_empty() {
        (t!("Перед изменениями создаётся точка восстановления · Экстренное восстановление: winrust-recovery.exe", "A recovery point is saved automatically · Emergency recovery: winrust-recovery.exe").into(), DIM, DIM)
    } else if ok {
        (m, GOOD, TEXT)
    } else {
        (m, BAD, BAD)
    };
    ui.shape(lay.status_dot, 4.0, argb(dot, 255));
    ui.text(&m, lay.status_box, 12.0, 400, fg, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);

    // 4) Modal Confirmation Dialog (dynamically centered inside current window size)
    if let Some((title, text, ok_label, danger)) = dialog {
        ui.shape([0.0, 0.0, w, h + 1.0], 0.0, argb(0, 170));
        let (d, cancel, okb) = layout::dialog_boxes(w, h);
        ui.round(d, 14.0, lerp(PANEL, BG, 0.25), BORDER);
        let accent = if danger { BAD } else { ACCENT };
        let icon = [d[0] + 22.0, d[1] + 20.0, d[0] + 56.0, d[1] + 54.0];
        let tint = lerp(PANEL, accent, 0.18);
        ui.round(icon, 17.0, tint, tint);
        ui.text_in(if danger { "\u{E7BA}" } else { "\u{E946}" }, icon, 15.0, 400, accent, DT_CENTER | DT_VCENTER | DT_SINGLELINE, winrust_common::style::icon_face());
        ui.text(&title, [icon[2] + 12.0, d[1] + 20.0, d[2] - 22.0, d[1] + 54.0], 16.5, 600, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
        ui.text(&text, [d[0] + 22.0, d[1] + 68.0, d[2] - 22.0, cancel[1] - 12.0], 12.5, 400, DIM, DT_LEFT | DT_WORDBREAK | DT_END_ELLIPSIS);
        button(&ui, cancel, t!("Отмена", "Cancel"), 1, hot(ID_CANCEL));
        let okfill = if hot(ID_OK) { lerp(accent, TEXT, 0.14) } else { accent };
        ui.round(okb, 8.0, okfill, okfill);
        ui.text(&ok_label, okb, 13.0, 600, rgb(255, 255, 255), DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }

    // 5) Plugin Settings Modal (topbar options)
    if let Some(i) = SETTINGS.get() {
        ui.shape([0.0, 0.0, w, h + 1.0], 0.0, argb(0, 170));
        let (d, _, closeb) = layout::dialog_boxes(w, h);
        ui.round(d, 14.0, lerp(PANEL, BG, 0.25), BORDER);
        let icon = [d[0] + 22.0, d[1] + 20.0, d[0] + 56.0, d[1] + 54.0];
        let tint = lerp(PANEL, ACCENT, 0.18);
        ui.round(icon, 17.0, tint, tint);
        ui.text_in("\u{E713}", icon, 15.0, 400, ACCENT, DT_CENTER | DT_VCENTER | DT_SINGLELINE, winrust_common::style::icon_face());
        ui.text(&tf!("Настройки {}", "{} settings", PLUGINS[i].name), [icon[2] + 12.0, d[1] + 20.0, d[2] - 22.0, d[1] + 54.0], 16.5, 600, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
        let (row1, sw1, row2, sw2) = layout::settings_switches(w, h);
        let off = lerp(PANEL, TEXT, 0.25);

        let hid = od_hidden();
        ui.round(row1, 10.0, PANEL, BORDER);
        ui.text(t!("Отключение OneDrive", "Disable OneDrive"), [row1[0] + 14.0, row1[1] + 8.0, sw1[0] - 10.0, row1[1] + 30.0], 13.5, 600, TEXT, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        ui.text(t!("Значок OneDrive не показывается в панели (синхронизация работает).", "Hides the OneDrive tray icon (sync keeps working)."), [row1[0] + 14.0, row1[1] + 30.0, sw1[0] - 10.0, row1[3] - 6.0], 11.5, 400, DIM, DT_LEFT | DT_WORDBREAK);
        let track1 = if hid { ACCENT } else { off };
        ui.round(sw1, 12.0, track1, track1);
        let kx1 = if hid { sw1[2] - 12.0 } else { sw1[0] + 12.0 };
        ui.round([kx1 - 8.0, sw1[1] + 4.0, kx1 + 8.0, sw1[3] - 4.0], 8.0, TEXT, TEXT);

        let on = explorer_idx().is_some_and(|e| SELECTED[e].load(Relaxed));
        ui.round(row2, 10.0, PANEL, BORDER);
        ui.text(t!("Оболочка: rust-explorer", "Shell: rust-explorer"), [row2[0] + 14.0, row2[1] + 8.0, sw2[0] - 10.0, row2[1] + 30.0], 13.5, 600, TEXT, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        ui.text(t!("Включено: установить rust-explorer для рабочего стола (Проводник Windows остаётся независимым).", "On: install rust-explorer desktop host (Windows File Explorer stays independent)."), [row2[0] + 14.0, row2[1] + 30.0, sw2[0] - 10.0, row2[3] - 6.0], 11.5, 400, DIM, DT_LEFT | DT_WORDBREAK);
        let track2 = if on { ACCENT } else { off };
        ui.round(sw2, 12.0, track2, track2);
        let kx2 = if on { sw2[2] - 12.0 } else { sw2[0] + 12.0 };
        ui.round([kx2 - 8.0, sw2[1] + 4.0, kx2 + 8.0, sw2[3] - 4.0], 8.0, TEXT, TEXT);

        button(&ui, closeb, t!("Закрыть", "Close"), 1, hot(ID_CANCEL));
    }

    BitBlt(hdc, 0, 0, cw, ch, mem, 0, 0, SRCCOPY);
    SelectObject(mem, ob);
    DeleteObject(bmp);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

// ---------- window ----------

/// What a confirmed dialog runs.
#[derive(Clone, Copy)]
pub(crate) enum Act {
    InstallAll,
    RestoreDefaults,
    Rollback,
}
pub(crate) struct Dialog {
    title: String,
    text: String,
    ok: String,
    danger: bool,
    act: Act,
}
thread_local! { pub(crate) static DIALOG: std::cell::RefCell<Option<Dialog>> = const { std::cell::RefCell::new(None) }; }

/// Clickable element under the cursor (ID_*). While a modal is open only its controls are active; while busy none are.
pub(crate) unsafe fn hit(hwnd: HWND, lp: isize) -> Option<usize> {
    let s = dpi_scale(hwnd);
    let (x, y) = (((lp & 0xFFFF) as i16) as f32 / s, (((lp >> 16) & 0xFFFF) as i16) as f32 / s);
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right as f32 / s, rc.bottom as f32 / s);
    let inside = |b: Box4| layout::box_contains(b, x, y);

    if SETTINGS.get().is_some() {
        let (_, _, closeb) = layout::dialog_boxes(w, h);
        let (_, sw1, _, sw2) = layout::settings_switches(w, h);
        return if inside(sw1) {
            Some(ID_SWITCH)
        } else if inside(sw2) {
            Some(ID_SHELL)
        } else if inside(closeb) {
            Some(ID_CANCEL)
        } else {
            None
        };
    }
    if DIALOG.with_borrow(|d| d.is_some()) {
        let (_, cancel, ok) = layout::dialog_boxes(w, h);
        return if inside(ok) { Some(ID_OK) } else if inside(cancel) { Some(ID_CANCEL) } else { None };
    }
    if BUSY.load(Relaxed) {
        return None;
    }

    let lay = layout::compute(w, h, N_READY, SCROLL.get());
    if inside(lay.lang_box) {
        return Some(ID_LANG);
    }
    if inside(lay.main_box) {
        return Some(ID_MAIN);
    }
    if inside(lay.undo_box) && i18n::recovery::history_count() > 0 {
        return Some(ID_UNDO);
    }
    if inside(lay.restore_box) {
        return Some(ID_RESTORE);
    }
    if inside(lay.remove_box) && INSTALLED.iter().any(|f| f.load(Relaxed)) {
        return Some(ID_ROLLBACK);
    }
    if inside(lay.list_box) {
        for (k, i) in ready().enumerate() {
            let Some(&card) = lay.cards.get(k) else { break };
            if PLUGINS[i].settings && inside(layout::card_gear(card)) {
                return Some(ID_GEAR);
            }
            if inside(card) {
                return Some(ID_CHECK + i);
            }
        }
    }
    None
}

/// Runs `job` off the UI thread; its Ok/Err text becomes the status line.
pub(crate) fn start(hwnd: HWND, which: usize, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
    JOB.store(which, Relaxed);
    BUSY.store(true, Relaxed);
    HOVER.set(None);
    unsafe { InvalidateRect(hwnd, null(), 0) };
    let h = hwnd as isize;
    thread::spawn(move || {
        *MSG.lock().unwrap() = match job() {
            Ok(m) => (m, true),
            Err(e) => (tf!("Ошибка: {e}", "Error: {e}"), false),
        };
        refresh();
        BUSY.store(false, Relaxed);
        unsafe { PostMessageW(h as HWND, WM_DONE, 0, 0) };
    });
}

/// One click: every ticked component in order; missing ones installed, installed ones updated from this installer.
pub(crate) fn install_all(h: isize) -> Result<String, String> {
    let all = chosen().all(|i| (PLUGINS[i].installed)());
    let (mut done, mut failed, mut warned) = (Vec::new(), Vec::new(), false);
    for i in chosen() {
        JOB.store(i, Relaxed);
        unsafe { InvalidateRect(h as HWND, null(), 0) };
        if let Err(e) = install_plugin(i) {
            failed.push(format!("{}: {e}", PLUGINS[i].name));
            continue;
        }
        warned |= !WARN.lock().unwrap().is_empty();
        done.push(PLUGINS[i].name);
        refresh();
    }
    if !failed.is_empty() {
        let ok = if done.is_empty() { String::new() } else { tf!(" Готово: {}.", " Done: {}.", done.join(", ")) };
        return Err(format!("{}.{ok}", failed.join("; ")));
    }
    let note = if warned { t!(" Есть предупреждения (см. %ProgramData%\\plugin-launcher\\install.log).", " Warnings logged to %ProgramData%\\plugin-launcher\\install.log.") } else { "" };
    Ok(format!("WinRust {}: {}.{note}", if all { t!("обновлён", "updated") } else { t!("установлен", "installed") }, done.join(", ")))
}

pub(crate) unsafe fn ask(hwnd: HWND, d: Dialog) {
    DIALOG.set(Some(d));
    HOVER.set(None);
    InvalidateRect(hwnd, null(), 0);
}

pub(crate) unsafe fn close_dialog(hwnd: HWND, run: bool) {
    let Some(d) = DIALOG.take() else { return };
    HOVER.set(None);
    InvalidateRect(hwnd, null(), 0);
    if !run {
        return;
    }
    match d.act {
        Act::Rollback => start(hwnd, PLUGINS.len(), rollback),
        Act::RestoreDefaults => start(hwnd, JOB_RESTORE, || {
            init_soft();
            i18n::recovery::restore_windows_defaults()
        }),
        Act::InstallAll => {
            let h = hwnd as isize;
            start(hwnd, STARTING, move || install_all(h))
        }
    }
}

pub(crate) unsafe fn click(hwnd: HWND, id: usize) {
    if SETTINGS.get().is_some() {
        if id == ID_SWITCH {
            if let Err(e) = set_od_hidden(!od_hidden()) {
                *MSG.lock().unwrap() = (tf!("Ошибка: {e}", "Error: {e}"), false);
            }
        } else if id == ID_SHELL {
            if let Some(e) = explorer_idx() {
                SELECTED[e].fetch_xor(true, Relaxed);
            }
        } else if id == ID_CANCEL {
            SETTINGS.set(None);
        }
        HOVER.set(None);
        InvalidateRect(hwnd, null(), 0);
        return;
    }
    if id == ID_OK || id == ID_CANCEL {
        return close_dialog(hwnd, id == ID_OK);
    }
    if BUSY.load(Relaxed) {
        return;
    }
    match id {
        i if i >= ID_CHECK => {
            SELECTED[i - ID_CHECK].fetch_xor(true, Relaxed);
            InvalidateRect(hwnd, null(), 0);
        }
        ID_MAIN if chosen().next().is_none() => {}
        ID_LANG => {
            i18n::set_english(!i18n::english());
            HOVER.set(None);
            InvalidateRect(hwnd, null(), 0);
        }
        ID_GEAR => {
            SETTINGS.set(PLUGINS.iter().position(|p| p.settings));
            HOVER.set(None);
            InvalidateRect(hwnd, null(), 0);
        }
        ID_UNDO => {
            *MSG.lock().unwrap() = match i18n::recovery::undo_last_change() {
                Ok(m) => (m, true),
                Err(e) => (e, false),
            };
            InvalidateRect(hwnd, null(), 0);
        }
        ID_RESTORE => ask(hwnd, Dialog {
            title: t!("Восстановить настройки Windows?", "Restore Windows Defaults?").into(),
            text: t!(
                "Будет создана точка восстановления, затем безопасно остановлены компоненты оболочки WinRust, восстановлены Проводник Windows, панель задач, параметры Winlogon и исходная видимость значков рабочего стола, с проверкой запуска рабочего стола.\n\nЛичные файлы не затрагиваются.",
                "A recovery point will be created first, then WinRust will safely stop its shell components, restore Windows File Explorer, the stock taskbar, Winlogon settings and your original desktop icon visibility, and verify that Explorer and the taskbar are active.\n\nNo personal files are touched."
            ).into(),
            ok: t!("Восстановить", "Restore Defaults").into(),
            danger: false,
            act: Act::RestoreDefaults,
        }),
        ID_ROLLBACK => ask(hwnd, Dialog {
            title: t!("Удалить WinRust полностью?", "Remove WinRust Completely?").into(),
            text: t!(
                "Все компоненты WinRust будут удалены. Вернутся поиск Windows, стандартный диспетчер задач, панель задач, Проводник Windows и исходная видимость значков рабочего стола (с проверкой запуска рабочего стола).\n\nЛичные файлы не затрагиваются.",
                "All WinRust components will be removed. Windows Search, stock Task Manager, the taskbar, Windows File Explorer and your original desktop icon visibility will be restored and verified.\n\nNo personal files are touched."
            ).into(),
            ok: t!("Удалить всё", "Remove All").into(),
            danger: true,
            act: Act::Rollback,
        }),
        ID_MAIN if chosen().all(|i| INSTALLED[i].load(Relaxed)) => ask(hwnd, Dialog {
            title: t!("Обновить WinRust?", "Update WinRust?").into(),
            text: t!("Выбранные компоненты будут переустановлены из этой версии установщика. Настройки сохранятся.", "The selected components will be reinstalled from this version of the installer. Settings are kept.").into(),
            ok: t!("Обновить", "Update").into(),
            danger: false,
            act: Act::InstallAll,
        }),
        ID_MAIN if chosen().count() < N_READY => {
            let mut text = tf!("Будет установлено или обновлено: {}.\n\nСоздаётся точка восстановления; «Восстановить» или «Удалить всё» безопасно вернут Windows.", "To be installed or updated: {}.\n\nA recovery point is created; 'Restore Defaults' or 'Remove All' safely reverts Windows.", chosen().map(|i| PLUGINS[i].name).collect::<Vec<_>>().join(", "));
            if let Some(i) = chosen().find(|&i| PLUGINS[i].name == RUST_EXPLORER && !INSTALLED[i].load(Relaxed)) {
                text.push_str("\n\n");
                text.push_str(PLUGINS[i].warn.get());
            }
            ask(hwnd, Dialog {
                title: t!("Установить выбранное?", "Install Selected?").into(),
                text,
                ok: t!("Установить", "Install").into(),
                danger: false,
                act: Act::InstallAll,
            });
        }
        ID_MAIN => ask(hwnd, Dialog {
            title: t!("Установить WinRust?", "Install WinRust?").into(),
            text: t!(
                "Компоненты Windows будут обновлены:\n\u{2022} rust-search (Alt+Space) вместо Windows Search;\n\u{2022} rtaskmgr вместо Диспетчера задач;\n\u{2022} topbar вместо панели задач;\n\u{2022} rust-explorer для рабочего стола (Проводник Windows остаётся независимым по Win+E).\n\nСоздаётся точка восстановления и автономный инструмент winrust-recovery.exe.",
                "Selected WinRust components will be installed:\n\u{2022} rust-search (Alt+Space) replacing Windows Search;\n\u{2022} rtaskmgr replacing Task Manager;\n\u{2022} topbar replacing the taskbar;\n\u{2022} rust-explorer desktop host (Windows File Explorer stays independent on Win+E).\n\nA recovery point and standalone winrust-recovery.exe are created automatically."
            ).into(),
            ok: t!("Установить", "Install").into(),
            danger: false,
            act: Act::InstallAll,
        }),
        _ => {}
    }
}

pub(crate) unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_GETMINMAXINFO => {
            let s = dpi_scale(hwnd);
            let mut r = RECT { left: 0, top: 0, right: (MIN_W * s).round() as i32, bottom: (MIN_H * s).round() as i32 };
            AdjustWindowRect(&mut r, WS_OVERLAPPEDWINDOW, 0);
            let mmi = &mut *(lp as *mut MINMAXINFO);
            mmi.ptMinTrackSize.x = r.right - r.left;
            mmi.ptMinTrackSize.y = r.bottom - r.top;
            return 0;
        }
        WM_SIZE => {
            let s = dpi_scale(hwnd);
            let (w, h) = (((lp & 0xFFFF) as f32 / s).max(1.0), (((lp >> 16) & 0xFFFF) as f32 / s).max(1.0));
            let lay = layout::compute(w, h, N_READY, SCROLL.get());
            SCROLL.set(lay.scroll);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_MOUSEWHEEL => {
            if SETTINGS.get().is_none() && DIALOG.with_borrow(|d| d.is_none()) {
                let delta = ((wp >> 16) as i16) as f32;
                let s = dpi_scale(hwnd);
                let mut rc: RECT = zeroed();
                GetClientRect(hwnd, &mut rc);
                let lay = layout::compute(rc.right as f32 / s, rc.bottom as f32 / s, N_READY, SCROLL.get() - (delta / 120.0) * 44.0);
                if (lay.scroll - SCROLL.get()).abs() > 0.1 {
                    SCROLL.set(lay.scroll);
                    InvalidateRect(hwnd, null(), 0);
                }
            }
        }
        WM_PAINT => paint(hwnd),
        WM_ERASEBKGND => return 1,
        WM_MOUSEMOVE => {
            let h = hit(hwnd, lp);
            if h != HOVER.get() {
                HOVER.set(h);
                InvalidateRect(hwnd, null(), 0);
            }
            let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut t);
        }
        WM_SETCURSOR if (lp & 0xFFFF) as u32 == HTCLIENT => {
            SetCursor(LoadCursorW(null_mut(), if HOVER.get().is_some() { IDC_HAND } else { IDC_ARROW }));
            return 1;
        }
        WM_MOUSELEAVE => {
            HOVER.set(None);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_LBUTTONUP => {
            if let Some(i) = hit(hwnd, lp) {
                click(hwnd, i);
            }
        }
        WM_KEYDOWN if SETTINGS.get().is_some() && wp as u16 == VK_ESCAPE => {
            SETTINGS.set(None);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_KEYDOWN if DIALOG.with_borrow(|d| d.is_some()) => match wp as u16 {
            VK_ESCAPE => close_dialog(hwnd, false),
            VK_RETURN if DIALOG.with_borrow(|d| d.as_ref().is_some_and(|d| !d.danger)) => close_dialog(hwnd, true),
            _ => {}
        },
        WM_DONE => {
            InvalidateRect(hwnd, null(), 0);
        }
        WM_CLOSE if BUSY.load(Relaxed) => {} // closing mid-install would leave the system half-changed
        WM_DESTROY => PostQuitMessage(0),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}
