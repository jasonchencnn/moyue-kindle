// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading interactions in the app: selecting words, the dictionary,
//! footnotes, highlights, bookmarks, the contents, "go to".
//!
//! How things reach the panel (`docs/reader.md`, "Reading interactions"):
//!
//! - **Appearing** (a popup, the action sheet, a list, the "go to" sheet)
//!   is one update: the frame is fully built, then sent as one full-screen
//!   mode-9 update when it has grays ([`App::present_full`]).
//! - **Feedback while the finger is down** (the held word when the
//!   dictionary is slow, a selection growing under a drag) is A2 on the
//!   selection's boxes only.
//! - **Closing** anything that covered the page shows the page again with
//!   the page-turn update, one full-screen mode-9 update
//!   ([`App::close_overlay`]), so nothing is left behind.
//!
//! Pages turn on release ([`crate::turns`]), so a long press finds the
//! page it started on: the selection and the popup come in one update.
//!
//! Night mode needs nothing here: the UI inverts its pixels (or the EPDC
//! does), so popups, selections and lists follow the night palette.
//! While a book still opens from its snapshot these gestures do nothing
//! (`App::opening_intent`).

use std::rc::Rc;
use std::time::{Duration, Instant};

use aristotle_i18n::{tf, tr, Msg};
use aristotle_library::index::AnnotationKind;
use aristotle_net::translate::Translation;
use aristotle_render::book::{Bounds, Engine, Link, PageFootnote, Position, Word};
use aristotle_render::plato_core::anyhow::Error;
use aristotle_render::richtext::{Block, RichText, Run};
use aristotle_ui::geom::{min_target, Point, Rect};
use aristotle_ui::input::Direction;
use aristotle_ui::screens::selection as selection_ui;
use aristotle_ui::screens::sheet::{self, Button};
use aristotle_ui::screens::toc as toc_ui;
use aristotle_ui::screens::translate as translate_ui;
use aristotle_ui::text::FamilyId;
use aristotle_ui::tree::{ActionId, NodeId, Refresh, Tree};
use aristotle_ui::ui::UiEvent;
use aristotle_ui::widgets::Slider;

use crate::dict::service::{Reply, Request};
use crate::dict::{self, Lookup};
use crate::marks;
use crate::overlay::{self, act, ListRow, Place, PopupSpec, SelectionMarks};
use crate::panel::Content;
use crate::selection::{self, Selection};
use crate::translate::{self as translation, Reply as TranslateReply};
use crate::turns::Turn;
use crate::{perf, App, Screen};

/// How long a long press waits for the dictionary before it shows the
/// selected word on its own.
const LOOKUP_PATIENCE: Duration = Duration::from_millis(150);

/// What covers the page.
#[derive(Default)]
pub enum Overlay {
    #[default]
    None,
    /// A held word, the dictionary still looking.
    Waiting {
        sel: Selection,
        words: Rc<Vec<Word>>,
    },
    Dict {
        sel: Selection,
        words: Rc<Vec<Word>>,
        lookup: Box<Lookup>,
        /// Annotation index of a highlight the word is in.
        highlight: Option<usize>,
        page: usize,
        /// The whole entry ("Więcej"), not only the senses.
        full: bool,
    },
    /// A selection under a dragging finger, with handles.
    Dragging {
        sel: Selection,
        words: Rc<Vec<Word>>,
    },
    /// A dragged selection, released: what to do with it.
    Actions {
        sel: Selection,
        words: Rc<Vec<Word>>,
    },
    /// A translation of the selection, or of the sentence it stands in.
    Translate {
        sel: Selection,
        words: Rc<Vec<Word>>,
        /// The text that was sent.
        source: String,
        /// The request's number: an answer with an older number is stale.
        seq: u64,
        /// The answer, when it came.
        result: Option<Result<Translation, String>>,
        page: usize,
    },
    /// A word-by-word gloss of the selection.
    Gloss {
        sel: Selection,
        words: Rc<Vec<Word>>,
        text: RichText,
        page: usize,
    },
    Note {
        text: RichText,
        link: Option<Link>,
        page: usize,
    },
    /// The table of contents: the book's chapters as an outline that can
    /// be folded (`aristotle_ui::screens::toc`).
    Toc {
        contents: Box<toc_ui::Contents>,
        page: usize,
    },
    Marks {
        rows: Vec<MarkRow>,
        page: usize,
    },
    Annotation {
        text: RichText,
        target: Option<Position>,
        page: usize,
    },
    /// A thought the reader wrote on WeRead, or why a cloud note has no
    /// position in this edition.
    WereadThought {
        text: RichText,
        target: Option<Position>,
        page: usize,
    },
    /// The reading fonts (fonts.rs).
    Fonts {
        target: crate::fonts::FontTarget,
        rows: Vec<crate::fonts::FontRow>,
        page: usize,
    },
    GoTo {
        value: f32,
    },
    /// Another device's position, offered (`sync.rs`): its page start and
    /// the question.
    Sync {
        target: usize,
        text: String,
    },
    /// Reader typeset panel according to Prototype v2 (F2, F3).
    Typeset {
        view: Box<aristotle_ui::screens::typeset::TypesetView>,
    },
}

impl Overlay {
    pub fn is_none(&self) -> bool {
        matches!(self, Overlay::None)
    }

    fn selection(&self) -> Option<(&Selection, &Rc<Vec<Word>>)> {
        match self {
            Overlay::Waiting { sel, words }
            | Overlay::Dict { sel, words, .. }
            | Overlay::Dragging { sel, words }
            | Overlay::Actions { sel, words }
            | Overlay::Translate { sel, words, .. }
            | Overlay::Gloss { sel, words, .. } => Some((sel, words)),
            _ => None,
        }
    }

    fn page(&self) -> Option<usize> {
        match self {
            Overlay::Dict { page, .. }
            | Overlay::Translate { page, .. }
            | Overlay::Gloss { page, .. }
            | Overlay::Note { page, .. }
            | Overlay::Toc { page, .. }
            | Overlay::Marks { page, .. }
            | Overlay::Annotation { page, .. }
            | Overlay::WereadThought { page, .. }
            | Overlay::Fonts { page, .. } => Some(*page),
            _ => None,
        }
    }

    fn page_mut(&mut self) -> Option<&mut usize> {
        match self {
            Overlay::Dict { page, .. }
            | Overlay::Translate { page, .. }
            | Overlay::Gloss { page, .. }
            | Overlay::Note { page, .. }
            | Overlay::Toc { page, .. }
            | Overlay::Marks { page, .. }
            | Overlay::Annotation { page, .. }
            | Overlay::WereadThought { page, .. }
            | Overlay::Fonts { page, .. } => Some(page),
            _ => None,
        }
    }
}

/// A line of the highlights list.
pub struct MarkRow {
    pub text: String,
    pub meta: String,
    pub target: Option<Position>,
    pub detail: Option<String>,
    /// The row comes from the WeRead cloud, so its detail sheet is not an
    /// imported annotation.
    pub weread: bool,
}

/// Nodes of the current overlay that change without a rebuild.
#[derive(Default)]
pub struct OverlayNodes {
    pub selection: Option<NodeId>,
    pub pages: Option<NodeId>,
    pub pager: Option<NodeId>,
    /// The pager's arrows, hidden when there is one page.
    pub arrows: Option<(NodeId, NodeId)>,
    /// A popup's sheet, to fit its height to the content.
    pub sheet: Option<NodeId>,
    pub slider: Option<NodeId>,
    pub label: Option<NodeId>,
    /// The contents list's own nodes: the fold markers and the pager.
    pub toc: Option<toc_ui::TocUi>,
}

/// Bounds to a UI rect.
fn rect(b: &Bounds) -> Rect {
    Rect::new(b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0)
}

/// The word nearest to a point, however far (a drag may run into the
/// margin).
fn nearest(words: &[Word], p: Point) -> Option<usize> {
    (0..words.len()).min_by_key(|&i| words[i].rect.dist2(p.x, p.y))
}

impl App {
    pub(crate) fn reading(&mut self) -> Option<&mut crate::Reading> {
        match &mut self.screen {
            Screen::Reader(r) => Some(r),
            _ => None,
        }
    }

    // ------------------------------------------------------------ frames

    /// Build the reader screen for the current state (page, overlay, menu)
    /// and install it. Nothing is sent to the panel.
    pub(crate) fn rebuild_reader(&mut self, refresh: Refresh) -> Result<(), Error> {
        self.build_reader(refresh, None)?;
        // A popup whose body fits on one page is built again at the height
        // its content needs (a quarter of the screen at least), with the
        // buttons right under it.
        if let Some(fit) = self.popup_fit() {
            self.build_reader(refresh, Some(fit))?;
        }
        Ok(())
    }

    /// The height the shown popup needs, if its body fits on one page and
    /// leaves room unused.
    fn popup_fit(&mut self) -> Option<i32> {
        let r = match &self.screen {
            Screen::Reader(r) => r,
            _ => return None,
        };
        if !matches!(
            r.overlay,
            Overlay::Dict { .. }
                | Overlay::Gloss { .. }
                | Overlay::Note { .. }
                | Overlay::Annotation { .. }
                | Overlay::WereadThought { .. }
                | Overlay::Sync { .. }
        ) {
            return None;
        }
        let pages = r.nodes.pages?;
        let sheet = r.nodes.sheet?;
        let t = &self.ui.tree;
        if t.page(pages).1 > 1 {
            return None;
        }
        let body = t.rect(pages);
        let used = t
            .node(pages)
            .children
            .iter()
            .map(|&c| t.rect(c).bottom())
            .max()
            .unwrap_or(body.y)
            - body.y;
        let density = self.ui.profile().density;
        let unused = body.h - used - density.px(8);
        let height = t.rect(sheet).h;
        (unused > density.px(24)).then(|| (height - unused).max(self.ui.bounds().h / 4))
    }

