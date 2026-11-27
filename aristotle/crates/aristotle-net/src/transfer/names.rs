// SPDX-License-Identifier: AGPL-3.0-or-later
//! The names of the files that come in over the air.
//!
//! A browser sends whatever name the file has on the machine it came from,
//! so every name here is hostile until it has been through [`sanitize`]:
//! the path parts are dropped, `..` and absolute paths are refused, and
//! control characters never reach the file system. What is left is a plain
//! name with a known extension.
//!
//! Two books may share a name, so [`unique_name`] gives the second one a
//! number instead of overwriting the first.

/// The extensions the reader can open, plus the two font formats it can
/// use and the pictures that may be a sleep screen. The list is a
/// whitelist: a file whose extension is not here is refused, not stored.
pub const BOOK_EXTENSIONS: &[&str] = &["epub", "txt", "mobi", "azw3", "pdf", "cbz"];
/// Font files, which go to the fonts folder instead of the library.
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc"];
/// Sleep-screen pictures, which go to the `screensavers` folder.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png"];

/// What a file turned out to be, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A book: it lands in the library folder.
    Book,
    /// A font: it lands in the fonts folder.
    Font,
    /// A sleep-screen picture: it lands in the `screensavers` folder.
    Image,
}

/// The name of a file that may be written to disk, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeName {
    /// The name itself, without any path.
    pub name: String,
    /// The extension, lower case, without the dot.
    pub extension: String,
    /// Book, font or picture.
    pub kind: Kind,
}

/// The name is empty, has no known extension, or is not a plain name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameError {
    /// Nothing was left after the path was taken off.
    Empty,
    /// The extension is not one the reader opens.
    Unsupported,
}

/// The longest name written to disk. Long enough for any real title, short
/// enough that the file system of the device takes it.
const MAX_NAME: usize = 120;

/// Strips the path off a name the browser sent and checks the extension.
///
/// The name is the part after the last `/` or `\`; a name that is only a
/// path (`..`, `/etc/passwd`) is refused rather than trimmed, because a
/// browser never sends one and a hand-written request might.
pub fn sanitize(raw: &str) -> Result<SafeName, NameError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(NameError::Empty);
    }
    // A name with a path in it is a path, not a name: take the last piece.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    if last.is_empty() || last == "." || last == ".." {
        return Err(NameError::Empty);
    }
    // Control characters would confuse the file system and the terminal.
    if last.chars().any(|c| c.is_control()) {
        return Err(NameError::Empty);
    }
    // The extension is what decides where the file goes.
    let (_, extension) = match last.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => return Err(NameError::Unsupported),
    };
    let extension = extension.to_lowercase();
    let kind = if BOOK_EXTENSIONS.contains(&extension.as_str()) {
        Kind::Book
    } else if FONT_EXTENSIONS.contains(&extension.as_str()) {
        Kind::Font
    } else if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        Kind::Image
    } else {
        return Err(NameError::Unsupported);
    };
    // A name too long for the file system is cut, but the extension is
    // kept, so the file still lands in the right folder.
    let room = MAX_NAME.saturating_sub(extension.chars().count() + 1);
    let stem: String = last
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(last)
        .chars()
        .take(room)
        .collect();
    let name = format!("{stem}.{extension}");
    if name.is_empty() || name == "." || name == ".." {
        return Err(NameError::Empty);
    }
    Ok(SafeName {
        name,
        extension,
        kind,
    })
}

