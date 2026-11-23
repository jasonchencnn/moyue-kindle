// SPDX-License-Identifier: AGPL-3.0-or-later
//! The page where books come in over Wi-Fi.
//!
//! The page says where to go in a browser, how to get there, and what has
//! arrived so far. It follows `docs/ui/prototype-v2/mcode/WifiTransfer.dc.html`
//! and the picture in `docs/ui/prototype-v2/reference/wifitransfer.png`.

use super::library::{status_line, RoundedBar, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER};
use crate::tree::{Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget};
use aristotle_i18n::{tf, tr, Msg};
use zeno::Transform;

pub mod action {
    use crate::tree::ActionId;
    /// The way back to the screen the page was opened from.
    pub const BACK: ActionId = 1;
    /// The button that stops the transfer, or starts it again.
    pub const TOGGLE: ActionId = 3;
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 104;
/// The size of the back button, in reference pixels.
const BACK_H: i32 = 72;
/// The size of the number in front of a step, in reference pixels.
const BADGE: i32 = 56;
/// The height of the three stages, in reference pixels.
const PHASE_H: i32 = 64;
/// The height of the bar that shows how far the file has come.
const BAR_H: i32 = 10;
/// The size of the icon in front of a file that arrived.
const FILE_ICON: i32 = 72;
/// The size of the tick in front of a file that arrived.
const TICK: i32 = 56;
/// The size of the dot that says the device is on Wi-Fi.
const DOT: i32 = 14;
/// How many files the page shows at once.
pub const MAX_RECEIVED: usize = 3;

/// Which of the three stages the transfer is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The page is open, but nothing has come in yet.
    Waiting,
    /// A file is on its way.
    Receiving,
    /// Everything that was sent has arrived.
    Done,
}

/// A file that has arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedBook {
    /// The name it was stored under.
    pub name: String,
    /// The kind of file, e.g. "EPUB".
    pub format: String,
    /// Its size in bytes.
    pub size: u64,
}

/// What the page shows.
pub struct Wifi<'a> {
    pub status: StatusLine<'a>,
    /// Whether the device is on Wi-Fi.
    pub connected: bool,
    /// Whether the firewall lets the port in. False when the rule could not
    /// be put in place, e.g. on a device whose `iptables` was not found.
    pub firewall_open: bool,
    /// The address the browser has to go to.
    pub address: String,
    /// Which stage the transfer is in.
    pub phase: Phase,
    /// The name of the file on its way.
    pub current_file: String,
    /// The share of the file that has arrived, from 0 to 1.
    pub progress: f32,
    /// The line under the bar.
    pub status_line: String,
    /// The files that have arrived, the newest first.
    pub received: Vec<ReceivedBook>,
    /// The total size of everything that has arrived, in bytes.
    pub total_size: u64,
    /// Whether the service is running.
    pub running: bool,
}

/// Handles to the nodes that change while a file is on its way.
pub struct WifiUi {
    /// The name of the file on its way.
    pub file_name: NodeId,
    /// The share of the file that has arrived.
    pub percent: NodeId,
    /// The bar that shows how far the file has come.
    pub progress_bar: NodeId,
    /// The line under the bar.
    pub status_line: NodeId,
    /// The label of the button that stops the transfer.
    pub toggle: NodeId,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
}

impl WifiUi {
    /// Builds the page.
    pub fn build(text: &mut TextEngine, s: &Styles, wifi: &Wifi) -> (Tree, Self) {
        Self::build_for_size(
            text,
            s,
            wifi,
            Size::new(s.density.px(1264), s.density.px(1680)),
        )
    }

