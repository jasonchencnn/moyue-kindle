//! RFC 4918 `PROPFIND` request body and `207 Multi-Status` response parsing (contract §15.2).
//!
//! Namespace prefixes are never trusted: every element is matched on its local name, which
//! accepts `D:`, `d:`, `DAV:` and a default `DAV:` namespace alike.

use roxmltree::{Document, Node};

use crate::jianguoyun::model::{is_ebook_extension, percent_decode, JianguoyunError, RemoteEntry};

/// The `PROPFIND` body that asks for exactly the properties the channel needs.
pub const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <d:getcontentlength/>
    <d:getlastmodified/>
    <d:getetag/>
    <d:getcontenttype/>
  </d:prop>
</d:propfind>
"#;

/// Parse a `PROPFIND` response into the browsable entries of `requested_dir_url`.
///
/// The directory itself, hidden entries (`.moyue`, `.DS_Store`, `*.sdr`) and files that are not
/// supported ebooks are dropped. Directories sort first, then ebooks, each group by name
/// case-insensitively, so two devices always present the same order.
pub fn parse_propfind_response(
    xml: &str,
    requested_dir_url: &str,
) -> Result<Vec<RemoteEntry>, JianguoyunError> {
    let document =
        Document::parse(xml).map_err(|error| JianguoyunError::XmlParse(error.to_string()))?;
    let multistatus = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "multistatus")
        .ok_or_else(|| JianguoyunError::XmlParse("missing <multistatus> element".to_string()))?;
    let requested_path = normalized_href_path(requested_dir_url);
    let mut entries = Vec::new();
    for response in multistatus
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "response")
    {
        if let Some(entry) = parse_response(response, &requested_path) {
            entries.push(entry);
        }
    }
    sort_entries(&mut entries);
    Ok(entries)
}

fn parse_response(response: Node<'_, '_>, requested_path: &str) -> Option<RemoteEntry> {
    let href = response
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "href")?
        .text()?
        .trim()
        .to_string();
    if href.is_empty() || response_is_not_found(response) {
        return None;
    }

    let path = normalized_href_path(&href);
    if path == requested_path {
        return None;
    }

    let mut display_name = None;
    let mut is_dir = false;
    let mut size = 0_u64;
    let mut etag = None;
    let mut last_modified = None;
    let mut content_type = None;
    let mut saw_prop = false;

    for propstat in response
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "propstat")
    {
        if let Some(status) = propstat_status(propstat) {
            if !(200..300).contains(&status) {
                continue;
            }
        }
        for prop in propstat
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "prop")
        {
            saw_prop = true;
            for child in prop.children().filter(|node| node.is_element()) {
                match child.tag_name().name() {
                    "displayname" => display_name = element_text(child).or(display_name),
                    "resourcetype" => {
                        is_dir = child.children().any(|node| {
                            node.is_element() && node.tag_name().name() == "collection"
                        });
                    }
                    "getcontentlength" => {
                        size = element_text(child)
                            .and_then(|text| text.parse::<u64>().ok())
                            .unwrap_or(0);
                    }
                    "getlastmodified" => {
                        last_modified = element_text(child).or(last_modified);
                    }
                    "getetag" => etag = element_text(child).or(etag),
                    "getcontenttype" => content_type = element_text(child).or(content_type),
                    _ => {}
                }
            }
        }
    }
    if !saw_prop {
        return None;
    }

    let segment = last_path_segment(&path);
    let name = display_name.unwrap_or_else(|| segment.to_string());
    if name.is_empty() {
        return None;
    }
    // Some servers return a display name without the leading dot of a hidden file.
    if name.starts_with('.') || segment.starts_with('.') {
        return None;
    }
    let lower_name = name.to_ascii_lowercase();
    let lower_segment = segment.to_ascii_lowercase();
    if lower_name.ends_with(".sdr") || lower_segment.ends_with(".sdr") {
        return None;
    }

    let format = if is_dir {
        None
    } else {
        let extension = file_extension(segment)?;
        if !is_ebook_extension(extension) {
            return None;
        }
        Some(extension.to_ascii_lowercase())
    };

    Some(RemoteEntry {
        path,
        name,
        is_dir,
        size,
        etag,
        last_modified,
        content_type,
        format,
    })
}

fn element_text(node: Node<'_, '_>) -> Option<String> {
    node.text()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn propstat_status(propstat: Node<'_, '_>) -> Option<u16> {
    let text = propstat
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "status")?
        .text()?;
    status_code(text)
}

fn response_is_not_found(response: Node<'_, '_>) -> bool {
    response
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "status")
        .filter_map(|node| node.text())
        .filter_map(status_code)
        .any(|status| status == 404)
}

/// Parse the numeric code of an RFC 7231 reason line such as `HTTP/1.1 200 OK`.
fn status_code(text: &str) -> Option<u16> {
    text.split_whitespace().find_map(|part| {
        if part.len() == 3 {
            part.parse::<u16>().ok()
        } else {
            None
        }
    })
}