/// The name to write, given what is already in the folder.
///
/// The first book keeps its name. The second one with the same name gets a
/// number: `book.epub` becomes `book (2).epub`, then `book (3).epub`. An
/// existing file is never overwritten.
///
/// `exists` says whether a name is taken; the caller passes the folder's
/// own answer rather than a directory, so this stays testable without a
/// file system.
pub fn unique_name(name: &str, mut exists: impl FnMut(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem.to_string(), ext.to_string()),
        None => (name.to_string(), String::new()),
    };
    let mut n = 2u32;
    loop {
        let candidate = if extension.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{extension}")
        };
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
        // A folder with two million books of the same name is not a case
        // worth looping over; the name grows and the loop ends.
        if n > 9999 {
            return candidate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_book_name_is_kept() {
        let n = sanitize("journey.epub").unwrap();
        assert_eq!(n.name, "journey.epub");
        assert_eq!(n.extension, "epub");
        assert_eq!(n.kind, Kind::Book);
    }

    #[test]
    fn the_extension_decides_where_the_file_goes() {
        assert_eq!(sanitize("a.TXT").unwrap().kind, Kind::Book);
        assert_eq!(sanitize("a.ttf").unwrap().kind, Kind::Font);
        assert_eq!(sanitize("a.otf").unwrap().kind, Kind::Font);
        assert_eq!(sanitize("a.ttc").unwrap().kind, Kind::Font);
        assert_eq!(sanitize("a.jpg").unwrap().kind, Kind::Image);
        assert_eq!(sanitize("a.exe").unwrap_err(), NameError::Unsupported);
        assert_eq!(sanitize("a").unwrap_err(), NameError::Unsupported);
    }

    #[test]
    fn a_picture_is_a_sleep_screen_picture_in_any_case() {
        for raw in ["photo.jpg", "photo.jpeg", "photo.png"] {
            assert_eq!(sanitize(raw).unwrap().kind, Kind::Image, "{raw}");
        }
        // The name is written lower case, so what is on the disk is
        // always the extension the reader looks for.
        let n = sanitize("Sunset.JPG").unwrap();
        assert_eq!(n.name, "Sunset.jpg");
        assert_eq!(n.extension, "jpg");
        let n = sanitize("Sunset.Png").unwrap();
        assert_eq!(n.name, "Sunset.png");
        assert_eq!(n.kind, Kind::Image);
    }

    #[test]
    fn a_picture_name_is_stripped_of_its_path_like_any_other() {
        let n = sanitize("../../mnt/us/screensavers/cover.png").unwrap();
        assert_eq!(n.name, "cover.png");
        assert_eq!(n.kind, Kind::Image);
        // A picture whose extension is not one the screen takes is not a
        // picture: the extension is the only thing that is looked at.
        assert_eq!(sanitize("photo.gif").unwrap_err(), NameError::Unsupported);
        assert_eq!(sanitize("photo.exe").unwrap_err(), NameError::Unsupported);
    }

    #[test]
    fn the_path_is_taken_off_the_name() {
        let n = sanitize("../../etc/passwd").unwrap_err();
        assert_eq!(n, NameError::Unsupported);
        // A path in front of a real name leaves the name.
        let n = sanitize("/tmp/evil/epub/book.epub").unwrap();
        assert_eq!(n.name, "book.epub");
        // A Windows path is a path too.
        let n = sanitize(r"C:\Users\me\book.epub").unwrap();
        assert_eq!(n.name, "book.epub");
    }

    #[test]
    fn a_name_that_is_only_a_path_is_refused() {
        assert_eq!(sanitize("..").unwrap_err(), NameError::Empty);
        assert_eq!(sanitize("/").unwrap_err(), NameError::Empty);
        assert_eq!(sanitize("").unwrap_err(), NameError::Empty);
        assert_eq!(sanitize("   ").unwrap_err(), NameError::Empty);
    }

    #[test]
    fn control_characters_never_reach_the_file_system() {
        let n = sanitize("book\u{0}.epub").unwrap_err();
        assert_eq!(n, NameError::Empty);
        let n = sanitize("bo\nok.epub").unwrap_err();
        assert_eq!(n, NameError::Empty);
    }

    #[test]
    fn a_chinese_title_stays_readable() {
        let n = sanitize("《西游记》.epub").unwrap();
        assert_eq!(n.name, "《西游记》.epub");
    }

    #[test]
    fn a_long_name_is_cut_but_keeps_its_extension() {
        let long = format!("{}.epub", "x".repeat(300));
        let n = sanitize(&long).unwrap();
        assert!(n.name.chars().count() <= MAX_NAME);
        assert!(n.name.ends_with(".epub"));
    }

    #[test]
    fn the_first_name_is_kept_and_the_next_gets_a_number() {
        let mut taken = std::collections::HashSet::new();
        let first = unique_name("book.epub", |n| taken.contains(n));
        assert_eq!(first, "book.epub");
        taken.insert(first);
        let second = unique_name("book.epub", |n| taken.contains(n));
        assert_eq!(second, "book (2).epub");
        taken.insert(second);
        let third = unique_name("book.epub", |n| taken.contains(n));
        assert_eq!(third, "book (3).epub");
    }

    #[test]
    fn a_name_without_an_extension_still_gets_a_number() {
        let mut taken = std::collections::HashSet::new();
        taken.insert("book".to_string());
        let name = unique_name("book", |n| taken.contains(n));
        assert_eq!(name, "book (2)");
    }
}
