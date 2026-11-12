// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reader page and reader menu.
//!
//! The page is the book and almost nothing else: a quiet running head,
//! justified text with real hyphenation, and a footer with the chapter and
//! the position. Touch is split into zones the size of a thumb: the left
//! third goes back, the rest goes forward (most turns are forward, so that
//! target is the biggest), and the top band opens the menu. Swipes do the
//! same. Nothing on the page reacts with a highlight: the new page is the
//! feedback.
//!
//! The menu is an overlay of two bars. Opening it repaints only the bars;
//! the page between them stays on the panel untouched.

use crate::geom::{Insets, Point, Rect, Size, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::icons::Icon;
use crate::input::Direction;
use crate::text::{
    layout_paragraph, Align, FamilyId, Paragraph, ParagraphLayout, ParagraphStyle, PolishSyllables,
    Span, TextEngine, TextStyle,
};
use crate::theme::{Styles, CARD, INK, INK_2, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Refresh, Tree, Widget,
};
use crate::ui::{Ui, UiEvent};
use crate::widgets::{BookmarkRibbon, FineProgressBar, Slider};
use aristotle_i18n::{tf, tr, Msg};
use std::ops::Range;
use std::rc::Rc;

/// Reading settings, as steps into the tables below.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Index into the reader's font families (Literata, Charis, Gentium).
    pub family: usize,
    /// Index into the reader's CJK font families.
    pub cjk_family: usize,
    pub size: usize,
    pub spacing: usize,
    pub margins: usize,
    /// Frontlight, 0 to 1.
    pub light: f32,
    pub night: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            family: 0,
            cjk_family: 0,
            size: 3,
            spacing: 1,
            margins: 1,
            light: 0.45,
            night: false,
        }
    }
}

/// Body sizes in pixels (9 steps from 32 to 64). The default, 44 px, is 10.5 pt.
pub const SIZES: [f32; 9] = [32.0, 36.0, 40.0, 44.0, 48.0, 52.0, 56.0, 60.0, 64.0];
/// Line pitch as a multiple of the size.
pub const SPACING: [f32; 3] = [1.3, 1.5, 1.7];
/// Side margins: 4.7, 7.5 and 10.8 mm.
pub const MARGINS: [i32; 3] = [56, 88, 128];
pub const FAMILY_NAMES: [&str; 3] = ["Literata", "Charis", "Gentium"];

const HEADER: i32 = 96;
const FOOTER: i32 = 130;

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const CONTENTS: ActionId = 2;
    pub const BOOKMARK: ActionId = 3;
    pub const PREV_CHAPTER: ActionId = 4;
    pub const NEXT_CHAPTER: ActionId = 5;
    pub const TYPESET: ActionId = 6;
    pub const SEARCH: ActionId = 7;
    pub const MARKS: ActionId = 8;
    pub const GOTO: ActionId = 9;
    pub const MENU: ActionId = 12;
    pub const LIGHT: ActionId = 11;
    pub const FONT: ActionId = 10;
    /// The device's font row: open the list of Latin fonts.
    pub const FONT_LIST: ActionId = 15;
    /// The device's CJK font row: open the list of Chinese fonts.
    pub const CJK_FONT_LIST: ActionId = 16;
    pub const SIZE_DOWN: ActionId = 20;
    pub const SIZE_UP: ActionId = 21;
    pub const SPACING: ActionId = 30;
    pub const MARGINS: ActionId = 40;
    pub const LIGHT_DOWN: ActionId = 50;
    pub const LIGHT_UP: ActionId = 51;
    pub const LIGHT_SET: ActionId = 52;
    pub const NIGHT: ActionId = 60;
    /// Pull the open book's cloud progress and notes (SYNC-111).
    pub const PULL_SYNC: ActionId = 70;
    /// Push the open book's progress, notes and tombstones (SYNC-111).
    pub const PUSH_SYNC: ActionId = 71;
}

/// Where a tap on the page lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Prev,
    Next,
    Menu,
}

/// The nodes of the reader's progress: the bar and the line of text beside
/// it.
#[derive(Clone, Copy, Debug)]
pub struct ProgressNodes {
    /// The row: the bar and the words beside it.
    pub row: NodeId,
    /// The bar: the pill at the bottom of the page.
    pub bar: NodeId,
    /// The line of text beside it: what is being laid out.
    pub text: NodeId,
}

/// The progress bar at the bottom of the page, and the line of text beside
/// it: what is being laid out and how far it has come.
///
/// The reader has no status bar, so the words stand next to the bar. The
/// text is empty while nothing is laid out, and one line long.
pub fn progress_row(t: &mut Tree, styles: &Styles) -> (NodeId, ProgressNodes) {
    let bar = t.add(
        Node::new(Kind::Custom(Box::new(crate::widgets::ProgressBar::new(0)))).hidden(true),
        [],
    );
    // The words are left-aligned so that they take their own width only:
    // a centred paragraph would take the whole row and push the bar out.
    let mut para = Paragraph::plain("", styles.font_xs(), Align::Left);
    para.style.max_lines = Some(1);
    let text = t.text(para);
    let row = t.add(Node::row(16).cross(Cross::Center), [text, bar]);
    (row, ProgressNodes { row, bar, text })
}

/// The tap zones of the page, as rectangles (for the preview overlay).
///
/// The top and the bottom band open the menu; the band in between turns
/// the page, the left third of it back and the rest forward.
pub fn zones(screen: Rect) -> [(Zone, Rect); 4] {
    let top = screen.h * 12 / 100;
    let bottom = screen.h * 12 / 100;
    let left = screen.w * 30 / 100;
    let middle_y = screen.y + top;
    let middle_h = screen.h - top - bottom;
    [
        (Zone::Menu, Rect::new(screen.x, screen.y, screen.w, top)),
        (Zone::Prev, Rect::new(screen.x, middle_y, left, middle_h)),
        (
            Zone::Next,
            Rect::new(screen.x + left, middle_y, screen.w - left, middle_h),
        ),
        (
            Zone::Menu,
            Rect::new(screen.x, screen.y + top + middle_h, screen.w, bottom),
        ),
    ]
}

pub fn zone_at(screen: Rect, p: Point) -> Zone {
    zones(screen)
        .iter()
        .find(|(_, r)| r.contains(p))
        .map(|(z, _)| *z)
        .unwrap_or(Zone::Next)
}

/// The opening of Bolesław Prus, *Lalka* (1890, public domain), lightly
/// abridged, as sample content. Book text, so it stays Polish in every UI
/// language, like the chapter names below.
const CHAPTER_NUMBER: &str = "Rozdział pierwszy";
/// The sample chapter's short name, for the footer and the menu.
const CHAPTER_SHORT: &str = "Rozdział I";
const CHAPTER_TITLE: &str = "Jak wygląda firma J. Mincel i S. Wokulski przez szkło butelek";
const PARAGRAPHS: [&str; 4] = [
    "W początkach roku 1878, kiedy świat polityczny zajmował się pokojem \
     san-stefańskim, wyborem nowego papieża albo szansami europejskiej wojny, \
     warszawscy kupcy tudzież inteligencja pewnej okolicy Krakowskiego \
     Przedmieścia nie mniej gorąco interesowała się przyszłością \
     galanteryjnego sklepu pod firmą J. Mincel i S. Wokulski.",
    "W renomowanej jadłodajni, gdzie wieczorami schodzą się właściciele składów \
     bielizny i składów wina, fabrykanci powozów i kapeluszy, poważni ojcowie \
     rodzin żyjący z własnych funduszów i posiadacze kamienic bez zajęcia, \
     równie dużo mówiono o uzbrojeniach Anglii, jak i o firmie J. Mincel \
     i S. Wokulski. Zatopieni w kłębach dymu cygar i pochyleni nad butelkami \
     z ciemnego szkła, obywatele dzielnicy owej jedni zakładali się o wygranę \
     lub przegranę Anglii, drudzy o bankructwo Wokulskiego; jedni nazywali \
     geniuszem Bismarcka, drudzy — awanturnikiem Wokulskiego; jedni krytykowali \
     postępowanie prezydenta MacMahona, inni twierdzili, że Wokulski jest \
     zdecydowanym wariatem, jeżeli nie czymś gorszym…",
    "Fabrykant powozów, pan Deklewski, który do majątku i stanowiska doszedł \
     ciągłym pilnowaniem jednego zajęcia, oraz radca sądowy, pan Węgrowicz, \
     który od dwudziestu lat płacił składkę do Towarzystwa Kredytowego, \
     przepowiadali najsmutniejszą przyszłość sklepowi, który — ich zdaniem — \
     zginie przez „romantyzm” swego właściciela.",
    "— Ma on swoje szczęście — mówił pan Deklewski — ale ma też i fantazję, \
     a fantazja w handlu to rzecz zgubna. Kto chce dziś uczciwie zarobić \
     grosz, musi siedzieć w sklepie od rana do nocy, liczyć każdą szpulkę \
     nici i patrzeć subiektom na ręce, a nie jeździć po świecie za sławą \
     i zaszczytami.",
];