    /// [`App::rebuild_reader`]'s one pass; `fit` caps the popup's height.
    fn build_reader(&mut self, refresh: Refresh, fit: Option<i32>) -> Result<(), Error> {
        let styles = self.styles.clone();
        let dicts: Vec<String> = self
            .dict
            .as_ref()
            .map(|d| d.dicts.iter().map(|x| x.label.clone()).collect())
            .unwrap_or_default();
        // Switcher tabs share the title row with the word: short labels
        // keep their width, long names are shortened to what is left.
        let shown: Vec<usize> = match self.reading().map(|r| &r.overlay) {
            Some(Overlay::Dict { lookup, .. }) => lookup.order.clone(),
            _ => Vec::new(),
        };
        let dicts = if shown.is_empty() {
            dicts
        } else {
            let row =
                (self.ui.bounds().w - 2 * styles.density.px(aristotle_ui::theme::margin())) as f32;
            let style = styles.ui_bold();
            let text = &mut self.ui.text;
            let widths: Vec<f32> = shown
                .iter()
                .filter_map(|&i| dicts.get(i))
                .map(|d| text.measure(d, &style))
                .collect();
            let max = overlay::tab_width(
                &widths,
                row - styles.density.scale(overlay::TITLE_MIN),
                styles.density,
            );
            dicts
                .iter()
                .map(|d| overlay::fit_label(d, max, |s| text.measure(s, &style)))
                .collect()
        };
        let h = self.ui.bounds().h;
        let panel_size = self.ui.bounds().size();
        // Popup text the UI font can draw (see `overlay::printable`), and
        // Greek in dictionaries without length marks.
        let body =
            match self.reading().map(|r| &r.overlay) {
                Some(Overlay::Dict { lookup, full, .. }) => Some(overlay::dictionary_text(
                    &dict_body(lookup, dicts.is_empty(), *full),
                )),
                Some(Overlay::Gloss { text, .. }) => Some(overlay::dictionary_text(text)),
                Some(Overlay::Note { text, .. }) => Some(text.clone()),
                Some(Overlay::Annotation { text, .. }) => Some(text.clone()),
                Some(Overlay::WereadThought { text, .. }) => Some(text.clone()),
                Some(Overlay::Sync { text, .. }) => Some(RichText::plain(text)),
                _ => None,
            }
            .map(|b| overlay::printable(&self.ui.text, &styles, &b))
            .unwrap_or_default();
        // The dictionary popup's words the same way: the headword (the
        // title), the tapped word and the suggestions.
        // The buttons of the overlay, measured with the text engine (the
        // tree does not own it), and the room they have.
        let buttons = self.overlay_buttons(&styles);
        let available = sheet::available_width(&styles, self.ui.bounds().w);
        let shown_lookup = match self.reading().map(|r| &r.overlay) {
            Some(Overlay::Dict { lookup, .. }) => Some(lookup.clone()),
            _ => None,
        }
        .map(|mut l| {
            let text = &self.ui.text;
            let clean = |s: &str, style: &aristotle_ui::text::TextStyle| {
                overlay::printable_text(&overlay::without_greek_length(s), |x| {
                    text.covers(x, style)
                })
            };
            {
                l.word = clean(&l.word, &styles.small());
                for e in l.found.iter_mut().flat_map(|f| f.entries.iter_mut()) {
                    e.headword = clean(&e.headword, &styles.title());
                }
                for s in &mut l.suggestions {
                    *s = clean(s, &styles.ui());
                }
            }
            l
        });
        // The typeset panel measures its labels with the text engine, so its
        // view is taken out before the reading state is borrowed.
        let typeset_view: Option<Box<aristotle_ui::screens::typeset::TypesetView>> =
            match self.reading().map(|r| &r.overlay) {
                Some(Overlay::Typeset { view, .. }) => Some((*view).clone()),
                _ => None,
            };
        let Some(book_id) = self.reading().map(|r| r.session.id.clone()) else {
            return Ok(());
        };
        let sync_channels = self.reader_sync_channels_all(&book_id);
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let page = r.session.page(r.session.current)?;
        let page_words = r.session.words(r.session.current);
        let mut nodes = OverlayNodes::default();
        let overlay = &r.overlay;
        let mut extra = |t: &mut Tree| -> Vec<NodeId> {
            let mut layers = Vec::new();
            if let Some((sel, words)) = overlay.selection() {
                let handles = matches!(overlay, Overlay::Dragging { .. } | Overlay::Actions { .. });
                let boxes = selection::line_boxes(words, sel.start, sel.end);
                let n = overlay::selection_layer(t, SelectionMarks { boxes, handles });
                nodes.selection = Some(n);
                layers.push(n);
            }
            let word = overlay.selection().and_then(|(s, w)| {
                let first = w.get(s.start)?.rect;
                let last = w.get(s.end)?.rect;
                Some((first.y0, last.y1))
            });
            let place = Place::away_from(word.map(|(t, b)| (t + b) / 2), h);
            // As tall as the room away from the word allows (less a line,
            // which the edge may grow by to fall between lines), so a
            // common word's senses fit on one popup page; then fitted to
            // the content.
            let room = match (word, place) {
                (Some((_, bottom)), Place::Bottom) => h - bottom - styles.density.px(70),
                (Some((top, _)), Place::Top) => top - styles.density.px(70),
                (None, _) => styles.density.px(overlay::POPUP_H),
            };
            let max = fit.unwrap_or(room.clamp(h / 4, h * 3 / 4));
            let height = popup_height(&page_words, place, h, max, styles.density);
            let built = match overlay {
                Overlay::Dict { lookup, .. } => Some(dict_popup(
                    t,
                    &styles,
                    shown_lookup.as_ref().unwrap_or(lookup),
                    &dicts,
                    &body,
                    &buttons,
                    available,
                    place,
                    height,
                )),
                Overlay::Waiting { .. } | Overlay::Dragging { .. } | Overlay::None => None,
                Overlay::Actions { .. } => {
                    let sheet = selection_ui::build(
                        t,
                        &styles,
                        tr(Msg::ReaderSelection),
                        &buttons,
                        available,
                        place.into(),
                    );
                    layers.push(sheet.root);
                    None
                }
                Overlay::Translate { source, result, .. } => {
                    let mut view = translate_view(source, result.as_ref());
                    view.buttons = buttons.clone();
                    let ui = translate_ui::build(t, &styles, &view, available, place.into(), h / 3);
                    nodes.pages = Some(ui.pages);
                    nodes.pager = Some(ui.pager);
                    nodes.arrows = Some(ui.arrows);
                    layers.push(ui.root);
                    None
                }
                Overlay::Gloss { .. } => Some(overlay::popup(
                    t,
                    &styles,
                    &PopupSpec {
                        title: tr(Msg::ReaderGlossTitle).into(),
                        subtitle: Some(tr(Msg::ReaderGlossSubtitle).into()),
                        tabs: Vec::new(),
                        body: &body,
                        suggestions: Vec::new(),
                        actions: buttons.clone(),
                        available,
                        place,
                        height,
                    },
                )),
                Overlay::Note { .. } => Some(overlay::popup(
                    t,
                    &styles,
                    &PopupSpec {
                        title: tr(Msg::ReaderFootnote).into(),
                        subtitle: None,
                        tabs: Vec::new(),
                        body: &body,
                        suggestions: Vec::new(),
                        actions: buttons.clone(),
                        available,
                        place: Place::Bottom,
                        height: popup_height(&page_words, Place::Bottom, h, max, styles.density),
                    },
                )),
                Overlay::Annotation { .. } => Some(overlay::popup(
                    t,
                    &styles,
                    &PopupSpec {
                        title: tr(Msg::ReaderImportedAnnotation).into(),
                        subtitle: None,
                        tabs: Vec::new(),
                        body: &body,
                        suggestions: Vec::new(),
                        actions: buttons.clone(),
                        available,
                        place: Place::Bottom,
                        height: popup_height(&page_words, Place::Bottom, h, max, styles.density),
                    },
                )),
                Overlay::WereadThought { .. } => Some(overlay::popup(
                    t,
                    &styles,
                    &PopupSpec {
                        title: tr(Msg::WereadNotesThought).into(),
                        subtitle: None,
                        tabs: Vec::new(),
                        body: &body,
                        suggestions: Vec::new(),
                        actions: buttons.clone(),
                        available,
                        place: Place::Bottom,
                        height: popup_height(&page_words, Place::Bottom, h, max, styles.density),
                    },
                )),
                Overlay::Sync { .. } => Some(overlay::popup(
                    t,
                    &styles,
                    &PopupSpec {
                        title: tr(Msg::ReaderSyncTitle).into(),
                        subtitle: None,
                        tabs: Vec::new(),
                        body: &body,
                        suggestions: Vec::new(),
                        actions: buttons.clone(),
                        available,
                        place: Place::Bottom,
                        height: popup_height(&page_words, Place::Bottom, h, max, styles.density),
                    },
                )),
                Overlay::Toc { contents, .. } => {
                    let l = toc_ui::build(
                        t,
                        &styles,
                        contents,
                        tr(Msg::MenuContents),
                        tr(Msg::ReaderNoContents),
                    );
                    nodes.pages = Some(l.pages);
                    nodes.pager = Some(l.pager);
                    // The pager's own buttons stay at the ends of the
                    // list and grey out there, so the generic pager
                    // (which hides its arrows) is not used here.
                    nodes.toc = Some(l);
                    layers.push(l.root);
                    None
                }
                Overlay::Marks { rows, .. } => {
                    let rows: Vec<ListRow> = rows
                        .iter()
                        .map(|m| ListRow {
                            text: m.text.clone(),
                            meta: Some(m.meta.clone()),
                            indent: 0,
                            current: false,
                            lines: 3,
                            family: None,
                        })
                        .collect();
                    let l = overlay::list(
                        t,
                        &styles,
                        tr(Msg::MenuHighlights),
                        &rows,
                        tr(Msg::ReaderNoHighlights),
                    );
                    nodes.pages = Some(l.pages);
                    nodes.pager = Some(l.pager);
                    nodes.arrows = Some(l.arrows);
                    layers.push(l.root);
                    None
                }
                Overlay::Fonts { target, rows, .. } => {
                    let rows: Vec<ListRow> = rows
                        .iter()
                        .map(|f| ListRow {
                            text: f.name.clone(),
                            meta: f.meta.clone(),
                            indent: 0,
                            current: false,
                            lines: 1,
                            family: f.ui,
                        })
                        .collect();
                    let title = match target {
                        crate::fonts::FontTarget::Latin => tr(Msg::MenuFontLatin),
                        crate::fonts::FontTarget::Cjk => tr(Msg::MenuFontCjk),
                    };
                    let l = overlay::list(t, &styles, title, &rows, "");
                    nodes.pages = Some(l.pages);
                    nodes.pager = Some(l.pager);
                    nodes.arrows = Some(l.arrows);
                    layers.push(l.root);
                    None
                }
                Overlay::GoTo { value } => {
                    let g = overlay::goto_sheet(t, &styles, *value, "");
                    nodes.slider = Some(g.slider);
                    nodes.label = Some(g.label);
                    layers.push(g.root);
                    None
                }
                // The typeset panel is built after the reading state is
                // released: it measures its labels with the text engine.
                Overlay::Typeset { .. } => None,
            };
            if let Some(p) = built {
                nodes.pages = Some(p.pages);
                nodes.pager = Some(p.pager);
                nodes.arrows = Some(p.arrows);
                nodes.sheet = Some(p.sheet);
                layers.push(p.root);
            }
            layers
        };
        let (mut tree, menu, page_node, nav, progress) = crate::reader_tree_with_sync_channels(
            &styles,
            &r.session.title,
            &r.settings,
            page,
            (0, 0),
            sync_channels,
            &mut extra,
        );
        r.menu = menu;
        r.page_node = page_node;
        r.nav = nav;
        r.progress = progress;
        r.nodes = nodes;
        // The typeset panel goes over the page, under the menu.
        if let Some(view) = typeset_view {
            let (typeset_root, _) = aristotle_ui::screens::typeset::build_typeset_screen_for_size(
                &mut tree,
                &mut self.ui.text,
                &styles,
                &view,
                panel_size,
            );
            if let Some(stack) = tree.root() {
                let mut children = tree.node(stack).children.clone();
                children.insert(1, typeset_root);
                tree.set_children(stack, children);
            }
        }
        self.ui.set_tree_with(tree, refresh);
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let menu_open = r.menu_open;
        let (root, nav) = (r.menu.root, r.nav);
        self.ui.tree.set_hidden(root, !menu_open);
        self.ui.tree.set_hidden(nav, !menu_open);
        if menu_open {
            if let Screen::Reader(r) = &mut self.screen {
                crate::update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
            }
        }
        self.sync_pager();
        Ok(())
    }

    /// Turn the overlay's `Pages` node to the overlay's page and label it.
    fn sync_pager(&mut self) {
        self.ui.layout();
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        let want = r.overlay.page_mut().map(|p| *p).unwrap_or(0);
        let (Some(pages), Some(pager)) = (r.nodes.pages, r.nodes.pager) else {
            return;
        };
        self.ui.tree.set_page(pages, want);
        self.ui.layout();
        let (p, n) = self.ui.tree.page(pages);
        if let Some(cur) = r.overlay.page_mut() {
            *cur = p;
        }
        if let Some(toc) = r.nodes.toc {
            // The contents list's own pager: the buttons stay at the
            // ends of the list and grey out there.
            toc.sync(&mut self.ui.tree);
            self.ui.layout();
            return;
        }
        let label = if n > 1 {
            format!("{}/{}", p + 1, n)
        } else {
            String::new()
        };
        self.ui.tree.set_text(pager, &label);
        if let Some((a, b)) = r.nodes.arrows {
            self.ui.tree.set_hidden(a, n <= 1);
            self.ui.tree.set_hidden(b, n <= 1);
        }
        self.ui.layout();
    }