    /// Builds the page for a panel of `size` native pixels. Six-inch panels
    /// tighten vertical spacing and cap the received list at two cards so the
    /// supported-formats strip and stop button stay visible without collapsing.
    pub fn build_for_size(
        _text: &mut TextEngine,
        s: &Styles,
        wifi: &Wifi,
        size: Size,
    ) -> (Tree, Self) {
        let compact = crate::theme::PanelLayout::for_size(size, s.density)
            == crate::theme::PanelLayout::Compact;
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &wifi.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

        let header = header(&mut t, s, wifi, compact);
        let card = address_card(&mut t, s, wifi, compact);
        let gap = t.gap(0, if compact { 12 } else { 28 });
        let steps = steps(&mut t, s, compact);
        let gap2 = t.gap(0, if compact { 12 } else { 28 });
        let (state, file_name, percent, progress_bar, status_line) =
            state_card(&mut t, s, wifi, compact);
        let gap3 = t.gap(0, if compact { 12 } else { 28 });
        let received = received(&mut t, s, wifi, compact);
        let gap4 = t.gap(0, if compact { 10 } else { 16 });
        let formats = formats(&mut t, s, compact);
        let gap5 = t.gap(0, if compact { 12 } else { 28 });
        let (stop, toggle) = stop_button(&mut t, s, wifi, compact);

        let col = t.add(
            Node::column(0),
            [
                header, card, gap, steps, gap2, state, gap3, received, gap4, formats, gap5, stop,
            ],
        );
        let col = t.pad(Insets::new(0, home_margin(), 0, home_margin()), col);
        let col = t.add(Node::column(0).flex(1), [col]);
        let root = t.add(Node::column(0), [status, col]);
        t.set_root(root);
        (
            t,
            WifiUi {
                file_name,
                percent,
                progress_bar,
                status_line,
                toggle,
                loading: bar.loading,
            },
        )
    }
}

/// The header: the way back, the name of the page and the state of the
/// connection.
fn header(t: &mut Tree, s: &Styles, wifi: &Wifi, compact: bool) -> NodeId {
    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(BACK_H, BACK_H)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(
        tr(Msg::WifiTitle),
        s.font_lg().bold().line_height(1.2).tracking(4.0),
    );
    let left = t.add(Node::row(20).cross(Cross::Center), [back, title]);

    let dot = t.custom(Dot::new(DOT, INK));
    // Wi-Fi alone does not mean a browser can reach the page: when the
    // firewall rule could not be put in place, say so instead of showing
    // "Connected".
    let reachable = wifi.connected && (!wifi.running || wifi.firewall_open);
    let link = t.label(
        if !wifi.connected {
            tr(Msg::WifiNotConnected)
        } else if !reachable {
            tr(Msg::WifiPortClosed)
        } else {
            tr(Msg::WifiConnected)
        },
        s.font_xs().gray(INK_MID),
    );
    let right = t.add(Node::row(10).cross(Cross::Center), [dot, link]);

    let spacer = t.spacer(1);
    let row = t.add(Node::row(0).cross(Cross::Center), [left, spacer, right]);
    t.fixed(None, Some(if compact { 88 } else { HEADER_H }), Some(row))
}

/// The card with the address the browser has to go to.
fn address_card(t: &mut Tree, s: &Styles, wifi: &Wifi, compact: bool) -> NodeId {
    let hint = t.label(
        if !wifi.connected {
            tr(Msg::WifiOffline)
        } else if wifi.running && !wifi.firewall_open {
            tr(Msg::WifiFirewallHint)
        } else {
            tr(Msg::WifiOpenHint)
        },
        s.font_xs().gray(INK_MID),
    );
    let top = hint;

    // The address is the one line of the card that is centred.
    let address = t.text(Paragraph::plain(
        &wifi.address,
        s.font_md().bold().line_height(1.2).tracking(1.0),
        Align::Center,
    ));
    let gap = t.gap(0, if compact { 8 } else { 14 });
    let inner = t.add(Node::column(0), [top, gap, address]);
    let pad = if compact {
        Insets::new(14, 24, 14, 24)
    } else {
        Insets::new(24, 32, 24, 32)
    };
    t.add(
        Node::new(Kind::Padding(pad))
            .bg(CARD)
            .border(0, CARD, if compact { 24 } else { 32 }),
        [inner],
    )
}

