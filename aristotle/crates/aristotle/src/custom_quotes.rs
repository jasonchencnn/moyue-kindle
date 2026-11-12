// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistence and memory pooling for user-added quotes and `quotes.txt`.
//!
//! - Built-in quotes: 100 quotes from `QUOTES` (IDs 1..=100).
//! - User-added quotes: saved to `quotes_custom.json` in the library data directory
//!   with stable IDs starting at 10000 (`CUSTOM_QUOTE_ID_START`).
//! - File quotes: read from `/mnt/us/documents/quotes.txt` (or `--root`) in UTF-8 or GBK,
//!   deduplicated and categorized as `Reading`. Cached in memory.

use std::borrow::Cow;
use std::fs;
use std::path::Path;

use aristotle_ui::screens::home::quotes::{
    allocate_custom_id, assign_file_quote_ids, dedup_file_quotes, parse_quotes_txt, QuoteCategory,
    QuoteItem, FILE_QUOTE_ID_START, QUOTES,
};

pub const CUSTOM_QUOTES_FILE: &str = "quotes_custom.json";

/// In-memory unified pool of all quotations: built-in, file-imported, and custom.
#[derive(Clone, Debug)]
pub struct QuotePool {
    /// User-created quotes loaded from `quotes_custom.json`.
    pub custom: Vec<QuoteItem>,
    /// Quotes parsed and deduplicated from `quotes.txt`.
    pub file_quotes: Vec<QuoteItem>,
    /// Unified cache of all active quotes.
    pub unified: Vec<QuoteItem>,
}

impl Default for QuotePool {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotePool {
    /// Creates an empty quote pool with only the built-in quotations.
    pub fn new() -> Self {
        let mut pool = Self {
            custom: Vec::new(),
            file_quotes: Vec::new(),
            unified: Vec::new(),
        };
        pool.rebuild_unified();
        pool
    }

    /// Loads the pool from `data_dir` (`quotes_custom.json`) and `root` (`quotes.txt`).
    pub fn load(data_dir: &Path, root: &Path) -> Self {
        let custom = Self::load_custom(data_dir);
        let file_quotes = Self::load_file_quotes(root, &custom);
        let mut pool = Self {
            custom,
            file_quotes,
            unified: Vec::new(),
        };
        pool.rebuild_unified();
        pool
    }

    /// Rebuilds the unified list of quotations.
    pub fn rebuild_unified(&mut self) {
        let mut list =
            Vec::with_capacity(QUOTES.len() + self.file_quotes.len() + self.custom.len());
        for q in &QUOTES {
            list.push(q.clone());
        }
        for q in &self.file_quotes {
            list.push(q.clone());
        }
        for q in &self.custom {
            list.push(q.clone());
        }
        self.unified = list;
    }

    /// Returns a slice of all quotations in the pool.
    pub fn all(&self) -> &[QuoteItem] {
        &self.unified
    }

    /// Finds a quotation by its unique ID.
    pub fn find(&self, id: u32) -> Option<&QuoteItem> {
        self.unified.iter().find(|q| q.id == id)
    }

    /// Total number of quotations in the "reading" category.
    pub fn reading_count(&self) -> usize {
        self.unified
            .iter()
            .filter(|q| q.category == QuoteCategory::Reading)
            .count()
    }

    /// Total number of quotations in the "mood" category.
    pub fn mood_count(&self) -> usize {
        self.unified
            .iter()
            .filter(|q| q.category == QuoteCategory::Mood)
            .count()
    }

    /// Returns true if the quotation was added by the user and is deletable.
    pub fn is_custom(&self, id: u32) -> bool {
        self.custom.iter().any(|q| q.id == id)
    }

    /// Reloads quotes from `quotes.txt` under `root` without touching custom quotes.
    pub fn reload_file_quotes(&mut self, root: &Path) {
        self.file_quotes = Self::load_file_quotes(root, &self.custom);
        self.rebuild_unified();
    }

    /// Adds a new custom quotation and saves it to `quotes_custom.json`.
    pub fn add_custom(
        &mut self,
        data_dir: &Path,
        text: String,
        author: String,
        source: String,
        category: QuoteCategory,
    ) -> std::io::Result<QuoteItem> {
        let next_id = allocate_custom_id(self.custom.iter().map(|q| q.id));
        let item = QuoteItem {
            id: next_id,
            text: Cow::Owned(text),
            author: Cow::Owned(author),
            source: Cow::Owned(source),
            category,
            is_original: false,
        };
        self.custom.push(item.clone());
        Self::save_custom(data_dir, &self.custom)?;
        self.rebuild_unified();
        Ok(item)
    }