    /// Send the frame as one update: grays go out as a full-screen mode-9
    /// update (clean, like a page turn), black and white changes as DU.
    pub(crate) fn present_full(&mut self) -> Result<(), Error> {
        let damage = self.ui.render();
        self.panel.present(&self.ui, &damage, Content::Page)?;
        Ok(())
    }

    /// Send the frame with the UI's own waveforms (A2 feedback, DU, GL16).
    fn present_fast(&mut self) -> Result<(), Error> {
        let damage = self.ui.render();
        self.panel.present(&self.ui, &damage, Content::Chrome)?;
        Ok(())
    }

    /// Send the frame's changes as cleaning updates (full-mode mode 9 of
    /// the changed rectangle): gray text that replaces gray text, or a
    /// selection drawn over it, must not leave the old pixels behind.
    fn present_clean(&mut self) -> Result<(), Error> {
        let damage = self.ui.render_clean();
        self.panel.present(&self.ui, &damage, Content::Chrome)?;
        Ok(())
    }

    /// Take the overlay away: the page comes back with one full mode-9
    /// update of the whole screen.
    pub(crate) fn close_overlay(&mut self) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let was_typeset = matches!(r.overlay, Overlay::Typeset { .. });
        let settings = r.settings;
        r.overlay = Overlay::None;
        r.gestures.reset();
        self.rebuild_reader(Refresh::Auto)?;
        // The UI's copy of the screen follows; the panel gets the page.
        self.ui.render();
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let page = r.session.page(r.session.current)?;
        self.panel.show_page(&page, false, "overlay closed")?;
        self.page_changed();
        if was_typeset {
            self.request_restyle(settings)?;
        }
        Ok(())
    }

    /// After the page on the panel changed: quiet zones for the gestures
    /// (links, the footnote button when present, and the bookmark corner).
    pub(crate) fn page_changed(&mut self) {
        let screen = self.ui.bounds();
        let density = self.ui.profile().density;
        let target = min_target(density) * 2 / 3;
        let Some(r) = self.reading() else {
            return;
        };
        let loc = r.session.current;
        let links = r.session.links(loc);
        let mut zones: Vec<Rect> = links
            .iter()
            .map(|l| rect(&l.rect).expand_to(target))
            .collect();
        if !r.session.footnotes(loc).is_empty() {
            zones.push(crate::turns::footnote_button_at(screen, density));
        }
        r.gestures.set_quiet(zones);
    }

    /// Show the page at `location` (a jump: contents, highlights, a link)
    /// with one full update, and close whatever covered it.
    pub(crate) fn jump_to(&mut self, location: usize) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let degraded = r.session.degraded;
        r.session.current = location;
        r.menu_open = false;
        if degraded {
            self.restyler.user_action(Instant::now());
        }
        self.close_overlay()?;
        let now = aristotle_library::library::now();
        if let Screen::Reader(r) = &mut self.screen {
            self.lib
                .set_progress(&r.session.id, r.session.progress(now));
            r.session.prerender();
        }
        self.speed.page_shown(Instant::now());
        self.debounce.changed(Instant::now());
        Ok(())
    }

    // ---------------------------------------------------------- gestures

    /// A long press at `at`.
    pub(crate) fn hold(&mut self, at: Point) -> Result<(), Error> {
        let t0 = Instant::now();
        if self.restyler.running() {
            return Ok(());
        }
        let Some(r) = self.reading() else {
            return Ok(());
        };
        if !r.overlay.is_none() || r.menu_open {
            return Ok(());
        }
        let loc = r.session.current;
        let words = r.session.words(loc);
        let Some(i) = selection::hit(&words, at.x, at.y, self.styles.density.px(selection::SLOP))
        else {
            return Ok(());
        };
        let sel = Selection::word(&words, i);
        let word = sel.text(&words);
        let highlight = self.highlight_at(words[sel.start].offset);
        let lang = self.book_lang();
        let Some(r) = self.reading() else {
            return Ok(());
        };
        r.overlay = Overlay::Waiting {
            sel,
            words: words.clone(),
        };
        let reply = match &self.dict {
            Some(d) if !d.is_empty() => {
                d.ask(Request::Lookup {
                    word: word.clone(),
                    book_lang: lang,
                });
                d.wait(LOOKUP_PATIENCE)
            }
            _ => None,
        };
        let no_dicts = self.dict.as_ref().is_none_or(|d| d.is_empty());
        match reply {
            Some(Reply::Lookup(l)) => {
                perf!("hold: \"{word}\" looked up in {:.1} ms", l.millis);
                self.show_lookup(l, highlight)?;
            }
            _ if no_dicts => {
                let l = Lookup {
                    word: word.clone(),
                    lang: String::new(),
                    order: Vec::new(),
                    shown: None,
                    found: None,
                    suggestions: Vec::new(),
                    indexing: false,
                    millis: 0.0,
                };
                self.show_lookup(l, highlight)?;
            }
            _ => {
                // The dictionary is slow: the word first, the popup when
                // the answer comes (see `background`).
                perf!("hold: \"{word}\" selected, dictionary still looking");
                // Not A2: it would crush the gray text under the marker.
                self.rebuild_reader(Refresh::Auto)?;
                self.present_clean()?;
            }
        }
        perf!(
            "hold: shown {:.0} ms after the long press",
            crate::ms(t0.elapsed())
        );
        Ok(())
    }

    /// The dictionary answered: show the popup (one update).
    fn show_lookup(&mut self, l: Lookup, highlight: Option<usize>) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let (sel, words) = match r.overlay.selection() {
            Some((s, w)) => (*s, w.clone()),
            None => return Ok(()),
        };
        r.overlay = Overlay::Dict {
            sel,
            words,
            lookup: Box::new(l),
            highlight,
            page: 0,
            full: false,
        };
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// Work between events: a late dictionary answer.
    pub(crate) fn poll_lookup(&mut self) -> Result<(), Error> {
        let waiting = matches!(
            self.reading().map(|r| &r.overlay),
            Some(Overlay::Waiting { .. })
        );
        if !waiting {
            return Ok(());
        }
        let Some(reply) = self.dict.as_ref().and_then(|d| d.wait(Duration::ZERO)) else {
            return Ok(());
        };
        if let Reply::Lookup(l) = reply {
            let highlight = match self.reading().map(|r| &r.overlay) {
                Some(Overlay::Waiting { sel, words }) => {
                    let off = words[sel.start].offset;
                    self.highlight_at(off)
                }
                _ => None,
            };
            self.show_lookup(l, highlight)?;
        }
        Ok(())
    }

    /// The finger moved while holding: grow the selection.
    pub(crate) fn drag(&mut self, p: Point) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let (mut sel, words, had_popup) = match &r.overlay {
            Overlay::Waiting { sel, words } => (*sel, words.clone(), false),
            Overlay::Dict { sel, words, .. } => (*sel, words.clone(), true),
            Overlay::Dragging { sel, words } => (*sel, words.clone(), false),
            _ => return Ok(()),
        };
        let Some(j) = nearest(&words, p) else {
            return Ok(());
        };
        if !sel.extend_to(&words, j) {
            return Ok(());
        }
        let first = !matches!(r.overlay, Overlay::Dragging { .. });
        r.overlay = Overlay::Dragging {
            sel,
            words: words.clone(),
        };
        if first {
            // Handles appear; a popup, if there was one, goes with a clean
            // full update.
            self.rebuild_reader(Refresh::Auto)?;
            if had_popup {
                return self.present_full();
            }
            if let Some(n) = self.reading().and_then(|r| r.nodes.selection) {
                self.ui.tree.invalidate(n, Refresh::Transient);
            }
            return self.present_fast();
        }
        let Some(n) = self.reading().and_then(|r| r.nodes.selection) else {
            return Ok(());
        };
        if let Some(w) = self.ui.tree.widget_mut::<SelectionMarks>(n) {
            w.boxes = selection::line_boxes(&words, sel.start, sel.end);
        }
        self.ui.tree.invalidate(n, Refresh::Transient);
        self.present_fast()
    }

    /// The finger lifted after a hold.
    pub(crate) fn release(&mut self, _p: Point) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        if let Overlay::Dragging { sel, words } = &r.overlay {
            r.overlay = Overlay::Actions {
                sel: *sel,
                words: words.clone(),
            };
            self.rebuild_reader(Refresh::Auto)?;
            self.present_full()?;
        }
        Ok(())
    }

    /// A tap in a quiet zone: the bookmark corner, the footnote button, or a link.
    pub(crate) fn quiet_tap(&mut self, p: Point) -> Result<(), Error> {
        if self.restyler.running() {
            return Ok(());
        }
        let screen = self.ui.bounds();
        let density = self.ui.profile().density;
        if crate::turns::bookmark_corner_at(screen, density).contains(p) {
            return self.toggle_bookmark();
        }
        if crate::turns::footnote_button_at(screen, density).contains(p) {
            let notes = self
                .reading()
                .map(|r| {
                    let loc = r.session.current;
                    r.session.footnotes(loc)
                })
                .unwrap_or_default();
            if !notes.is_empty() {
                let (text, link) = format_page_footnotes(&notes);
                if let Some(r) = self.reading() {
                    r.overlay = Overlay::Note {
                        text,
                        link,
                        page: 0,
                    };
                }
                self.ui.reset_gestures();
                self.rebuild_reader(Refresh::Auto)?;
                return self.present_full();
            }
        }
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let loc = r.session.current;
        let links = r.session.links(loc);
        let Some(link) = links
            .iter()
            .filter(|l| {
                rect(&l.rect)
                    .expand_to(min_target(density) * 2 / 3)
                    .contains(p)
            })
            .min_by_key(|l| l.rect.dist2(p.x, p.y))
            .cloned()
        else {
            return Ok(());
        };
        let words = r.session.words(loc);
        let marker: Vec<&str> = words
            .iter()
            .filter(|w| {
                let (x, y) = ((w.rect.x0 + w.rect.x1) / 2, (w.rect.y0 + w.rect.y1) / 2);
                link.rect.contains(x, y)
            })
            .map(|w| w.text.as_str())
            .collect();
        let marker = marker.join("");
        let t = Instant::now();
        let note = r.session.book().footnote(&link, &marker);
        perf!(
            "link {:?} ({:?}): {} in {:.0} ms",
            link.uri,
            marker,
            if note.is_some() { "footnote" } else { "a jump" },
            crate::ms(t.elapsed())
        );
        match note {
            Some(text) => {
                r.overlay = Overlay::Note {
                    text,
                    link: Some(link),
                    page: 0,
                };
                self.ui.reset_gestures();
                self.rebuild_reader(Refresh::Auto)?;
                self.present_full()
            }
            None => match r.session.book().link_page(loc, &link.uri) {
                Some(target) => self.jump_to(target),
                None => Ok(()),
            },
        }
    }

    // ------------------------------------------------------ marks

    fn book_lang(&mut self) -> Option<String> {
        let r = self.reading()?;
        r.session.book().language().map(String::from)
    }

    /// Plato locations of this book's highlights and bookmarks, from the
    /// index, into the session (pages are redrawn with them).
    pub(crate) fn reload_marks(&mut self) {
        // The cloud notes are placed from the cache: this reads memory only.
        let hash = match &self.screen {
            Screen::Reader(r) => r.session.id.clone(),
            _ => return,
        };
        let cloud = self.weread_notes_positions(&hash);
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        let Some(rec) = self.lib.book(&r.session.id) else {
            return;
        };
        let mut raw_hl: Vec<(Option<usize>, Position, Position)> =
            marks::ours_versioned(rec, AnnotationKind::Highlight)
                .into_iter()
                .filter_map(|(id, s, e, version)| {
                    let s = r.session.map_stored_position(&s, version)?;
                    let e = r.session.map_stored_position(&e, version)?;
                    Some((Some(id), s, e))
                })
                .collect();
        // Cloud highlights reuse the reader's mark path; they have no
        // annotation index, so tapping one never offers "remove".
        raw_hl.extend(cloud);
        let raw_bm: Vec<Position> = marks::ours_versioned(rec, AnnotationKind::Bookmark)
            .into_iter()
            .filter_map(|(_, s, _, version)| r.session.map_stored_position(&s, version))
            .collect();
        let book = r.session.book();
        let hl = raw_hl
            .iter()
            .filter_map(|(id, s, e)| book.resolve_mark(*id, s, e))
            .collect();
        let bm = raw_bm.iter().filter_map(|p| book.location_of(p)).collect();
        r.session.raw_marks = raw_hl;
        r.session.raw_bookmarks = raw_bm;
        r.session.set_marks(hl, bm);
    }

    /// The annotation index of a highlight covering `offset`.
    fn highlight_at(&mut self, offset: usize) -> Option<usize> {
        let Screen::Reader(r) = &mut self.screen else {
            return None;
        };
        let rec = self.lib.book(&r.session.id)?;
        let book = r.session.book();
        if book.engine() == aristotle_render::book::Engine::Plato {
            marks::ours(rec, AnnotationKind::Highlight)
                .into_iter()
                .find(|(_, s, e)| {
                    matches!(book.mark_range(s, e), Some((a, b)) if a <= offset && offset <= b)
                })
                .map(|(i, _, _)| i)
        } else {
            let page = r.session.current;
            let page_has_marks = r.session.deco.marks.iter().any(|m| match m {
                aristotle_render::book::Mark::Crengine(cm) => {
                    cm.start_page <= page && page <= cm.end_page
                }
                _ => false,
            });
            if !page_has_marks {
                return None;
            }
            let words = r.session.words(page);
            for m in &r.session.deco.marks {
                let aristotle_render::book::Mark::Crengine(cm) = m else {
                    continue;
                };
                if cm.start_page > page || page > cm.end_page {
                    continue;
                }
                let start_idx = if page > cm.start_page {
                    0
                } else {
                    aristotle_render::book::mark_start_word_index(
                        &words,
                        cm.start_xpointer.as_deref(),
                    )
                };
                let end_idx = if page < cm.end_page {
                    words.len().saturating_sub(1)
                } else {
                    aristotle_render::book::mark_end_word_index(&words, cm.end_xpointer.as_deref())
                };
                if start_idx <= end_idx && end_idx < words.len() {
                    let s_off = words[start_idx].offset;
                    let e_off = words[end_idx].offset;
                    if s_off <= offset && offset <= e_off {
                        return cm.id;
                    }
                }
            }
            None
        }
    }

    /// Save the selection as a highlight, export it, close.
    fn highlight_selection(&mut self) -> Result<(), Error> {
        let created = marks::local_time(aristotle_library::library::now());
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let Some((sel, words)) = r.overlay.selection().map(|(s, w)| (*s, w.clone())) else {
            return Ok(());
        };
        let (s, e) = (words[sel.start].offset, words[sel.end].offset);
        let txt_version = r.session.txt_version();
        let book = r.session.book();
        let start = match &words[sel.start].start_xpointer {
            Some(xp) => Position::XPointer {
                xpointer: xp.clone(),
            },
            None => book.position_at(s),
        };
        let end = match &words[sel.end].end_xpointer {
            Some(xp) => Position::XPointer {
                xpointer: xp.clone(),
            },
            None => book.position_at(e),
        };
        let bytes = |p: &Position, loc: usize| match p {
            Position::Kindle(k) => *k,
            _ => loc as u64,
        };
        let fixed = book.engine() != Engine::Plato;
        let page_idx = if s >= (1 << 16) { s >> 16 } else { s };
        let a = marks::highlight(marks::NewHighlight {
            text: sel.quote(&words),
            start_bytes: bytes(&start, s),
            end_bytes: bytes(&end, e) + words[sel.end].text.len() as u64,
            start,
            end,
            txt_version,
            page: fixed.then_some(page_idx + 1),
            chapter: book.chapter_title(page_idx),
            created,
        });
        let id = r.session.id.clone();
        perf!("highlight: {:?}", a.text);
        self.lib.add_annotation(&id, a);
        self.saver.save(&self.lib);
        self.debounce.saved();
        if let Some(rec) = self.lib.book(&id) {
            marks::export(&self.exports, rec, self.prefs.clippings);
        }
        self.reload_marks();
        self.close_overlay()?;
        // The page now shows the change: the next open's snapshot too.
        self.save_snapshot();
        Ok(())
    }

    fn remove_highlight(&mut self, index: usize) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let id = r.session.id.clone();
        let target = self
            .lib
            .book(&id)
            .and_then(|b| b.annotations.get(index))
            .and_then(|a| a.position.clone());
        if let Some(pos) = target {
            self.lib.remove_annotations(&id, |a| {
                marks::is_editable_source(&a.source)
                    && a.kind == AnnotationKind::Highlight
                    && a.position.as_ref() == Some(&pos)
            });
            self.saver.save(&self.lib);
            self.debounce.saved();
            if let Some(rec) = self.lib.book(&id) {
                marks::export(&self.exports, rec, self.prefs.clippings);
            }
        }
        self.reload_marks();
        self.close_overlay()?;
        // The page now shows the change: the next open's snapshot too.
        self.save_snapshot();
        Ok(())
    }

    /// The top right corner: add or remove a bookmark on this page. The
    /// ribbon is black on white, so it changes with a quick DU update.
    pub(crate) fn toggle_bookmark(&mut self) -> Result<(), Error> {
        let created = marks::local_time(aristotle_library::library::now());
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let id = r.session.id.clone();
        let loc = r.session.current;
        match r.session.bookmarked(loc) {
            Some(b) => {
                let book = r.session.book();
                let at = book.position_at(b);
                self.lib.remove_annotations(&id, |a| {
                    marks::is_editable_source(&a.source)
                        && a.kind == AnnotationKind::Bookmark
                        && a.position
                            .as_ref()
                            .and_then(|p| marks::decode(&p.0))
                            .is_some_and(|pos| pos == at || book.location_of(&pos) == Some(b))
                });
                perf!("bookmark removed");
            }
            None => {
                let txt_version = r.session.txt_version();
                let book = r.session.book();
                let at = book.position_at(loc);
                let bytes = match at {
                    Position::Kindle(k) => k,
                    _ => loc as u64,
                };
                let chapter = book.chapter_title(loc);
                self.lib.add_annotation(
                    &id,
                    marks::bookmark(&at, bytes, chapter, created, txt_version),
                );
                perf!("bookmark added");
            }
        }
        self.saver.save(&self.lib);
        self.debounce.saved();
        self.reload_marks();
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let page = r.session.page(r.session.current)?;
        if let Some(w) = self.ui.tree.widget_mut::<crate::PageView>(r.page_node) {
            w.page = page;
        }
        self.ui.tree.invalidate(r.page_node, Refresh::Auto);
        self.present_fast()?;
        // The page now shows the change: the next open's snapshot too.
        self.save_snapshot();
        Ok(())
    }

    // ------------------------------------------------------ lists

    pub(crate) fn open_toc(&mut self) -> Result<(), Error> {
        if self.restyler.running() {
            return Ok(());
        }
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let loc = r.session.current;
        let items = r.session.book().toc_items();
        let current = r.session.book().toc_current(loc);
        // The list is folded: only the group the reader is in is open,
        // so the contents open at the chapter being read.
        let entries = items
            .into_iter()
            .map(|it| toc_ui::Entry {
                title: if it.title.is_empty() {
                    "—".into()
                } else {
                    it.title
                },
                depth: it.depth,
                index: it.index,
                page: it.page,
            })
            .collect();
        let contents = toc_ui::Contents::new(entries, current);
        r.overlay = Overlay::Toc {
            contents: Box::new(contents),
            page: 0,
        };
        r.menu_open = false;
        self.rebuild_reader(Refresh::Auto)?;
        // Turn the list to the current entry.
        let target = match self.reading() {
            Some(r) => match (&r.overlay, r.nodes.pages) {
                (Overlay::Toc { contents, .. }, Some(pages)) => {
                    contents.current_row().map(|row| (row, pages))
                }
                _ => None,
            },
            None => None,
        };
        if let Some((row, pages)) = target {
            let node = self.ui.tree.node(pages).children.get(row).copied();
            let mut p = 0;
            loop {
                self.ui.tree.set_page(pages, p);
                self.ui.layout();
                let (cur, n) = self.ui.tree.page(pages);
                // Only rows on the shown page can be hit.
                let visible = node.is_some_and(|id| {
                    let r = self.ui.tree.rect(id);
                    let c = Point {
                        x: r.x + r.w / 2,
                        y: r.y + r.h / 2,
                    };
                    self.ui.tree.hit(c) == aristotle_ui::tree::Hit::Target(id)
                });
                if visible || cur + 1 >= n {
                    if let Some(Overlay::Toc { page, .. }) = self.reading().map(|r| &mut r.overlay)
                    {
                        *page = cur;
                    }
                    break;
                }
                p += 1;
            }
        }
        self.sync_pager();
        self.present_full()
    }

    /// What a tap in the contents list means.
    fn toc_action(&mut self, action: toc_ui::TocAction) -> Result<(), Error> {
        match action {
            toc_ui::TocAction::Close => self.close_overlay(),
            toc_ui::TocAction::PrevPage => self.overlay_page(-1),
            toc_ui::TocAction::NextPage => self.overlay_page(1),
            toc_ui::TocAction::Select(row) => self.list_item(row),
            toc_ui::TocAction::Toggle(row) => self.toc_toggle(row),
        }
    }

    /// Folds the children of the chapter on a row of the contents list
    /// open or shut, and draws the list again.
    fn toc_toggle(&mut self, row: usize) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Overlay::Toc { contents, .. } = &mut r.overlay else {
            return Ok(());
        };
        if !contents.toggle_row(row) {
            return Ok(());
        }
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// A page-turn key while the contents list is open: it turns the
    /// list's own pages, not the book's.
    pub(crate) fn toc_page_key(&mut self, dir: Turn) -> Result<(), Error> {
        let open = matches!(
            self.reading().map(|r| &r.overlay),
            Some(Overlay::Toc { .. })
        );
        if !open {
            return Ok(());
        }
        self.overlay_page(match dir {
            Turn::Forward => 1,
            Turn::Back => -1,
        })
    }

    pub(crate) fn open_marks(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let mut rows = Vec::new();
        if let Some(rec) = self.lib.book(&r.session.id) {
            let mobi = r.session.book().mobi().map(|m| m.info.clone());
            let mut all: Vec<_> = rec
                .annotations
                .iter()
                .map(|a| {
                    let p = marks::target(a, rec.format, mobi.as_ref())
                        .filter(|p| r.session.book().location_of(p).is_some());
                    let loc = p.as_ref().and_then(|p| r.session.book().location_of(p));
                    (loc.unwrap_or(usize::MAX), a, p)
                })
                .filter(|(_, a, p)| a.source != marks::SOURCE || p.is_some())
                .collect();
            all.sort_by_key(|x| x.0);
            for (_, a, p) in all {
                let is_imported = !marks::is_editable_source(&a.source);
                let date = a.created.as_deref().map(|c| c.get(..10).unwrap_or(c));
                let meta: Vec<String> = [
                    (is_imported || p.is_none())
                        .then(|| tr(Msg::ReaderImportedAnnotation).to_string()),
                    a.chapter.clone(),
                    aristotle_library::export::location_note(a.location),
                    a.page
                        .as_ref()
                        .map(|page| tf!(Msg::ReaderAnnotationPage, page = page)),
                    date.map(String::from),
                ]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect();
                let text = match a.kind {
                    AnnotationKind::Bookmark => tr(Msg::ReaderBookmark).to_string(),
                    AnnotationKind::Note => a.text.clone().unwrap_or_default(),
                    _ => a
                        .text
                        .as_deref()
                        .map(aristotle_i18n::quoted)
                        .unwrap_or_else(|| tr(Msg::ReaderHighlight).to_string()),
                };
                let detail = (is_imported || a.note.is_some() || p.is_none()).then(|| {
                    let mut parts = vec![meta.join(" · "), text.clone()];
                    if let Some(note) = &a.note {
                        parts.push(note.clone());
                    }
                    if p.is_none() {
                        parts.push(tr(Msg::ReaderAnnotationNoPosition).to_string());
                    }
                    parts.join("\n\n")
                });
                rows.push(MarkRow {
                    text,
                    meta: meta.join(" · "),
                    target: p,
                    detail,
                    weread: false,
                });
            }
        }
        // The WeRead cloud's highlights and thoughts come after the local
        // ones, in page order. Their rows carry the thought (or the reason a
        // note has no position) as the detail sheet.
        if let Some(book_id) = self.weread.downloads.book_id_of_hash(&r.session.id) {
            if let Some(positions) = self.weread.downloads.positions_of(&book_id) {
                let cloud_rows = self.weread.notes.rows(&book_id, &positions);
                let mut cloud: Vec<(usize, MarkRow)> = cloud_rows
                    .into_iter()
                    .map(|row| {
                        let sort = row
                            .target
                            .as_ref()
                            .and_then(|position| r.session.book().location_of(position))
                            .unwrap_or(usize::MAX);
                        (
                            sort,
                            MarkRow {
                                text: row.text,
                                meta: row.meta,
                                target: row.target,
                                detail: row.detail,
                                weread: true,
                            },
                        )
                    })
                    .collect();
                cloud.sort_by_key(|(sort, _)| *sort);
                rows.extend(cloud.into_iter().map(|(_, row)| row));
            }
        }
        r.overlay = Overlay::Marks { rows, page: 0 };
        r.menu_open = false;
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    pub(crate) fn open_goto(&mut self) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let here = r.session.current;
        let value = r.session.book().fraction(here);
        r.overlay = Overlay::GoTo { value };
        r.menu_open = false;
        self.rebuild_reader(Refresh::Auto)?;
        self.update_goto_label();
        self.present_full()
    }

    fn update_goto_label(&mut self) {
        let Some(r) = self.reading() else {
            return;
        };
        let Overlay::GoTo { value } = r.overlay else {
            return;
        };
        let detail = match r.session.book().known_page_count() {
            Some(n) if matches!(r.session.book().engine(), Engine::Plato | Engine::Vertical) => {
                tf!(
                    Msg::ReaderPageOf,
                    page = ((value * n as f32).ceil() as usize).clamp(1, n),
                    count = n
                )
            }
            _ => String::new(),
        };
        let (label, slider) = (r.nodes.label, r.nodes.slider);
        if let Some(l) = label {
            self.ui
                .tree
                .set_text(l, &overlay::percent_label(value, &detail));
        }
        if let Some(s) = slider {
            if let Some(w) = self.ui.tree.widget_mut::<Slider>(s) {
                w.value = value;
            }
            self.ui.tree.invalidate(s, Refresh::Auto);
        }
    }

    /// The typeset bottom sheet: gathers installed fonts and current typography settings.
    pub(crate) fn open_typeset(&mut self) -> Result<(), Error> {
        let (menu_root, book_id, is_fixed, current_fixed, title, author, settings) = {
            let Screen::Reader(r) = &mut self.screen else {
                return Ok(());
            };
            r.menu_open = false;
            let book_id = r.session.id.clone();
            let is_fixed = r.session.is_fixed();
            let current_fixed = r.session.style.fixed_layout;
            let title = r.session.book().title().unwrap_or("").to_string();
            let author = r.session.book().author().unwrap_or_default();
            (
                r.menu.root,
                book_id,
                is_fixed,
                current_fixed,
                title,
                author,
                r.settings,
            )
        };
        self.ui.tree.set_hidden(menu_root, true);

        let data = self.lib.data_dir().to_path_buf();
        let user_families = crate::fonts::families(&data, &self.fonts_dir);

        // Gather CJK fonts: default "思源黑体" (Noto Sans SC) first, then user CJK fonts
        let mut cjk_fonts = vec![aristotle_ui::screens::typeset::FontOption {
            name: crate::fonts::DEFAULT_CJK.into(),
            sample_font: FamilyId(0),
        }];
        let mut latin_fonts = vec![aristotle_ui::screens::typeset::FontOption {
            name: crate::fonts::DEFAULT_LATIN.into(),
            sample_font: FamilyId(0),
        }];
        for f in &user_families {
            if f.name == crate::fonts::DEFAULT_CJK || f.name == crate::fonts::DEFAULT_LATIN {
                continue;
            }
            let is_cjk = crate::fonts::classify_family(f) == crate::fonts::FontScript::Cjk;
            let ui = self.ui_family(f);
            let option = aristotle_ui::screens::typeset::FontOption {
                name: f.name.clone(),
                sample_font: ui,
            };
            if is_cjk {
                cjk_fonts.push(option);
            } else {
                latin_fonts.push(option);
            }
        }

        let selected_cjk = cjk_fonts
            .iter()
            .position(|f| f.name == self.prefs.font_cjk_in_use())
            .unwrap_or(0);
        let selected_latin = latin_fonts
            .iter()
            .position(|f| f.name == self.prefs.font_latin_in_use())
            .unwrap_or(0);

        let margin_idx = 2usize.saturating_sub(settings.margins);

        let orientation_idx = if is_fixed {
            self.prefs.orientation_for_book(&book_id)
        } else {
            self.prefs.orientation.min(3)
        };
        let view = aristotle_ui::screens::typeset::TypesetView {
            title,
            author,
            cjk_fonts,
            selected_cjk_font: selected_cjk,
            cjk_page: selected_cjk / aristotle_ui::screens::typeset::FONTS_PER_PAGE,
            latin_fonts,
            selected_latin_font: selected_latin,
            latin_page: selected_latin / aristotle_ui::screens::typeset::FONTS_PER_PAGE,
            size_index: settings
                .size
                .min(aristotle_ui::screens::typeset::SIZES_PX.len() - 1),
            font_size_half_pt: self.prefs.font_size_half_pt(),
            spacing_index: settings
                .spacing
                .min(aristotle_ui::screens::typeset::SPACING_RATIO.len() - 1),
            spacing_tenths: self.prefs.spacing_tenths(),
            spacing_pct: self.prefs.spacing_pct(),
            margin_index: margin_idx.min(aristotle_ui::screens::typeset::MARGINS_PX.len() - 1),
            margin_ref_px: self.prefs.margin_ref_px(),
            indent_index: self.prefs.cjk_indent.min(1),
            indent_tenths: self.prefs.cjk_indent_tenths(),
            // READER-001 C.4: script conversion, punctuation trim and page
            // animation are not offered; old reader.json fields are still
            // read, but they no longer shape the preview.
            script_index: 0,
            gap_index: self.prefs.cjk_gap.min(2),
            gap_tenths: self.prefs.cjk_gap_tenths(),
            underline_index: self.prefs.underline.min(2),
            underline_scope: self.prefs.underline_scope.min(1),
            fine_typesetting: self.prefs.fine_typesetting,
            embedded_fonts: self.prefs.embedded_fonts,
            fine_locks_fonts: false,
            fine_locks_spacing: false,
            font_weight: self.prefs.font_weight,
            writing_mode: self.prefs.writing_mode.min(1),
            orientation: orientation_idx,
            fixed_layout: is_fixed,
            fit_mode: current_fixed.fit.index(),
            page_direction: current_fixed.direction.index(),
            fullscreen: current_fixed.fullscreen,
            auto_crop: current_fixed.auto_crop,
            split_spread: current_fixed.split_spread,
            keep_images_in_night: current_fixed.keep_images_in_night,
            trim_index: 0,
            anim_enabled: false,
            anim_speed: 1,
        };

        if let Screen::Reader(r) = &mut self.screen {
            r.overlay = Overlay::Typeset {
                view: Box::new(view),
            };
        }
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// Handles actions within the typeset overlay sheet.
    pub(crate) fn handle_typeset_action(&mut self, id: ActionId) -> Result<(), Error> {
        use aristotle_ui::screens::typeset::{self, action as a};
        if id == a::COLLAPSE {
            return self.close_overlay();
        }
        if id == a::ADD_FONT {
            return self.show_wifi_transfer(Refresh::Full, crate::SoonBack::Reader);
        }

        let mut prefs_changed = false;
        let mut orientation_changed = false;
        let mut fixed_changed = false;
        let mut applied_orientation = self.prefs.orientation;
        // The guide is a render option: the book needs no new layout, only a
        // new render of the page.
        let mut underline_changed = false;

        match id {
            a::PREV_CJK_PAGE | a::NEXT_CJK_PAGE | a::PREV_LATIN_PAGE | a::NEXT_LATIN_PAGE => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                match id {
                    a::PREV_CJK_PAGE => view.cjk_page = view.cjk_page.saturating_sub(1),
                    a::NEXT_CJK_PAGE => {
                        let last = view.cjk_fonts.len().saturating_sub(1) / typeset::FONTS_PER_PAGE;
                        view.cjk_page = (view.cjk_page + 1).min(last);
                    }
                    a::PREV_LATIN_PAGE => view.latin_page = view.latin_page.saturating_sub(1),
                    a::NEXT_LATIN_PAGE => {
                        let last =
                            view.latin_fonts.len().saturating_sub(1) / typeset::FONTS_PER_PAGE;
                        view.latin_page = (view.latin_page + 1).min(last);
                    }
                    _ => {}
                }
            }
            i if (a::CJK_FONT_BASE..a::LATIN_FONT_BASE).contains(&i) => {
                let idx = (i - a::CJK_FONT_BASE) as usize;
                let font_name = {
                    let Screen::Reader(r) = &mut self.screen else {
                        return Ok(());
                    };
                    let Overlay::Typeset { view } = &mut r.overlay else {
                        return Ok(());
                    };
                    if idx >= view.cjk_fonts.len() || idx == view.selected_cjk_font {
                        return Ok(());
                    }
                    view.selected_cjk_font = idx;
                    view.cjk_fonts[idx].name.clone()
                };

                self.prefs.font_cjk = font_name.clone();
                let data = self.lib.data_dir().to_path_buf();
                let families = crate::fonts::families(&data, &self.fonts_dir);
                if font_name == crate::fonts::DEFAULT_CJK {
                    self.prefs.font_cjk_dir = None;
                    self.prefs.font_cjk_ui = 0;
                } else if let Some(fam) = families.iter().find(|f| f.name == font_name) {
                    if let Ok(dir) = crate::fonts::prepare(&data, fam) {
                        self.prefs.font_cjk_dir = Some(dir.to_string_lossy().into_owned());
                        self.prefs.font_cjk_ui = self.ui_family(fam).0 as usize;
                    }
                }
                if let Screen::Reader(r) = &mut self.screen {
                    r.settings.cjk_family = self.prefs.font_cjk_ui;
                }
                prefs_changed = true;
            }
            i if (a::LATIN_FONT_BASE..a::SIZE_DOWN).contains(&i) => {
                let idx = (i - a::LATIN_FONT_BASE) as usize;
                let font_name = {
                    let Screen::Reader(r) = &mut self.screen else {
                        return Ok(());
                    };
                    let Overlay::Typeset { view } = &mut r.overlay else {
                        return Ok(());
                    };
                    if idx >= view.latin_fonts.len() || idx == view.selected_latin_font {
                        return Ok(());
                    }
                    view.selected_latin_font = idx;
                    view.latin_fonts[idx].name.clone()
                };

                self.prefs.font_latin = font_name.clone();
                let data = self.lib.data_dir().to_path_buf();
                let families = crate::fonts::families(&data, &self.fonts_dir);
                if font_name == crate::fonts::DEFAULT_LATIN {
                    self.prefs.font_dir = None;
                    self.prefs.font_ui = 0;
                } else if let Some(fam) = families.iter().find(|f| f.name == font_name) {
                    if let Ok(dir) = crate::fonts::prepare(&data, fam) {
                        self.prefs.font_dir = Some(dir.to_string_lossy().into_owned());
                        self.prefs.font_ui = self.ui_family(fam).0 as usize;
                    }
                }
                if let Screen::Reader(r) = &mut self.screen {
                    r.settings.family = self.prefs.font_ui;
                }
                prefs_changed = true;
            }
            a::SIZE_DOWN | a::SIZE_UP => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                let next =
                    typeset::font_size_half_pt_stepped(view.font_size_half_pt, id == a::SIZE_UP);
                if next != view.font_size_half_pt {
                    view.font_size_half_pt = next;
                    self.prefs.font_size_half_pt = Some(next);
                    prefs_changed = true;
                }
            }
            a::SPACING_DOWN | a::SPACING_UP => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                let next = typeset::spacing_pct_stepped(view.spacing_pct, id == a::SPACING_UP);
                if next != view.spacing_pct {
                    view.spacing_pct = next;
                    view.spacing_tenths = (next / 10).min(typeset::SPACING_MAX_TENTHS as u16) as u8;
                    self.prefs.line_spacing_pct = Some(next);
                    prefs_changed = true;
                }
            }
            a::MARGIN_DOWN | a::MARGIN_UP => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                let next = typeset::margin_ref_px_stepped(view.margin_ref_px, id == a::MARGIN_UP);
                if next != view.margin_ref_px {
                    view.margin_ref_px = next;
                    self.prefs.margin_ref_px = Some(next);
                    prefs_changed = true;
                }
            }
            a::INDENT_DOWN | a::INDENT_UP => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                let next = typeset::indent_tenths_stepped(view.indent_tenths, id == a::INDENT_UP);
                if next != view.indent_tenths {
                    view.indent_tenths = next;
                    self.prefs.cjk_indent_tenths = Some(next);
                    prefs_changed = true;
                }
            }
            i if (a::SCRIPT_BASE..a::GAP_BASE).contains(&i) => {
                let idx = (i - a::SCRIPT_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx <= 1 && idx != view.script_index {
                    view.script_index = idx;
                    self.prefs.cjk_script = idx;
                    prefs_changed = true;
                }
            }
            a::GAP_DOWN | a::GAP_UP => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                let next = typeset::gap_tenths_stepped(view.gap_tenths, id == a::GAP_UP);
                if next != view.gap_tenths {
                    view.gap_tenths = next;
                    self.prefs.cjk_gap_tenths = Some(next);
                    prefs_changed = true;
                }
            }
            i if (a::TRIM_BASE..a::ANIM_BASE).contains(&i) => {
                let idx = (i - a::TRIM_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx <= 1 && idx != view.trim_index {
                    view.trim_index = idx;
                    self.prefs.cjk_trim = typeset::trim_is_on(idx);
                    prefs_changed = true;
                }
            }
            // The writing direction: horizontal or vertical. It picks the
            // engine, so it needs a new layout, and the sheet's own close
            // asks for one.
            i if (a::WRITING_MODE_BASE..a::WRITING_MODE_BASE + 2).contains(&i) => {
                let mode = (i - a::WRITING_MODE_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if mode != view.writing_mode {
                    view.writing_mode = mode;
                    self.prefs.writing_mode = mode;
                    prefs_changed = true;
                }
            }
            i if (a::UNDERLINE_BASE..a::UNDERLINE_BASE + 3).contains(&i) => {
                let idx = (i - a::UNDERLINE_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx <= 2 && idx != view.underline_index {
                    view.underline_index = idx;
                    self.prefs.underline = idx;
                    prefs_changed = true;
                    underline_changed = true;
                }
            }
            // How wide the guide is: a render option like the guide itself,
            // so the page is drawn again rather than laid out again.
            i if (a::UNDERLINE_SCOPE_BASE..a::UNDERLINE_SCOPE_BASE + 2).contains(&i) => {
                let idx = (i - a::UNDERLINE_SCOPE_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx != view.underline_scope {
                    view.underline_scope = idx;
                    self.prefs.underline_scope = idx;
                    prefs_changed = true;
                    underline_changed = true;
                }
            }
            // Fine typesetting is a typographic setting: the engine takes
            // it in the next layout, and the sheet's own close asks for
            // one.
            i if (a::FINE_BASE..a::FINE_BASE + 2).contains(&i) => {
                let on = i == a::FINE_BASE;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if on != view.fine_typesetting {
                    view.fine_typesetting = on;
                    self.prefs.fine_typesetting = on;
                    prefs_changed = true;
                }
            }
            i if (a::EMBEDDED_FONTS_BASE..a::EMBEDDED_FONTS_BASE + 2).contains(&i) => {
                let on = i == a::EMBEDDED_FONTS_BASE;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if on != view.embedded_fonts {
                    view.embedded_fonts = on;
                    self.prefs.embedded_fonts = on;
                    prefs_changed = true;
                }
            }
            // The three quick buttons, then the stepper on either side of
            // them. The weight is a typographic setting: it is laid out
            // again when the sheet closes.
            i if (a::WEIGHT_BASE..a::WEIGHT_DOWN).contains(&i) => {
                let Some(&weight) = typeset::WEIGHT_PRESETS.get((i - a::WEIGHT_BASE) as usize)
                else {
                    return Ok(());
                };
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if weight != view.font_weight {
                    view.font_weight = weight;
                    self.prefs.font_weight = weight;
                    prefs_changed = true;
                }
            }
            a::WEIGHT_DOWN | a::WEIGHT_UP => {
                let up = id == a::WEIGHT_UP;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                let weight = typeset::weight_stepped(view.font_weight, up);
                if weight != view.font_weight {
                    view.font_weight = weight;
                    self.prefs.font_weight = weight;
                    prefs_changed = true;
                }
            }
            i if (a::ORIENTATION_BASE..a::ORIENTATION_BASE + 4).contains(&i) => {
                let idx = (i - a::ORIENTATION_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx != view.orientation {
                    view.orientation = idx;
                    applied_orientation = idx;
                    if view.fixed_layout {
                        self.prefs
                            .book_fixed
                            .entry(book_id)
                            .or_default()
                            .orientation = Some(idx);
                        fixed_changed = true;
                    } else {
                        self.prefs.orientation = idx;
                    }
                    prefs_changed = true;
                    orientation_changed = true;
                }
            }
            i if (a::FIT_MODE_BASE..a::FIT_MODE_BASE + 3).contains(&i) => {
                let idx = (i - a::FIT_MODE_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx != view.fit_mode {
                    view.fit_mode = idx;
                    self.prefs.book_fixed.entry(book_id).or_default().fit = Some(idx);
                    prefs_changed = true;
                    fixed_changed = true;
                }
            }
            i if (a::PAGE_DIR_BASE..a::PAGE_DIR_BASE + 2).contains(&i) => {
                let idx = (i - a::PAGE_DIR_BASE) as usize;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if idx != view.page_direction {
                    view.page_direction = idx;
                    self.prefs.book_fixed.entry(book_id).or_default().direction = Some(idx);
                    prefs_changed = true;
                    fixed_changed = true;
                }
            }
            i if (a::FULLSCREEN_BASE..a::FULLSCREEN_BASE + 2).contains(&i) => {
                let on = i == a::FULLSCREEN_BASE;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if on != view.fullscreen {
                    view.fullscreen = on;
                    self.prefs.book_fixed.entry(book_id).or_default().fullscreen = Some(on);
                    prefs_changed = true;
                    fixed_changed = true;
                }
            }
            i if (a::AUTO_CROP_BASE..a::AUTO_CROP_BASE + 2).contains(&i) => {
                let on = i == a::AUTO_CROP_BASE;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if on != view.auto_crop {
                    view.auto_crop = on;
                    self.prefs.book_fixed.entry(book_id).or_default().auto_crop = Some(on);
                    prefs_changed = true;
                    fixed_changed = true;
                }
            }
            i if (a::SPLIT_SPREAD_BASE..a::SPLIT_SPREAD_BASE + 2).contains(&i) => {
                let on = i == a::SPLIT_SPREAD_BASE;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if on != view.split_spread {
                    view.split_spread = on;
                    self.prefs
                        .book_fixed
                        .entry(book_id)
                        .or_default()
                        .split_spread = Some(on);
                    prefs_changed = true;
                    fixed_changed = true;
                }
            }
            i if (a::NIGHT_KEEP_IMG_BASE..a::NIGHT_KEEP_IMG_BASE + 2).contains(&i) => {
                let on = i == a::NIGHT_KEEP_IMG_BASE;
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let book_id = r.session.id.clone();
                let Overlay::Typeset { view } = &mut r.overlay else {
                    return Ok(());
                };
                if on != view.keep_images_in_night {
                    view.keep_images_in_night = on;
                    self.prefs
                        .book_fixed
                        .entry(book_id)
                        .or_default()
                        .keep_images_in_night = Some(on);
                    prefs_changed = true;
                    fixed_changed = true;
                }
            }
            _ => return Ok(()),
        }

        if prefs_changed {
            let _ = self.prefs.save(self.lib.data_dir());
        }
        if orientation_changed {
            let orient = aristotle_screen::Orientation::from_index(applied_orientation);
            self.apply_orientation(orient);
        }
        if fixed_changed {
            let new_style = self.page_style_for_current();
            if let Screen::Reader(r) = &mut self.screen {
                if r.session.has_book() && r.session.is_fixed() {
                    let old_cur = r.session.current;
                    let anchor_pos = r.session.book().position_at(old_cur);
                    if r.session.book().set_layout(&new_style).is_ok() {
                        r.session.style = new_style.clone();
                        r.session.current = r.session.book().location_of(&anchor_pos).unwrap_or(0);
                        r.session.clear_pages();
                    }
                    r.gestures.set_vertical(r.session.is_rtl_turns());
                }
            }
            self.restyler.set_applied(&new_style);
        }
        if orientation_changed {
            let bounds = self.ui.bounds();
            let density = self.ui.profile().density;
            if let Screen::Reader(r) = &mut self.screen {
                let rtl = r.session.is_rtl_turns();
                r.gestures = crate::TurnGestures::with_density(bounds, density);
                r.gestures.set_vertical(rtl);
                r.gestures.set_touch_prefs(
                    aristotle_screen::TouchZoneLayout::from_index(self.prefs.touch_zone_layout),
                    self.prefs.disable_touch_turn,
                );
            }
            self.rebuild_reader(Refresh::Full)?;
            return self.present_full();
        }
        if underline_changed {
            let Screen::Reader(r) = &mut self.screen else {
                return Ok(());
            };
            // A layout worker may hold the book for a moment; its result
            // takes both settings from the preferences when it comes back
            // (`adopt_*_restyle`), so nothing is lost by skipping here.
            if r.session.has_book() {
                let underline = self.prefs.underline();
                let scope = self.prefs.underline_scope();
                let book = r.session.book();
                book.set_underline(underline);
                book.set_underline_scope(scope);
                r.session.clear_pages();
            }
        }

        self.rebuild_reader(Refresh::Auto)?;
        self.present_clean()
    }

    // ------------------------------------------------------ UI events

    /// UI events while an overlay is open (or the reading row of the menu).
    pub(crate) fn on_overlay_event(&mut self, ev: UiEvent) -> Result<(), Error> {
        match ev {
            UiEvent::Tap(_) => {
                // Outside the sheet: close. Lists cover the screen, so
                // this only happens around popups.
                self.close_overlay()
            }
            UiEvent::Swipe(Direction::Left) => self.overlay_page(1),
            UiEvent::Swipe(Direction::Right) => self.overlay_page(-1),
            UiEvent::Action { id, node, at } => self.overlay_action(id, node, at),
            _ => Ok(()),
        }
    }

    fn overlay_page(&mut self, delta: i32) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Some(p) = r.overlay.page_mut() else {
            return Ok(());
        };
        let before = *p;
        let new = (before as i32 + delta).max(0) as usize;
        if new == before {
            // Already on the first page: there is nothing to draw, and
            // a full update would only flash the screen.
            return Ok(());
        }
        *p = new;
        self.sync_pager();
        let after = self
            .reading()
            .and_then(|r| r.overlay.page())
            .unwrap_or(before);
        if after == before {
            // The list ends before the page asked for: the pager kept
            // the page, so there is nothing to draw.
            return Ok(());
        }
        let full = matches!(
            self.reading().map(|r| &r.overlay),
            Some(Overlay::Toc { .. } | Overlay::Marks { .. } | Overlay::Fonts { .. })
        );
        if full {
            // A list page is a new screen of text: clean, like a page.
            self.present_full()
        } else {
            // A popup's next page: new gray text over the old.
            self.present_clean()
        }
    }

    fn overlay_action(&mut self, id: ActionId, node: NodeId, at: Point) -> Result<(), Error> {
        if matches!(
            self.reading().map(|r| &r.overlay),
            Some(Overlay::Typeset { .. })
        ) {
            return self.handle_typeset_action(id);
        }
        // The contents list's own actions: the way back, the pages of
        // the list, a chapter and a fold. They are decoded, not read
        // out of number ranges, so nothing else can land on them.
        if matches!(
            self.reading().map(|r| &r.overlay),
            Some(Overlay::Toc { .. })
        ) {
            if let Some(action) = toc_ui::decode(id) {
                return self.toc_action(action);
            }
        }
        match id {
            // The translation sheet's own pager.
            translate_ui::action::PREV => self.overlay_page(-1),
            translate_ui::action::NEXT => self.overlay_page(1),
            act::TRANSLATE => self.translate_selection(false),
            act::TRANSLATE_SENTENCE => self.translate_selection(true),
            act::NAV_TOC => self.open_toc(),
            act::NAV_MARKS => self.open_marks(),
            act::NAV_GOTO => self.open_goto(),
            act::GO_ANNOTATION => {
                let target = match self.reading().map(|r| &r.overlay) {
                    Some(Overlay::Annotation { target, .. })
                    | Some(Overlay::WereadThought { target, .. }) => target.clone(),
                    _ => None,
                };
                let location = target.and_then(|p| {
                    let r = self.reading()?;
                    r.session
                        .book()
                        .location_of(&p)
                        .and_then(|l| r.session.book().page_start(l))
                });
                match location {
                    Some(l) => self.jump_to(l),
                    None => Ok(()),
                }
            }
            act::PREV => self.overlay_page(-1),
            act::NEXT => self.overlay_page(1),
            act::BACK
                if matches!(
                    self.reading().map(|r| &r.overlay),
                    Some(Overlay::Fonts { .. })
                ) =>
            {
                self.fonts_back()
            }
            act::CANCEL | act::BACK => self.close_overlay(),
            act::HIGHLIGHT => self.highlight_selection(),
            act::UNHIGHLIGHT => {
                let idx = match self.reading().map(|r| &r.overlay) {
                    Some(Overlay::Dict {
                        highlight: Some(i), ..
                    }) => Some(*i),
                    _ => None,
                };
                match idx {
                    Some(i) => self.remove_highlight(i),
                    None => self.close_overlay(),
                }
            }
            act::LOOKUP => self.lookup_selection(),
            act::MORE => {
                if let Some(Overlay::Dict { full, page, .. }) =
                    self.reading().map(|r| &mut r.overlay)
                {
                    *full = true;
                    *page = 0;
                }
                self.rebuild_reader(Refresh::Auto)?;
                // The popup grows: the page under it is replaced, cleanly.
                self.present_full()
            }
            act::GLOSS => self.gloss_selection(),
            act::GO_NOTE => {
                let Some(r) = self.reading() else {
                    return Ok(());
                };
                let target = match &r.overlay {
                    Overlay::Note {
                        link: Some(link), ..
                    } => {
                        let (loc, uri) = (r.session.current, link.uri.clone());
                        r.session.book().link_page(loc, &uri)
                    }
                    _ => None,
                };
                match target {
                    Some(t) => self.jump_to(t),
                    None => self.close_overlay(),
                }
            }
            id if (act::TAB..act::TAB + 100).contains(&id) => {
                self.switch_dict((id - act::TAB) as usize)
            }
            id if (act::SUGGEST..act::SUGGEST + 100).contains(&id) => {
                self.suggestion((id - act::SUGGEST) as usize)
            }
            id if (act::ITEM..act::GOTO_SET).contains(&id) => {
                self.list_item((id - act::ITEM) as usize)
            }
            act::GOTO_SET => {
                let v = Slider::value_at(self.ui.tree.rect(node), at.x);
                self.set_goto(|_| v)
            }
            act::SYNC_GO => {
                let target = match self.reading().map(|r| &r.overlay) {
                    Some(Overlay::Sync { target, .. }) => Some(*target),
                    _ => None,
                };
                match target {
                    Some(t) => {
                        perf!("sync: went to the synced position");
                        self.jump_to(t)
                    }
                    None => self.close_overlay(),
                }
            }
            act::GOTO_MINUS10 => self.set_goto(|v| v - 0.10),
            act::GOTO_MINUS1 => self.set_goto(|v| v - 0.01),
            act::GOTO_PLUS1 => self.set_goto(|v| v + 0.01),
            act::GOTO_PLUS10 => self.set_goto(|v| v + 0.10),
            act::GOTO_GO => {
                let Some(r) = self.reading() else {
                    return Ok(());
                };
                let Overlay::GoTo { value } = r.overlay else {
                    return Ok(());
                };
                let book = r.session.book();
                let target = book
                    .location_of(&Position::Fraction(value.min(0.9999)))
                    .and_then(|l| book.page_start(l));
                match target {
                    Some(t) => self.jump_to(t),
                    None => self.close_overlay(),
                }
            }
            _ => Ok(()),
        }
    }

    fn set_goto(&mut self, f: impl Fn(f32) -> f32) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        if let Overlay::GoTo { value } = &mut r.overlay {
            *value = f(*value).clamp(0.0, 1.0);
        }
        self.update_goto_label();
        self.present_fast()
    }

    fn list_item(&mut self, i: usize) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        if matches!(r.overlay, Overlay::Fonts { .. }) {
            return self.pick_font(i);
        }
        if let Overlay::Marks { rows, .. } = &r.overlay {
            if let Some(row) = rows.get(i).filter(|row| row.detail.is_some()) {
                let text = RichText::plain(row.detail.as_deref().unwrap_or_default());
                let target = row.target.clone();
                let overlay = if row.weread {
                    Overlay::WereadThought {
                        text,
                        target,
                        page: 0,
                    }
                } else {
                    Overlay::Annotation {
                        text,
                        target,
                        page: 0,
                    }
                };
                if let Screen::Reader(r) = &mut self.screen {
                    r.overlay = overlay;
                }
                self.rebuild_reader(Refresh::Auto)?;
                return self.present_full();
            }
        }
        let target = match &r.overlay {
            Overlay::Toc { contents, .. } => {
                let index = contents.row_index(i);
                index.and_then(|ix| r.session.book().toc_page(ix))
            }
            Overlay::Marks { rows, .. } => {
                let pos = rows.get(i).and_then(|m| m.target.clone());
                let book = r.session.book();
                pos.and_then(|p| book.location_of(&p))
                    .and_then(|l| book.page_start(l))
            }
            _ => None,
        };
        match target {
            Some(t) => self.jump_to(t),
            None => Ok(()),
        }
    }

    fn switch_dict(&mut self, i: usize) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Overlay::Dict { lookup, .. } = &r.overlay else {
            return Ok(());
        };
        // Tab `i` is the i-th dictionary of the lookup's order.
        let Some(&i) = lookup.order.get(i) else {
            return Ok(());
        };
        if lookup.shown == Some(i) {
            return Ok(());
        }
        let l = (**lookup).clone();
        let reply = self.dict.as_ref().and_then(|d| {
            d.ask(Request::Switch { lookup: l, dict: i });
            d.wait(Duration::from_secs(2))
        });
        if let Some(Reply::Lookup(l)) = reply {
            if let Some(r) = self.reading() {
                if let Overlay::Dict {
                    lookup, page, full, ..
                } = &mut r.overlay
                {
                    **lookup = l;
                    *page = 0;
                    *full = false;
                }
            }
            self.rebuild_reader(Refresh::Auto)?;
            // Only the popup's inside changes: a cleaning update of it.
            self.present_clean()?;
        }
        Ok(())
    }

    fn suggestion(&mut self, i: usize) -> Result<(), Error> {
        let lang = self.book_lang();
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Overlay::Dict { lookup, .. } = &r.overlay else {
            return Ok(());
        };
        let Some(word) = lookup.suggestions.get(i).cloned() else {
            return Ok(());
        };
        let reply = self.dict.as_ref().and_then(|d| {
            d.ask(Request::Lookup {
                word,
                book_lang: lang,
            });
            d.wait(Duration::from_secs(2))
        });
        if let Some(Reply::Lookup(l)) = reply {
            if let Some(r) = self.reading() {
                if let Overlay::Dict {
                    lookup, page, full, ..
                } = &mut r.overlay
                {
                    **lookup = l;
                    *page = 0;
                    *full = false;
                }
            }
            self.rebuild_reader(Refresh::Auto)?;
            self.present_clean()?;
        }
        Ok(())
    }

    /// "Słownik" on a dragged selection: look the phrase up (dictionaries
    /// have multi-word entries), else its first word.
    fn lookup_selection(&mut self) -> Result<(), Error> {
        let lang = self.book_lang();
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Some((sel, words)) = r.overlay.selection().map(|(s, w)| (*s, w.clone())) else {
            return Ok(());
        };
        let phrase = sel.text(&words);
        let first = selection::text(&words, sel.anchor.0, sel.anchor.1);
        let Some(d) = self.dict.as_ref() else {
            return Ok(());
        };
        d.ask(Request::Lookup {
            word: phrase,
            book_lang: lang.clone(),
        });
        let mut l = match d.wait(Duration::from_secs(2)) {
            Some(Reply::Lookup(l)) => l,
            _ => return Ok(()),
        };
        if l.found.is_none() {
            d.ask(Request::Lookup {
                word: first,
                book_lang: lang,
            });
            if let Some(Reply::Lookup(x)) = d.wait(Duration::from_secs(2)) {
                l = x;
            }
        }
        let highlight = self.highlight_at(words[sel.start].offset);
        if let Some(r) = self.reading() {
            r.overlay = Overlay::Dict {
                sel,
                words,
                lookup: Box::new(l),
                highlight,
                page: 0,
                full: false,
            };
        }
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// The buttons of the overlay: what the reader can do with what is
    /// on the screen.
    ///
    /// They are measured here, because the tree does not own the text
    /// engine, and the popups are built inside the reader's tree.
    fn overlay_buttons(&mut self, st: &aristotle_ui::theme::Styles) -> Vec<Button> {
        let items: Vec<(&str, ActionId)> = {
            let Some(r) = self.reading() else {
                return Vec::new();
            };
            match &r.overlay {
                Overlay::Dict {
                    lookup,
                    highlight,
                    full,
                    ..
                } => {
                    // In a highlight the button takes the highlight away.
                    let mut actions = vec![if highlight.is_some() {
                        (tr(Msg::ReaderUnhighlight), act::UNHIGHLIGHT)
                    } else {
                        (tr(Msg::ReaderHighlight), act::HIGHLIGHT)
                    }];
                    if !*full && lookup.found.as_ref().is_some_and(dict::has_more) {
                        actions.push((tr(Msg::CommonMore), act::MORE));
                    }
                    actions.push((tr(Msg::ReaderTranslate), act::TRANSLATE));
                    actions.push((tr(Msg::ReaderTranslateSentence), act::TRANSLATE_SENTENCE));
                    actions.push((tr(Msg::CommonClose), act::CANCEL));
                    actions
                }
                Overlay::Actions { .. } => vec![
                    (tr(Msg::ReaderHighlight), act::HIGHLIGHT),
                    (tr(Msg::ReaderDictionary), act::LOOKUP),
                    (tr(Msg::ReaderGloss), act::GLOSS),
                    (tr(Msg::ReaderTranslate), act::TRANSLATE),
                    (tr(Msg::ReaderTranslateSentence), act::TRANSLATE_SENTENCE),
                    (tr(Msg::CommonCancel), act::CANCEL),
                ],
                Overlay::Translate { .. } => vec![(tr(Msg::CommonClose), act::CANCEL)],
                Overlay::Gloss { .. } => vec![
                    (tr(Msg::ReaderHighlight), act::HIGHLIGHT),
                    (tr(Msg::CommonClose), act::CANCEL),
                ],
                Overlay::Note { link, .. } => {
                    let mut actions = Vec::new();
                    if link.is_some() {
                        actions.push((tr(Msg::ReaderGoToNote), act::GO_NOTE));
                    }
                    actions.push((tr(Msg::CommonClose), act::CANCEL));
                    actions
                }
                Overlay::Annotation { target, .. } | Overlay::WereadThought { target, .. } => {
                    let mut actions = Vec::new();
                    if target.is_some() {
                        actions.push((tr(Msg::ReaderGo), act::GO_ANNOTATION));
                    }
                    actions.push((tr(Msg::CommonBack), act::NAV_MARKS));
                    actions
                }
                Overlay::Sync { .. } => vec![
                    (tr(Msg::ReaderSyncGo), act::SYNC_GO),
                    (tr(Msg::ReaderSyncStay), act::CANCEL),
                ],
                _ => Vec::new(),
            }
        };
        let text = &mut self.ui.text;
        items
            .into_iter()
            .map(|(label, action)| Button::new(&mut *text, st, label, action))
            .collect()
    }

    /// Translate the selected text, or the sentence it stands in.
    ///
    /// The service is on the network, so the sheet opens with the text
    /// that was sent and fills in when the answer comes.
    fn translate_selection(&mut self, sentence: bool) -> Result<(), Error> {
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let (sel, words) = match &r.overlay {
            Overlay::Actions { sel, words } | Overlay::Dict { sel, words, .. } => {
                (*sel, words.clone())
            }
            _ => return Ok(()),
        };
        let (start, end) = if sentence {
            selection::sentence(&words, &sel)
        } else {
            (sel.start, sel.end)
        };
        let source = selection::range_text(&words, start, end);
        if source.trim().is_empty() {
            return Ok(());
        }
        translation::allow_hosts();
        let service = self
            .translate
            .get_or_insert_with(translation::Service::start);
        let seq = service.ask(source.clone());
        let Some(r) = self.reading() else {
            return Ok(());
        };
        r.overlay = Overlay::Translate {
            sel,
            words,
            source,
            seq,
            result: None,
            page: 0,
        };
        r.menu_open = false;
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// The translation service answered.
    pub(crate) fn poll_translate(&mut self) -> Result<(), Error> {
        let Some(reply) = self.translate.as_mut().and_then(|s| s.take()) else {
            return Ok(());
        };
        let TranslateReply::Translated { seq, result } = reply;
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Overlay::Translate {
            seq: current,
            result: slot,
            ..
        } = &mut r.overlay
        else {
            return Ok(());
        };
        if *current != seq {
            // A stale answer: the reader has asked again since.
            return Ok(());
        }
        *slot = Some(result);
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// "Tłumacz": each word's first sense from the dictionaries.
    fn gloss_selection(&mut self) -> Result<(), Error> {
        let lang = self.book_lang();
        let Some(r) = self.reading() else {
            return Ok(());
        };
        let Some((sel, words)) = r.overlay.selection().map(|(s, w)| (*s, w.clone())) else {
            return Ok(());
        };
        let list: Vec<String> = sel
            .text(&words)
            .split_whitespace()
            .map(dict::clean_word)
            .filter(|w| !w.is_empty())
            .take(40)
            .collect();
        let Some(d) = self.dict.as_ref() else {
            return Ok(());
        };
        let t = Instant::now();
        d.ask(Request::Gloss {
            words: list,
            book_lang: lang,
        });
        let Some(Reply::Gloss(lines)) = d.wait(Duration::from_secs(5)) else {
            return Ok(());
        };
        perf!(
            "gloss: {} words in {:.0} ms",
            lines.len(),
            crate::ms(t.elapsed())
        );
        let text = RichText {
            blocks: lines
                .into_iter()
                .map(|(w, g)| Block {
                    runs: vec![
                        Run {
                            text: w,
                            bold: true,
                            italic: false,
                        },
                        Run {
                            text: format!("  {g}"),
                            bold: false,
                            italic: false,
                        },
                    ],
                    heading: false,
                })
                .collect(),
        };
        if let Some(r) = self.reading() {
            r.overlay = Overlay::Gloss {
                sel,
                words,
                text,
                page: 0,
            };
        }
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }
}

/// What the translation sheet shows: the text that was sent, the
/// translation, and where it came from.
fn translate_view<'a>(
    source: &'a str,
    result: Option<&'a Result<Translation, String>>,
) -> translate_ui::TranslateView<'a> {
    let mut view = translate_ui::TranslateView {
        title: tr(Msg::ReaderTranslateTitle),
        original_label: tr(Msg::ReaderTranslateOriginal),
        original: source,
        translation: None,
        origin: None,
        status: None,
        buttons: Vec::new(),
    };
    match result {
        None => view.status = Some(tr(Msg::ReaderTranslateWaiting)),
        Some(Ok(t)) => {
            view.translation = Some(&t.text);
            view.origin = Some(t.source.label());
        }
        Some(Err(e)) => view.status = Some(e.as_str()),
    }
    view
}

