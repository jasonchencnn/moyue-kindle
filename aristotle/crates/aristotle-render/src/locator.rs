// SPDX-License-Identifier: AGPL-3.0-or-later
//! Between the library index's locators and the renderer's positions.
//!
//! The library index stores where a book was left as a [`Locator`] (and how
//! far it was read); the renderer works with [`Position`]s. Which locator
//! the renderer can place depends on the engine that lays the book out:
//! Plato's engine places a spine offset, crengine-ng places the XPointers
//! it writes itself.
//!
//! The positions here outlive one layout: an XPointer names a place in the
//! book's document, so it survives a change of font size or margin, while a
//! page number does not.

use aristotle_library::{Locator, Progress};

use crate::book::{Engine, Position};

/// Where to open a book with this progress, most exact first.
///
/// The stored locator comes first, when the engine that lays the book out
/// can place it; the stored fraction is always there as a fallback, so a
/// progress another engine wrote still opens the book at about the right
/// place.
pub fn positions(
    engine: Engine,
    progress: Option<&Progress>,
    stock_position_compatible: bool,
) -> Vec<Position> {
    let Some(progress) = progress else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match &progress.locator {
        Locator::Epub { spine, offset } if engine == Engine::Plato => {
            out.push(Position::Spine {
                item: *spine as usize,
                offset: *offset as usize,
            });
        }
        Locator::Kindle { position }
            if engine == Engine::Plato
                && (stock_position_compatible
                    || progress.source.as_deref() == Some("aristotle")) =>
        {
            out.push(Position::Kindle(*position));
        }
        // A page index means a page of the engine that wrote it. crengine-ng
        // pages a book differently, and a locator it reads is either from an
        // older engine (MuPDF reflow put TXT progress there) or a bare
        // fraction stored as page 0: neither names a crengine page, so the
        // fraction below is the only honest place to open.
        Locator::Page { index } if engine != Engine::Crengine => {
            out.push(Position::Page(*index as usize));
        }
        // An XPointer names a place in the book; only crengine-ng places
        // one. The others fall back to the fraction.
        Locator::XPointer { xpointer } if engine == Engine::Crengine => {
            out.push(Position::XPointer {
                xpointer: xpointer.clone(),
            });
        }
        _ => {}
    }
    out.push(Position::Fraction(progress.fraction));
    out
}

/// The locator for a renderer position.
///
/// A bare fraction (a book whose spine could not be read) is stored as a page
/// locator, which Plato's engine does not place, so the next open falls back
/// to the fraction.
pub fn locator(position: &Position) -> Locator {
    match position {
        Position::Spine { item, offset } => Locator::Epub {
            spine: *item as u32,
            offset: *offset as u32,
        },
        Position::Kindle(position) => Locator::Kindle {
            position: *position,
        },
        Position::Page(index) => Locator::Page {
            index: *index as u32,
        },
        Position::Fraction(_) => Locator::Page { index: 0 },
        Position::XPointer { xpointer } => Locator::XPointer {
            xpointer: xpointer.clone(),
        },
    }
}

/// A renderer position, written into the library index as text.
///
/// The forms are `epub:<spine item>:<offset>`, `kindle:<position>`,
/// `page:<index>`, `fraction:<f>` and `xpointer:<xpointer>`.
pub fn encode(position: &Position) -> String {
    match position {
        Position::Spine { item, offset } => format!("epub:{item}:{offset}"),
        Position::Kindle(k) => format!("kindle:{k}"),
        Position::Page(i) => format!("page:{i}"),
        Position::Fraction(f) => format!("fraction:{f}"),
        Position::XPointer { xpointer } => format!("xpointer:{xpointer}"),
    }
}