/// The three steps that tell the reader what to do.
fn steps(t: &mut Tree, s: &Styles, compact: bool) -> NodeId {
    let badge_side = if compact { 44 } else { BADGE };
    let mut rows = Vec::new();
    for (i, msg) in [Msg::WifiStep1, Msg::WifiStep2, Msg::WifiStep3]
        .into_iter()
        .enumerate()
    {
        let number = t.label(
            &(i + 1).to_string(),
            s.font_xs().bold().line_height(1.0).gray(PAPER),
        );
        let badge = t.add(
            Node::new(Kind::MinSize(Size::new(badge_side, badge_side)))
                .bg(INK)
                .border(0, INK, badge_side / 2),
            [number],
        );
        let text = t.label(
            tr(msg),
            if compact {
                s.font_xs().line_height(1.3)
            } else {
                s.font_sm().line_height(1.4)
            },
        );
        let row = t.add(
            Node::row(if compact { 14 } else { 20 }).cross(Cross::Center),
            [badge, text],
        );
        rows.push(row);
    }
    t.add(Node::column(if compact { 8 } else { 16 }), rows)
}

/// The card that says what the transfer is doing.
fn state_card(
    t: &mut Tree,
    s: &Styles,
    wifi: &Wifi,
    compact: bool,
) -> (NodeId, NodeId, NodeId, NodeId, NodeId) {
    let phase_h = if compact { 52 } else { PHASE_H };
    let mut chips = Vec::new();
    for (phase, msg) in [
        (Phase::Waiting, Msg::WifiPhaseWaiting),
        (Phase::Receiving, Msg::WifiPhaseReceiving),
        (Phase::Done, Msg::WifiPhaseDone),
    ] {
        // The stage that is happening is drawn the other way round: the
        // whole chip is inverted, so the text turns white on black.
        let active = phase == wifi.phase;
        let style = s.font_xs().line_height(1.0);
        let label = t.label(tr(msg), if active { style.bold() } else { style });
        let aligned = t.add(Node::new(Kind::Align { x: 0.5, y: 0.5 }), [label]);
        let chip = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(phase_h),
            })
            .flex(1)
            .bg(PAPER)
            .border(0, PAPER, 16)
            .inverted(active),
            [aligned],
        );
        chips.push(chip);
    }
    let phases = t.add(Node::row(12), chips);

    let file_name = t.label(&wifi.current_file, s.font_base().bold().line_height(1.3));
    let percent = t.label(
        &percent_text(wifi.progress),
        s.font_base().bold().line_height(1.3),
    );
    let spacer = t.spacer(1);
    let file_row = t.add(
        Node::row(0).cross(Cross::Center),
        [file_name, spacer, percent],
    );

    let progress_bar = t.custom(RoundedBar {
        value: wifi.progress,
        height: BAR_H,
        track_gray: PAPER,
    });
    let status_line = t.label(
        &wifi.status_line,
        s.font_xs().gray(INK_MID).line_height(1.3),
    );

    let gap1 = t.gap(0, if compact { 14 } else { 26 });
    let gap2 = t.gap(0, if compact { 10 } else { 16 });
    let gap3 = t.gap(0, if compact { 10 } else { 16 });
    let inner = t.add(
        Node::column(0),
        [
            phases,
            gap1,
            file_row,
            gap2,
            progress_bar,
            gap3,
            status_line,
        ],
    );
    let pad = if compact {
        Insets::new(16, 24, 16, 24)
    } else {
        Insets::new(24, 32, 24, 32)
    };
    let card = t.add(
        Node::new(Kind::Padding(pad)).bg(CARD).border(0, CARD, 24),
        [inner],
    );
    (card, file_name, percent, progress_bar, status_line)
}