/// What the dictionary popup says: the senses (or with `full` the whole
/// entry), or why there is nothing.
fn dict_body(l: &Lookup, no_dicts: bool, full: bool) -> RichText {
    match &l.found {
        Some(f) => dict::as_rich_text(f, full),
        None if no_dicts => RichText::plain(tr(Msg::DictNone)),
        None if l.indexing => RichText::plain(tr(Msg::DictIndexing)),
        None => RichText::plain(tr(Msg::DictNotFound)),
    }
}

/// The dictionary popup for a lookup.
#[allow(clippy::too_many_arguments)]
fn dict_popup(
    t: &mut Tree,
    st: &aristotle_ui::theme::Styles,
    l: &Lookup,
    dicts: &[String],
    body: &RichText,
    buttons: &[Button],
    available: i32,
    place: Place,
    height: i32,
) -> overlay::Popup {
    // Tab k shows dictionary `l.order[k]` (see `App::switch_dict`).
    let tabs: Vec<(String, bool)> = l
        .order
        .iter()
        .map(|&i| {
            (
                dicts.get(i).cloned().unwrap_or_default(),
                l.shown == Some(i),
            )
        })
        .collect();
    // The title is the dictionary form; the word as tapped goes under it
    // when it differs.
    let headword = l
        .found
        .as_ref()
        .and_then(|f| f.entries.first())
        .map(|e| e.headword.clone());
    let title = headword.clone().unwrap_or_else(|| l.word.clone());
    let differs = headword.is_some_and(|h| h.to_lowercase() != l.word.to_lowercase());
    let subtitle = match &l.found {
        Some(f) if f.approximate => Some(tf!(Msg::DictBaseFormGuessed, word = l.word)),
        Some(_) if differs => Some(tf!(Msg::DictBaseForm, word = l.word)),
        _ => None,
    };
    overlay::popup(
        t,
        st,
        &PopupSpec {
            title,
            subtitle,
            tabs,
            body,
            suggestions: l.suggestions.clone(),
            actions: buttons.to_vec(),
            available,
            place,
            height,
        },
    )
}