/// The position an encoded position names.
pub fn decode(text: &str) -> Option<Position> {
    if text.starts_with('/') {
        return Some(Position::XPointer {
            xpointer: text.to_string(),
        });
    }
    let (kind, rest) = text.split_once(':')?;
    match kind {
        "epub" => {
            let (item, offset) = rest.split_once(':')?;
            Some(Position::Spine {
                item: item.parse().ok()?,
                offset: offset.parse().ok()?,
            })
        }
        "kindle" => rest.parse().ok().map(Position::Kindle),
        "page" => rest.parse().ok().map(Position::Page),
        "fraction" => rest.parse().ok().map(Position::Fraction),
        "xpointer" => Some(Position::XPointer {
            xpointer: rest.to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A progress record with the given locator.
    fn progress(locator: Locator, fraction: f32) -> Progress {
        Progress {
            fraction,
            locator,
            updated: 1,
            source: Some("aristotle".into()),
            page: None,
        }
    }

    #[test]
    fn an_xpointer_is_kept_for_crengine() {
        let p = progress(
            Locator::XPointer {
                xpointer: "/body/DocFragment[3]/body/div/p[5]".into(),
            },
            0.25,
        );
        let positions = positions(Engine::Crengine, Some(&p), false);
        assert_eq!(
            positions[0],
            Position::XPointer {
                xpointer: "/body/DocFragment[3]/body/div/p[5]".into()
            }
        );
        // The fraction is still there, for an engine that cannot place it.
        assert_eq!(positions[1], Position::Fraction(0.25));
    }

    #[test]
    fn plato_does_not_place_an_xpointer() {
        let p = progress(
            Locator::XPointer {
                xpointer: "/body/DocFragment[3]".into(),
            },
            0.6,
        );
        assert_eq!(
            positions(Engine::Plato, Some(&p), false),
            vec![Position::Fraction(0.6)]
        );
    }

    #[test]
    fn crengine_ignores_an_old_page_progress() {
        // Progress the MuPDF reflow wrote for a TXT book: a page in a
        // pagination crengine-ng does not share.
        let p = progress(Locator::Page { index: 41 }, 0.62);
        assert_eq!(
            positions(Engine::Crengine, Some(&p), false),
            vec![Position::Fraction(0.62)]
        );
    }

    #[test]
    fn a_page_progress_still_opens_the_other_engines_by_page() {
        let p = progress(Locator::Page { index: 41 }, 0.62);
        for engine in [Engine::Plato, Engine::MupdfReflow, Engine::Fixed] {
            assert_eq!(
                positions(engine, Some(&p), false),
                vec![Position::Page(41), Position::Fraction(0.62)],
                "{engine:?} should place the page"
            );
        }
    }

    #[test]
    fn a_zero_fraction_opens_a_crengine_book_at_the_beginning() {
        // The only locator a bare fraction leaves behind is a page 0.
        let p = progress(Locator::Page { index: 0 }, 0.0);
        assert_eq!(
            positions(Engine::Crengine, Some(&p), false),
            vec![Position::Fraction(0.0)]
        );
    }

    #[test]
    fn an_old_spine_progress_falls_back_to_the_fraction() {
        let p = progress(
            Locator::Epub {
                spine: 3,
                offset: 1200,
            },
            0.4,
        );
        // crengine-ng does not know Plato's spine offsets.
        assert_eq!(
            positions(Engine::Crengine, Some(&p), false),
            vec![Position::Fraction(0.4)]
        );
        // Plato's engine places it exactly.
        assert_eq!(
            positions(Engine::Plato, Some(&p), false),
            vec![
                Position::Spine {
                    item: 3,
                    offset: 1200
                },
                Position::Fraction(0.4)
            ]
        );
    }

    #[test]
    fn a_kindle_progress_needs_the_converter_to_agree() {
        let mut p = progress(Locator::Kindle { position: 7510 }, 0.5);
        // A progress the stock reader wrote: only usable when the offsets
        // share the converter's address space.
        p.source = Some("kindle".into());
        assert_eq!(
            positions(Engine::Plato, Some(&p), false),
            vec![Position::Fraction(0.5)]
        );
        assert_eq!(
            positions(Engine::Plato, Some(&p), true),
            vec![Position::Kindle(7510), Position::Fraction(0.5)]
        );
        // crengine-ng has no position map for a MOBI book.
        assert_eq!(
            positions(Engine::Crengine, Some(&p), true),
            vec![Position::Fraction(0.5)]
        );
    }

    #[test]
    fn no_progress_means_no_position() {
        assert!(positions(Engine::Plato, None, false).is_empty());
        assert!(positions(Engine::Crengine, None, false).is_empty());
    }

    #[test]
    fn an_xpointer_survives_the_round_trip_through_the_index() {
        let position = Position::XPointer {
            xpointer: "/body/DocFragment[7]/body/div/p[2]/text().3".into(),
        };
        let text = encode(&position);
        assert_eq!(text, "xpointer:/body/DocFragment[7]/body/div/p[2]/text().3");
        assert_eq!(decode(&text), Some(position.clone()));
        // And as a locator in the index.
        assert_eq!(
            locator(&position),
            Locator::XPointer {
                xpointer: "/body/DocFragment[7]/body/div/p[2]/text().3".into()
            }
        );
    }

    #[test]
    fn the_other_positions_still_round_trip() {
        let cases = [
            Position::Spine {
                item: 3,
                offset: 1200,
            },
            Position::Kindle(7510),
            Position::Page(41),
            Position::Fraction(0.25),
        ];
        for position in cases {
            assert_eq!(decode(&encode(&position)), Some(position));
        }
    }

    #[test]
    fn an_unknown_position_is_not_decoded() {
        assert_eq!(decode("epub:1"), None);
        assert_eq!(decode("nonsense"), None);
        assert_eq!(decode(""), None);
    }
}
