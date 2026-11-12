//! 页内可以点、可以选的东西：词与链接。

use inkread_platform::Rect;

/// 页内的一个词。
///
/// 断词规则由排版引擎决定：西文按空格与标点断词，汉字一个字算一个词。
/// 所以中文正文里拿到的是一个个单字，不是词组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// 词的文本。
    pub text: String,
    /// 词在页面上的矩形。跨行时每行一个，通常只有一个。
    pub rects: Vec<Rect>,
    /// 词首的位置（XPointer）。
    pub start_xpointer: String,
    /// 词尾的位置（XPointer）。
    pub end_xpointer: String,
}

/// 链接指向哪里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// 书内锚点。
    ///
    /// 书里的链接写坏了、目标定位不到时，`xpointer` 是空串、`page` 是
    /// `None`。
    Internal {
        /// 目标位置（XPointer）。
        xpointer: String,
        /// 目标所在页码，0 起，按当前排版算。
        page: Option<u32>,
    },
    /// 书外 URL。
    External {
        /// 外部地址。
        url: String,
    },
}

/// 页内的一个链接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// 链接目标原文。
    ///
    /// 引擎会把书内的锚点改写成内部形式（形如 `#_doc_fragment_2_ 注 1`），
    /// 这个字段就是改写后的样子。
    pub href: String,
    /// 链接指向哪里。
    pub target: LinkTarget,
    /// 链接在页面上的矩形。跨行时每行一个。
    pub rects: Vec<Rect>,
}