const ZH_RUNNING_HEAD: &str = "曹雪芹 · 红楼梦";
const ZH_CHAPTER_TITLE: &str = "第七回 送宫花贾琏戏熙凤";
const ZH_PARAGRAPHS: [&str; 4] = [
    "话说周瑞家的送了刘姥姥去后，便上来回王夫人话，谁知王夫人不在上房。问丫鬟们，方知往薛姨妈那边说话儿去了。周瑞家的听说，便出东角门，过东院，往梨香院来。刚至院门前，只见王夫人的丫鬟金钏儿和那一个才留头的小女孩儿站在台阶儿上玩呢。看见周瑞家的进来，便知有话来回，因往里努嘴儿。",
    "周瑞家的轻轻掀帘进去，见王夫人正和薛姨妈长篇大套的说些家务人情话。周瑞家的不敢惊动，遂进里间来。只见薛宝钗家常打扮，头上只挽着一个髻儿，坐在炕里边，伏在几上，和丫鬟莺儿正在那里描花样子呢。",
    "见他进来，便放下笔，转过身，满面堆笑，让：“周姐姐坐。”周瑞家的也忙陪笑问道：“姑娘好？”一面炕沿边坐了，因说：“这两三天也没见姑娘到那边逛逛去，只怕是你宝兄弟冲撞了你不成？”",
    "宝钗笑道：“那里的话？只因我那宗病又发了，所以且静养两天。”周瑞家的道：“正是呢，姑娘到底有什么病谤儿？也该趁早请个大夫认真医治医治。小小的年纪儿倒作下个病谤儿，也不是玩的呢。”",
];

/// A piece of the chapter: one paragraph and the space above it.
struct Block {
    layout: ParagraphLayout,
    space_before: f32,
    /// Never end a page with this block (headings).
    keep_with_next: bool,
}

/// Lines of one block placed on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Slice {
    pub block: usize,
    pub lines: Range<usize>,
    pub y: f32,
}

/// Fills pages with lines, keeping headings with what follows and never
/// leaving a paragraph's first line alone at the bottom of a page (an
/// orphan) or its last line alone at the top of the next (a widow).
fn paginate(blocks: &[Block], height: f32) -> Vec<Vec<Slice>> {
    let mut pages: Vec<Vec<Slice>> = vec![Vec::new()];
    let mut y = 0.0f32;
    for (bi, b) in blocks.iter().enumerate() {
        let n = b.layout.lines.len();
        let pitch = b.layout.pitch;
        if y > 0.0 || bi == 0 {
            y += b.space_before;
        }
        if b.keep_with_next {
            let next_pitch = blocks.get(bi + 1).map_or(0.0, |n| n.layout.pitch);
            let need = b.layout.height
                + blocks.get(bi + 1).map_or(0.0, |n| n.space_before)
                + 2.0 * next_pitch;
            if y > 0.0 && y + need > height && bi > 0 {
                pages.push(Vec::new());
                y = 0.0;
            }
        }
        let mut line = 0;
        while line < n {
            let room = ((height - y) / pitch).floor().max(0.0) as usize;
            let left = n - line;
            let mut take = room.min(left);
            if take < left {
                if line == 0 && take == 1 && left > 1 {
                    take = 0; // orphan
                } else if left - take == 1 && take >= 2 {
                    take -= 1; // widow
                }
            }
            if take > 0 {
                pages.last_mut().unwrap().push(Slice {
                    block: bi,
                    lines: line..line + take,
                    y,
                });
                y += take as f32 * pitch;
                line += take;
            }
            if line < n {
                pages.push(Vec::new());
                y = 0.0;
            }
        }
    }
    pages
}

/// One page of book text.
pub struct TextPage {
    blocks: Rc<Vec<Block>>,
    slices: Vec<Slice>,
    margin: i32,
}

impl Widget for TextPage {
    fn measure(&mut self, c: Constraints, _: &mut TextEngine) -> Size {
        c.constrain(c.max)
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        for s in &self.slices {
            let b = &self.blocks[s.block];
            b.layout.draw_lines(
                cx.text,
                cx.painter,
                (r.x + self.margin) as f32,
                r.y as f32 + s.y,
                s.lines.clone(),
            );
        }
    }
}

/// What the reader menu offers. The prototype offers everything; the
/// device reader leaves out what its page renderer cannot do yet.
pub struct MenuSpec<'a> {
    /// Book title, centred in the top bar.
    pub title: &'a str,
    /// Reading faces for the font row, each label set in its own face;
    /// `None` hides the row.
    pub families: Option<[FamilyId; 3]>,
    /// The device's font row instead: the reading font's name, set in its
    /// own face (`FamilyId(settings.family)`; the text comes later through
    /// [`Menu::font_label`]), opening the list of fonts ([`action::FONT_LIST`]).
    pub font_list: bool,
    /// Offer night mode.
    pub night: bool,
    /// The two cloud-sync actions for the open book: pull its cloud progress
    /// and notes, or push what is here (SYNC-111).
    pub sync: bool,
    /// Extra white below the top bar and above the bottom sheet, so their
    /// edges fall into the gap between two lines of the page instead of
    /// cutting a line through its letters ([`mask_extents`]).
    pub top_extra: i32,
    pub bottom_extra: i32,
}

/// Nodes of the reader menu that change while it is open.
pub struct Menu {
    /// The whole overlay; hidden while reading.
    pub root: NodeId,
    /// The top bar and the bottom sheet, the two opaque parts.
    pub top: NodeId,
    pub sheet: NodeId,
    /// The extra white built in, from [`MenuSpec`].
    pub top_extra: i32,
    pub bottom_extra: i32,
    pub progress: NodeId,
    pub page_label: NodeId,
    pub chapter_label: NodeId,
    /// The frontlight node or button.
    pub light: NodeId,
    /// The size [`Steps`].
    pub size_steps: NodeId,
    /// The night toggle or button, if offered.
    pub night_toggle: Option<NodeId>,
    /// The reading font's name in the font row, if offered.
    pub font_label: Option<NodeId>,
    /// The CJK reading font's name in the CJK font row, if offered.
    pub cjk_font_label: Option<NodeId>,
    /// The busy mark at the right of the top bar, hidden: shown while a
    /// change takes longer than about 300 ms (laying out a long chapter).
    pub busy: NodeId,
    pub bookmark: NodeId,
    pub search: NodeId,
    pub prev_chapter: NodeId,
    pub next_chapter: NodeId,
    pub btn_contents: NodeId,
    pub btn_marks: NodeId,
    pub btn_goto: NodeId,
    pub btn_typeset: NodeId,
    pub btn_light: NodeId,
    pub btn_night: NodeId,
    /// Pull / push the open book's cloud state, when the menu offers them.
    pub btn_pull: Option<NodeId>,
    pub btn_push: Option<NodeId>,
    /// One line of feedback under the sync row ("Pushing…", "Push failed:
    /// network unavailable"), hidden while there is nothing to say.
    pub sync_status: Option<NodeId>,
}

