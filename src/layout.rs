//! Pure responsive layout geometry for the main WinRust executable (`WinRust.exe`).
//! Kept Win32-free so it can be thoroughly unit-tested across screen resolutions and window sizes.

pub type Box4 = [f32; 4]; // [left, top, right, bottom] in logical px

// Landscape-oriented default dimensions designed to fit laptops (1366x768, 1080p @ 150% DPI)
// and desktop monitors without hiding footer controls or needing scrolling at default size.
pub const WIN_W: f32 = 920.0;
pub const WIN_H: f32 = 560.0;
pub const MIN_W: f32 = 640.0;
pub const MIN_H: f32 = 430.0;
pub const M: f32 = 22.0; // outer margin
pub const HEADER_H: f32 = 76.0;
pub const SECTION_H: f32 = 24.0;
pub const LIST_TOP: f32 = HEADER_H + SECTION_H; // 100.0
pub const FOOT_H: f32 = 96.0;
pub const CARD_H: f32 = 74.0;
pub const CARD_GAP: f32 = 10.0;
pub const LIST_PAD: f32 = 10.0;
pub const TWO_COL_MIN_W: f32 = 760.0;
pub const BTN_H: f32 = 40.0;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
    pub cols: usize,
    pub lang_box: Box4,
    pub list_box: Box4,
    pub content_h: f32,
    pub scroll: f32,
    pub max_scroll: f32,
    pub scrollbar: Option<Box4>,
    pub cards: Vec<Box4>,
    pub footer_top: f32,
    pub main_box: Box4,
    pub undo_box: Box4,
    pub restore_box: Box4,
    pub remove_box: Box4,
    pub status_dot: Box4,
    pub status_box: Box4,
}

/// Computes a safe initial client size (logical px) clamped to the current monitor work area
/// so the main window never opens taller or wider than the user's visible desktop.
pub fn initial_client_size(work_w: f32, work_h: f32) -> (f32, f32) {
    let max_w = (work_w * 0.90).max(320.0);
    let max_h = (work_h * 0.88).max(320.0);
    let w = WIN_W.min(max_w).max(MIN_W.min(max_w));
    let h = WIN_H.min(max_h).max(MIN_H.min(max_h));
    (w.round(), h.round())
}

/// Computes the complete responsive layout for client size `(w, h)` in logical px, `n_items` component cards,
/// and requested vertical `scroll` offset.
pub fn compute(w: f32, h: f32, n_items: usize, scroll: f32) -> Layout {
    let w = w.max(MIN_W * 0.75);
    let h = h.max(MIN_H * 0.75);
    let cols = if w >= TWO_COL_MIN_W { 2 } else { 1 };
    let rows = n_items.div_ceil(cols).max(1);
    let footer_top = (h - FOOT_H).max(LIST_TOP + 80.0);
    let list_box: Box4 = [M, LIST_TOP, w - M, footer_top - 12.0];
    let viewport_h = (list_box[3] - list_box[1]).max(1.0);
    let content_h = LIST_PAD * 2.0 + rows as f32 * CARD_H + (rows.saturating_sub(1)) as f32 * CARD_GAP;
    let max_scroll = (content_h - viewport_h).max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);

    let scrollbar = if max_scroll > 0.5 {
        let track_t = list_box[1] + 8.0;
        let track_b = list_box[3] - 8.0;
        let track_h = (track_b - track_t).max(16.0);
        let thumb_h = (track_h * (viewport_h / content_h)).clamp(28.0, track_h);
        let frac = if max_scroll > 0.0 { scroll / max_scroll } else { 0.0 };
        let thumb_t = track_t + (track_h - thumb_h) * frac;
        Some([list_box[2] - 8.0, thumb_t, list_box[2] - 3.0, thumb_t + thumb_h])
    } else {
        None
    };

    let sb_pad = if scrollbar.is_some() { 10.0 } else { 0.0 };
    let inner_l = list_box[0] + LIST_PAD;
    let inner_r = list_box[2] - LIST_PAD - sb_pad;
    let col_w = ((inner_r - inner_l) - (cols.saturating_sub(1)) as f32 * CARD_GAP) / cols as f32;

    let mut cards = Vec::with_capacity(n_items);
    for k in 0..n_items {
        let r = k / cols;
        let c = k % cols;
        let l = inner_l + c as f32 * (col_w + CARD_GAP);
        let t = list_box[1] + LIST_PAD + r as f32 * (CARD_H + CARD_GAP) - scroll;
        cards.push([l, t, l + col_w, t + CARD_H]);
    }

    let lang_box: Box4 = [w - M - 92.0, 22.0, w - M, 54.0];

    // Bottom action row: [ Primary Install/Update ... ]  [ Undo ] [ Restore Defaults ] [ Remove All ]
    let compact = w < 760.0;
    let undo_w = if compact { 76.0 } else { 94.0 };
    let restore_w = if compact { 126.0 } else { 150.0 };
    let remove_w = if compact { 106.0 } else { 126.0 };
    let gap = 8.0;

    let act_t = footer_top + 12.0;
    let act_b = act_t + BTN_H;
    let remove_box: Box4 = [w - M - remove_w, act_t, w - M, act_b];
    let restore_box: Box4 = [remove_box[0] - gap - restore_w, act_t, remove_box[0] - gap, act_b];
    let undo_box: Box4 = [restore_box[0] - gap - undo_w, act_t, restore_box[0] - gap, act_b];
    let main_box: Box4 = [M, act_t, (undo_box[0] - 12.0).max(M + 140.0), act_b];

    // Status strip below action row
    let st_t = act_b + 8.0;
    let st_b = h - 8.0;
    let st_mid = (st_t + st_b) / 2.0;
    let status_dot: Box4 = [M, st_mid - 4.0, M + 8.0, st_mid + 4.0];
    let status_box: Box4 = [M + 16.0, st_t, w - M, st_b];

    Layout {
        w,
        h,
        cols,
        lang_box,
        list_box,
        content_h,
        scroll,
        max_scroll,
        scrollbar,
        cards,
        footer_top,
        main_box,
        undo_box,
        restore_box,
        remove_box,
        status_dot,
        status_box,
    }
}