/// The files that have arrived.
fn received(t: &mut Tree, s: &Styles, wifi: &Wifi, compact: bool) -> NodeId {
    let count = wifi.received.len();
    let head = t.label(
        &tf!(Msg::WifiReceivedTitle, count = count),
        s.font_sm().bold().line_height(1.2),
    );
    let total = t.label(
        &tf!(Msg::WifiReceivedTotal, size = size_label(wifi.total_size)),
        s.font_xs().gray(INK_MID).line_height(1.2),
    );
    let spacer = t.spacer(1);
    let head_row = t.add(Node::row(0).cross(Cross::Center), [head, spacer, total]);

    let limit = if compact { 2 } else { MAX_RECEIVED };
    let mut rows = Vec::new();
    for book in wifi.received.iter().take(limit) {
        let icon = t.custom(FileGlyph::new(if compact { 56 } else { FILE_ICON }));
        let name = t.label(&book.name, s.font_sm().bold().line_height(1.2));
        let meta = t.label(
            &format!("{} · {}", book.format, size_label(book.size)),
            s.font_xs().gray(INK_MID).line_height(1.2),
        );
        let texts = t.add(Node::column(4).cross(Cross::Start), [name, meta]);
        let spacer = t.spacer(1);
        let tick = t.custom(Tick::new(if compact { 44 } else { TICK }));
        let row = t.add(
            Node::row(if compact { 14 } else { 20 }).cross(Cross::Center),
            [icon, texts, spacer, tick],
        );
        let pad = if compact {
            Insets::new(10, 20, 10, 20)
        } else {
            Insets::new(18, 24, 18, 24)
        };
        let card = t.add(
            Node::new(Kind::Padding(pad)).bg(CARD).border(0, CARD, 20),
            [row],
        );
        rows.push(card);
    }
    let list = t.add(Node::column(if compact { 8 } else { 14 }), rows);
    let gap = t.gap(0, if compact { 10 } else { 20 });
    t.add(Node::column(0), [head_row, gap, list])
}

/// The formats the device takes.
fn formats(t: &mut Tree, s: &Styles, compact: bool) -> NodeId {
    let label = t.label(tr(Msg::WifiSupportedFormats), s.font_xs().gray(INK_MID));
    let mut pic_para = Paragraph::plain(
        tr(Msg::WifiPictureHint),
        s.font_xs().gray(INK_MID),
        Align::Right,
    );
    if compact {
        pic_para.style.max_lines = Some(1);
    }
    let pictures = t.text(pic_para);
    let pictures = t.add(Node::column(0).flex(1), [pictures]);
    let head = t.add(Node::row(12).cross(Cross::Center), [label, pictures]);
    let mut chips = Vec::new();
    for msg in [
        Msg::WifiFormatEpub,
        Msg::WifiFormatTxt,
        Msg::WifiFormatMobi,
        Msg::WifiFormatPdf,
        Msg::WifiFormatFont,
    ] {
        let text = t.label(tr(msg), s.font_xs());
        let pad = if compact {
            Insets::new(6, 12, 6, 12)
        } else {
            Insets::new(11, 22, 11, 22)
        };
        let chip = t.add(
            Node::new(Kind::Padding(pad)).bg(PAPER).border(2, INK, 16),
            [text],
        );
        chips.push(chip);
    }
    let row = t.add(Node::row(if compact { 8 } else { 14 }), chips);
    let gap = t.gap(0, if compact { 8 } else { 16 });
    t.add(Node::column(0), [head, gap, row])
}

/// The button that stops the transfer, or starts it again.
fn stop_button(t: &mut Tree, s: &Styles, wifi: &Wifi, compact: bool) -> (NodeId, NodeId) {
    let label = t.label(
        if wifi.running {
            tr(Msg::WifiStop)
        } else {
            tr(Msg::WifiRestart)
        },
        s.font_base().line_height(1.0),
    );
    let aligned = t.add(Node::new(Kind::Align { x: 0.5, y: 0.5 }), [label]);
    let button = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(if compact { 80 } else { 96 }),
        })
        .bg(PAPER)
        .border(2, INK, 20)
        .action(action::TOGGLE)
        .feedback(Feedback::Invert),
        [aligned],
    );
    (button, label)
}