/// Builds the reader menu according to Prototype v2 (`ReaderMenu.dc.html`):
/// a top bar (‹ 书库, book title, 搜索, bookmark) and a bottom sheet (position
/// with chapter skips, then six entry buttons: 目录, 标注, 跳转, 排版, 前光, 夜间).
/// No dimming mask over page text.
pub fn build_menu(t: &mut Tree, st: &Styles, spec: &MenuSpec, s: &Settings) -> Menu {
    // 1. Top bar: height 104px, background CARD, bottom border radius 32px, padding 0 88px.
    // Back to library button (action::BACK)
    let back_icon = t.icon(Icon::ChevronLeft, 28, INK);
    let back_label = t.label(tr(Msg::LibraryTitle), st.font_xs().gray(INK));
    let g10 = t.gap(10, 1);
    let back_content = t.add(
        Node::row(0).cross(Cross::Center),
        [back_icon, g10, back_label],
    );
    let back_pad = t.pad(Insets::vh(0, 16), back_content);
    let back_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(64),
        })
        .action(action::BACK)
        .feedback(Feedback::Invert),
        [back_pad],
    );

    // Book title (centered)
    let title_label = t.label(spec.title, st.font_base().bold().gray(INK));
    let title_align = t.align(0.5, 0.5, title_label);
    let title_col = t.add(Node::column(0).flex(1), [title_align]);

    // Search button (action::SEARCH)
    let search_icon = t.icon(Icon::Search, 32, INK);
    let search_label = t.label(tr(Msg::CommonSearch), st.font_xs().gray(INK));
    let g10_s = t.gap(10, 1);
    let search_content = t.add(
        Node::row(0).cross(Cross::Center),
        [search_icon, g10_s, search_label],
    );
    let search_pad = t.pad(Insets::vh(0, 20), search_content);
    let search_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(64),
        })
        .bg(PAPER)
        .border(2, INK, 16)
        .action(action::SEARCH)
        .feedback(Feedback::Invert),
        [search_pad],
    );

    // Bookmark button (action::BOOKMARK)
    let bookmark_ribbon = t.custom(BookmarkRibbon::new(false, 36, 48));
    let bookmark_center = t.align(0.5, 0.5, bookmark_ribbon);
    let bookmark_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(64),
            h: Some(64),
        })
        .action(action::BOOKMARK)
        .feedback(Feedback::Invert),
        [bookmark_center],
    );

    let g16_r = t.gap(16, 1);
    let right_group = t.add(
        Node::row(0).cross(Cross::Center),
        [search_btn, g16_r, bookmark_btn],
    );

    let top_row = t.add(
        Node::row(0).cross(Cross::Center),
        [back_btn, title_col, right_group],
    );
    let top_pad = t.pad(Insets::vh(0, 88), top_row);
    let top = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(104),
        })
        .bg(CARD)
        .bottom_border(0, CARD, 32),
        [top_pad],
    );

    // 2. Bottom panel: background CARD, top border radius 32px, padding Insets::new(24, 88, 26, 88).
    // Row 1: Chapter navigation & progress:
    let prev_icon = t.icon(Icon::ChevronLeft, 24, INK);
    let prev_label = t.label(tr(Msg::ReaderPrevChapter), st.font_xs().gray(INK));
    let g8_p = t.gap(8, 1);
    let prev_content = t.add(
        Node::row(0).cross(Cross::Center),
        [prev_icon, g8_p, prev_label],
    );
    let prev_pad = t.pad(Insets::vh(0, 18), prev_content);
    let prev_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(64),
        })
        .bg(PAPER)
        .border(0, INK, 16)
        .action(action::PREV_CHAPTER)
        .feedback(Feedback::Invert),
        [prev_pad],
    );

    // Center chapter title & progress bar:
    let chapter_label = t.label("", st.font_xs().gray(INK));
    let progress_bar = t.custom(FineProgressBar::new(0.0, PAPER));
    let chapter_col = t.add(
        Node::column(10).cross(Cross::Stretch).flex(1),
        [chapter_label, progress_bar],
    );

    // Page count label
    let page_label = t.label("", st.font_xs().gray(INK));

    // Next chapter button
    let next_label = t.label(tr(Msg::ReaderNextChapter), st.font_xs().gray(INK));
    let next_icon = t.icon(Icon::ChevronRight, 24, INK);
    let g8_n = t.gap(8, 1);
    let next_content = t.add(
        Node::row(0).cross(Cross::Center),
        [next_label, g8_n, next_icon],
    );
    let next_pad = t.pad(Insets::vh(0, 18), next_content);
    let next_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(64),
        })
        .bg(PAPER)
        .border(0, INK, 16)
        .action(action::NEXT_CHAPTER)
        .feedback(Feedback::Invert),
        [next_pad],
    );

    let g16_1 = t.gap(16, 1);
    let g16_2 = t.gap(16, 1);
    let g16_3 = t.gap(16, 1);
    let nav_row = t.add(
        Node::row(0).cross(Cross::Center),
        [
            prev_btn,
            g16_1,
            chapter_col,
            g16_2,
            page_label,
            g16_3,
            next_btn,
        ],
    );
    let nav_row = t.fixed(None, Some(64), Some(nav_row));

    // Row 2: 6 action buttons
    let make_menu_btn =
        |t: &mut Tree, st: &Styles, icon: Icon, label: &str, act: ActionId, active: bool| {
            let (bg, fg, border_w) = if active {
                (INK, PAPER, 0)
            } else {
                (PAPER, INK, 2)
            };
            let ic = t.icon(icon, 32, fg);
            let lb = t.label(label, st.font_xs().gray(fg));
            let g8 = t.gap(8, 1);
            let row = t.add(Node::row(0).cross(Cross::Center), [ic, g8, lb]);
            let center = t.align(0.5, 0.5, row);
            t.add(
                Node::new(Kind::Fixed {
                    w: None,
                    h: Some(88),
                })
                .flex(1)
                .bg(bg)
                .border(border_w, INK, 20)
                .action(act)
                .feedback(Feedback::Invert),
                [center],
            )
        };

    let btn_contents = make_menu_btn(
        t,
        st,
        Icon::Contents,
        tr(Msg::MenuContents),
        action::CONTENTS,
        false,
    );
    let btn_marks = make_menu_btn(t, st, Icon::Marks, tr(Msg::MenuMarks), action::MARKS, false);
    let btn_goto = make_menu_btn(t, st, Icon::Goto, tr(Msg::MenuJump), action::GOTO, false);
    let btn_typeset = make_menu_btn(
        t,
        st,
        Icon::Typeset,
        tr(Msg::MenuTypeset),
        action::TYPESET,
        true,
    );
    let btn_light = make_menu_btn(
        t,
        st,
        Icon::Sun,
        tr(Msg::MenuFrontlight),
        action::LIGHT,
        false,
    );
    let btn_night = make_menu_btn(
        t,
        st,
        Icon::Moon,
        tr(Msg::MenuNight),
        action::NIGHT,
        s.night,
    );

    let g16_b1 = t.gap(16, 1);
    let g16_b2 = t.gap(16, 1);
    let g16_b3 = t.gap(16, 1);
    let g16_b4 = t.gap(16, 1);
    let g16_b5 = t.gap(16, 1);
    let actions_row = t.add(
        Node::row(0).cross(Cross::Center),
        [
            btn_contents,
            g16_b1,
            btn_marks,
            g16_b2,
            btn_goto,
            g16_b3,
            btn_typeset,
            g16_b4,
            btn_light,
            g16_b5,
            btn_night,
        ],
    );
    let actions_row = t.fixed(None, Some(88), Some(actions_row));

    // Row 3: the frontlight's intensity. The slider takes a tap where the
    // reader wants it; the two buttons step it by one of the hardware's
    // steps. The frontlight button above turns it on and off.
    let light_label = t.label(tr(Msg::MenuFrontlight), st.font_xs().gray(INK));
    let slider = t.add(
        Node::new(Kind::Custom(Box::new(Slider::new(s.light))))
            .action(action::LIGHT_SET)
            .feedback(Feedback::None),
        [],
    );
    let slider_col = t.add(Node::column(0).flex(1).cross(Cross::Stretch), [slider]);
    let step_btn = |t: &mut Tree, st: &Styles, label: &str, act: ActionId| {
        let lb = t.label(label, st.font_base().bold().gray(INK));
        let center = t.align(0.5, 0.5, lb);
        t.add(
            Node::new(Kind::Fixed {
                w: Some(64),
                h: Some(64),
            })
            .bg(PAPER)
            .border(2, INK, 16)
            .action(act)
            .feedback(Feedback::Invert),
            [center],
        )
    };
    let light_down = step_btn(t, st, "−", action::LIGHT_DOWN);
    let light_up = step_btn(t, st, "+", action::LIGHT_UP);
    let light_row = t.add(
        Node::row(16).cross(Cross::Center),
        [light_label, slider_col, light_down, light_up],
    );
    let light_row = t.fixed(None, Some(64), Some(light_row));

    // A second, wider row for the two cloud-sync actions of the open book: the
    // six action buttons above already share the row to its limit, and these
    // two carry a label long enough ("Pull progress") to need half the width
    // each. Every target stays 88 px high.
    let (sync_row, sync_status_opt, btn_pull, btn_push) = if spec.sync {
        let wide_btn = |t: &mut Tree, st: &Styles, icon: Icon, label: &str, act: ActionId| {
            let ic = t.icon(icon, 32, INK);
            let lb = t.label(label, st.font_xs().gray(INK));
            let g12 = t.gap(12, 1);
            let row = t.add(Node::row(0).cross(Cross::Center), [ic, g12, lb]);
            let center = t.align(0.5, 0.5, row);
            t.add(
                Node::new(Kind::Fixed {
                    w: None,
                    h: Some(88),
                })
                .flex(1)
                .bg(PAPER)
                .border(2, INK, 20)
                .action(act)
                .feedback(Feedback::Invert),
                [center],
            )
        };
        let pull = wide_btn(
            t,
            st,
            Icon::Download,
            tr(Msg::MenuPullSync),
            action::PULL_SYNC,
        );
        let push = wide_btn(
            t,
            st,
            Icon::Upload,
            tr(Msg::MenuPushSync),
            action::PUSH_SYNC,
        );
        let g16_s = t.gap(16, 1);
        let row = t.add(Node::row(0).cross(Cross::Center), [pull, g16_s, push]);
        let row = t.fixed(None, Some(88), Some(row));
        let mut status = Paragraph::plain("", st.font_xs().gray(INK_2), Align::Left);
        status.style.max_lines = Some(1);
        let status_line = t.text(status);
        t.set_hidden(status_line, true);
        let col = t.add(Node::column(8), [row, status_line]);
        (Some(col), Some(status_line), Some(pull), Some(push))
    } else {
        (None, None, None, None)
    };

    let g20_nav = t.gap(1, 20);
    let g20_light = t.gap(1, 20);
    let mut sheet_children = vec![nav_row, g20_nav, light_row, g20_light, actions_row];
    if let Some(row) = sync_row {
        let g20_sync = t.gap(1, 20);
        sheet_children.push(g20_sync);
        sheet_children.push(row);
    }
    let sheet_col = t.add(Node::column(0).cross(Cross::Stretch), sheet_children);
    let sheet_pad = t.pad(Insets::new(24, 88, 26, 88), sheet_col);
    let sheet = t.add(
        Node::column(0).bg(CARD).top_border(0, CARD, 32),
        [sheet_pad],
    );

    // Assemble root: top bar + empty spacer (no dimming mask) + bottom panel
    let fill = t.spacer(1);
    let root = t.add(Node::column(0).hidden(true), [top, fill, sheet]);

    let busy = t.add(Node::plain().hidden(true), []);
    // The slider of the frontlight row: the app writes the level into it.
    let light = slider;
    let size_steps = t.add(Node::plain().hidden(true), []);
    let night_toggle = Some(btn_night);

    let menu = Menu {
        root,
        top,
        sheet,
        top_extra: spec.top_extra.max(0),
        bottom_extra: spec.bottom_extra.max(0),
        progress: progress_bar,
        page_label,
        chapter_label,
        light,
        size_steps,
        night_toggle,
        font_label: None,
        cjk_font_label: None,
        busy,
        bookmark: bookmark_ribbon,
        search: search_btn,
        prev_chapter: prev_btn,
        next_chapter: next_btn,
        btn_contents,
        btn_marks,
        btn_goto,
        btn_typeset,
        btn_light,
        btn_night,
        btn_pull,
        btn_push,
        sync_status: sync_status_opt,
    };
    menu.update_light(t, s.light);
    menu
}