/// Checkbox visual box and generous click hitbox inside a component card.
pub fn card_check(card: Box4) -> (Box4, Box4) {
    let mid = (card[1] + card[3]) / 2.0;
    let vis = [card[0] + 12.0, mid - 9.0, card[0] + 30.0, mid + 9.0];
    let hit = [card[0] + 4.0, card[1] + 6.0, card[0] + 38.0, card[3] - 6.0];
    (vis, hit)
}

/// Settings gear button inside a component card that supports settings (topbar).
pub fn card_gear(card: Box4) -> Box4 {
    let mid = (card[1] + card[3]) / 2.0;
    [card[2] - 10.0 - 30.0, mid - 15.0, card[2] - 10.0, mid + 15.0]
}

/// Centered modal dialog panel and its (cancel, ok) buttons, guaranteed inside `(0..w, 0..h)`.
pub fn dialog_boxes(w: f32, h: f32) -> (Box4, Box4, Box4) {
    let dw = (w - 48.0).clamp(360.0, 560.0);
    let dh = (h - 48.0).clamp(260.0, 368.0);
    let l = ((w - dw) / 2.0).max(12.0);
    let t = ((h - dh) / 2.0).max(12.0);
    let bt = t + dh - 20.0 - 36.0;
    let r = l + dw - 22.0;
    let ok_w = 148.0_f32.min((dw - 60.0) * 0.45);
    let cancel_w = 112.0_f32.min((dw - 60.0) * 0.35);
    (
        [l, t, l + dw, t + dh],
        [r - ok_w - 10.0 - cancel_w, bt, r - ok_w - 10.0, bt + 36.0],
        [r - ok_w, bt, r, bt + 36.0],
    )
}

/// Toggle switch boxes inside the settings modal panel.
pub fn settings_switches(w: f32, h: f32) -> (Box4, Box4, Box4, Box4) {
    let (d, _, _) = dialog_boxes(w, h);
    let row1 = [d[0] + 22.0, d[1] + 74.0, d[2] - 22.0, d[1] + 144.0];
    let row2 = [row1[0], row1[3] + 10.0, row1[2], row1[3] + 80.0];
    let sw1 = [row1[2] - 16.0 - 44.0, row1[1] + 23.0, row1[2] - 16.0, row1[1] + 47.0];
    let sw2 = [row2[2] - 16.0 - 44.0, row2[1] + 23.0, row2[2] - 16.0, row2[1] + 47.0];
    (row1, sw1, row2, sw2)
}