    /// Deletes a custom quotation by ID and updates `quotes_custom.json`.
    /// Returns true if a quote was found and deleted. Built-in quotes cannot be deleted.
    pub fn delete_custom(&mut self, data_dir: &Path, id: u32) -> std::io::Result<bool> {
        if let Some(pos) = self.custom.iter().position(|q| q.id == id) {
            self.custom.remove(pos);
            Self::save_custom(data_dir, &self.custom)?;
            self.rebuild_unified();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Loads custom quotes from `data_dir/quotes_custom.json`.
    fn load_custom(data_dir: &Path) -> Vec<QuoteItem> {
        let path = data_dir.join(CUSTOM_QUOTES_FILE);
        if !path.exists() {
            return Vec::new();
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Atomically saves custom quotes to `data_dir/quotes_custom.json`.
    fn save_custom(data_dir: &Path, quotes: &[QuoteItem]) -> std::io::Result<()> {
        let path = data_dir.join(CUSTOM_QUOTES_FILE);
        let tmp = data_dir.join(format!("{CUSTOM_QUOTES_FILE}.tmp.{}", std::process::id()));
        let json = serde_json::to_string_pretty(quotes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Reads and parses `/mnt/us/documents/quotes.txt` under `root`.
    fn load_file_quotes(root: &Path, custom: &[QuoteItem]) -> Vec<QuoteItem> {
        let path = root.join("quotes.txt");
        if !path.exists() {
            return Vec::new();
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        // Decode text with encoding detection (UTF-8, GBK, GB18030)
        let text = aristotle_library::encoding::decode_guess(&bytes);
        let parsed = parse_quotes_txt(&text);

        // Deduplicate against built-in quotes and user custom quotes
        let existing_texts = QUOTES
            .iter()
            .map(|q| q.text.as_ref())
            .chain(custom.iter().map(|q| q.text.as_ref()));
        let deduped = dedup_file_quotes(existing_texts, parsed);

        assign_file_quote_ids(deduped, FILE_QUOTE_ID_START)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aristotle_ui::screens::home::quotes::{MOOD_COUNT, READING_COUNT};
    use std::path::PathBuf;

    fn temp_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aristotle-quotes-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_pool_contains_only_builtin() {
        let pool = QuotePool::new();
        assert_eq!(pool.all().len(), 100);
        assert_eq!(pool.reading_count(), READING_COUNT);
        assert_eq!(pool.mood_count(), MOOD_COUNT);
        assert!(!pool.is_custom(1));
    }

    #[test]
    fn custom_quote_lifecycle_and_persistence() {
        let data_dir = temp_test_dir("lifecycle-data");
        let root = temp_test_dir("lifecycle-root");

        let mut pool = QuotePool::load(&data_dir, &root);
        assert_eq!(pool.all().len(), 100);

        // Add custom quote
        let item = pool
            .add_custom(
                &data_dir,
                "自己添加的句子一".into(),
                "张三".into(),
                "《心得》".into(),
                QuoteCategory::Reading,
            )
            .unwrap();
        assert_eq!(item.id, 10000);
        assert_eq!(pool.all().len(), 101);
        assert_eq!(pool.reading_count(), READING_COUNT + 1);
        assert!(pool.is_custom(10000));

        // Add second quote in Mood category
        let item2 = pool
            .add_custom(
                &data_dir,
                "今天心情很好".into(),
                "".into(),
                "".into(),
                QuoteCategory::Mood,
            )
            .unwrap();
        assert_eq!(item2.id, 10001);
        assert_eq!(pool.all().len(), 102);
        assert_eq!(pool.mood_count(), MOOD_COUNT + 1);

        // Reload pool from disk
        let pool_reloaded = QuotePool::load(&data_dir, &root);
        assert_eq!(pool_reloaded.all().len(), 102);
        assert_eq!(pool_reloaded.find(10000).unwrap().text, "自己添加的句子一");
        assert_eq!(pool_reloaded.find(10001).unwrap().text, "今天心情很好");

        // Delete first custom quote
        let mut pool_del = pool_reloaded;
        assert!(pool_del.delete_custom(&data_dir, 10000).unwrap());
        assert_eq!(pool_del.all().len(), 101);
        assert!(pool_del.find(10000).is_none());

        // Built-in quotes cannot be deleted
        assert!(!pool_del.delete_custom(&data_dir, 1).unwrap());
        assert_eq!(pool_del.all().len(), 101);

        // Clean up
        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn quotes_txt_utf8_and_gbk_import() {
        let data_dir = temp_test_dir("txt-data");
        let root = temp_test_dir("txt-root");

        // Write UTF-8 quotes.txt
        let txt_content =
            "导入句子一|李四|《随笔》\n导入句子二\n纸上得来终觉浅，绝知此事要躬行。|陆游\n";
        fs::write(root.join("quotes.txt"), txt_content).unwrap();

        let pool = QuotePool::load(&data_dir, &root);
        // "纸上得来终觉浅..." is a duplicate of built-in quote 1, so only 2 new quotes added
        assert_eq!(pool.all().len(), 102);
        assert_eq!(pool.file_quotes.len(), 2);
        assert_eq!(pool.file_quotes[0].id, FILE_QUOTE_ID_START);
        assert_eq!(pool.file_quotes[0].text, "导入句子一");
        assert_eq!(pool.file_quotes[0].author, "李四");
        assert_eq!(pool.file_quotes[0].source, "《随笔》");
        assert_eq!(pool.file_quotes[0].category, QuoteCategory::Reading);
        // Cannot delete file quotes
        assert!(!pool.is_custom(FILE_QUOTE_ID_START));

        // Test GBK quotes.txt
        let gbk_text = "GBK句子|作者|出处\n";
        let (encoded, _, _) = encoding_rs::GBK.encode(gbk_text);
        fs::write(root.join("quotes.txt"), &encoded).unwrap();

        let mut pool_gbk = pool;
        pool_gbk.reload_file_quotes(&root);
        assert_eq!(pool_gbk.file_quotes.len(), 1);
        assert_eq!(pool_gbk.file_quotes[0].text, "GBK句子");
        assert_eq!(pool_gbk.file_quotes[0].author, "作者");

        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&root);
    }
}