impl Menu {
    /// Updates frontlight controls without requesting a rendered book page.
    pub fn update_light(&self, t: &mut Tree, value: f32) {
        if let Some(slider) = t.widget_mut::<Slider>(self.light) {
            slider.value = value;
        }
        t.set_border_width(self.btn_light, if value > 0.0 { 0 } else { 2 });
        t.set_inverted(self.btn_light, value > 0.0);
        t.invalidate(self.light, crate::tree::Refresh::Auto);
    }
}

/// How much white the menu's two parts need beyond their nominal edges so
/// that neither edge cuts a line of `page` through its letters.
///
/// `page` is 8-bit gray, `stride` bytes per row, `width` × `height`. The top
/// bar's nominal bottom edge is `top_edge`, the sheet's nominal top edge
/// `sheet_edge`. The edge (and its `rule` px rule) moves outwards, over the
/// page, into the first band of blank rows, four or twice the rule, within
/// `reach` pixels; then the bar hides whole lines only. Returns
/// `(top_extra, bottom_extra)`, 0 when the edge is already in white or no
/// gap is near (a full-page image).
#[allow(clippy::too_many_arguments)]
pub fn mask_extents(
    page: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    top_edge: i32,
    sheet_edge: i32,
    reach: i32,
    rule: i32,
) -> (i32, i32) {
    let band_h = (2 * rule).max(4);
    let blank = |y: i32| -> bool {
        if y < 0 || y as usize >= height {
            return true;
        }
        let row = &page[y as usize * stride..y as usize * stride + width];
        row.iter().all(|&v| v >= 0xd0)
    };
    let band = |y: i32| (y..y + band_h).all(blank);
    // The bar's rule takes the first rows of the band.
    let top = (0..reach)
        .find(|&d| band(top_edge + d))
        .map(|d| d + rule)
        .unwrap_or(0);
    // The sheet's rule takes the rows below its edge, inside the band.
    let bottom = (0..reach)
        .find(|&d| band(sheet_edge - d - band_h))
        .map(|d| d + band_h - rule)
        .unwrap_or(0);
    (top, bottom)
}

/// Handles to the reader's nodes and its pagination.
pub struct Reader {
    pub settings: Settings,
    pub families: [FamilyId; 3],
    pub page: NodeId,
    pub footer_chapter: NodeId,
    pub footer_remaining: NodeId,
    pub footer_bar: NodeId,
    pub footer_pct: NodeId,
    pub footer_page: NodeId,
    pub bookmark_ribbon: NodeId,
    pub bookmarked: bool,
    pub menu: NodeId,
    pub menu_progress: NodeId,
    pub menu_page_label: NodeId,
    pub menu_chapter_label: NodeId,
    pub menu_bookmark: NodeId,
    /// The sheet's frame, for tests that check the sync row stays inside it.
    pub menu_sheet: NodeId,
    /// The cloud-sync actions of the menu, when it offers them.
    pub menu_pull: Option<NodeId>,
    pub menu_push: Option<NodeId>,
    pub menu_sync_status: Option<NodeId>,
    pub light: NodeId,
    pub night_toggle: NodeId,
    pub size_steps: NodeId,
    blocks: Rc<Vec<Block>>,
    pages: Vec<Vec<Slice>>,
    pub current: usize,
    menu_open: bool,
}

