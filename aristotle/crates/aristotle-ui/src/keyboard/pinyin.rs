// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pinyin to single-character lookup and pinyin initials matching.
//!
//! Based on pinyin mapping from rime-pinyin-simp / KOReader (Apache-2.0).

use super::opencc_data::to_simplified_str;
use super::pinyin_data::{CHAR_ENTRIES, SYLLABLES};

/// Looks up the exact candidate characters for a pinyin syllable.
///
/// Characters in the returned string are ordered by usage frequency.
pub fn lookup(syllable: &str) -> Option<&'static str> {
    let syl = syllable.trim().to_ascii_lowercase();
    let idx = SYLLABLES
        .binary_search_by_key(&syl.as_str(), |(k, _)| *k)
        .ok()?;
    Some(SYLLABLES[idx].1)
}

/// Retrieves single-character candidates for a pinyin query string.
///
/// If `query` is an exact syllable (e.g. "hong"), returns its candidates.
/// If `query` is a prefix of one or more syllables (e.g. "ho"), collects
/// candidates from all matching syllables in frequency order, deduplicated.
/// Returns an empty vector for invalid or unrecognized pinyin strings.
pub fn candidates(query: &str) -> Vec<char> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }

    if let Some(cands) = lookup(&q) {
        return cands.chars().collect();
    }

    // Prefix match across all syllables.
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Use binary search to locate the range of syllables starting with `q`.
    let start_idx = match SYLLABLES.binary_search_by_key(&q.as_str(), |(k, _)| *k) {
        Ok(i) => i,
        Err(i) => i,
    };

    for &(syl, chars_str) in &SYLLABLES[start_idx..] {
        if !syl.starts_with(&q) {
            break;
        }
        for ch in chars_str.chars() {
            if seen.insert(ch) {
                out.push(ch);
            }
        }
    }

    out
}

/// Returns the primary pinyin syllable for a Chinese character.
///
/// For polyphonic characters, returns the most frequent/common reading.
pub fn char_primary_pinyin(c: char) -> Option<&'static str> {
    match CHAR_ENTRIES.binary_search_by_key(&c, |&(k, _, _)| k) {
        Ok(idx) => Some(SYLLABLES[CHAR_ENTRIES[idx].2 as usize].0),
        Err(_) => None,
    }
}

/// Returns the primary pinyin initial letter ('a'..='z') for a Chinese character.
pub fn char_pinyin_initial(c: char) -> Option<char> {
    char_primary_pinyin(c).and_then(|s| s.chars().next())
}

/// Returns the initial letter bitmask for a Chinese character.
///
/// Bit 0 corresponds to 'a', bit 25 corresponds to 'z'.
/// Returns 0 for non-Chinese or unmapped characters.
pub fn char_initial_mask(c: char) -> u32 {
    match CHAR_ENTRIES.binary_search_by_key(&c, |&(k, _, _)| k) {
        Ok(idx) => CHAR_ENTRIES[idx].1,
        Err(_) => 0,
    }
}

/// Checks whether character `c` has the given pinyin initial ('a'..='z').
pub fn char_has_initial(c: char, initial: char) -> bool {
    let initial = initial.to_ascii_lowercase();
    if !initial.is_ascii_lowercase() {
        return false;
    }
    let bit = 1 << ((initial as u8) - b'a');
    (char_initial_mask(c) & bit) != 0
}

/// Checks if a sequence of Chinese characters in `text` has pinyin initials
/// matching `query` (case-insensitive ASCII string).
///
/// For example, "hlm" matches "红楼梦", "cxq" matches "曹雪芹".
pub fn match_initials(text: &str, query: &str) -> bool {
    let q: Vec<char> = query
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();

    if q.is_empty() {
        return false;
    }

    let text_chars: Vec<char> = text.chars().collect();
    if text_chars.is_empty() {
        return false;
    }

    // Check at every possible start position in text.
    for start in 0..text_chars.len() {
        let mut text_idx = start;
        let mut q_idx = 0;

        while text_idx < text_chars.len() && q_idx < q.len() {
            let tc = text_chars[text_idx];
            let qc = q[q_idx];

            if char_has_initial(tc, qc) {
                q_idx += 1;
                text_idx += 1;
            } else if tc.is_ascii_alphabetic() && tc.to_ascii_lowercase() == qc {
                // If text contains latin character, match directly
                q_idx += 1;
                text_idx += 1;
            } else if char_initial_mask(tc) == 0
                && (tc.is_whitespace() || tc.is_ascii_punctuation())
            {
                // Skip punctuation/whitespace in text
                text_idx += 1;
            } else {
                break;
            }
        }

        if q_idx == q.len() {
            return true;
        }
    }

    false
}

