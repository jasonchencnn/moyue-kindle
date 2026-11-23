// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quotation library screen: browse, favorite, hide, and choose display modes.
//!
//! Follows `docs/ui/prototype-v2/reference/quotelibrary.png` and
//! `docs/ui/prototype-v2/mcode/QuoteLibrary.dc.html`.

use super::home::quotes::{QuoteCategory, QuoteItem, QuoteMode, MOOD_COUNT, QUOTES, READING_COUNT};
use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_MID, PAPER, RULE};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, Tree};
use aristotle_i18n::{tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// Return to the "mine" screen.
    pub const BACK: ActionId = 1;
    /// Switch to the "reading" category.
    pub const TAB_READING: ActionId = 2;
    /// Switch to the "mood" category.
    pub const TAB_MOOD: ActionId = 3;
    /// Display mode: all random.
    pub const MODE_ALL_RANDOM: ActionId = 4;
    /// Display mode: favorites random.
    pub const MODE_FAVORITES_RANDOM: ActionId = 5;
    /// Display mode: fixed quotation.
    pub const MODE_FIXED: ActionId = 6;
    /// Add quote button (opens coming soon page).
    pub const ADD_QUOTE: ActionId = 7;
    /// Previous page.
    pub const PREV_PAGE: ActionId = 8;
    /// Next page.
    pub const NEXT_PAGE: ActionId = 9;

    /// Tap quote card to set fixed quote (index 0..3).
    pub const CARD_BASE: ActionId = 20;
    /// Toggle favorite on quote (index 0..3).
    pub const FAVORITE_BASE: ActionId = 30;
    /// Hide quote from pool (index 0..3).
    pub const HIDE_BASE: ActionId = 40;
    /// Restore hidden quote to pool (index 0..3).
    pub const RESTORE_BASE: ActionId = 50;
    /// Delete user quotation on current page (index 0..3).
    pub const DELETE_BASE: ActionId = 60;
    /// Cancel delete confirmation dialog.
    pub const DIALOG_CANCEL: ActionId = 70;
    /// Confirm delete quotation.
    pub const DIALOG_CONFIRM: ActionId = 71;
}

const HEADER_H: i32 = 112;

/// One quotation card on the current page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuoteCardItem<'a> {
    pub item: &'a QuoteItem,
    pub is_favorite: bool,
    pub is_hidden: bool,
    pub can_delete: bool,
}

/// What the quote library screen shows.
pub struct QuoteScreen<'a> {
    pub status: StatusLine<'a>,
    pub tab: QuoteCategory,
    pub mode: QuoteMode,
    pub page: usize,
    pub total_pages: usize,
    pub reading_count: usize,
    pub mood_count: usize,
    pub daily_quote: &'a QuoteItem,
    pub page_quotes: Vec<QuoteCardItem<'a>>,
    pub confirm_delete: Option<&'a QuoteItem>,
}

/// Handles to the nodes of the quote library screen.
pub struct QuoteUi {
    pub loading: LoadingNodes,
}

impl QuoteUi {
    /// Builds the quote library screen tree.
    pub fn build(text: &mut TextEngine, s: &Styles, screen: &QuoteScreen) -> (Tree, Self) {
        Self::build_for_size(
            text,
            s,
            screen,
            Size::new(s.density.px(1264), s.density.px(1680)),
        )
    }