/// What the app must do after the reader handled an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    None,
    /// Leave the book for the library.
    Library,
}

/// Fake position of this chapter inside the whole book, for the footer.
const BOOK_PAGE_OFFSET: usize = 11;
const BOOK_PAGES: usize = 684;
const WORDS_PER_MINUTE: f32 = 230.0;

impl Reader {
    fn build_blocks(
        text: &mut TextEngine,
        families: &[FamilyId; 3],
        s: &Settings,
        screen_width: i32,
        density: crate::geom::Density,
    ) -> Vec<Block> {
        let fam = families[s.family.min(2)];
        let size = density.scale(SIZES[s.size.min(SIZES.len() - 1)]);
        let lh = SPACING[s.spacing.min(2)];
        let width = (screen_width - 2 * density.px(MARGINS[s.margins.min(2)])) as f32;

        if aristotle_i18n::language() == "zh" {
            let body = TextStyle::new(fam, size).line_height(lh);
            let hy = PolishSyllables::default();
            let mut blocks = Vec::new();
            for (i, p) in ZH_PARAGRAPHS.iter().enumerate() {
                let para = Paragraph::new(
                    vec![Span::new(*p, body.clone())],
                    ParagraphStyle {
                        align: Align::Justify,
                        indent: size * 2.0,
                        hyphenate: false,
                        ..Default::default()
                    },
                );
                blocks.push(Block {
                    layout: layout_paragraph(text, &para, width, &hy),
                    space_before: if i == 0 { 0.0 } else { density.scale(26.0) },
                    keep_with_next: false,
                });
            }
            return blocks;
        }

        let body = TextStyle::new(fam, size).line_height(lh).oldstyle();
        let pitch = (size * lh).round();
        let hy = PolishSyllables::default();
        let mut blocks = Vec::new();
        let caps = TextStyle::new(fam, size * 0.82)
            .small_caps()
            .tracking(size * 0.06)
            .line_height(lh)
            .gray(INK_2);
        let num = Paragraph::plain(CHAPTER_NUMBER, caps, Align::Center);
        blocks.push(Block {
            layout: layout_paragraph(text, &num, width, &hy),
            // Chapter openings sink: the first page starts lower, as in
            // printed books, which marks the chapter without ornament.
            space_before: pitch * 2.0,
            keep_with_next: true,
        });
        let title_style = TextStyle::new(fam, size * 1.3).italic().line_height(1.3);
        let title = Paragraph::new(
            vec![Span::new(CHAPTER_TITLE, title_style)],
            ParagraphStyle {
                align: Align::Center,
                balance: true,
                ..Default::default()
            },
        );
        blocks.push(Block {
            layout: layout_paragraph(text, &title, width * 0.85, &hy),
            space_before: pitch * 0.5,
            keep_with_next: true,
        });
        for (i, p) in PARAGRAPHS.iter().enumerate() {
            let para = Paragraph::new(
                vec![Span::new(*p, body.clone())],
                ParagraphStyle {
                    align: Align::Justify,
                    // No indent after a heading, the classic convention.
                    indent: if i == 0 { 0.0 } else { size * 1.5 },
                    hyphenate: true,
                    ..Default::default()
                },
            );
            blocks.push(Block {
                layout: layout_paragraph(text, &para, width, &hy),
                space_before: if i == 0 { pitch * 1.5 } else { 0.0 },
                keep_with_next: false,
            });
        }
        // The narrower title is centred within the full measure.
        let shift = width * 0.075;
        for line in &mut blocks[1].layout.lines {
            for piece in &mut line.pieces {
                piece.x += shift;
            }
        }
        blocks
    }

    pub fn build(
        text: &mut TextEngine,
        st: &Styles,
        families: [FamilyId; 3],
        settings: Settings,
    ) -> (Tree, Self) {
        Self::build_for_size(
            text,
            st,
            families,
            settings,
            Size::new(SCREEN_WIDTH, SCREEN_HEIGHT),
        )
    }

