// SPDX-License-Identifier: AGPL-3.0-or-later
//! The status bar at the top of the screens: the slogan on the left, the
//! loading area in the middle and the wireless link with the battery on
//! the right.
//!
//! The bar is built once, when the screen is built. What changes while the
//! screen is up is the loading area in the middle: [`Loading`] says what is
//! loading and how far it has come, and [`set_loading`] writes it into the
//! tree. The area is empty while nothing loads, and the slogan and the
//! battery never move: the area stands in the room they leave.

use crate::geom::{Rect, Size};
use crate::icons::Icon;
use crate::text::{Align, Paragraph, TextStyle};
use crate::theme::INK;
use crate::tree::{Constraints, Cross, Kind, Node, NodeId, PaintCx, Refresh, Tree, Widget};
use crate::widgets::{Battery, ProgressBar};

use aristotle_i18n::{tr, Msg};

/// The height of the status bar, in reference pixels: the prototype's
/// 88 px band, with the slogan, the loading area and the battery in it.
pub const STATUS_H: i32 = 88;

/// The gap between the line of text and the bar under it.
const BAR_GAP: i32 = 8;

/// The gap between the time and the slogan on the left.
const LEFT_GAP: i32 = 18;

/// The status line: the clock and the battery.
#[derive(Clone, Copy, Debug, Default)]
pub struct StatusLine<'a> {
    /// Wall-clock time, e.g. "21:38".
    pub clock: &'a str,
    /// Battery charge from 0 to 1, when known.
    pub battery: Option<f32>,
    pub charging: bool,
}

/// What stands on the left of the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Left {
    /// The slogan alone.
    Slogan,
    /// The time and then the slogan: the home screen without its clock card.
    TimeAndSlogan,
}

/// The slogan in brush script, as a coverage mask of [`SLOGAN`] pixels
/// (`assets/slogan.README.md` says where it comes from).
static SLOGAN_MASK: &[u8] = include_bytes!("../assets/slogan.mask");
/// The size of the slogan's picture.
pub const SLOGAN: (i32, i32) = (394, 48);

/// The slogan of the status bar, drawn from its picture.
struct SloganMark;

impl Widget for SloganMark {
    fn measure(&mut self, c: Constraints, _text: &mut crate::text::TextEngine) -> Size {
        c.constrain(Size {
            w: SLOGAN.0,
            h: SLOGAN.1,
        })
    }

    fn paint(&self, rect: Rect, cx: &mut PaintCx) {
        cx.painter.blend_mask(
            rect.x,
            rect.y,
            SLOGAN.0 as u32,
            SLOGAN.1 as u32,
            SLOGAN_MASK,
            INK,
        );
    }
}

/// The left of the status bar: the slogan, and the time before it when the
/// bar shows the time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftText {
    /// The time, when the bar shows one.
    pub time: Option<String>,
    /// The slogan.
    pub slogan: String,
}

/// The left of the status bar: the slogan alone, or the time and then the
/// slogan (the home screen without its clock card).
pub fn left_text(left: Left, clock: &str) -> LeftText {
    let slogan = tr(Msg::StatusSlogan).to_string();
    match left {
        Left::Slogan => LeftText { time: None, slogan },
        Left::TimeAndSlogan => LeftText {
            time: Some(clock.to_string()),
            slogan,
        },
    }
}

/// The share of the work done, as the loading area shows it: a whole
/// percent, rounded and clamped to 0..=100.
pub fn percent(fraction: f32) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
}

/// What the status bar shows in the middle while something loads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loading {
    /// The line of text: "正在排版《红楼梦》".
    pub label: String,
    /// The share of the work done, when the engine reports one.
    pub percent: Option<u8>,
}

impl Loading {
    /// A loading with a share of the work done, as a whole percent.
    pub fn with_percent(label: impl Into<String>, percent: u8) -> Self {
        Self {
            label: label.into(),
            percent: Some(percent.min(100)),
        }
    }

    /// A loading with a share of the work done, as a fraction from 0 to 1.
    pub fn with_fraction(label: impl Into<String>, fraction: f32) -> Self {
        Self::with_percent(label, percent(fraction))
    }