/// Matches a sequence of syllables and initials against a query.
/// Supports:
/// - Exact initials matching ("hlm" matches "红楼梦")
/// - Full pinyin prefix matching ("hong", "hongl", "honglou", "hongloumeng")
/// - Mixed syllables and initials ("honglm", "hongl")
pub fn match_pinyin_seq(syllables: &[&str], initials: &str, query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return false;
    }
    let q_bytes = q.as_bytes();
    let n = syllables.len();
    if n == 0 {
        return false;
    }
    let init_bytes = initials.as_bytes();

    for start in 0..n {
        if match_pinyin_seq_helper(syllables, init_bytes, start, q_bytes, 0) {
            return true;
        }
    }
    false
}

fn match_pinyin_seq_helper(
    syllables: &[&str],
    init_bytes: &[u8],
    c_idx: usize,
    q_bytes: &[u8],
    q_idx: usize,
) -> bool {
    if q_idx == q_bytes.len() {
        return true;
    }
    if c_idx >= syllables.len() {
        return false;
    }

    // Option 1: match initial letter
    if c_idx < init_bytes.len()
        && q_bytes[q_idx] == init_bytes[c_idx]
        && match_pinyin_seq_helper(syllables, init_bytes, c_idx + 1, q_bytes, q_idx + 1)
    {
        return true;
    }

    // Option 2: match full syllable or syllable prefix
    let syl = syllables[c_idx].as_bytes();
    let rem_q = &q_bytes[q_idx..];
    if syl.starts_with(rem_q) {
        return true;
    }
    if rem_q.starts_with(syl)
        && match_pinyin_seq_helper(syllables, init_bytes, c_idx + 1, q_bytes, q_idx + syl.len())
    {
        return true;
    }

    false
}