    /// Builds the quote library screen for a panel of `size` native pixels.
    /// Six-inch panels tighten card padding and gaps so all four cards, the
    /// add button and the pager fit above the bottom edge.
    pub fn build_for_size(
        _text: &mut TextEngine,
        s: &Styles,
        screen: &QuoteScreen,
        size: Size,
    ) -> (Tree, Self) {
        let compact = crate::theme::PanelLayout::for_size(size, s.density)
            == crate::theme::PanelLayout::Compact;
        let mut t = Tree::new();

        // 1. Status bar
        let bar = status_line(&mut t, s, &screen.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let status_fixed = t.fixed(None, Some(88), Some(status));

        // 2. Header: "‹ 名言库"
        let back_icon = t.icon(Icon::ChevronLeft, 44, INK);
        let back_pad = t.pad(Insets::all(14), back_icon);
        let back_btn = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .action(action::BACK)
                .feedback(Feedback::Invert),
            [back_pad],
        );
        let title_label = t.label(
            tr(Msg::QuotesTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header_row = t.add(Node::row(20).cross(Cross::Center), [back_btn, title_label]);
        let header_fixed = t.fixed(None, Some(HEADER_H), Some(header_row));
        let header = t.pad(
            Insets::new(0, home_margin(), 0, home_margin()),
            header_fixed,
        );

        // 3. Segmented control: "读书 · N / 心情 · N"
        let seg_btn = |t: &mut Tree, text: &str, active: bool, act: ActionId| {
            let style = if active {
                s.font_sm().bold().gray(PAPER)
            } else {
                s.font_sm().gray(INK)
            };
            let l = t.text(Paragraph::plain(text, style, Align::Center));
            let pad = t.pad(Insets::vh(12, 0), l);
            let mut node = Node::column(0)
                .cross(Cross::Stretch)
                .flex(1)
                .action(act)
                .feedback(Feedback::Invert);
            if active {
                node = node.bg(INK).border(0, INK, 18);
            }
            t.add(node, [pad])
        };

        let reading_active = screen.tab == QuoteCategory::Reading;
        let reading_text = tf!(Msg::QuotesTabReading, count = screen.reading_count);
        let btn_reading = seg_btn(&mut t, &reading_text, reading_active, action::TAB_READING);

        let mood_active = screen.tab == QuoteCategory::Mood;
        let mood_text = tf!(Msg::QuotesTabMood, count = screen.mood_count);
        let btn_mood = seg_btn(&mut t, &mood_text, mood_active, action::TAB_MOOD);

        let seg_row = t.add(
            Node::row(6).cross(Cross::Stretch).flex(1),
            [btn_reading, btn_mood],
        );
        let seg_inner = t.pad(Insets::all(6), seg_row);
        let segmented = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(76),
            })
            .bg(CARD)
            .border(0, CARD, 24),
            [seg_inner],
        );

        // 4. Mode selector: "显示方式 全部随机 只在收藏里随机 固定一句"
        let mode_label = t.label(
            tr(Msg::QuotesModeLabel),
            s.font_xs().gray(INK_MID).line_height(1.2),
        );
        let mode_btn = |t: &mut Tree, text: &str, active: bool, act: ActionId| {
            let style = if active {
                s.font_xs().bold().gray(PAPER)
            } else {
                s.font_xs().gray(INK)
            };
            let l = t.label(text, style);
            let pad = t.pad(Insets::vh(18, 24), l);
            t.add(
                Node::row(0)
                    .cross(Cross::Center)
                    .bg(if active { INK } else { PAPER })
                    .border(if active { 0 } else { 2 }, INK, 20)
                    .action(act)
                    .feedback(Feedback::Invert),
                [pad],
            )
        };
        let btn_all = mode_btn(
            &mut t,
            tr(Msg::QuotesModeAllRandom),
            screen.mode == QuoteMode::AllRandom,
            action::MODE_ALL_RANDOM,
        );
        let btn_fav = mode_btn(
            &mut t,
            tr(Msg::QuotesModeFavoritesRandom),
            screen.mode == QuoteMode::FavoritesRandom,
            action::MODE_FAVORITES_RANDOM,
        );
        let btn_fixed = mode_btn(
            &mut t,
            tr(Msg::QuotesModeFixed),
            screen.mode == QuoteMode::Fixed,
            action::MODE_FIXED,
        );
        let mode_row = t.add(
            Node::row(16).cross(Cross::Center),
            [mode_label, btn_all, btn_fav, btn_fixed],
        );