pub fn box_contains(b: Box4, x: f32, y: f32) -> bool {
    x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside_window(b: Box4, w: f32, h: f32) -> bool {
        b[0] >= 0.0 && b[1] >= 0.0 && b[2] <= w && b[3] <= h && b[2] > b[0] && b[3] > b[1]
    }

    fn disjoint_x(a: Box4, b: Box4) -> bool {
        a[2] <= b[0] || b[2] <= a[0]
    }

    #[test]
    fn default_landscape_fits_all_controls_with_zero_scrolling() {
        let l = compute(WIN_W, WIN_H, 7, 0.0);
        assert_eq!(l.cols, 2, "default landscape size must use 2 columns");
        assert_eq!(l.max_scroll, 0.0, "default landscape size must need zero scrolling for 7 plugins");
        assert!(l.scrollbar.is_none());
        for c in &l.cards {
            assert!(c[1] >= l.list_box[1] && c[3] <= l.list_box[3], "card {c:?} overflows list {:?}", l.list_box);
        }
        for b in [l.lang_box, l.main_box, l.undo_box, l.restore_box, l.remove_box, l.status_box] {
            assert!(inside_window(b, WIN_W, WIN_H), "control {b:?} outside default window");
        }
        assert!(disjoint_x(l.main_box, l.undo_box));
        assert!(disjoint_x(l.undo_box, l.restore_box));
        assert!(disjoint_x(l.restore_box, l.remove_box));
    }

    #[test]
    fn multiple_window_sizes_keep_footer_and_dialogs_unclipped() {
        let sizes = [
            (MIN_W, MIN_H),
            (720.0, 480.0),
            (800.0, 520.0),
            (920.0, 560.0),
            (1280.0, 720.0),
            (1920.0, 1080.0),
        ];
        for (w, h) in sizes {
            let l = compute(w, h, 7, 0.0);
            for b in [l.lang_box, l.main_box, l.undo_box, l.restore_box, l.remove_box, l.status_box] {
                assert!(inside_window(b, w, h), "control {b:?} clipped at {w}x{h}");
            }
            assert!(disjoint_x(l.main_box, l.undo_box), "main/undo overlap at {w}x{h}");
            assert!(disjoint_x(l.undo_box, l.restore_box), "undo/restore overlap at {w}x{h}");
            assert!(disjoint_x(l.restore_box, l.remove_box), "restore/remove overlap at {w}x{h}");
            assert!(l.list_box[3] < l.footer_top, "list overlaps footer at {w}x{h}");
            let (d, cancel, ok) = dialog_boxes(w, h);
            assert!(inside_window(d, w, h), "dialog clipped at {w}x{h}");
            assert!(inside_window(cancel, w, h) && inside_window(ok, w, h), "dialog buttons clipped at {w}x{h}");
            assert!(disjoint_x(cancel, ok), "dialog buttons overlap at {w}x{h}");
        }
    }

    #[test]
    fn compact_height_enables_bounded_scrolling_and_single_column() {
        let l = compute(MIN_W, MIN_H, 7, 9999.0);
        assert_eq!(l.cols, 1);
        assert!(l.max_scroll > 0.0);
        assert_eq!(l.scroll, l.max_scroll);
        assert!(l.scrollbar.is_some());
        let last = l.cards.last().unwrap();
        assert!(last[3] <= l.list_box[3] + 0.5, "bottom card must be reachable when scrolled to bottom");
    }

    #[test]
    fn initial_size_clamps_to_small_laptop_or_high_dpi_work_area() {
        // 1920x1080 at 150% DPI -> logical work area ~1280x680
        assert_eq!(initial_client_size(1280.0, 680.0), (WIN_W, WIN_H));
        // 1366x768 at 125% DPI -> logical work area ~1092x582
        let (w, h) = initial_client_size(1092.0, 582.0);
        assert!(w <= 1092.0 * 0.91 && h <= 582.0 * 0.89, "got {w}x{h}");
        // Tiny 800x600 screen -> clamps safely inside screen
        let (w2, h2) = initial_client_size(800.0, 560.0);
        assert!(w2 <= 720.0 && h2 <= 493.0, "got {w2}x{h2}");
    }
}