/// Matches a book's title and author against a user query string.
///
/// Normalizes both book metadata and user query to Simplified Chinese before matching.
/// Matches if:
/// 1. Query is a substring of simplified title or author.
/// 2. Query matches pinyin initials or full pinyin prefix sequence (e.g. "hong", "hlm", "honglou").
pub fn matches_book(title: &str, author: &str, query: &str) -> bool {
    let q_norm = to_simplified_str(query);
    let q = q_norm.trim();
    if q.is_empty() {
        return false;
    }

    let t_simp = to_simplified_str(title);
    let a_simp = to_simplified_str(author);

    // 1. Direct substring match on simplified text
    if t_simp.contains(q) || a_simp.contains(q) {
        return true;
    }

    // 2. Extract syllables and initials
    let mut t_syls = Vec::new();
    let mut t_inits = String::new();
    for c in t_simp.chars() {
        if let Some(syl) = char_primary_pinyin(c) {
            t_syls.push(syl);
            t_inits.push(syl.chars().next().unwrap());
        } else if c.is_ascii_alphabetic() {
            t_inits.push(c.to_ascii_lowercase());
        }
    }

    let mut a_syls = Vec::new();
    let mut a_inits = String::new();
    for c in a_simp.chars() {
        if let Some(syl) = char_primary_pinyin(c) {
            a_syls.push(syl);
            a_inits.push(syl.chars().next().unwrap());
        } else if c.is_ascii_alphabetic() {
            a_inits.push(c.to_ascii_lowercase());
        }
    }

    // Initials substring match
    if t_inits.contains(q) || a_inits.contains(q) {
        return true;
    }

    // Syllables sequence / prefix match
    if match_pinyin_seq(&t_syls, &t_inits, q) || match_pinyin_seq(&a_syls, &a_inits, q) {
        return true;
    }

    // Fallback: multi-tone initials match
    if match_initials(&t_simp, q) || match_initials(&a_simp, q) {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_syllables_lookup() {
        let ba = lookup("ba").expect("ba should exist");
        assert!(ba.starts_with("吧把八爸"));

        let hong = lookup("hong").expect("hong should exist");
        assert!(hong.starts_with("红宏洪哄鸿"));

        let zhong = lookup("zhong").expect("zhong should exist");
        assert!(zhong.contains('中'));
        assert!(zhong.contains('重'));

        let ni = lookup("ni").expect("ni should exist");
        assert!(ni.starts_with("你拟尼呢泥"));

        let hao = lookup("hao").expect("hao should exist");
        assert!(hao.starts_with("好号浩豪"));
        assert!(hao.contains('毫'));
    }

    #[test]
    fn polyphonic_characters() {
        // 重: zhong / chong
        let zhong = lookup("zhong").expect("zhong");
        let chong = lookup("chong").expect("chong");
        assert!(zhong.contains('重'));
        assert!(chong.contains('重'));

        assert!(char_has_initial('重', 'z'));
        assert!(char_has_initial('重', 'c'));

        // 行: hang / xing
        let hang = lookup("hang").expect("hang");
        let xing = lookup("xing").expect("xing");
        assert!(hang.contains('行'));
        assert!(xing.contains('行'));

        assert!(char_has_initial('行', 'h'));
        assert!(char_has_initial('行', 'x'));

        // 乐: le / yue
        let le = lookup("le").expect("le");
        let yue = lookup("yue").expect("yue");
        assert!(le.contains('乐'));
        assert!(yue.contains('乐'));

        assert!(char_has_initial('乐', 'l'));
        assert!(char_has_initial('乐', 'y'));
    }

    #[test]
    fn invalid_and_empty_syllables() {
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("   "), None);
        assert_eq!(lookup("xyz"), None);
        assert_eq!(lookup("123"), None);
        assert_eq!(lookup("!@#"), None);

        assert!(candidates("").is_empty());
        assert!(candidates("xyz").is_empty());
        assert!(candidates("123").is_empty());
    }

    #[test]
    fn prefix_candidates() {
        let cands_ho = candidates("ho");
        assert!(!cands_ho.is_empty());
        assert!(cands_ho.contains(&'红'));

        let cands_h = candidates("h");
        assert!(!cands_h.is_empty());
        assert!(cands_h.contains(&'哈'));
        assert!(cands_h.contains(&'红'));
    }

    #[test]
    fn pinyin_initials_matching() {
        assert!(match_initials("红楼梦", "hlm"));
        assert!(match_initials("红楼梦", "hl"));
        assert!(match_initials("红楼梦", "lm"));
        assert!(!match_initials("红楼梦", "abc"));

        assert!(match_initials("曹雪芹", "cxq"));
        assert!(match_initials("曹雪芹", "cx"));
        assert!(!match_initials("曹雪芹", "z"));

        // Polyphone matching
        assert!(match_initials("重复", "cf"));
        assert!(match_initials("重复", "zf"));

        // Mixed with whitespace / punctuation
        assert!(match_initials("平凡 的 世界", "pfdsj"));
    }

    #[test]
    fn book_matching() {
        let title = "紅樓夢";
        let author = "曹雪芹";

        // Traditional title with simplified candidate query
        assert!(matches_book(title, author, "红"));
        assert!(matches_book(title, author, "楼"));
        assert!(matches_book(title, author, "梦"));

        // Traditional title with traditional query
        assert!(matches_book(title, author, "紅"));
        assert!(matches_book(title, author, "樓"));

        // Author substring
        assert!(matches_book(title, author, "曹"));
        assert!(matches_book(title, author, "雪芹"));

        // Uncommitted pinyin & prefix matching
        assert!(matches_book(title, author, "hong"));
        assert!(matches_book(title, author, "hlm"));
        assert!(matches_book(title, author, "honglou"));
        assert!(matches_book(title, author, "hongl"));
        assert!(matches_book(title, author, "hongloum"));
        assert!(matches_book(title, author, "cxq"));
        assert!(matches_book(title, author, "cao"));
        assert!(matches_book(title, author, "caoxue"));

        // Non-match
        assert!(!matches_book(title, author, "三国"));
        assert!(!matches_book(title, author, "luoxun"));

        // Latin book
        assert!(matches_book("War and Peace", "Leo Tolstoy", "peace"));
        assert!(matches_book("War and Peace", "Leo Tolstoy", "tolstoy"));
    }
}