        // 5. Daily quote preview bar: "首页每日一句 「...」 —— 作者"
        let chip_label = t.label(
            tr(Msg::QuotesPreviewBadge),
            s.font_xs().gray(INK).line_height(1.2),
        );
        let chip_pad = t.pad(Insets::vh(8, 16), chip_label);
        let chip = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .bg(PAPER)
                .border(2, INK, 16),
            [chip_pad],
        );

        let daily_quote_str = format!("「{}」", screen.daily_quote.text);
        let mut dp = Paragraph::plain(
            &daily_quote_str,
            s.font_link().gray(INK).line_height(1.4),
            Align::Left,
        );
        dp.style.max_lines = Some(1);
        let daily_lbl = t.text(dp);
        let daily_text = t.add(Node::column(0).flex(1), [daily_lbl]);

        let daily_author_str = format!("—— {}", screen.daily_quote.by());
        let daily_author = t.label(
            &daily_author_str,
            s.font_xs().gray(INK_MID).line_height(1.2),
        );

        let preview_inner = t.add(
            Node::row(20).cross(Cross::Center),
            [chip, daily_text, daily_author],
        );
        let preview_pad = t.pad(Insets::vh(18, 24), preview_inner);
        let preview_bar = t.add(
            Node::new(Kind::MinSize(Size::new(0, 76)))
                .bg(CARD)
                .border(0, CARD, 24),
            [preview_pad],
        );

        // 6. Quotation cards list (up to 4 items on current page)
        let card_pad_v = if compact { 12 } else { 24 };
        let card_min_h = if compact { 120 } else { 196 };
        let card_gap = if compact { 10 } else { 20 };
        let mut card_nodes = Vec::new();
        for (i, q) in screen.page_quotes.iter().enumerate() {
            let act_idx = i as ActionId;
            if !q.is_hidden {
                let q_text_str = format!("「{}」", q.item.text);
                let mut qp = Paragraph::plain(
                    &q_text_str,
                    s.font_sm()
                        .gray(INK)
                        .line_height(if compact { 1.3 } else { 1.45 }),
                    Align::Left,
                );
                if compact {
                    qp.style.max_lines = Some(2);
                }
                let text_node = t.text(qp);
                let q_by_str = format!("—— {}", q.item.by());
                let mut bp = Paragraph::plain(
                    &q_by_str,
                    s.font_xs().gray(INK_MID).line_height(1.25),
                    Align::Left,
                );
                if compact {
                    bp.style.max_lines = Some(1);
                }
                let by_node = t.text(bp);
                let gap_node = t.gap(0, if compact { 4 } else { 8 });
                let left_col = t.add(
                    Node::column(0)
                        .flex(1)
                        .action(action::CARD_BASE + act_idx)
                        .feedback(Feedback::None),
                    [text_node, gap_node, by_node],
                );

                let icon_pad = if compact { 14 } else { 18 };
                let star_icon = if q.is_favorite {
                    t.icon(Icon::StarFilled, 40, PAPER)
                } else {
                    t.icon(Icon::Star, 40, INK)
                };
                let star_pad = t.pad(Insets::all(icon_pad), star_icon);
                let star_btn = t.add(
                    Node::row(0)
                        .cross(Cross::Center)
                        .bg(if q.is_favorite { INK } else { PAPER })
                        .border(2, INK, 20)
                        .action(action::FAVORITE_BASE + act_idx)
                        .feedback(Feedback::Invert),
                    [star_pad],
                );

                let hide_icon = t.icon(Icon::EyeOff, 40, INK);
                let hide_pad = t.pad(Insets::all(icon_pad), hide_icon);
                let hide_btn = t.add(
                    Node::row(0)
                        .cross(Cross::Center)
                        .bg(PAPER)
                        .border(2, INK, 20)
                        .action(action::HIDE_BASE + act_idx)
                        .feedback(Feedback::Invert),
                    [hide_pad],
                );

                let mut btn_nodes = vec![star_btn, hide_btn];
                if q.can_delete {
                    let trash_icon = t.icon(Icon::Trash, 40, INK);
                    let trash_pad = t.pad(Insets::all(icon_pad), trash_icon);
                    let trash_btn = t.add(
                        Node::row(0)
                            .cross(Cross::Center)
                            .bg(PAPER)
                            .border(2, INK, 20)
                            .action(action::DELETE_BASE + act_idx)
                            .feedback(Feedback::Invert),
                        [trash_pad],
                    );
                    btn_nodes.push(trash_btn);
                }

                let right_row = t.add(Node::row(14).cross(Cross::Center), btn_nodes);
                let card_row = t.add(Node::row(20).cross(Cross::Center), [left_col, right_row]);
                let card_pad = t.pad(Insets::vh(card_pad_v, 24), card_row);
                let card = t.add(
                    Node::new(Kind::MinSize(Size::new(0, card_min_h)))
                        .bg(CARD)
                        .border(0, CARD, 24),
                    [card_pad],
                );
                card_nodes.push(card);
            } else {
                let hide_icon = t.icon(Icon::EyeOff, 36, INK);
                let hint_label = t.label(
                    tr(Msg::QuotesHiddenHint),
                    s.font_xs().gray(INK_MID).line_height(1.3),
                );
                let hint_node = t.add(Node::column(0).flex(1), [hint_label]);

                let restore_label = t.label(tr(Msg::QuotesRestore), s.font_xs().gray(INK));
                let restore_pad = t.pad(Insets::vh(14, 24), restore_label);
                let restore_btn = t.add(
                    Node::row(0)
                        .cross(Cross::Center)
                        .bg(PAPER)
                        .border(2, INK, 20)
                        .action(action::RESTORE_BASE + act_idx)
                        .feedback(Feedback::Invert),
                    [restore_pad],
                );

                let strip_row = t.add(
                    Node::row(16).cross(Cross::Center),
                    [hide_icon, hint_node, restore_btn],
                );
                let strip_pad = t.pad(Insets::new(6, 12, 6, 24), strip_row);
                let strip = t.add(
                    Node::new(Kind::MinSize(Size::new(0, 76)))
                        .bg(CARD)
                        .border(0, CARD, 20),
                    [strip_pad],
                );
                card_nodes.push(strip);
            }
        }
        let list_col = t.add(Node::column(card_gap), card_nodes);

        // 7. Add quote button + hint row
        let plus_icon = t.icon(Icon::Plus, 32, PAPER);
        let add_label = t.label(tr(Msg::QuotesAddQuote), s.font_sm().bold().gray(PAPER));
        let add_inner = t.add(Node::row(12).cross(Cross::Center), [plus_icon, add_label]);
        let add_pad = t.pad(Insets::vh(if compact { 16 } else { 24 }, 28), add_inner);
        let add_btn = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .bg(INK)
                .border(0, INK, 20)
                .action(action::ADD_QUOTE)
                .feedback(Feedback::Invert),
            [add_pad],
        );

        let mut add_hint_para = Paragraph::plain(
            tr(Msg::QuotesAddQuoteHint),
            s.font_xs().gray(INK_MID).line_height(1.25),
            Align::Left,
        );
        if compact {
            add_hint_para.style.max_lines = Some(2);
        }
        let add_hint = t.text(add_hint_para);
        let add_hint_box = t.add(Node::column(0).flex(1), [add_hint]);
        let add_row = t.add(Node::row(16).cross(Cross::Center), [add_btn, add_hint_box]);

        // 8. Pager: "‹  第 X / Y 页 上下滑动或按翻页键  ›"
        let can_prev = screen.page > 1;
        let prev_icon = t.icon(Icon::ChevronLeft, 44, if can_prev { INK } else { RULE });
        let prev_pad = t.pad(Insets::all(22), prev_icon);
        let prev_btn = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .action(action::PREV_PAGE)
                .feedback(Feedback::Invert),
            [prev_pad],
        );

        let page_of = tf!(
            Msg::LibraryPagerPageOf,
            page = screen.page,
            count = screen.total_pages
        );
        let page_label = t.text(Paragraph::plain(
            page_of,
            s.font_sm().gray(INK).line_height(1.3),
            Align::Center,
        ));
        let hint_label = t.text(Paragraph::plain(
            tr(Msg::LibraryPagerHintScroll),
            s.font_xs().gray(INK_MID).line_height(1.3),
            Align::Center,
        ));
        let pager_center_col = t.add(
            Node::column(0).cross(Cross::Center),
            [page_label, hint_label],
        );
        let pager_center = t.add(
            Node::column(0).cross(Cross::Center).flex(1),
            [pager_center_col],
        );

        let can_next = screen.page < screen.total_pages;
        let next_icon = t.icon(Icon::ChevronRight, 44, if can_next { INK } else { RULE });
        let next_pad = t.pad(Insets::all(22), next_icon);
        let next_btn = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .action(action::NEXT_PAGE)
                .feedback(Feedback::Invert),
            [next_pad],
        );

        let pager_row = t.add(
            Node::row(0).cross(Cross::Center),
            [prev_btn, pager_center, next_btn],
        );
        let pager_fixed = t.fixed(None, Some(104), Some(pager_row));

        // Assemble content column
        let gap_seg = t.gap(0, if compact { 12 } else { 24 });
        let gap_mode = t.gap(0, if compact { 10 } else { 20 });
        let gap_preview = t.gap(0, if compact { 12 } else { 20 });
        let gap_list = t.gap(0, if compact { 12 } else { 20 });
        // 13 px: the pager needs 64 px for its 64 px targets. The Polish
        // labels wrap and used to squeeze it to 57 px (QA-101 P2-3).
        let gap_add = t.gap(0, if compact { 8 } else { 13 });

        let content_col = t.add(
            Node::column(0).flex(1),
            [
                segmented,
                gap_seg,
                mode_row,
                gap_mode,
                preview_bar,
                gap_preview,
                list_col,
                gap_list,
                add_row,
                gap_add,
                pager_fixed,
            ],
        );
        let content_padded = t.pad(
            Insets::new(
                if compact { 12 } else { 24 },
                home_margin(),
                if compact { 14 } else { 28 },
                home_margin(),
            ),
            content_col,
        );
        let content = t.add(Node::column(0).flex(1), [content_padded]);

        let base_root = t.add(Node::column(0), [status_fixed, header, content]);

        let root = if let Some(item) = screen.confirm_delete {
            let scrim = t.add(
                Node::column(0)
                    .flex(1)
                    .action(action::DIALOG_CANCEL)
                    .feedback(Feedback::None),
                [],
            );

            let title_text = tr(Msg::QuotesDeleteConfirmTitle);
            let title_node = t.label(title_text, s.title().bold());

            let quote_str = format!("「{}」", item.text);
            let mut qp = Paragraph::plain(
                &quote_str,
                s.font_sm().gray(INK).line_height(1.4),
                Align::Left,
            );
            qp.style.max_lines = Some(3);
            let quote_node = t.text(qp);

            let hint_node = t.label(tr(Msg::QuotesDeleteConfirmHint), s.ui().gray(INK_2));

            let cancel_lbl = t.label(tr(Msg::CommonCancel), s.ui_bold());
            let cancel_pad = t.pad(Insets::vh(16, 24), cancel_lbl);
            let cancel_btn = t.button(
                Node::plain().bg(PAPER).border(2, INK, 18),
                cancel_pad,
                action::DIALOG_CANCEL,
            );
            let cancel_col = t.add(Node::column(0).flex(1), [cancel_btn]);

            let del_lbl = t.label(tr(Msg::QuotesDeleteConfirmButton), s.ui_bold().gray(PAPER));
            let del_pad = t.pad(Insets::vh(16, 24), del_lbl);
            let del_btn = t.button(
                Node::plain().bg(INK).border(0, INK, 18),
                del_pad,
                action::DIALOG_CONFIRM,
            );
            let del_col = t.add(Node::column(0).flex(1), [del_btn]);

            let btn_gap = t.gap(20, 1);
            let buttons_row = t.add(
                Node::row(0).cross(Cross::Center),
                [cancel_col, btn_gap, del_col],
            );

            let g1 = t.gap(1, 14);
            let g2 = t.gap(1, 14);
            let g3 = t.gap(1, 24);

            let dialog_content = t.add(
                Node::column(0).cross(Cross::Stretch),
                [title_node, g1, quote_node, g2, hint_node, g3, buttons_row],
            );
            let padded_dialog = t.pad(Insets::all(32), dialog_content);
            let rule = t.rule(3, INK);
            let sheet = t.add(Node::column(0).bg(PAPER), [rule, padded_dialog]);
            let overlay = t.add(Node::column(0), [scrim, sheet]);
            t.add(Node::new(Kind::Stack), [base_root, overlay])
        } else {
            base_root
        };

        t.set_root(root);
        (
            t,
            QuoteUi {
                loading: bar.loading,
            },
        )
    }
}