/// Height of a popup sheet that ends between two lines of the page
/// instead of cutting through one: [`overlay::POPUP_H`], or a little less
/// when its edge would cross a line.
fn popup_height(
    words: &[Word],
    place: Place,
    screen_h: i32,
    max: i32,
    density: aristotle_ui::geom::Density,
) -> i32 {
    let edge = match place {
        Place::Bottom => screen_h - max,
        Place::Top => max,
    };
    // A line of text the edge would cross (word boxes on one line overlap
    // vertically; any word will do).
    let Some(w) = words.iter().find(|w| w.rect.y0 < edge && w.rect.y1 > edge) else {
        return max;
    };
    // Word boxes span the font's whole ascender and descender, a little
    // more than the line pitch, and glyphs keep inside them: a sheet edge
    // at the end of one line's box leaves the next line's glyphs alone.
    // The sheet grows over the crossed line (never shrinks below what it
    // was asked, which a fitted popup's content needs).
    match place {
        // Start the sheet under the box of the line above it.
        Place::Bottom => {
            let above = words
                .iter()
                .filter(|x| x.rect.y0 < w.rect.y0 - density.px(10))
                .map(|x| x.rect.y1)
                .max()
                .unwrap_or(w.rect.y0);
            screen_h - (above + 1)
        }
        // End the sheet under the crossed line's box.
        Place::Top => w.rect.y1 + 1,
    }
    .clamp(max, max + density.px(80))
}

