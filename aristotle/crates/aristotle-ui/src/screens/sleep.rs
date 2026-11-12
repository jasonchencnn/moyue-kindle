// SPDX-License-Identifier: AGPL-3.0-or-later
//! The screensaver: what the screen shows while nobody reads.
//!
//! E-ink keeps its picture without power, so this is the face of the
//! device most of the time. It supports full cover mode, or the
//! hero cover with quotation / highlights of the day.

use super::library::Book;
use crate::geom::{mm, Insets, Rect, Size};
use crate::screens::home::pet::CAT_MASK;
use crate::screens::home::quotes::{quote_for_day, QuoteItem, QuoteMode};
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{Space, Styles, INK, INK_MID, PAPER};
use crate::tree::{Constraints, Cross, Kind, Node, PaintCx, Tree, Widget};
use crate::widgets::{Cover, GrayImage};
use aristotle_i18n::{tr, Msg};
use zeno::Transform;

/// The fishing cat illustration for the screensaver:
/// 287 px wide by 280 px tall (water ripples extend to 322 px on the right).
#[derive(Clone, Copy, Debug, Default)]
pub struct FishingCat;

impl Widget for FishingCat {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(287, 280))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let x = r.x;
        let y = r.y;
        // Fishing line from tip of rod to hook
        p.fill_rect(Rect::new(x + 284, y + 11, 2, 189), INK);
        // Desk cat mask
        p.blend_mask(x, y, 287, 280, CAT_MASK, INK);
        // Empty hook
        p.stroke_path(
            "M 13 8 L 13 19 C 13 26 5 26 5 20 L 2 16",
            2.0,
            Some(Transform::translation(x as f32 + 272.0, y as f32 + 192.0)),
            INK,
        );
        // Water ripples
        p.stroke_path(
            "M 2 10 C 8 5.33 14 5.33 20 10 C 26 14.67 32 14.67 38 10 C 44 5.33 50 5.33 56 10 C 62 14.67 67 14.67 72 10 \
             M 12 32 C 18 27.33 24 27.33 30 32 C 36 36.67 42 36.67 48 32 C 54 27.33 60 27.33 66 32",
            2.0,
            Some(Transform::translation(x as f32 + 248.0, y as f32 + 228.0)),
            INK,
        );
    }
}

/// What kind of quote content the screensaver shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepQuoteKind {
    DailyQuote,
    Highlights,
}

/// Text and byline to display in the quotation area of the screensaver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SleepContent {
    /// Kicker line above quote: e.g. "每日一句" or "我的划线".
    pub kicker: String,
    /// The quotation or highlight text.
    pub text: String,
    /// Source line: e.g. "—— 陆游《冬夜读书示子聿》" or "——《红楼梦》".
    pub byline: String,
}

/// Format quotation text with corner brackets if not already quoted.
pub fn format_quote_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if (trimmed.starts_with('「') && trimmed.ends_with('」'))
        || (trimmed.starts_with('“') && trimmed.ends_with('”'))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
    {
        trimmed.to_string()
    } else {
        format!("「{trimmed}」")
    }
}

/// Format book title as a byline source (e.g. "——《红楼梦》" or "—— Title").
pub fn format_book_source(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with('《') && trimmed.ends_with('》') {
        format!("—— {trimmed}")
    } else if trimmed
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        format!("——《{trimmed}》")
    } else {
        format!("—— {trimmed}")
    }
}