fn file_extension(segment: &str) -> Option<&str> {
    let (stem, extension) = segment.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(extension)
}

fn last_path_segment(path: &str) -> &str {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("")
}

/// Reduce a `href` to a percent-decoded absolute path without a trailing slash.
fn normalized_href_path(href: &str) -> String {
    let trimmed = href.trim();
    let mut path = trimmed;
    if let Some(index) = trimmed.find("://") {
        let after_scheme = &trimmed[index + 3..];
        path = match after_scheme.find('/') {
            Some(slash) => &after_scheme[slash..],
            None => "/",
        };
    } else if let Some(after_host) = trimmed.strip_prefix("//") {
        path = match after_host.find('/') {
            Some(slash) => &after_host[slash..],
            None => "/",
        };
    }
    let without_query = path.split(['?', '#']).next().unwrap_or("/");
    let decoded = percent_decode(without_query);
    let mut normalized = String::with_capacity(decoded.len() + 1);
    for segment in decoded.split('/') {
        if segment.is_empty() {
            continue;
        }
        normalized.push('/');
        normalized.push_str(segment);
    }
    if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    }
}

fn sort_entries(entries: &mut [RemoteEntry]) {
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.path.cmp(&right.path))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR_URL: &str = "https://dav.jianguoyun.com/dav/Books/";

    fn prefix_of(namespace: &str) -> String {
        if namespace.is_empty() {
            String::new()
        } else {
            format!("{namespace}:")
        }
    }

    fn document(namespace: &str, body: &str) -> String {
        let prefix = prefix_of(namespace);
        let declaration = if namespace.is_empty() {
            "xmlns=\"DAV:\"".to_string()
        } else {
            format!("xmlns:{namespace}=\"DAV:\"")
        };
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <{prefix}multistatus {declaration}>{body}</{prefix}multistatus>"
        )
    }

    fn response(
        namespace: &str,
        href: &str,
        display_name: &str,
        is_dir: bool,
        length: u64,
    ) -> String {
        let prefix = prefix_of(namespace);
        let resource_type = if is_dir {
            format!("<{prefix}resourcetype><{prefix}collection/></{prefix}resourcetype>")
        } else {
            format!("<{prefix}resourcetype/>")
        };
        format!(
            "<{prefix}response><{prefix}href>{href}</{prefix}href>\
             <{prefix}propstat><{prefix}prop>\
             <{prefix}displayname>{display_name}</{prefix}displayname>{resource_type}\
             <{prefix}getcontentlength>{length}</{prefix}getcontentlength>\
             <{prefix}getlastmodified>Mon, 01 Jan 2026 00:00:00 GMT</{prefix}getlastmodified>\
             <{prefix}getetag>\"etag-{display_name}\"</{prefix}getetag>\
             <{prefix}getcontenttype>application/octet-stream</{prefix}getcontenttype>\
             </{prefix}prop><{prefix}status>HTTP/1.1 200 OK</{prefix}status>\
             </{prefix}propstat></{prefix}response>"
        )
    }

    fn fixture(namespace: &str) -> String {
        let body = [
            response(namespace, "/dav/Books/", "Books", true, 0),
            response(
                namespace,
                "/dav/Books/%E5%AD%90%E7%9B%AE%E5%BD%95/",
                "子目录",
                true,
                0,
            ),
            response(
                namespace,
                "/dav/Books/%E4%B8%89%E4%BD%93.epub",
                "三体",
                false,
                1234,
            ),
            response(namespace, "/dav/Books/readme.md", "readme.md", false, 12),
            response(namespace, "/dav/Books/三体.sdr", "三体.sdr", false, 3),
            response(namespace, "/dav/Books/.DS_Store", ".DS_Store", false, 6),
            response(namespace, "/dav/Books/.moyue/", ".moyue", true, 0),
        ]
        .concat();
        document(namespace, &body)
    }

    #[test]
    fn parses_every_namespace_prefix() {
        for namespace in ["D", "d", "DAV", ""] {
            let entries = parse_propfind_response(&fixture(namespace), DIR_URL).unwrap();
            assert_eq!(entries.len(), 2, "namespace `{namespace}`");
            assert!(entries[0].is_dir);
            assert_eq!(entries[0].name, "子目录");
            assert_eq!(entries[0].path, "/dav/Books/子目录");
            assert_eq!(entries[1].name, "三体");
            assert_eq!(entries[1].path, "/dav/Books/三体.epub");
            assert_eq!(entries[1].size, 1234);
            assert_eq!(entries[1].format.as_deref(), Some("epub"));
            assert_eq!(entries[1].etag.as_deref(), Some("\"etag-三体\""));
        }
    }

    #[test]
    fn skips_the_requested_directory_itself() {
        let entries = parse_propfind_response(&fixture("D"), DIR_URL).unwrap();
        assert!(entries.iter().all(|entry| entry.name != "Books"));
        assert!(entries.iter().all(|entry| entry.path != "/dav/Books"));
    }

    #[test]
    fn requested_directory_matches_with_or_without_trailing_slash() {
        let xml = document("D", &response("D", "/dav/Books/", "Books", true, 0));
        assert!(
            parse_propfind_response(&xml, "https://dav.jianguoyun.com/dav/Books")
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_propfind_response(&xml, "https://dav.jianguoyun.com/dav/Books/")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn filters_hidden_sync_and_non_ebook_entries() {
        let entries = parse_propfind_response(&fixture("d"), DIR_URL).unwrap();
        for filtered in ["readme.md", "三体.sdr", ".DS_Store", ".moyue"] {
            assert!(
                entries.iter().all(|entry| entry.name != filtered),
                "`{filtered}` should be filtered"
            );
        }
    }

    #[test]
    fn sorts_directories_first_then_books_case_insensitively() {
        let body = [
            response("D", "/dav/Books/", "Books", true, 0),
            response("D", "/dav/Books/zeta/", "zeta", true, 0),
            response("D", "/dav/Books/Alpha/", "Alpha", true, 0),
            response("D", "/dav/Books/b.EPUB", "b.EPUB", false, 1),
            response("D", "/dav/Books/A.pdf", "A.pdf", false, 2),
            response("D", "/dav/Books/c.txt", "c.txt", false, 3),
        ]
        .concat();
        let entries = parse_propfind_response(&document("D", &body), DIR_URL).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta", "A.pdf", "b.EPUB", "c.txt"]);
    }

    #[test]
    fn falls_back_to_href_name_when_displayname_is_missing() {
        let xml = document(
            "D",
            "<D:response><D:href>/dav/Books/dune.fb2</D:href><D:propstat><D:prop>\
             <D:resourcetype/><D:getcontentlength>9</D:getcontentlength>\
             </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        );
        let entries = parse_propfind_response(&xml, DIR_URL).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "dune.fb2");
        assert_eq!(entries[0].format.as_deref(), Some("fb2"));
    }

    #[test]
    fn accepts_absolute_hrefs_with_encoded_spaces() {
        let body = response(
            "D",
            "https://dav.jianguoyun.com/dav/Books/My%20Book.azw3",
            "My Book",
            false,
            42,
        );
        let entries = parse_propfind_response(&document("D", &body), DIR_URL).unwrap();
        assert_eq!(entries[0].path, "/dav/Books/My Book.azw3");
        assert_eq!(entries[0].name, "My Book");
        assert_eq!(entries[0].format.as_deref(), Some("azw3"));
    }

    #[test]
    fn skips_entries_whose_only_propstat_is_not_found() {
        let xml = document(
            "D",
            "<D:response><D:href>/dav/Books/ghost.epub</D:href>\
             <D:propstat><D:prop><D:displayname>ghost</D:displayname></D:prop>\
             <D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response>\
             <D:response><D:href>/dav/Books/missing.epub</D:href>\
             <D:status>HTTP/1.1 404 Not Found</D:status></D:response>",
        );
        assert!(parse_propfind_response(&xml, DIR_URL).unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(matches!(
            parse_propfind_response("<not-xml", DIR_URL),
            Err(JianguoyunError::XmlParse(_))
        ));
        assert!(matches!(
            parse_propfind_response("<root/>", DIR_URL),
            Err(JianguoyunError::XmlParse(_))
        ));
    }

    #[test]
    fn empty_multistatus_is_a_valid_empty_listing() {
        let xml = document("D", "");
        assert!(parse_propfind_response(&xml, DIR_URL).unwrap().is_empty());
    }

    #[test]
    fn skips_the_directory_when_the_requested_url_is_encoded() {
        let xml = document(
            "D",
            &response("D", "/dav/%E4%B9%A6%E5%BA%93/", "书库", true, 0),
        );
        assert!(
            parse_propfind_response(&xml, "https://dav.jianguoyun.com/dav/书库/")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn status_code_reads_reason_lines() {
        assert_eq!(status_code("HTTP/1.1 207 Multi-Status"), Some(207));
        assert_eq!(status_code("HTTP/1.1 404 Not Found"), Some(404));
        assert_eq!(status_code("garbage"), None);
    }

    #[test]
    fn normalized_href_path_handles_hosts_and_queries() {
        assert_eq!(
            normalized_href_path("https://dav.jianguoyun.com/dav/Books/%E4%B8%89%E4%BD%93.epub"),
            "/dav/Books/三体.epub"
        );
        assert_eq!(normalized_href_path("//host/dav/x/"), "/dav/x");
        assert_eq!(normalized_href_path("/dav/x/?depth=1"), "/dav/x");
        assert_eq!(normalized_href_path("/"), "/");
    }
}
