// SPDX-License-Identifier: AGPL-3.0-or-later
//! State management for the screen keyboard and input method.

use super::pinyin;

/// Keyboard input mode: Chinese (pinyin) or English (direct text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputMode {
    #[default]
    Chinese,
    English,
}

/// The active keyboard page: standard QWERTY letters or numbers/symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeyboardPage {
    #[default]
    Letters,
    Symbols,
}

/// Shift state for English letters: off, next letter only, or caps lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shift {
    #[default]
    Off,
    Once,
    Lock,
}

/// The complete state of the on-screen keyboard, candidate bar, and input text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardState {
    pub mode: InputMode,
    pub page: KeyboardPage,
    /// Uncommitted pinyin syllable string (e.g. "hong").
    pub pinyin: String,
    /// Text already committed to the input field.
    pub committed: String,
    /// Current 0-based candidate page index.
    pub cand_page: usize,
    /// Number of candidates displayed per page (default: 7).
    pub cands_per_page: usize,
    /// Cached candidates for the current pinyin string.
    pub all_candidates: Vec<char>,
    /// Uppercase state for English letter input.
    pub shift: Shift,
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyboardState {
    pub const DEFAULT_CANDS_PER_PAGE: usize = 7;

    /// Creates a fresh keyboard state in Chinese mode on the Letters page.
    pub fn new() -> Self {
        Self {
            mode: InputMode::Chinese,
            page: KeyboardPage::Letters,
            pinyin: String::new(),
            committed: String::new(),
            cand_page: 0,
            cands_per_page: Self::DEFAULT_CANDS_PER_PAGE,
            all_candidates: Vec::new(),
            shift: Shift::Off,
        }
    }

    /// Creates a keyboard state preloaded with committed text.
    pub fn with_committed(committed: &str) -> Self {
        let mut s = Self::new();
        s.committed = committed.to_string();
        s
    }

    /// The effective query text: committed text if non-empty, otherwise pinyin or top candidate.
    pub fn query(&self) -> String {
        if !self.committed.is_empty() {
            self.committed.clone()
        } else if !self.all_candidates.is_empty() {
            self.all_candidates[0].to_string()
        } else {
            self.pinyin.clone()
        }
    }

    /// Total number of candidate pages for the current pinyin.
    pub fn total_cand_pages(&self) -> usize {
        if self.all_candidates.is_empty() {
            0
        } else {
            self.all_candidates.len().div_ceil(self.cands_per_page)
        }
    }

    /// The slice of candidates visible on the current page.
    pub fn current_candidates(&self) -> &[char] {
        if self.all_candidates.is_empty() {
            &[]
        } else {
            let start = self.cand_page * self.cands_per_page;
            if start >= self.all_candidates.len() {
                &[]
            } else {
                let end = (start + self.cands_per_page).min(self.all_candidates.len());
                &self.all_candidates[start..end]
            }
        }
    }

    pub fn has_prev_cand_page(&self) -> bool {
        self.cand_page > 0
    }

    pub fn has_next_cand_page(&self) -> bool {
        self.cand_page + 1 < self.total_cand_pages()
    }

    /// Handles a character key press.
    pub fn press_key(&mut self, ch: char) {
        if self.mode == InputMode::Chinese
            && self.page == KeyboardPage::Letters
            && ch.is_ascii_alphabetic()
        {
            self.pinyin.push(ch.to_ascii_lowercase());
            self.all_candidates = pinyin::candidates(&self.pinyin);
            self.cand_page = 0;
            return;
        }
        // Direct commitment (English mode or Symbol page or non-letter key).
        if ch.is_ascii_alphabetic() && self.shift != Shift::Off {
            self.committed.push(ch.to_ascii_uppercase());
            if self.shift == Shift::Once {
                self.shift = Shift::Off;
            }
            return;
        }
        self.committed.push(ch);
    }

    /// Handles space key press.
    ///
    /// In Chinese mode with candidates: commits the first candidate.
    /// In Chinese mode with uncommitted pinyin but no candidates: commits pinyin directly.
    /// Otherwise: appends a space character.
    pub fn press_space(&mut self) {
        if self.mode == InputMode::Chinese && !self.all_candidates.is_empty() {
            self.committed.push(self.all_candidates[0]);
            self.pinyin.clear();
            self.all_candidates.clear();
            self.cand_page = 0;
        } else if !self.pinyin.is_empty() {
            self.committed.push_str(&self.pinyin);
            self.pinyin.clear();
            self.all_candidates.clear();
            self.cand_page = 0;
        } else {
            self.committed.push(' ');
        }
    }

    /// Handles delete / backspace key press.
    ///
    /// Deletes the last pinyin letter first; if pinyin is empty, deletes the last committed character.
    pub fn press_delete(&mut self) {
        if !self.pinyin.is_empty() {
            self.pinyin.pop();
            self.all_candidates = if self.pinyin.is_empty() {
                Vec::new()
            } else {
                pinyin::candidates(&self.pinyin)
            };
            self.cand_page = 0;
        } else if !self.committed.is_empty() {
            self.committed.pop();
        }
    }

    /// Picks a candidate by index on the current candidate page (0-based).
    pub fn pick_candidate(&mut self, index_on_page: usize) -> bool {
        let global_idx = self.cand_page * self.cands_per_page + index_on_page;
        if global_idx < self.all_candidates.len() {
            let chosen = self.all_candidates[global_idx];
            self.committed.push(chosen);
            self.pinyin.clear();
            self.all_candidates.clear();
            self.cand_page = 0;
            true
        } else {
            false
        }
    }

    pub fn prev_cand_page(&mut self) {
        if self.cand_page > 0 {
            self.cand_page -= 1;
        }
    }

    pub fn next_cand_page(&mut self) {
        if self.cand_page + 1 < self.total_cand_pages() {
            self.cand_page += 1;
        }
    }

    /// Switches between Chinese and English input modes.
    pub fn set_mode(&mut self, mode: InputMode) {
        if self.mode != mode {
            if !self.pinyin.is_empty() {
                self.committed.push_str(&self.pinyin);
                self.pinyin.clear();
                self.all_candidates.clear();
                self.cand_page = 0;
            }
            self.mode = mode;
        }
    }

    /// Cycles the shift key: off → next letter uppercase → caps lock → off.
    pub fn press_shift(&mut self) {
        self.shift = match self.shift {
            Shift::Off => Shift::Once,
            Shift::Once => Shift::Lock,
            Shift::Lock => Shift::Off,
        };
    }

    /// Sets the active page (Letters or Symbols).
    pub fn set_page(&mut self, page: KeyboardPage) {
        self.page = page;
    }

    /// Toggles between Letters and Symbols page.
    pub fn toggle_page(&mut self) {
        self.page = match self.page {
            KeyboardPage::Letters => KeyboardPage::Symbols,
            KeyboardPage::Symbols => KeyboardPage::Letters,
        };
    }

    /// Resets all input and pages to the initial state.
    pub fn clear(&mut self) {
        self.pinyin.clear();
        self.committed.clear();
        self.cand_page = 0;
        self.all_candidates.clear();
        self.page = KeyboardPage::Letters;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_typing_and_candidate_selection() {
        let mut state = KeyboardState::new();
        assert_eq!(state.mode, InputMode::Chinese);
        assert_eq!(state.page, KeyboardPage::Letters);

        // Type "hong"
        for c in "hong".chars() {
            state.press_key(c);
        }
        assert_eq!(state.pinyin, "hong");
        assert!(!state.all_candidates.is_empty());
        assert_eq!(state.all_candidates[0], '红');

        // Pick 1st candidate "红"
        assert!(state.pick_candidate(0));
        assert_eq!(state.committed, "红");
        assert_eq!(state.pinyin, "");
        assert!(state.all_candidates.is_empty());

        // Type "lou"
        for c in "lou".chars() {
            state.press_key(c);
        }
        assert_eq!(state.pinyin, "lou");
        assert_eq!(state.all_candidates[0], '楼');

        // Space selects 1st candidate
        state.press_space();
        assert_eq!(state.committed, "红楼");
        assert_eq!(state.pinyin, "");

        // Type "meng"
        for c in "meng".chars() {
            state.press_key(c);
        }
        assert_eq!(state.pinyin, "meng");
        state.press_space();
        assert_eq!(state.committed, "红楼梦");
    }

    #[test]
    fn delete_behavior() {
        let mut state = KeyboardState::new();
        for c in "ho".chars() {
            state.press_key(c);
        }
        assert_eq!(state.pinyin, "ho");

        // Delete pops from pinyin
        state.press_delete();
        assert_eq!(state.pinyin, "h");

        state.press_delete();
        assert_eq!(state.pinyin, "");

        // Delete pops from committed
        state.committed = "红楼".to_string();
        state.press_delete();
        assert_eq!(state.committed, "红");
    }

    #[test]
    fn english_mode_direct_commit() {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::English);

        for c in "Rust".chars() {
            state.press_key(c);
        }
        assert_eq!(state.committed, "Rust");
        assert_eq!(state.pinyin, "");

        state.press_space();
        assert_eq!(state.committed, "Rust ");
    }

    #[test]
    fn symbols_mode() {
        let mut state = KeyboardState::new();
        state.toggle_page();
        assert_eq!(state.page, KeyboardPage::Symbols);

        state.press_key('1');
        state.press_key('2');
        state.press_key('3');
        assert_eq!(state.committed, "123");

        state.toggle_page();
        assert_eq!(state.page, KeyboardPage::Letters);
    }

    #[test]
    fn shift_once_then_lock() {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::English);
        state.press_shift();
        state.press_key('a');
        state.press_key('b');
        state.press_shift();
        state.press_shift();
        state.press_key('c');
        state.press_key('d');
        state.press_key('1');
        state.press_shift();
        state.press_key('e');
        assert_eq!(state.committed, "AbCD1e");
    }

    #[test]
    fn candidate_pagination() {
        let mut state = KeyboardState::new();
        for c in "hong".chars() {
            state.press_key(c);
        }
        assert!(state.total_cand_pages() > 1);
        assert!(!state.has_prev_cand_page());
        assert!(state.has_next_cand_page());

        let page0 = state.current_candidates().to_vec();
        assert_eq!(page0.len(), 7);

        state.next_cand_page();
        assert_eq!(state.cand_page, 1);
        assert!(state.has_prev_cand_page());
        let page1 = state.current_candidates().to_vec();
        assert_ne!(page0, page1);

        state.prev_cand_page();
        assert_eq!(state.cand_page, 0);
        assert_eq!(state.current_candidates(), &page0[..]);
    }
}