    /// A loading without a share of the work done: the engine does not say
    /// how far it has come, so no bar is drawn.
    pub fn without_percent(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            percent: None,
        }
    }

    /// The line of text the loading area shows: the label, and the share
    /// when there is one.
    pub fn text(&self) -> String {
        match self.percent {
            Some(p) => format!("{} {}%", self.label, p),
            None => self.label.clone(),
        }
    }
}

/// The nodes of the loading area in the middle of the status bar.
#[derive(Clone, Copy, Debug)]
pub struct LoadingNodes {
    /// The area itself: hidden while nothing loads.
    pub area: NodeId,
    /// The line of text.
    pub text: NodeId,
    /// The progress bar: hidden when the engine reports no share.
    pub bar: NodeId,
}

/// The status bar of a screen.
#[derive(Clone, Copy, Debug)]
pub struct StatusBar {
    /// The bar: the band at the top of the screen.
    pub root: NodeId,
    /// The time on the left, when the bar shows one.
    pub time: Option<NodeId>,
    /// The loading area in the middle.
    pub loading: LoadingNodes,
}

/// Builds the status bar: the slogan on the left, the loading area in the
/// middle, the wireless link and the battery on the right.
///
/// `small` is the style of the small text of the bar: the slogan, the time
/// and the charge. The home screen has its own type scale and passes its
/// own; the other screens pass the theme's.
pub fn status_bar(t: &mut Tree, small: &TextStyle, line: &StatusLine<'_>, left: Left) -> StatusBar {
    // The left: the slogan, and the time before it when the bar shows one.
    let left_text = left_text(left, line.clock);
    let mut time = None;
    let mut left_nodes = Vec::new();
    if let Some(clock) = left_text.time {
        let clock = t.label(&clock, small.clone());
        time = Some(clock);
        left_nodes.push(clock);
        left_nodes.push(t.gap(LEFT_GAP, 1));
    }
    let slogan = t.custom(SloganMark);
    left_nodes.push(slogan);
    let left_row = t.add(Node::row(0).cross(Cross::Center), left_nodes);

    // The middle: the loading area, empty while nothing loads.
    let loading = loading_area(t, small);

    // The right: the wireless link, the charge and the battery.
    let mut right_nodes = Vec::new();
    if let Some(level) = line.battery {
        let wifi = t.icon(Icon::Wifi, 32, INK);
        let pct = t.label(&format!("{:.0}%", level * 100.0), small.clone());
        let bat = t.custom(Battery {
            level,
            charging: line.charging,
        });
        let g1 = t.gap(18, 1);
        let g2 = t.gap(18, 1);
        right_nodes.extend([wifi, g1, pct, g2, bat]);
    }
    let right = t.add(Node::row(0).cross(Cross::Center), right_nodes);

    // The loading area stands in the room the slogan and the battery leave,
    // centred in it: showing and hiding it never moves them.
    let middle = t.add(
        Node::new(Kind::Align { x: 0.5, y: 0.5 }).flex(1),
        [loading.area],
    );
    let row = t.add(Node::row(0).cross(Cross::Center), [left_row, middle, right]);
    let root = t.fixed(None, Some(STATUS_H), Some(row));
    StatusBar {
        root,
        time,
        loading,
    }
}

/// Builds the loading area: a line of text and, under it, the progress
/// bar. The area is hidden until something loads.
fn loading_area(t: &mut Tree, small: &TextStyle) -> LoadingNodes {
    // One line only: a long title is shortened with an ellipsis.
    let mut para = Paragraph::plain("", small.clone(), Align::Center);
    para.style.max_lines = Some(1);
    let text = t.text(para);
    let bar = t.custom(ProgressBar::thin(0));
    t.set_hidden(bar, true);
    let area = t.add(
        Node::column(BAR_GAP).cross(Cross::Center).hidden(true),
        [text, bar],
    );
    LoadingNodes { area, text, bar }
}