/// The share of the file that has arrived, as the page shows it.
pub fn percent_text(fraction: f32) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as i32)
}

/// A size, as the page shows it.
///
/// Everything is in megabytes, the way the page says it; only a file too
/// small for that gets kilobytes, so that it is not shown as nothing.
pub fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if (mb * 10.0).round() / 10.0 >= 0.1 {
        format!("{mb:.1} MB")
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// A small black dot, for the state of the connection.
pub struct Dot {
    size: i32,
    gray: u8,
}

impl Dot {
    /// A dot of the given size.
    pub fn new(size: i32, gray: u8) -> Self {
        Self { size, gray }
    }
}

impl Widget for Dot {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(self.size, self.size))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let side = r.w.min(r.h);
        cx.painter.fill_circle(
            crate::geom::Point {
                x: r.x + side / 2,
                y: r.y + side / 2,
            },
            side as f32 / 2.0,
            self.gray,
        );
    }
}

/// A page with a folded corner, in front of a file that arrived.
pub struct FileGlyph {
    size: i32,
}

impl FileGlyph {
    /// A glyph of the given size.
    pub fn new(size: i32) -> Self {
        Self { size }
    }
}

impl Widget for FileGlyph {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(self.size, self.size))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        // The page sits on a white square, the way the prototype has it.
        cx.painter.fill_round_rect(r, 20.0, PAPER);
        // A 40 px page in the middle of the square, as in the prototype.
        let s = 40.0 / 24.0;
        let x = r.x as f32 + (r.w as f32 - 40.0) / 2.0;
        let y = r.y as f32 + (r.h as f32 - 40.0) / 2.0;
        let t = Transform::scale(s, s).then_translate(x, y);
        cx.painter
            .stroke_path("M6 3H14L19 8V21H6ZM14 3V8H19", 2.2, Some(t), INK);
    }
}

/// A tick in a black circle, in front of a file that arrived.
pub struct Tick {
    size: i32,
}

impl Tick {
    /// A tick of the given size.
    pub fn new(size: i32) -> Self {
        Self { size }
    }
}

