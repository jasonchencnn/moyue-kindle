//! 目录（TOC）数据模型与「这一页属于哪一节」的纯逻辑。

/// 目录项。
///
/// 书里的目录是树形的，这里把它摊平成一个列表：按书里的顺序排列，
/// [`TocEntry::level`] 记层级（顶层是 0，往里每嵌套一层加 1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    /// 标题文本。
    pub title: String,
    /// 层级，0 起：顶层目录项是 0。
    pub level: u32,
    /// 目录项的位置（XPointer），可以拿去反查页码。
    pub xpointer: String,
    /// 所在页码，0 起，按当前排版算。
    pub page: u32,
}

/// 第 `page` 页（0 起）属于哪个目录项。
///
/// 返回目录项在列表里的下标。页面上同时落在几层目录项的范围里时，取层级
/// 最深的那一个。第一个目录项之前的页不属于任何目录项，返回 `None`。
///
/// 一个目录项的范围是：从它自己的页开始，到下一个同级（或更浅）目录项的
/// 页之前为止。目录里没有更深一层时，同级目录项的范围就是它的范围。
///
/// 纯逻辑，不碰引擎：目录列表与排版对得上，这里算出来的结果就对得上。
pub fn chapter_of_page(entries: &[TocEntry], page: u32) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, entry) in entries.iter().enumerate() {
        if entry.page > page {
            continue;
        }
        // 下一个同级或更浅的目录项就是本项范围的终点。
        let end = entries[index + 1..]
            .iter()
            .find(|next| next.level <= entry.level)
            .map(|next| next.page)
            .unwrap_or(u32::MAX);
        if page >= end {
            continue; // 本项的范围已经过去了
        }
        match best {
            Some(current) if entries[current].level >= entry.level => {}
            _ => best = Some(index),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个目录项。
    fn entry(title: &str, level: u32, page: u32) -> TocEntry {
        TocEntry {
            title: title.to_string(),
            level,
            xpointer: format!("/body/DocFragment[{page}]"),
            page,
        }
    }

    #[test]
    fn an_empty_toc_has_no_chapter() {
        assert_eq!(chapter_of_page(&[], 0), None);
        assert_eq!(chapter_of_page(&[], 42), None);
    }

    #[test]
    fn pages_before_the_first_entry_have_no_chapter() {
        let toc = vec![entry("第一章", 0, 3), entry("第二章", 0, 7)];
        assert_eq!(chapter_of_page(&toc, 0), None);
        assert_eq!(chapter_of_page(&toc, 2), None);
        assert_eq!(chapter_of_page(&toc, 3), Some(0));
    }

    #[test]
    fn a_flat_toc_covers_pages_up_to_the_next_entry() {
        let toc = vec![
            entry("第一章", 0, 0),
            entry("第二章", 0, 3),
            entry("第三章", 0, 6),
        ];
        assert_eq!(chapter_of_page(&toc, 0), Some(0));
        assert_eq!(chapter_of_page(&toc, 2), Some(0));
        assert_eq!(chapter_of_page(&toc, 3), Some(1));
        assert_eq!(chapter_of_page(&toc, 5), Some(1));
        assert_eq!(chapter_of_page(&toc, 6), Some(2));
        // 末章一直管到最后一页。
        assert_eq!(chapter_of_page(&toc, 999), Some(2));
    }

    #[test]
    fn a_nested_toc_picks_the_deepest_entry() {
        let toc = vec![
            entry("第一章", 0, 0),
            entry("1.1", 1, 1),
            entry("1.2", 1, 4),
            entry("第二章", 0, 8),
        ];
        assert_eq!(chapter_of_page(&toc, 0), Some(0));
        assert_eq!(chapter_of_page(&toc, 1), Some(1));
        assert_eq!(chapter_of_page(&toc, 3), Some(1));
        assert_eq!(chapter_of_page(&toc, 4), Some(2));
        assert_eq!(chapter_of_page(&toc, 7), Some(2));
        assert_eq!(chapter_of_page(&toc, 8), Some(3));
    }

    #[test]
    fn a_child_starting_on_the_same_page_wins() {
        let toc = vec![
            entry("第一章", 0, 0),
            entry("1.1", 1, 0),
            entry("第二章", 0, 5),
        ];
        assert_eq!(chapter_of_page(&toc, 0), Some(1));
    }

    #[test]
    fn the_last_entry_owns_the_rest_of_the_book() {
        let toc = vec![entry("第一章", 0, 0), entry("第二章", 0, 2)];
        assert_eq!(chapter_of_page(&toc, 1), Some(0));
        assert_eq!(chapter_of_page(&toc, 2), Some(1));
        assert_eq!(chapter_of_page(&toc, 10_000), Some(1));
    }
}