/// Pure function to select a highlight from candidate strings deterministically using `seed`.
///
/// Ignores empty or whitespace-only candidates.
/// Returns `None` if candidates list is empty or has only whitespace.
pub fn select_highlight<'a>(candidates: &'a [&'a str], seed: i64) -> Option<&'a str> {
    let non_empty: Vec<&'a str> = candidates
        .iter()
        .copied()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if non_empty.is_empty() {
        None
    } else {
        let idx = seed.rem_euclid(non_empty.len() as i64) as usize;
        Some(non_empty[idx])
    }
}

/// Context for selecting quotations: mode, favorite/hidden predicates, and optional fixed quote ID.
#[derive(Clone, Copy)]
pub struct QuoteFilter<F, H> {
    pub mode: QuoteMode,
    pub is_favorite: F,
    pub is_hidden: H,
    pub fixed_id: Option<u32>,
}

/// Pure function to select a quotation from `quotes` according to the active mode and filters.
pub fn select_quote<F, H>(filter: QuoteFilter<F, H>, quotes: &[QuoteItem], day: i64) -> &QuoteItem
where
    F: Fn(u32) -> bool,
    H: Fn(u32) -> bool,
{
    quote_for_day(
        filter.mode,
        filter.is_favorite,
        filter.is_hidden,
        filter.fixed_id,
        quotes,
        day,
    )
}

/// Pure function to resolve the kicker, quote text, and byline for screensaver content.
///
/// When `kind` is `Highlights`, if non-empty highlights exist for the current book,
/// one is selected and byline is set to the book title.
/// If no highlights exist, it falls back to the daily quote.
pub fn resolve_sleep_content<F, H>(
    kind: SleepQuoteKind,
    highlights: &[&str],
    book_title: &str,
    filter: QuoteFilter<F, H>,
    quotes: &[QuoteItem],
    day: i64,
) -> SleepContent
where
    F: Fn(u32) -> bool,
    H: Fn(u32) -> bool,
{
    match kind {
        SleepQuoteKind::Highlights => {
            if let Some(h) = select_highlight(highlights, day) {
                return SleepContent {
                    kicker: tr(Msg::SleepKickerClip).to_string(),
                    text: format_quote_text(h),
                    byline: format_book_source(book_title),
                };
            }
            // Fall back to daily quote when no highlights are present
            let q = select_quote(filter, quotes, day);
            SleepContent {
                kicker: tr(Msg::SleepKickerQuote).to_string(),
                text: format_quote_text(&q.text),
                byline: format!("—— {}", q.by()),
            }
        }
        SleepQuoteKind::DailyQuote => {
            let q = select_quote(filter, quotes, day);
            SleepContent {
                kicker: tr(Msg::SleepKickerQuote).to_string(),
                text: format_quote_text(&q.text),
                byline: format!("—— {}", q.by()),
            }
        }
    }
}

/// The classic screensaver for `book` (or a plain one without a book). The
/// book's own cover is shown when the library has one; the generated card
/// otherwise.
pub fn build(s: &Styles, book: Option<&Book>) -> Tree {
    let mut t = Tree::new();
    let mut column = Vec::new();
    let top = t.spacer(1);
    column.push(top);
    if let Some(b) = book {
        let cover = Cover::new(&b.title, &b.author, s.family)
            .with_mark(b.mark())
            .with_image(b.cover.clone());
        let cover = t.custom(cover);
        let cover = t.fixed(Some(mm(40.0)), Some(mm(60.0)), Some(cover));
        let g = t.gap(1, Space::XL);
        // The card already names the book; below it, how far along it is.
        let pct = (b.progress * 100.0).floor();
        let line = if b.chapter.is_empty() {
            format!("{pct:.0}%")
        } else {
            format!("{pct:.0}% · {}", b.chapter)
        };
        let mut lp = Paragraph::plain(line, s.small(), Align::Center);
        lp.style.max_lines = Some(1);
        let line = t.text(lp);
        let block = t.add(
            Node::column(Space::XS).cross(Cross::Center),
            [cover, g, line],
        );
        column.push(block);
    } else {
        let l = t.text(Paragraph::plain("墨阅", s.display(), Align::Center));
        column.push(l);
    }
    let bottom = t.spacer(1);
    let hint = t.text(Paragraph::plain(
        tr(Msg::SleepWakeHint),
        s.caps(),
        Align::Center,
    ));
    column.extend([bottom, hint]);
    let col = t.add(Node::column(0).cross(Cross::Center), column);
    let root = t.pad(Insets::new(mm(10.0), mm(12.0), mm(8.0), mm(12.0)), col);
    t.set_root(root);
    t
}

/// Build the "Cover + Quote" or "Cover + Highlights" screensaver tree.
///
/// Follows `docs/ui/prototype-v2/reference/sleep.png` and `Sleep.dc.html`.
/// If `book` is None, the cover area is left empty while the cat illustration,
/// quotation/highlight text, and wake hint are displayed normally without crashing.
pub fn build_quote(s: &Styles, book: Option<&Book>, content: &SleepContent) -> Tree {
    let mut t = Tree::new();

    // 1. Top container: height 872 px.
    // Contains row with Cover (560x747) + 48 gap + FishingCat (287x280),
    // bottom-aligned with 60 px bottom padding, centered horizontally.
    let cover_node = if let Some(b) = book {
        let cover = Cover::new(&b.title, &b.author, s.family)
            .with_mark(b.mark())
            .with_binding(INK, PAPER)
            .with_image(b.cover.clone());
        let cover_custom = t.custom(cover);
        t.fixed(Some(560), Some(747), Some(cover_custom))
    } else {
        // No book: leave cover area empty (560x747 placeholder)
        t.fixed(Some(560), Some(747), None)
    };

    let cat = t.custom(FishingCat);
    let cat_fixed = t.fixed(Some(287), Some(280), Some(cat));

    let row = t.add(Node::row(48).cross(Cross::End), [cover_node, cat_fixed]);
    let row_pad = t.pad(Insets::new(0, 0, 60, 0), row);
    let top_align = t.align(0.5, 1.0, row_pad);
    let top_box = t.fixed(None, Some(872), Some(top_align));

    // 2. Quote container: flex 1, padding: 130px top, 120px left/right.
    let kicker_p = Paragraph::plain(
        &content.kicker,
        s.font_px(32).gray(INK_MID).line_height(1.3),
        Align::Center,
    );
    let kicker_node = t.text(kicker_p);

    let gap1 = t.gap(0, 22);

    let mut quote_p = Paragraph::plain(
        &content.text,
        s.font_px(52).gray(INK).line_height(1.5),
        Align::Center,
    );
    quote_p.style.max_lines = Some(4);
    let quote_node = t.text(quote_p);

    let gap2 = t.gap(0, 28);

    let byline_p = Paragraph::plain(
        &content.byline,
        s.font_px(32).gray(INK_MID).line_height(1.3),
        Align::Center,
    );
    let byline_node = t.text(byline_p);

    let quote_col = t.add(
        Node::column(0).cross(Cross::Center),
        [kicker_node, gap1, quote_node, gap2, byline_node],
    );
    let quote_pad = t.pad(Insets::new(130, 120, 0, 120), quote_col);
    let quote_box = t.add(Node::column(0).flex(1), [quote_pad]);

    let main_col = t.add(Node::column(0).cross(Cross::Stretch), [top_box, quote_box]);

    // 3. Wake hint in bottom right: "墨阅 · 按电源键唤醒", right 88, bottom 40.
    let hint_p = Paragraph::plain(
        tr(Msg::SleepWakeHint),
        s.font_px(32).gray(INK_MID).line_height(1.2),
        Align::Right,
    );
    let hint_node = t.text(hint_p);
    let hint_pad = t.pad(Insets::new(0, 88, 40, 0), hint_node);
    let hint_align = t.align(1.0, 1.0, hint_pad);

    // Stack main_col and hint_align
    let root = t.add(Node::new(Kind::Stack), [main_col, hint_align]);
    t.set_root(root);
    t
}

/// The screensaver with a picture of the reader's own.
///
/// The picture fills the whole screen, cropped in the middle like the
/// prototype's `object-fit: cover`, and the wake hint sits in a white
/// plaque in the lower right corner so it stays readable on a dark
/// picture.
///
/// `night` says whether the screen is drawn in night mode. The picture
/// is then drawn in its inverted colours, because the night mode of the
/// app inverts the whole screen: the picture would otherwise come out
/// as a negative. The plaque is pre-inverted the same way, so it stays
/// white with dark words in the prototype's night picture instead of
/// turning into a black sign with light words.
pub fn build_image(text: &mut TextEngine, s: &Styles, image: &GrayImage, night: bool) -> Tree {
    let mut t = Tree::new();
    let photo = SleepPhoto {
        image: if night {
            image.inverted()
        } else {
            image.clone()
        },
    };
    let photo = t.custom(photo);

    // The wake hint on a white plaque, in the lower right corner. The
    // plaque is as wide as the words, so it stays a small sign and does
    // not cover the picture. At night the colours are the inverse of the
    // plaque's own, so the app's whole-screen inversion brings them back
    // to white paper and #333 words.
    let plaque = if night { INK } else { PAPER };
    let words = if night { 0xFF - INK_MID } else { INK_MID };
    let style = s.font_px(32).gray(words).line_height(1.2);
    let width = text.measure(tr(Msg::SleepWakeHint), &style).ceil() as i32;
    let hint = t.text(Paragraph::plain(tr(Msg::SleepWakeHint), style, Align::Left));
    let hint = t.add(
        Node::new(Kind::Padding(Insets::new(10, 18, 10, 18))),
        [hint],
    );
    let plaque = t.add(
        Node::new(Kind::Fixed {
            w: Some(width + 36),
            h: Some(58),
        })
        .border(0, plaque, 12)
        .bg(plaque),
        [hint],
    );
    let plaque = t.pad(Insets::new(0, 88, 40, 0), plaque);
    let plaque = t.align(1.0, 1.0, plaque);

    let root = t.add(Node::new(Kind::Stack), [photo, plaque]);
    t.set_root(root);
    t
}

/// The screensaver's own picture: it fills the screen and is cropped in
/// the middle.
struct SleepPhoto {
    image: GrayImage,
}

impl Widget for SleepPhoto {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        // The picture takes the whole screen it is given.
        c.constrain(c.max)
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let img = &self.image;
        if img.width == 0 || img.height == 0 {
            return;
        }
        // The picture covers the screen; the overflow is cropped away in
        // the middle, like the prototype's `object-fit: cover`.
        let scale = (r.w as f32 / img.width as f32).max(r.h as f32 / img.height as f32);
        let w = ((img.width as f32 * scale).round() as i32).max(r.w);
        let h = ((img.height as f32 * scale).round() as i32).max(r.h);
        let scaled = if (w as u32, h as u32) == (img.width, img.height) {
            img.clone()
        } else {
            img.resized(w as u32, h as u32)
        };
        let ox = (w - r.w) / 2;
        let oy = (h - r.h) / 2;
        let mut crop = vec![0u8; (r.w * r.h) as usize];
        for row in 0..r.h {
            let src = ((row + oy) * w + ox) as usize;
            let dst = (row * r.w) as usize;
            let n = r.w as usize;
            crop[dst..dst + n].copy_from_slice(&scaled.pixels[src..src + n]);
        }
        let p = &mut *cx.painter;
        p.blit_gray(r.x, r.y, r.w as u32, r.h as u32, r.w as usize, &crop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::screens::home::quotes::QUOTES;
    use crate::screens::library::sample_books;
    use crate::text::{FamilyId, Fonts, TextEngine};
    use crate::ui::Ui;

    #[test]
    fn builds_with_and_without_a_book() {
        let s = Styles::new(FamilyId(0));
        let books = sample_books();
        for b in [Some(&books[0]), None] {
            let mut ui = Ui::new(
                build(&s, b),
                TextEngine::new(Fonts::embedded()),
                SCREEN_WIDTH as u32,
                SCREEN_HEIGHT as u32,
            );
            let d = ui.render();
            assert_eq!(d.len(), 1);
            // Something is drawn in the middle of the screen.
            let ink = (0..SCREEN_HEIGHT)
                .filter(|&y| ui.screen().get(SCREEN_WIDTH / 2, y) < 128)
                .count();
            assert!(ink > 10);
        }
    }

    /// The screensaver draws the book's cover without asking the library's
    /// cover setting: a flat gray stand-in is all over the middle of the
    /// screen, where the generated card would be black or white.
    #[test]
    fn the_screensaver_draws_the_book_cover() {
        let s = Styles::new(FamilyId(0));
        let mut book = sample_books()[0].clone();
        book.cover = Some(crate::widgets::GrayImage::new(
            200,
            300,
            vec![128; 200 * 300],
        ));
        let mut ui = Ui::new(
            build(&s, Some(&book)),
            TextEngine::new(Fonts::embedded()),
            1264,
            1680,
        );
        ui.layout();
        ui.render();
        let c = ui.screen();
        let mut gray = 0;
        for y in 200..1400 {
            for x in 0..1264 {
                gray += usize::from(c.get(x, y) == 128);
            }
        }
        assert!(gray > 50_000, "the cover is not on the screen: {gray}");
    }

    #[test]
    fn builds_quote_screen_with_and_without_book() {
        let s = Styles::new(FamilyId(0));
        let books = sample_books();
        let content = SleepContent {
            kicker: "每日一句".to_string(),
            text: "「纸上得来终觉浅，绝知此事要躬行。」".to_string(),
            byline: "—— 陆游《冬夜读书示子聿》".to_string(),
        };

        for b in [Some(&books[0]), None] {
            let mut ui = Ui::new(
                build_quote(&s, b, &content),
                TextEngine::new(Fonts::embedded()),
                1264,
                1680,
            );
            let d = ui.render();
            assert_eq!(d.len(), 1);

            // Ink exists in top half (cat and/or cover) and bottom half (quote text)
            let top_ink = (0..872)
                .filter(|&y| (0..1264).any(|x| ui.screen().get(x, y) < 128))
                .count();
            assert!(top_ink > 20);

            let bottom_ink = (872..1680)
                .filter(|&y| (0..1264).any(|x| ui.screen().get(x, y) < 128))
                .count();
            assert!(bottom_ink > 20);
        }
    }

    #[test]
    fn the_own_picture_fills_the_screen() {
        let s = Styles::new(FamilyId(0));
        // A picture that is not the shape of the screen: it is cropped.
        let picture = GrayImage::new(400, 200, vec![0x40; 400 * 200]);
        for night in [false, true] {
            let mut text = TextEngine::new(Fonts::embedded());
            let mut ui = Ui::new(
                build_image(&mut text, &s, &picture, night),
                TextEngine::new(Fonts::embedded()),
                1264,
                1680,
            );
            ui.layout();
            ui.render();
            // The picture is everywhere on the screen: only the plaque
            // in the corner is not picture.
            let mut gray = 0;
            for y in 0..1680 {
                for x in 0..1264 {
                    let p = ui.screen().get(x, y);
                    gray += usize::from(p == 0x40 || p == 0xBF);
                }
            }
            assert!(gray > 1_900_000, "the picture is not on the screen: {gray}");
        }
    }

    #[test]
    fn the_own_picture_is_not_inverted_in_night_mode() {
        let s = Styles::new(FamilyId(0));
        let picture = GrayImage::new(200, 200, vec![0x40; 200 * 200]);
        let mut text = TextEngine::new(Fonts::embedded());
        let mut day = Ui::new(
            build_image(&mut text, &s, &picture, false),
            TextEngine::new(Fonts::embedded()),
            1264,
            1680,
        );
        day.layout();
        day.render();
        let mut text = TextEngine::new(Fonts::embedded());
        let mut night = Ui::new(
            build_image(&mut text, &s, &picture, true),
            TextEngine::new(Fonts::embedded()),
            1264,
            1680,
        );
        // The night mode of the app inverts the whole screen.
        night.set_night(true);
        night.layout();
        night.render();
        // The picture is drawn in its inverted colours, so what the
        // screen shows is the picture as it is.
        assert_eq!(day.screen().get(600, 700), night.screen().get(600, 700));
        // R11: the plaque is pre-inverted too, so at night it stays white
        // with dark words, exactly as in the prototype. Before the fix the
        // whole-screen inversion turned it into a black sign.
        let mut differ = 0;
        for y in 1500..1680 {
            for x in 700..1264 {
                if day.screen().get(x, y) != night.screen().get(x, y) {
                    differ += 1;
                }
            }
        }
        assert_eq!(differ, 0, "the night plaque is inverted: {differ} pixels");
    }

    #[test]
    fn test_select_highlight_pure_function() {
        assert_eq!(select_highlight(&[], 0), None);
        assert_eq!(select_highlight(&["", "   "], 0), None);

        let list = ["第一条划线", "第二条划线", "第三条划线"];
        assert_eq!(select_highlight(&list, 0), Some("第一条划线"));
        assert_eq!(select_highlight(&list, 1), Some("第二条划线"));
        assert_eq!(select_highlight(&list, 2), Some("第三条划线"));
        assert_eq!(select_highlight(&list, 3), Some("第一条划线"));
        assert_eq!(select_highlight(&list, -1), Some("第三条划线"));

        let list_with_blanks = ["", "第一条", "   ", "第二条"];
        assert_eq!(select_highlight(&list_with_blanks, 0), Some("第一条"));
        assert_eq!(select_highlight(&list_with_blanks, 1), Some("第二条"));
        assert_eq!(select_highlight(&list_with_blanks, 2), Some("第一条"));
    }

    #[test]
    fn test_formatting_helpers() {
        assert_eq!(format_quote_text(""), "");
        assert_eq!(format_quote_text("  hello  "), "「hello」");
        assert_eq!(format_quote_text("「already」"), "「already」");
        assert_eq!(format_quote_text("“quoted”"), "“quoted”");
        assert_eq!(format_quote_text("\"ascii\""), "\"ascii\"");

        assert_eq!(format_book_source(""), "");
        assert_eq!(format_book_source("红楼梦"), "——《红楼梦》");
        assert_eq!(format_book_source("《西游记》"), "—— 《西游记》");
        assert_eq!(
            format_book_source("Pride and Prejudice"),
            "—— Pride and Prejudice"
        );
    }

    #[test]
    fn test_resolve_sleep_content() {
        let filter = QuoteFilter {
            mode: QuoteMode::AllRandom,
            is_favorite: |_| false,
            is_hidden: |_| false,
            fixed_id: None,
        };

        // Daily quote resolution
        let c_daily = resolve_sleep_content(
            SleepQuoteKind::DailyQuote,
            &[],
            "红楼梦",
            filter,
            &QUOTES,
            0,
        );
        assert_eq!(c_daily.kicker, tr(Msg::SleepKickerQuote));
        assert!(c_daily.text.starts_with('「'));
        assert!(c_daily.byline.starts_with("—— "));

        // Highlights resolution with candidates
        let highlights = ["质本洁来还洁去，强于污淖陷渠沟。"];
        let c_clip = resolve_sleep_content(
            SleepQuoteKind::Highlights,
            &highlights,
            "红楼梦",
            filter,
            &QUOTES,
            0,
        );
        assert_eq!(c_clip.kicker, tr(Msg::SleepKickerClip));
        assert_eq!(c_clip.text, "「质本洁来还洁去，强于污淖陷渠沟。」");
        assert_eq!(c_clip.byline, "——《红楼梦》");

        // Highlights resolution fallback when empty
        let c_fallback = resolve_sleep_content(
            SleepQuoteKind::Highlights,
            &[],
            "红楼梦",
            filter,
            &QUOTES,
            0,
        );
        assert_eq!(c_fallback.kicker, tr(Msg::SleepKickerQuote));
        assert_eq!(c_fallback.text, c_daily.text);
    }
}