impl Widget for Tick {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(self.size, self.size))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let side = r.w.min(r.h);
        let radius = side as f32 / 2.0;
        let center = crate::geom::Point {
            x: r.x + side / 2,
            y: r.y + side / 2,
        };
        cx.painter.fill_circle(center, radius, INK);
        // A 32 px tick in the middle of the circle, as in the prototype.
        let s = 32.0 / 24.0;
        let offset = (side as f32 - 32.0) / 2.0;
        let t = Transform::scale(s, s).then_translate(r.x as f32 + offset, r.y as f32 + offset);
        cx.painter
            .stroke_path("M5 12.5L9.5 17L19 7.5", 3.0, Some(t), PAPER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Fonts, TextEngine};
    use crate::theme::Styles;

    /// A page in the state the picture shows.
    fn sample() -> Wifi<'static> {
        Wifi {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            connected: true,
            firewall_open: true,
            address: "http://192.168.1.23:8080".to_string(),
            phase: Phase::Receiving,
            current_file: "《西游记》.epub".to_string(),
            progress: 0.62,
            status_line: "4.1 MB / 6.6 MB · 还需约 20 秒".to_string(),
            received: vec![
                ReceivedBook {
                    name: "红楼梦.epub".to_string(),
                    format: "EPUB".to_string(),
                    size: 3_251_712,
                },
                ReceivedBook {
                    name: "呐喊.txt".to_string(),
                    format: "TXT".to_string(),
                    size: 419_430,
                },
                ReceivedBook {
                    name: "朝花夕拾.pdf".to_string(),
                    format: "PDF".to_string(),
                    size: 13_004_800,
                },
            ],
            total_size: 16_675_942,
            running: true,
        }
    }

    #[test]
    fn the_page_has_a_way_back() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _view) = WifiUi::build(&mut text, &s, &sample());
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::BACK));
    }

    #[test]
    fn the_page_has_the_button_that_stops_the_transfer() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _view) = WifiUi::build(&mut text, &s, &sample());
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::TOGGLE));
    }

    #[test]
    fn only_the_files_that_fit_are_shown() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let mut wifi = sample();
        wifi.received = (0..10)
            .map(|i| ReceivedBook {
                name: format!("book{i}.epub"),
                format: "EPUB".to_string(),
                size: 1024,
            })
            .collect();
        let (tree, _view) = WifiUi::build(&mut text, &s, &wifi);
        let root = tree.root().unwrap();
        let mut names = Vec::new();
        collect_text(&tree, root, &mut names);
        assert!(names.iter().any(|t| t == "book0.epub"));
        assert!(!names.iter().any(|t| t == "book9.epub"));
    }

    #[test]
    fn a_share_is_shown_as_a_percentage() {
        assert_eq!(percent_text(0.62), "62%");
        assert_eq!(percent_text(0.0), "0%");
        assert_eq!(percent_text(1.0), "100%");
    }

    #[test]
    fn a_size_is_shown_in_megabytes() {
        assert_eq!(size_label(3_251_712), "3.1 MB");
        assert_eq!(size_label(419_430), "0.4 MB");
        assert_eq!(size_label(13_004_800), "12.4 MB");
    }

    #[test]
    fn a_small_file_is_shown_in_kilobytes() {
        assert_eq!(size_label(20 * 1024), "20 KB");
        assert_eq!(size_label(500), "500 B");
    }

    #[test]
    fn a_closed_firewall_is_not_shown_as_connected() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let mut wifi = sample();
        wifi.firewall_open = false;
        let (tree, _view) = WifiUi::build(&mut text, &s, &wifi);
        let root = tree.root().unwrap();
        let mut texts = Vec::new();
        collect_text(&tree, root, &mut texts);
        assert!(texts.iter().any(|t| t == tr(Msg::WifiPortClosed)));
        assert!(texts.iter().any(|t| t == tr(Msg::WifiFirewallHint)));
        assert!(!texts.iter().any(|t| t == tr(Msg::WifiConnected)));
    }

    /// The page takes sleep pictures as well as books; it must say which
    /// formats, the size limit and the folder they land in.
    #[test]
    fn the_page_says_what_sleep_pictures_it_takes() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _view) = WifiUi::build(&mut text, &s, &sample());
        let root = tree.root().unwrap();
        let mut texts = Vec::new();
        collect_text(&tree, root, &mut texts);
        assert!(texts.iter().any(|t| t == tr(Msg::WifiPictureHint)));
        let hint = tr(Msg::WifiPictureHint);
        for part in ["JPG", "PNG", "5 MB", "screensavers"] {
            assert!(hint.contains(part), "{part} is not in {hint:?}");
        }
    }

    /// Whether the subtree holds a node with this action.
    fn has_action(tree: &Tree, id: NodeId, action: crate::tree::ActionId) -> bool {
        if tree.node(id).action == Some(action) {
            return true;
        }
        tree.node(id)
            .children
            .iter()
            .any(|c| has_action(tree, *c, action))
    }

    /// Every text in the subtree.
    fn collect_text(tree: &Tree, id: NodeId, out: &mut Vec<String>) {
        if let crate::tree::Kind::Text { para, .. } = &tree.node(id).kind {
            for span in &para.spans {
                out.push(span.text.clone());
            }
        }
        for c in &tree.node(id).children {
            collect_text(tree, *c, out);
        }
    }
}