/// Build the popup body and optional jump target for the current page's
/// footnotes (opened from the top-right magnifying-glass button).
///
/// A single footnote keeps its original rich text and jump link; multiple
/// footnotes on the same page are combined in order with bold marker prefixes
/// so the reader can browse every note on the page in one popup.
fn format_page_footnotes(notes: &[PageFootnote]) -> (RichText, Option<Link>) {
    if notes.len() == 1 {
        let first = &notes[0];
        return (first.text.clone(), Some(first.link.clone()));
    }
    let mut blocks = Vec::new();
    for (idx, note) in notes.iter().enumerate() {
        let marker = note.marker.trim();
        let label = if marker.is_empty() {
            format!("[{}]", idx + 1)
        } else {
            marker.to_string()
        };
        let mut note_blocks = note.text.blocks.clone();
        if let Some(first_block) = note_blocks.first_mut() {
            let trimmed_body = first_block.text();
            let already_prefixed =
                !marker.is_empty() && trimmed_body.trim_start().starts_with(marker);
            if !already_prefixed {
                first_block.runs.insert(
                    0,
                    Run {
                        text: format!("{label} "),
                        italic: false,
                        bold: true,
                    },
                );
            }
        }
        blocks.extend(note_blocks);
    }
    (RichText { blocks }, None)
}