/// A sample of the screen matching `docs/ui/prototype-v2/reference/quotelibrary.png`.
pub fn sample(status: StatusLine<'static>) -> QuoteScreen<'static> {
    QuoteScreen {
        status,
        tab: QuoteCategory::Reading,
        mode: QuoteMode::AllRandom,
        page: 1,
        total_pages: 13,
        reading_count: READING_COUNT,
        mood_count: MOOD_COUNT,
        daily_quote: &QUOTES[1],
        page_quotes: vec![
            QuoteCardItem {
                item: &QUOTES[0],
                is_favorite: true,
                is_hidden: false,
                can_delete: false,
            },
            QuoteCardItem {
                item: &QUOTES[1],
                is_favorite: false,
                is_hidden: false,
                can_delete: false,
            },
            QuoteCardItem {
                item: &QUOTES[2],
                is_favorite: false,
                is_hidden: false,
                can_delete: false,
            },
            QuoteCardItem {
                item: &QUOTES[3],
                is_favorite: false,
                is_hidden: false,
                can_delete: false,
            },
        ],
        confirm_delete: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Fonts;
    use crate::tree::NodeId;

    #[test]
    fn quote_library_screen_builds_without_panic() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let sample_screen = sample(StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        });
        let (_tree, _ui) = QuoteUi::build(&mut text, &s, &sample_screen);
    }

    #[test]
    fn hidden_quote_renders_restore_action() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let screen = QuoteScreen {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            tab: QuoteCategory::Reading,
            mode: QuoteMode::Fixed,
            page: 1,
            total_pages: 13,
            reading_count: READING_COUNT,
            mood_count: MOOD_COUNT,
            daily_quote: &QUOTES[0],
            page_quotes: vec![QuoteCardItem {
                item: &QUOTES[0],
                is_favorite: false,
                is_hidden: true,
                can_delete: false,
            }],
            confirm_delete: None,
        };
        let (tree, _ui) = QuoteUi::build(&mut text, &s, &screen);
        let has_action = |act: ActionId| {
            (0..tree.len()).any(|i| tree.node(NodeId(i as u32)).action == Some(act))
        };
        assert!(has_action(action::RESTORE_BASE));
        assert!(has_action(action::BACK));
        assert!(has_action(action::TAB_READING));
        assert!(has_action(action::TAB_MOOD));
        assert!(has_action(action::MODE_FIXED));
        assert!(has_action(action::ADD_QUOTE));
    }

    #[test]
    fn custom_quote_renders_delete_action_and_dialog() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let screen = QuoteScreen {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            tab: QuoteCategory::Reading,
            mode: QuoteMode::AllRandom,
            page: 1,
            total_pages: 1,
            reading_count: 1,
            mood_count: 0,
            daily_quote: &QUOTES[0],
            page_quotes: vec![QuoteCardItem {
                item: &QUOTES[0],
                is_favorite: false,
                is_hidden: false,
                can_delete: true,
            }],
            confirm_delete: Some(&QUOTES[0]),
        };
        let (tree, _ui) = QuoteUi::build(&mut text, &s, &screen);
        let has_action = |act: ActionId| {
            (0..tree.len()).any(|i| tree.node(NodeId(i as u32)).action == Some(act))
        };
        assert!(has_action(action::DELETE_BASE));
        assert!(has_action(action::DIALOG_CANCEL));
        assert!(has_action(action::DIALOG_CONFIRM));
    }
}