    /// Builds the prototype at the supplied dimensions and the style's density.
    pub fn build_for_size(
        text: &mut TextEngine,
        st: &Styles,
        families: [FamilyId; 3],
        settings: Settings,
        size: Size,
    ) -> (Tree, Self) {
        let density = st.density;
        let is_zh = aristotle_i18n::language() == "zh";
        let blocks = Rc::new(Self::build_blocks(
            text, &families, &settings, size.w, density,
        ));
        let body_height = size.h - density.px(HEADER) - density.px(FOOTER);
        let body_h = body_height as f32;
        let pages = paginate(&blocks, body_h);
        let m = MARGINS[settings.margins.min(2)];
        let mut t = Tree::new();

        // 1. Running head and bookmark ribbon
        let head_title = if is_zh {
            ZH_RUNNING_HEAD
        } else {
            "Bolesław Prus · Lalka"
        };
        let head_bg = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(HEADER),
            })
            .action(action::MENU)
            .feedback(Feedback::None),
            [],
        );
        let head_text = t.label(head_title, st.font_xs().gray(INK_2));
        let head_spacer = t.spacer(1);
        let bookmark_ribbon = t.custom(BookmarkRibbon::new(true, 48, 64));
        let bookmark_align = t.align(1.0, 0.0, bookmark_ribbon);
        let bookmark_btn = t.add(
            Node::new(Kind::Fixed {
                w: Some(88),
                h: Some(88),
            })
            .action(action::BOOKMARK)
            .feedback(Feedback::Invert),
            [bookmark_align],
        );
        let head_row = t.add(
            Node::row(0).cross(Cross::Center),
            [head_text, head_spacer, bookmark_btn],
        );
        let head_pad = t.pad(Insets::new(0, 0, 0, m), head_row);
        let head = t.add(Node::new(Kind::Stack), [head_bg, head_pad]);
        let head = t.fixed(None, Some(HEADER), Some(head));

        // 2. Text page
        let page = t.custom(TextPage {
            blocks: blocks.clone(),
            slices: pages[0].clone(),
            margin: density.px(m),
        });
        let page = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(body_height),
            })
            .in_pixels(),
            [page],
        );

        // 3. Footer
        let footer_chapter = t.label("", st.font_xs().gray(INK_2));
        let foot_r1_spacer = t.spacer(1);
        let footer_remaining = t.label("", st.font_xs().gray(INK_2));
        let foot_r1 = t.add(
            Node::row(0).cross(Cross::Center),
            [footer_chapter, foot_r1_spacer, footer_remaining],
        );

        let footer_bar = t.custom(FineProgressBar::new(0.43, CARD));
        let footer_bar_node = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(6),
            })
            .flex(1),
            [footer_bar],
        );
        let g24_1 = t.gap_pixels(24, 1);
        let footer_pct = t.label("43%", st.font_xs().gray(INK));
        let g24_2 = t.gap_pixels(24, 1);
        let footer_page = t.label("128 / 1046", st.font_xs().gray(INK));
        let foot_r2 = t.add(
            Node::row(0).cross(Cross::Center),
            [footer_bar_node, g24_1, footer_pct, g24_2, footer_page],
        );
        let foot_r2 = t.fixed(None, Some(32), Some(foot_r2));

        let g18 = t.gap_pixels(1, 18);
        let foot_col = t.add(
            Node::column(0).cross(Cross::Stretch),
            [foot_r1, g18, foot_r2],
        );
        let foot_pad = t.pad(Insets::new(0, m, 32, m), foot_col);
        let foot = t.fixed(None, Some(FOOTER), Some(foot_pad));

        let reading = t.add(Node::column(0), [head, page, foot]);

        // 4. Menu overlay.
        let spec = MenuSpec {
            title: if is_zh { "红楼梦" } else { "Lalka" },
            families: None,
            font_list: true,
            night: true,
            sync: true,
            top_extra: 0,
            bottom_extra: 0,
        };
        let ids = build_menu(&mut t, st, &spec, &settings);
        if let Some(l) = ids.font_label {
            t.set_text(l, "Literata");
        }
        if let Some(l) = ids.cjk_font_label {
            t.set_text(l, "Noto Sans SC");
        }
        // The prototype opens on a bookmarked page (ReaderMenu.dc.html).
        if let Some(w) = t.widget_mut::<BookmarkRibbon>(ids.bookmark) {
            w.bookmarked = true;
        }
        let menu = ids.root;
        let root = t.add(Node::new(Kind::Stack), [reading, menu]);
        t.set_root(root);

        let mut r = Self {
            settings,
            families,
            page: t.node(page).children[0],
            footer_chapter,
            footer_remaining,
            footer_bar,
            footer_pct,
            footer_page,
            bookmark_ribbon,
            bookmarked: true,
            menu,
            menu_progress: ids.progress,
            menu_page_label: ids.page_label,
            menu_chapter_label: ids.chapter_label,
            menu_bookmark: ids.bookmark,
            menu_sheet: ids.sheet,
            menu_pull: ids.btn_pull,
            menu_push: ids.btn_push,
            menu_sync_status: ids.sync_status,
            light: ids.light,
            night_toggle: ids.night_toggle.expect("the prototype offers night mode"),
            size_steps: ids.size_steps,
            blocks,
            pages,
            current: 0,
            menu_open: false,
        };
        r.update_position(&mut t);
        (t, r)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The line slices of every page.
    pub fn pages(&self) -> &[Vec<Slice>] {
        &self.pages
    }

    /// Number of lines block `i` was set in.
    pub fn block_lines(&self, i: usize) -> usize {
        self.blocks[i].layout.lines.len()
    }

    /// Words from the start of the current page to the end of the chapter.
    fn words_left(&self) -> usize {
        if aristotle_i18n::language() == "zh" {
            let first = self.pages[self.current]
                .first()
                .map(|s| s.block)
                .unwrap_or(0);
            ZH_PARAGRAPHS
                .iter()
                .enumerate()
                .filter(|(i, _)| *i >= first)
                .map(|(_, p)| p.chars().count())
                .sum()
        } else {
            let first = self.pages[self.current]
                .first()
                .map(|s| s.block)
                .unwrap_or(0);
            PARAGRAPHS
                .iter()
                .enumerate()
                .filter(|(i, _)| i + 2 >= first)
                .map(|(_, p)| p.split_whitespace().count())
                .sum()
        }
    }

    fn update_position(&mut self, t: &mut Tree) {
        let is_zh = aristotle_i18n::language() == "zh";
        let minutes = (self.words_left() as f32 / WORDS_PER_MINUTE).ceil() as usize;
        let (chapter, remaining) = if is_zh {
            (
                ZH_CHAPTER_TITLE,
                // The prototype's sample is one page of a longer chapter.
                tf!(Msg::ReaderChapterMinutesLeft, minutes = 12),
            )
        } else {
            (
                CHAPTER_SHORT,
                if self.current + 1 == self.pages.len() {
                    tr(Msg::ReaderLastPage).to_string()
                } else {
                    tf!(Msg::ReaderMinutesLeft, minutes = minutes)
                },
            )
        };
        t.set_text(self.footer_chapter, chapter);
        t.set_text(self.footer_remaining, &remaining);

        let (pct_str, page_str, frac) = if is_zh {
            ("43%".to_string(), "128 / 1046".to_string(), 0.43)
        } else {
            let book_page = BOOK_PAGE_OFFSET + self.current + 1;
            let frac = (self.current + 1) as f32 / self.pages.len() as f32;
            let pct = (frac * 100.0).round() as usize;
            (
                format!("{pct}%"),
                format!("{book_page} / {BOOK_PAGES}"),
                frac,
            )
        };
        t.set_text(self.footer_pct, &pct_str);
        t.set_text(self.footer_page, &page_str);
        t.set_progress(self.footer_bar, frac);

        // Update menu labels
        t.set_text(self.menu_chapter_label, chapter);
        t.set_text(self.menu_page_label, &page_str);
        t.set_progress(self.menu_progress, frac);
    }

    /// Turns the page. Returns false at either end of the chapter.
    pub fn turn(&mut self, t: &mut Tree, forward: bool) -> bool {
        let next = if forward {
            self.current + 1
        } else {
            match self.current.checked_sub(1) {
                Some(p) => p,
                None => return false,
            }
        };
        if next >= self.pages.len() {
            return false;
        }
        self.current = next;
        let slices = self.pages[next].clone();
        if let Some(w) = t.widget_mut::<TextPage>(self.page) {
            w.slices = slices;
        }
        t.invalidate(self.page, Refresh::Page);
        self.update_position(t);
        true
    }

    pub fn menu_open(&self) -> bool {
        self.menu_open
    }

    pub fn set_menu(&mut self, t: &mut Tree, open: bool) {
        self.menu_open = open;
        t.set_hidden(self.menu, !open);
    }

    /// Handles a UI event. Settings that change the text layout rebuild the
    /// screen and swap it in with a diffed refresh.
    pub fn handle(&mut self, ui: &mut Ui, ev: UiEvent) -> Command {
        let screen = ui.bounds();
        match ev {
            UiEvent::Tap(p) if !self.menu_open => match zone_at(screen, p) {
                Zone::Prev => {
                    self.turn(&mut ui.tree, false);
                }
                Zone::Next => {
                    self.turn(&mut ui.tree, true);
                }
                Zone::Menu => self.set_menu(&mut ui.tree, true),
            },
            // A tap on the page between the bars closes the menu.
            UiEvent::Tap(_) => self.set_menu(&mut ui.tree, false),
            UiEvent::Swipe(dir) if !self.menu_open => match dir {
                Direction::Left => {
                    self.turn(&mut ui.tree, true);
                }
                Direction::Right => {
                    self.turn(&mut ui.tree, false);
                }
                Direction::Down => self.set_menu(&mut ui.tree, true),
                Direction::Up => {}
            },
            UiEvent::Action { id, node, at } => return self.action(ui, id, node, at),
            _ => {}
        }
        Command::None
    }

    fn action(&mut self, ui: &mut Ui, id: ActionId, node: NodeId, at: Point) -> Command {
        let mut s = self.settings;
        match id {
            action::BACK => return Command::Library,
            action::MENU => {
                self.set_menu(&mut ui.tree, true);
                return Command::None;
            }
            action::BOOKMARK => {
                self.bookmarked = !self.bookmarked;
                if let Some(w) = ui.tree.widget_mut::<BookmarkRibbon>(self.bookmark_ribbon) {
                    w.bookmarked = self.bookmarked;
                }
                if let Some(w) = ui.tree.widget_mut::<BookmarkRibbon>(self.menu_bookmark) {
                    w.bookmarked = self.bookmarked;
                }
                ui.tree.invalidate(self.bookmark_ribbon, Refresh::Auto);
                ui.tree.invalidate(self.menu_bookmark, Refresh::Auto);
                return Command::None;
            }
            action::PREV_CHAPTER => {
                self.turn(&mut ui.tree, false);
                return Command::None;
            }
            action::NEXT_CHAPTER => {
                self.turn(&mut ui.tree, true);
                return Command::None;
            }
            action::LIGHT => {
                s.light = if s.light > 0.0 { 0.0 } else { 0.45 };
                self.settings = s;
                return Command::None;
            }
            action::NIGHT => {
                s.night = !s.night;
                ui.set_night(s.night);
                self.settings = s;
                return Command::None;
            }
            action::LIGHT_SET | action::LIGHT_DOWN | action::LIGHT_UP => {
                s.light = match id {
                    action::LIGHT_SET => Slider::value_at(ui.tree.rect(node), at.x),
                    action::LIGHT_DOWN => (s.light - 1.0 / 24.0).max(0.0),
                    _ => (s.light + 1.0 / 24.0).min(1.0),
                };
                if let Some(w) = ui.tree.widget_mut::<Slider>(self.light) {
                    w.value = s.light;
                }
                ui.tree.invalidate(self.light, Refresh::Auto);
                self.settings = s;
                return Command::None;
            }
            action::SIZE_DOWN => s.size = s.size.saturating_sub(1),
            action::SIZE_UP => s.size = (s.size + 1).min(SIZES.len() - 1),
            i if (action::FONT..action::FONT + 3).contains(&i) => {
                s.family = (i - action::FONT) as usize
            }
            i if (action::SPACING..action::SPACING + 3).contains(&i) => {
                s.spacing = (i - action::SPACING) as usize
            }
            i if (action::MARGINS..action::MARGINS + 3).contains(&i) => {
                s.margins = (i - action::MARGINS) as usize
            }
            _ => return Command::None,
        }
        if s != self.settings {
            // Keep the reader's place: the page holding the same first
            // paragraph line is close enough for a prototype.
            let styles = Styles::new(self.families[0]).with_density(ui.profile().density);
            let size = ui.bounds().size();
            let (tree, mut fresh) =
                Reader::build_for_size(&mut ui.text, &styles, self.families, s, size);
            fresh.set_menu_quiet(self.menu_open, tree, ui);
            *self = fresh;
        }
        Command::None
    }

    fn set_menu_quiet(&mut self, open: bool, mut tree: Tree, ui: &mut Ui) {
        self.menu_open = open;
        tree.set_hidden(self.menu, !open);
        ui.set_tree_with(tree, Refresh::Auto);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Phase, TouchEvent};
    use crate::text::Fonts;
    use crate::waveform::Waveform;

    fn reader() -> (Ui, Reader) {
        let mut text = TextEngine::new(Fonts::embedded());
        let fam = FamilyId(0);
        let (tree, r) = Reader::build(&mut text, &Styles::new(fam), [fam; 3], Settings::default());
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.render();
        (ui, r)
    }

    #[test]
    fn frontlight_controls_update_in_place_across_zero() {
        let mut tree = Tree::new();
        let menu = build_menu(
            &mut tree,
            &Styles::new(FamilyId(0)),
            &MenuSpec {
                title: "Test",
                families: None,
                font_list: false,
                night: true,
                sync: false,
                top_extra: 0,
                bottom_extra: 0,
            },
            &Settings::default(),
        );
        let nodes = tree.len();
        for value in [1.0, 0.5, 0.0, 0.1, 0.0] {
            menu.update_light(&mut tree, value);
            assert_eq!(tree.len(), nodes);
            assert_eq!(tree.widget::<Slider>(menu.light).unwrap().value, value);
            assert_eq!(tree.node(menu.btn_light).inverted, value > 0.0);
            assert_eq!(
                tree.node(menu.btn_light).border.unwrap().width,
                if value > 0.0 { 0 } else { 2 }
            );
        }
    }

    fn tap(ui: &mut Ui, x: i32, y: i32) -> Vec<UiEvent> {
        let mut ev = ui.handle_touch(TouchEvent {
            phase: Phase::Down,
            id: 0,
            x,
            y,
            time_us: 0,
        });
        ev.extend(ui.handle_touch(TouchEvent {
            phase: Phase::Up,
            id: 0,
            x,
            y,
            time_us: 60_000,
        }));
        ev
    }

    #[test]
    fn menu_edges_move_into_the_gaps_between_lines() {
        // A page of 20 px lines of "ink" with 10 px gaps from y = 100.
        let (w, h) = (50usize, 600usize);
        let mut page = vec![255u8; w * h];
        for line in 0..15 {
            let y0 = 100 + line * 30;
            for y in y0..y0 + 20 {
                page[y * w..y * w + w].fill(0);
            }
        }
        // Nominal edges cut lines: the top bar ends at 110 (inside the line
        // 100..120) and the sheet starts at 345 (inside the line 340..360).
        let (top, bottom) = mask_extents(&page, w, w, h, 110, 345, 60, 2);
        let bar_end = 110 + top;
        let sheet_start = 345 - bottom;
        let blank = |y: usize| page[y * w..y * w + w].iter().all(|&v| v == 255);
        // Every row the masks do not cover, next to their edges, is white,
        // and the 2 px rules sit on white rows.
        assert!(blank(bar_end as usize) && blank(bar_end as usize - 1));
        assert!(blank(bar_end as usize - 2));
        assert!(blank(sheet_start as usize - 1));
        assert!(blank(sheet_start as usize) && blank(sheet_start as usize + 1));
        assert!(top > 0 && top < 40, "{top}");
        assert!(bottom > 0 && bottom < 40, "{bottom}");
        // A full-page picture has no gap: the edges stay.
        let dark = vec![0u8; w * h];
        assert_eq!(mask_extents(&dark, w, w, h, 110, 345, 60, 2), (0, 0));
        // A thicker rule at a higher density still lands on white rows only.
        for rule in [1, 3, 4] {
            let (top, bottom) = mask_extents(&page, w, w, h, 110, 345, 60, rule);
            let bar_end = (110 + top) as usize;
            let sheet_start = (345 - bottom) as usize;
            let rule = rule as usize;
            assert!((bar_end - rule..bar_end + 1).all(blank), "{rule}");
            assert!((sheet_start - 1..sheet_start + rule).all(blank), "{rule}");
        }
    }

    #[test]
    fn zones_cover_the_screen() {
        let s = Rect::screen();
        assert_eq!(zone_at(s, Point { x: 100, y: 800 }), Zone::Prev);
        assert_eq!(zone_at(s, Point { x: 800, y: 800 }), Zone::Next);
        assert_eq!(zone_at(s, Point { x: 500, y: 60 }), Zone::Menu);
        // The bottom band opens the menu as well.
        assert_eq!(
            zone_at(
                s,
                Point {
                    x: 100,
                    y: s.h - 20
                }
            ),
            Zone::Menu
        );
        assert_eq!(
            zone_at(
                s,
                Point {
                    x: 800,
                    y: s.h - 20
                }
            ),
            Zone::Menu
        );
        let area: i64 = zones(s).iter().map(|(_, r)| r.area()).sum();
        assert_eq!(area, s.area());
    }

    /// The bottom band opens the menu; the band right above it still turns
    /// the page, so the paging did not move down.
    #[test]
    fn the_bottom_band_opens_the_menu_and_the_band_above_it_turns_the_page() {
        let s = Rect::screen();
        let z = zones(s);
        let (_, bottom) = z[3];
        let (_, prev) = z[1];
        assert_eq!(zone_at(s, bottom.center()), Zone::Menu);
        assert_eq!(
            zone_at(
                s,
                Point {
                    x: prev.x + 10,
                    y: bottom.y - 4
                }
            ),
            Zone::Prev
        );
        assert_eq!(
            zone_at(
                s,
                Point {
                    x: s.w - 10,
                    y: bottom.y - 4
                }
            ),
            Zone::Next
        );
    }

    #[test]
    fn chapter_paginates_without_widows_or_orphans() {
        let (_, r) = reader();
        assert!(r.page_count() >= 2);
        for page in r.pages() {
            for s in page {
                let n = r.block_lines(s.block);
                if n > 1 {
                    assert!(s.lines.len() > 1, "single line of a paragraph: {s:?}");
                }
            }
        }
    }

    #[test]
    fn page_turn_damages_text_and_footer_only() {
        let (mut ui, mut r) = reader();
        let ev = tap(&mut ui, 900, 900);
        assert_eq!(ev, vec![UiEvent::Tap(Point { x: 900, y: 900 })]);
        r.handle(&mut ui, ev[0]);
        assert_eq!(r.current, 1);
        let d = ui.render();
        assert!(!d.is_empty());
        for dmg in &d {
            assert_eq!(dmg.waveform, Waveform::GL16);
            // The running head is untouched.
            assert!(dmg.rect.y >= HEADER, "{dmg:?}");
        }
    }

    #[test]
    fn menu_opens_from_the_top_band_without_touching_the_page() {
        let (mut ui, mut r) = reader();
        let ev = tap(&mut ui, 500, 50);
        r.handle(&mut ui, ev[0]);
        assert!(r.menu_open());
        ui.render();
        let d = ui.minimal_damage().to_vec();
        let menu_top = 122;
        let sheet_top = d
            .iter()
            .map(|d| d.rect.y)
            .filter(|&y| y >= menu_top)
            .min()
            .unwrap();
        // Two bars, and the page between them is not refreshed.
        assert!(d
            .iter()
            .all(|d| d.rect.bottom() <= menu_top || d.rect.y >= sheet_top));
        assert!(sheet_top > 400, "{d:?}");
        // While the menu is open, page-zone taps close it instead of turning.
        let ev = tap(&mut ui, 900, 300);
        r.handle(&mut ui, ev[0]);
        assert!(!r.menu_open());
        assert_eq!(r.current, 0);
    }

    #[test]
    fn page_turns_and_relayout_keep_the_selected_screen_size() {
        let mut counts = Vec::new();
        for (w, h, dpi) in [
            (800, 1200, 300),
            (1072, 1448, 300),
            (1236, 1648, 300),
            (600, 800, 167),
            (758, 1024, 212),
        ] {
            let profile = crate::geom::Screen::new(w, h, dpi).unwrap();
            let d = profile.density;
            let size = Size::new(w as i32, h as i32);
            let mut text = TextEngine::new(Fonts::embedded());
            let fam = FamilyId(0);
            let (tree, mut r) = Reader::build_for_size(
                &mut text,
                &Styles::new(fam).with_density(d),
                [fam; 3],
                Settings::default(),
                size,
            );
            counts.push(r.page_count());
            let mut ui = Ui::with_screen(tree, text, profile);
            ui.render();
            assert_eq!(ui.tree.rect(r.page).h, size.h - d.px(HEADER) - d.px(FOOTER));
            let p = Point {
                x: size.w * 4 / 5,
                y: size.h / 2,
            };
            for ev in tap(&mut ui, p.x, p.y) {
                r.handle(&mut ui, ev);
            }
            assert_eq!(r.current, 1);
            let damage = ui.render();
            assert!(!damage.is_empty());
            assert!(damage
                .iter()
                .all(|d| d.rect.intersect(&ui.bounds()) == Some(d.rect)));
            r.set_menu(&mut ui.tree, true);
            ui.render();
            let toggle = ui.tree.rect(r.night_toggle);
            assert!(!toggle.is_empty());
            assert_eq!(toggle.intersect(&ui.bounds()), Some(toggle));
            let node = r.size_steps;
            r.handle(
                &mut ui,
                UiEvent::Action {
                    id: action::SIZE_UP,
                    node,
                    at: p,
                },
            );
            ui.render();
            assert_eq!(r.settings.size, Settings::default().size + 1);
            assert!(r.menu_open());
            assert_eq!(ui.tree.rect(r.page).h, size.h - d.px(HEADER) - d.px(FOOTER));
            let width = (size.w - 2 * d.px(MARGINS[r.settings.margins])) as f32;
            let widest = r.blocks.iter().map(|b| b.layout.width).fold(0.0, f32::max);
            assert!(
                widest <= width + 1.0,
                "screen {size:?}, measure {width}, widest {widest}"
            );
        }
        assert!(counts[0] > counts[2], "{counts:?}");
    }

    #[test]
    fn larger_type_means_more_pages() {
        let mut text = TextEngine::new(Fonts::embedded());
        let fam = FamilyId(0);
        let st = Styles::new(fam);
        let small = Settings {
            size: 0,
            ..Default::default()
        };
        let large = Settings {
            size: 6,
            ..Default::default()
        };
        let (_, a) = Reader::build(&mut text, &st, [fam; 3], small);
        let (_, b) = Reader::build(&mut text, &st, [fam; 3], large);
        assert!(b.page_count() > a.page_count());
    }

    #[test]
    fn chinese_font_labels_fit_within_row_label_width() {
        aristotle_i18n::with_language("zh", || {
            let fonts = Fonts::embedded();
            let mut text = TextEngine::new(fonts);
            let st = Styles::new(FamilyId(0));
            let latin_str = aristotle_i18n::tr(Msg::MenuFontLatin);
            let cjk_str = aristotle_i18n::tr(Msg::MenuFontCjk);
            let latin_w = text.measure(latin_str, &st.ui());
            let cjk_w = text.measure(cjk_str, &st.ui());
            eprintln!("latin_w: {latin_w}, cjk_w: {cjk_w}");
            assert_eq!(latin_str, "西文字体");
            assert_eq!(cjk_str, "中文字体");
            assert!(
                latin_w <= 196.0,
                "西文字体 label width {latin_w} must fit within 196"
            );
            assert!(
                cjk_w <= 196.0,
                "中文字体 label width {cjk_w} must fit within 196"
            );
        });
    }

    #[test]
    fn the_menu_offers_the_two_cloud_sync_actions_for_the_open_book() {
        let (mut ui, mut r) = reader();
        r.set_menu(&mut ui.tree, true);
        ui.layout();
        let pull = r.menu_pull.expect("pull button");
        let push = r.menu_push.expect("push button");
        let status = r.menu_sync_status.expect("status line");

        for (id, act) in [(pull, action::PULL_SYNC), (push, action::PUSH_SYNC)] {
            assert_eq!(ui.tree.node(id).action, Some(act));
            let rect = ui.tree.rect(id);
            assert!(rect.h >= 64, "tap target {act} is {} px high", rect.h);
        }
        // Both buttons sit inside the sheet, above its bottom edge.
        let sheet = ui.tree.rect(r.menu_sheet);
        assert!(ui.tree.rect(pull).y >= sheet.y);
        assert!(ui.tree.rect(pull).right() <= sheet.right());

        // The feedback line starts hidden and carries ≥ 32 px text once it has
        // something to say.
        assert!(
            ui.tree.node(status).hidden,
            "no message before the first tap"
        );
        ui.tree.set_text(status, tr(Msg::ReaderSyncPushRunning));
        ui.tree.set_hidden(status, false);
        ui.layout();
        let sz = ui.tree.rect(status);
        assert!(sz.h >= 32, "status line is {} px high", sz.h);
        assert!(sz.w <= sheet.w, "status line is wider than the sheet");
    }

    #[test]
    fn the_sync_actions_stay_out_of_a_menu_that_does_not_offer_them() {
        let fam = FamilyId(0);
        let st = Styles::new(fam);
        let mut t = Tree::new();
        let spec = MenuSpec {
            title: "Lalka",
            families: None,
            font_list: true,
            night: true,
            sync: false,
            top_extra: 0,
            bottom_extra: 0,
        };
        let menu = build_menu(&mut t, &st, &spec, &Settings::default());
        assert!(menu.btn_pull.is_none() && menu.btn_push.is_none());
        assert!(menu.sync_status.is_none());
    }

    #[test]
    fn bookmark_toggles_and_menu_buttons() {
        let (mut ui, mut r) = reader();
        assert!(r.bookmarked);
        // Tap bookmark button (action::BOOKMARK)
        r.handle(
            &mut ui,
            UiEvent::Action {
                id: action::BOOKMARK,
                node: r.bookmark_ribbon,
                at: Point::default(),
            },
        );
        assert!(!r.bookmarked);
        r.handle(
            &mut ui,
            UiEvent::Action {
                id: action::BOOKMARK,
                node: r.bookmark_ribbon,
                at: Point::default(),
            },
        );
        assert!(r.bookmarked);

        // Tap running head / menu action opens menu
        r.handle(
            &mut ui,
            UiEvent::Action {
                id: action::MENU,
                node: r.menu,
                at: Point::default(),
            },
        );
        assert!(r.menu_open());
    }
}