/// Writes the loading into the status bar: the line of text, the bar and
/// whether the area is drawn at all. `None` empties the area.
///
/// Writing the same loading twice changes nothing, so this can be called
/// before every render. When something changes, the whole area goes out in
/// one update: the words and the bar are one picture.
pub fn set_loading(t: &mut Tree, nodes: &LoadingNodes, loading: Option<&Loading>) {
    // The ids come from the tree the screen was built with. A tree replaced
    // under the screen (the screensaver) leaves them stale: ignore them
    // rather than index out of bounds.
    if !t.contains(nodes.area) || !t.contains(nodes.text) || !t.contains(nodes.bar) {
        return;
    }
    let mut changed = false;

    // The words.
    if let Some(loading) = loading {
        let text = loading.text();
        let current = match &t.node(nodes.text).kind {
            Kind::Text { para, .. } => para.text().to_string(),
            _ => String::new(),
        };
        if current != text {
            t.set_text(nodes.text, &text);
            changed = true;
        }
    }

    // The bar: it stands only when the engine says how far it has come.
    let bar_hidden = t.node(nodes.bar).hidden;
    match loading.and_then(|l| l.percent) {
        Some(percent) => {
            let moved = match t.widget_mut::<ProgressBar>(nodes.bar) {
                Some(bar) if bar.percent != percent => {
                    bar.percent = percent;
                    true
                }
                _ => false,
            };
            if moved {
                changed = true;
            }
            if bar_hidden {
                t.set_hidden(nodes.bar, false);
                changed = true;
            }
        }
        None => {
            if !bar_hidden {
                t.set_hidden(nodes.bar, true);
                changed = true;
            }
        }
    }

    // The area itself: empty while nothing loads.
    let area_hidden = t.node(nodes.area).hidden;
    match (loading.is_some(), area_hidden) {
        (true, true) => {
            t.set_hidden(nodes.area, false);
            changed = true;
        }
        (false, false) => {
            t.set_hidden(nodes.area, true);
            changed = true;
        }
        _ => {}
    }

    if changed {
        t.invalidate(nodes.area, Refresh::Transient);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{FamilyId, FontData, Fonts, TextEngine};
    use crate::theme::Styles;

    /// The fonts of the test: Literata and the Chinese fixture behind it,
    /// so the Chinese words have real glyphs.
    fn fonts() -> Fonts {
        let mut f = Fonts::new();
        let literata = f
            .add_face(FontData::Static(include_bytes!(
                "../assets/fonts/Literata-Regular.ttf"
            )))
            .expect("Literata is valid");
        let cjk = f
            .add_face(FontData::Static(include_bytes!(
                "../assets/test-fonts/CjkFixture-Regular.ttf"
            )))
            .expect("the fixture is valid");
        let latin = f
            .add_family("Literata", [Some(literata), None, None, None])
            .expect("Literata has a regular face");
        let chinese = f
            .add_family("Noto Sans SC", [Some(cjk), None, None, None])
            .expect("the fixture has a regular face");
        f.add_fallback(chinese);
        let _ = latin;
        f
    }

    fn line() -> StatusLine<'static> {
        StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        }
    }

    /// The status bar of a screen, and the tree it is in.
    fn bar(left: Left) -> (Tree, StatusBar) {
        let mut t = Tree::new();
        let styles = Styles::new(FamilyId(0));
        let bar = status_bar(&mut t, &styles.font_xs(), &line(), left);
        (t, bar)
    }

    #[test]
    fn the_left_is_the_slogan_alone_without_a_clock() {
        let left = left_text(Left::Slogan, "21:38");
        assert_eq!(left.time, None);
        assert_eq!(left.slogan, tr(Msg::StatusSlogan));
    }

    #[test]
    fn the_left_keeps_the_time_before_the_slogan() {
        let left = left_text(Left::TimeAndSlogan, "21:38");
        assert_eq!(left.time.as_deref(), Some("21:38"));
        assert_eq!(left.slogan, tr(Msg::StatusSlogan));
    }

    #[test]
    fn the_loading_text_carries_the_share_when_there_is_one() {
        let l = Loading::with_percent("正在排版《红楼梦》", 45);
        assert_eq!(l.text(), "正在排版《红楼梦》 45%");
    }

    #[test]
    fn the_loading_text_is_the_label_alone_without_a_share() {
        let l = Loading::without_percent("正在打开《红楼梦》");
        assert_eq!(l.text(), "正在打开《红楼梦》");
    }

    #[test]
    fn the_share_is_a_whole_percent() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.456), 46);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(1.4), 100);
        assert_eq!(percent(-0.2), 0);
    }

    #[test]
    fn the_share_never_passes_a_hundred() {
        assert_eq!(Loading::with_percent("x", 120).percent, Some(100));
    }

    #[test]
    fn the_loading_area_is_empty_until_something_loads() {
        let (t, bar) = bar(Left::Slogan);
        assert!(t.node(bar.loading.area).hidden);
        assert!(t.node(bar.loading.bar).hidden);
    }

    #[test]
    fn a_loading_shows_the_words_and_the_bar() {
        let (mut t, bar) = bar(Left::Slogan);
        set_loading(
            &mut t,
            &bar.loading,
            Some(&Loading::with_percent("正在排版《红楼梦》", 45)),
        );
        assert!(!t.node(bar.loading.area).hidden);
        assert!(!t.node(bar.loading.bar).hidden);
        let w = t
            .widget::<ProgressBar>(bar.loading.bar)
            .expect("the bar is there");
        assert_eq!(w.percent, 45);
        let Kind::Text { para, .. } = &t.node(bar.loading.text).kind else {
            panic!("the text is a text node");
        };
        assert_eq!(para.text(), "正在排版《红楼梦》 45%");
    }

    #[test]
    fn a_loading_without_a_share_shows_no_bar() {
        let (mut t, bar) = bar(Left::Slogan);
        set_loading(
            &mut t,
            &bar.loading,
            Some(&Loading::without_percent("正在打开《红楼梦》")),
        );
        assert!(!t.node(bar.loading.area).hidden);
        assert!(t.node(bar.loading.bar).hidden);
    }

    #[test]
    fn an_emptied_loading_area_hides_everything() {
        let (mut t, bar) = bar(Left::Slogan);
        set_loading(
            &mut t,
            &bar.loading,
            Some(&Loading::with_percent("正在排版《红楼梦》", 45)),
        );
        set_loading(&mut t, &bar.loading, None);
        assert!(t.node(bar.loading.area).hidden);
    }

    #[test]
    fn loading_nodes_of_a_replaced_tree_are_ignored() {
        let (t, bar) = bar(Left::Slogan);
        // The tree the ids belong to is gone (the screensaver replaced it):
        // writing through them must not index out of bounds.
        let mut other = Tree::new();
        other.gap(1, 1);
        assert!(!other.contains(bar.loading.area));
        set_loading(
            &mut other,
            &bar.loading,
            Some(&Loading::with_percent("正在排版《红楼梦》", 45)),
        );
        assert_eq!(other.len(), 1, "nothing was written into the new tree");
        assert!(t.node(bar.loading.area).hidden, "the old tree is untouched");
    }

    #[test]
    fn the_bar_lays_out_with_its_words() {
        let (mut t, bar) = bar(Left::Slogan);
        let mut text = TextEngine::new(fonts());
        set_loading(
            &mut t,
            &bar.loading,
            Some(&Loading::with_percent("正在排版《红楼梦》", 45)),
        );
        t.set_root(bar.root);
        t.layout(crate::geom::Rect::new(0, 0, 1264, 1680), &mut text);
        let area = t.rect(bar.loading.area);
        assert!(area.w > 0 && area.h > 0, "the area has a size");
        let line = t.rect(bar.loading.text);
        let track = t.rect(bar.loading.bar);
        assert!(line.w > 0, "the words are there");
        assert!(track.w > 0 && track.h > 0, "the bar is there");
        // The words stand above the bar, in the same column.
        assert!(line.bottom() <= track.y, "the words are above the bar");
    }
}
