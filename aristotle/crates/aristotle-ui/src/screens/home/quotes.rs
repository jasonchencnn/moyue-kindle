// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quotations of the home screen and quote library.
//!
//! The hundred quotations are compiled into the binary from
//! `docs/ui/quotes-zh.json`, so the screen never reads a file. One of them
//! is the quotation of the day; the button on the card walks to the next.
//!
//! The quotations are Chinese, and so are their authors and their
//! sources: the file the task names has no other language. An English
//! screen shows the same Chinese words.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The category of a quotation: either reading or mood.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuoteCategory {
    Reading,
    Mood,
}

impl QuoteCategory {
    pub const ALL: [QuoteCategory; 2] = [QuoteCategory::Reading, QuoteCategory::Mood];

    pub fn as_str(self) -> &'static str {
        match self {
            QuoteCategory::Reading => "读书",
            QuoteCategory::Mood => "心情",
        }
    }
}

/// How the quotation of the day is selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuoteMode {
    #[default]
    AllRandom,
    FavoritesRandom,
    Fixed,
}

/// One quotation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteItem {
    pub id: u32,
    pub text: Cow<'static, str>,
    #[serde(default)]
    pub author: Cow<'static, str>,
    #[serde(default)]
    pub source: Cow<'static, str>,
    pub category: QuoteCategory,
    /// True for the ones the app wrote itself: they carry the app's mark
    /// instead of an author. The file marks them by their source, not by
    /// the author being empty: a classical line whose author the file
    /// does not name is still a classical line.
    #[serde(default)]
    pub is_original: bool,
}

impl QuoteItem {
    pub const fn builtin(
        id: u32,
        text: &'static str,
        author: &'static str,
        source: &'static str,
        category: QuoteCategory,
        is_original: bool,
    ) -> Self {
        Self {
            id,
            text: Cow::Borrowed(text),
            author: Cow::Borrowed(author),
            source: Cow::Borrowed(source),
            category,
            is_original,
        }
    }

    pub fn custom(
        id: u32,
        text: impl Into<String>,
        author: impl Into<String>,
        source: impl Into<String>,
        category: QuoteCategory,
    ) -> Self {
        Self {
            id,
            text: Cow::Owned(text.into()),
            author: Cow::Owned(author.into()),
            source: Cow::Owned(source.into()),
            category,
            is_original: false,
        }
    }

    /// Formatted author and source: "作者《出处》", "《出处》", or "墨阅".
    pub fn by(&self) -> String {
        if self.is_original {
            "墨阅".to_string()
        } else if self.author.is_empty() {
            self.source.to_string()
        } else {
            format!("{}{}", self.author, self.source)
        }
    }
}

/// The number of quotations.
pub const COUNT: usize = 100;
/// The number of quotations in the "reading" category.
pub const READING_COUNT: usize = 51;
/// The number of quotations in the "mood" category.
pub const MOOD_COUNT: usize = 49;

/// The quotation of the day.
///
/// `day` is the number of days since the epoch, in the local timezone, so
/// the screen changes its quotation at local midnight.
pub fn of_day(day: i64) -> usize {
    day.rem_euclid(COUNT as i64) as usize
}

/// The quotation after `index`, round the end of the list.
pub fn next(index: usize) -> usize {
    (index + 1) % COUNT
}

/// The quotes eligible for selection under `mode`.
///
/// - `AllRandom`: all quotes except `hidden`. If all quotes are hidden,
///   falls back to all quotes.
/// - `FavoritesRandom`: all quotes in `favorites` except `hidden`. If that
///   set is empty, falls back to `AllRandom`.
/// - `Fixed`: if `fixed_id` is set to an existing, non-hidden quote, returns
///   a pool of only that quote. If `fixed_id` is None, missing, or hidden,
///   falls back to the first visible quote in `AllRandom`.
pub fn active_pool<'a, F, H>(
    mode: QuoteMode,
    is_favorite: F,
    is_hidden: H,
    fixed_id: Option<u32>,
    quotes: &'a [QuoteItem],
) -> Vec<&'a QuoteItem>
where
    F: Fn(u32) -> bool,
    H: Fn(u32) -> bool,
{
    let visible: Vec<&'a QuoteItem> = quotes.iter().filter(|q| !is_hidden(q.id)).collect();
    let visible_pool = if visible.is_empty() {
        quotes.iter().collect::<Vec<_>>()
    } else {
        visible
    };

    match mode {
        QuoteMode::AllRandom => visible_pool,
        QuoteMode::FavoritesRandom => {
            let favs: Vec<&'a QuoteItem> = visible_pool
                .iter()
                .copied()
                .filter(|q| is_favorite(q.id))
                .collect();
            if favs.is_empty() {
                visible_pool
            } else {
                favs
            }
        }
        QuoteMode::Fixed => {
            if let Some(id) = fixed_id {
                if let Some(q) = quotes.iter().find(|q| q.id == id && !is_hidden(q.id)) {
                    return vec![q];
                }
            }
            vec![visible_pool[0]]
        }
    }
}

/// The quotation chosen for `day` under `mode`.
pub fn quote_for_day<F, H>(
    mode: QuoteMode,
    is_favorite: F,
    is_hidden: H,
    fixed_id: Option<u32>,
    quotes: &[QuoteItem],
    day: i64,
) -> &QuoteItem
where
    F: Fn(u32) -> bool,
    H: Fn(u32) -> bool,
{
    let pool = active_pool(mode, is_favorite, is_hidden, fixed_id, quotes);
    let idx = if pool.is_empty() {
        0
    } else {
        day.rem_euclid(pool.len() as i64) as usize
    };
    pool[idx]
}

/// Advances to the next quotation in `pool` after `current_id`.
pub fn next_in_pool<'a>(current_id: u32, pool: &[&'a QuoteItem]) -> &'a QuoteItem {
    if pool.is_empty() {
        return &QUOTES[0];
    }
    if let Some(pos) = pool.iter().position(|q| q.id == current_id) {
        let next_pos = (pos + 1) % pool.len();
        pool[next_pos]
    } else {
        pool[0]
    }
}

/// A parsed quotation entry from `quotes.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedQuote {
    pub text: String,
    pub author: String,
    pub source: String,
}

/// Parses lines of `quotes.txt` into quote entries.
///
/// Format per line: `句子|作者|出处` (author and source are optional).
/// Empty lines or lines with empty quotation text are skipped safely.
pub fn parse_quotes_txt(raw_text: &str) -> Vec<ParsedQuote> {
    let mut out = Vec::new();
    for line in raw_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        let text = parts.first().copied().unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let author = parts.get(1).copied().unwrap_or("");
        let source = if parts.len() > 2 {
            parts[2..].join("|")
        } else {
            String::new()
        };
        out.push(ParsedQuote {
            text: text.to_string(),
            author: author.to_string(),
            source,
        });
    }
    out
}

/// Deduplicates newly parsed file quotes against existing quotes and within the file.
///
/// Quotations with identical text (trimmed) to an existing quote or an earlier line
/// are skipped.
pub fn dedup_file_quotes<'a, I>(existing_texts: I, parsed: Vec<ParsedQuote>) -> Vec<ParsedQuote>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = std::collections::HashSet::new();
    for t in existing_texts {
        seen.insert(t.trim().to_string());
    }
    let mut out = Vec::new();
    for q in parsed {
        let norm = q.text.trim().to_string();
        if seen.insert(norm) {
            out.push(q);
        }
    }
    out
}

pub const CUSTOM_QUOTE_ID_START: u32 = 10000;
pub const FILE_QUOTE_ID_START: u32 = 50000;

/// Allocates a new stable ID for a user-added custom quote.
///
/// Custom IDs begin at 10000 and monotonically increase above any existing custom ID.
pub fn allocate_custom_id(existing_ids: impl IntoIterator<Item = u32>) -> u32 {
    let mut max_id = CUSTOM_QUOTE_ID_START.saturating_sub(1);
    for id in existing_ids {
        if (CUSTOM_QUOTE_ID_START..FILE_QUOTE_ID_START).contains(&id) && id > max_id {
            max_id = id;
        }
    }
    max_id.saturating_add(1)
}

/// Assigns IDs starting at `start_id` to deduplicated file quotes, categorizing them as `Reading`.
pub fn assign_file_quote_ids(quotes: Vec<ParsedQuote>, start_id: u32) -> Vec<QuoteItem> {
    quotes
        .into_iter()
        .enumerate()
        .map(|(idx, q)| QuoteItem {
            id: start_id.saturating_add(idx as u32),
            text: Cow::Owned(q.text),
            author: Cow::Owned(q.author),
            source: Cow::Owned(q.source),
            category: QuoteCategory::Reading,
            is_original: false,
        })
        .collect()
}

pub static QUOTES: [QuoteItem; 100] = [
    QuoteItem::builtin(
        1,
        "纸上得来终觉浅，绝知此事要躬行。",
        "陆游",
        "《冬夜读书示子聿》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        2,
        "古人学问无遗力，少壮工夫老始成。",
        "陆游",
        "《冬夜读书示子聿》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        3,
        "问渠那得清如许？为有源头活水来。",
        "朱熹",
        "《观书有感》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        4,
        "读书有三到，谓心到、眼到、口到。",
        "朱熹",
        "《训学斋规》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        5,
        "读书破万卷，下笔如有神。",
        "杜甫",
        "《奉赠韦左丞丈二十二韵》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        6,
        "学而不思则罔，思而不学则殆。",
        "孔子",
        "《论语·为政》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        7,
        "温故而知新，可以为师矣。",
        "孔子",
        "《论语·为政》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        8,
        "学而时习之，不亦说乎？",
        "孔子",
        "《论语·学而》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        9,
        "知之者不如好之者，好之者不如乐之者。",
        "孔子",
        "《论语·雍也》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        10,
        "三人行，必有我师焉。",
        "孔子",
        "《论语·述而》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        11,
        "敏而好学，不耻下问。",
        "孔子",
        "《论语·公冶长》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        12,
        "发愤忘食，乐以忘忧，不知老之将至云尔。",
        "孔子",
        "《论语·述而》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        13,
        "欲速则不达。",
        "孔子",
        "《论语·子路》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        14,
        "岁寒，然后知松柏之后凋也。",
        "孔子",
        "《论语·子罕》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        15,
        "少壮不努力，老大徒伤悲。",
        "汉乐府",
        "《长歌行》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        16,
        "黑发不知勤学早，白首方悔读书迟。",
        "颜真卿",
        "《劝学》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        17,
        "旧书不厌百回读，熟读深思子自知。",
        "苏轼",
        "《送安惇秀才失解西归》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        18,
        "粗缯大布裹生涯，腹有诗书气自华。",
        "苏轼",
        "《和董传留别》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        19,
        "博观而约取，厚积而薄发。",
        "苏轼",
        "《稼说送张琥》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        20,
        "不积跬步，无以至千里；不积小流，无以成江海。",
        "荀子",
        "《荀子·劝学》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        21,
        "锲而舍之，朽木不折；锲而不舍，金石可镂。",
        "荀子",
        "《荀子·劝学》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        22,
        "吾生也有涯，而知也无涯。",
        "庄子",
        "《庄子·养生主》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        23,
        "玉不琢，不成器；人不学，不知道。",
        "",
        "《礼记·学记》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        24,
        "学然后知不足，教然后知困。",
        "",
        "《礼记·学记》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        25,
        "博学之，审问之，慎思之，明辨之，笃行之。",
        "",
        "《礼记·中庸》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        26,
        "业精于勤，荒于嬉；行成于思，毁于随。",
        "韩愈",
        "《进学解》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        27,
        "读书百遍而义自见。",
        "董遇",
        "《三国志·魏书·王朗传》裴松之注引《魏略》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        28,
        "好读书，不求甚解；每有会意，便欣然忘食。",
        "陶渊明",
        "《五柳先生传》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        29,
        "读书不觉已春深，一寸光阴一寸金。",
        "王贞白",
        "《白鹿洞二首·其一》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        30,
        "书卷多情似故人，晨昏忧乐每相亲。",
        "于谦",
        "《观书》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        31,
        "眼前直下三千字，胸次全无一点尘。",
        "于谦",
        "《观书》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        32,
        "路漫漫其修远兮，吾将上下而求索。",
        "屈原",
        "《离骚》",
        QuoteCategory::Reading,
        false,
    ),
    QuoteItem::builtin(
        33,
        "千里之行，始于足下。",
        "老子",
        "《老子》第六十四章",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        34,
        "知人者智，自知者明。",
        "老子",
        "《老子》第三十三章",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        35,
        "不以物喜，不以己悲。",
        "范仲淹",
        "《岳阳楼记》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        36,
        "欲穷千里目，更上一层楼。",
        "王之涣",
        "《登鹳雀楼》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        37,
        "会当凌绝顶，一览众山小。",
        "杜甫",
        "《望岳》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        38,
        "长风破浪会有时，直挂云帆济沧海。",
        "李白",
        "《行路难·其一》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        39,
        "天生我材必有用，千金散尽还复来。",
        "李白",
        "《将进酒》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        40,
        "山重水复疑无路，柳暗花明又一村。",
        "陆游",
        "《游山西村》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        41,
        "小楼一夜听春雨，深巷明朝卖杏花。",
        "陆游",
        "《临安春雨初霁》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        42,
        "沉舟侧畔千帆过，病树前头万木春。",
        "刘禹锡",
        "《酬乐天扬州初逢席上见赠》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        43,
        "莫听穿林打叶声，何妨吟啸且徐行。",
        "苏轼",
        "《定风波·莫听穿林打叶声》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        44,
        "竹杖芒鞋轻胜马，谁怕？一蓑烟雨任平生。",
        "苏轼",
        "《定风波·莫听穿林打叶声》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        45,
        "回首向来萧瑟处，归去，也无风雨也无晴。",
        "苏轼",
        "《定风波·莫听穿林打叶声》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        46,
        "人生如逆旅，我亦是行人。",
        "苏轼",
        "《临江仙·送钱穆父》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        47,
        "人有悲欢离合，月有阴晴圆缺，此事古难全。",
        "苏轼",
        "《水调歌头·明月几时有》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        48,
        "人间有味是清欢。",
        "苏轼",
        "《浣溪沙·细雨斜风作晓寒》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        49,
        "且将新火试新茶，诗酒趁年华。",
        "苏轼",
        "《望江南·超然台作》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        50,
        "试问岭南应不好，却道：此心安处是吾乡。",
        "苏轼",
        "《定风波·南海归赠王定国侍人寓娘》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        51,
        "一年好景君须记，最是橙黄橘绿时。",
        "苏轼",
        "《赠刘景文》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        52,
        "行到水穷处，坐看云起时。",
        "王维",
        "《终南别业》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        53,
        "采菊东篱下，悠然见南山。",
        "陶渊明",
        "《饮酒·其五》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        54,
        "此中有真意，欲辨已忘言。",
        "陶渊明",
        "《饮酒·其五》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        55,
        "盛年不重来，一日难再晨。及时当勉励，岁月不待人。",
        "陶渊明",
        "《杂诗·其一》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        56,
        "春有百花秋有月，夏有凉风冬有雪。若无闲事挂心头，便是人间好时节。",
        "慧开",
        "《无门关》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        57,
        "千磨万击还坚劲，任尔东西南北风。",
        "郑燮",
        "《竹石》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        58,
        "海内存知己，天涯若比邻。",
        "王勃",
        "《送杜少府之任蜀州》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        59,
        "东隅已逝，桑榆非晚。",
        "王勃",
        "《滕王阁序》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        60,
        "穷且益坚，不坠青云之志。",
        "王勃",
        "《滕王阁序》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        61,
        "落红不是无情物，化作春泥更护花。",
        "龚自珍",
        "《己亥杂诗·其五》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        62,
        "莫愁前路无知己，天下谁人不识君。",
        "高适",
        "《别董大二首·其一》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        63,
        "不畏浮云遮望眼，自缘身在最高层。",
        "王安石",
        "《登飞来峰》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        64,
        "等闲识得东风面，万紫千红总是春。",
        "朱熹",
        "《春日》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        65,
        "万物静观皆自得，四时佳兴与人同。",
        "程颢",
        "《秋日偶成》",
        QuoteCategory::Mood,
        false,
    ),
    QuoteItem::builtin(
        66,
        "今天也读一页吧，一页就好。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        67,
        "把手机放远一点，把书拿近一点。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        68,
        "慢慢读，书不会跑。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        69,
        "读不下去的时候，就合上书，去散个步。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        70,
        "每读完一本书，都是送给未来自己的礼物。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        71,
        "心乱的时候，读几页旧书。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        72,
        "累了就停，书签会替你记住读到哪里。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        73,
        "不必读得快，读得进去就好。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        74,
        "灯下一卷书，胜过窗外万千事。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        75,
        "有些书要等到某个年纪才读得懂，不急。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        76,
        "读到喜欢的句子，就划下来，日后还会再遇见它。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        77,
        "一本书，一杯茶，一个安静的下午。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        78,
        "世界很吵，书里很静。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        79,
        "读书不是为了赶路，是为了看风景。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        80,
        "今天心情不好？那就读点轻松的。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        81,
        "每天读一点，日子会慢慢变得不一样。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        82,
        "合上书，故事还在心里继续。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        83,
        "好书不怕晚读，只怕不读。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        84,
        "书是最安静的朋友，从不打扰，一直都在。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        85,
        "睡前十分钟，交给一本书。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        86,
        "今天比昨天多读了一页，也是进步。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        87,
        "遇到读不懂的地方，先放一放，接着往下读。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        88,
        "下雨天适合读书，晴天也是。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        89,
        "心里有书，走到哪里都不孤单。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        90,
        "一页一页地读，一天一天地过。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        91,
        "不必读完所有的书，读好手边这一本。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        92,
        "有书可读的日子，都是好日子。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        93,
        "把烦恼夹进书页里，过几天再翻开，也就不算什么了。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        94,
        "认真读完一本书，比匆匆翻过十本更有收获。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        95,
        "今天的阅读目标：享受它。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        96,
        "读书是一场不需要门票的远行。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        97,
        "安静下来，听书里的人说话。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
    QuoteItem::builtin(
        98,
        "先读一章，再决定今天要不要努力。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        99,
        "你在读的这本书，就是此刻最好的陪伴。",
        "",
        "墨阅原创",
        QuoteCategory::Reading,
        true,
    ),
    QuoteItem::builtin(
        100,
        "晚安，明天接着读。",
        "",
        "墨阅原创",
        QuoteCategory::Mood,
        true,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_whole_and_numbered() {
        assert_eq!(QUOTES.len(), COUNT);
        let mut reading = 0;
        let mut mood = 0;
        for (i, q) in QUOTES.iter().enumerate() {
            assert_eq!(q.id as usize, i + 1, "quote {i} is out of order");
            assert!(!q.text.is_empty());
            assert_eq!(q.is_original, q.source == "墨阅原创");
            if q.is_original {
                assert!(q.author.is_empty());
                assert_eq!(q.by(), "墨阅");
            }
            match q.category {
                QuoteCategory::Reading => reading += 1,
                QuoteCategory::Mood => mood += 1,
            }
        }
        assert_eq!(reading, READING_COUNT);
        assert_eq!(mood, MOOD_COUNT);
    }

    #[test]
    fn the_quotation_of_the_day_turns_over_every_hundred_days() {
        let day = 20_712;
        assert_eq!(of_day(day), of_day(day + 100));
        assert_ne!(of_day(day), of_day(day + 1));
        for d in [-3, 0, 1, 20_712, 20_712 + 100 * 40] {
            assert!(of_day(d) < COUNT);
        }
    }

    #[test]
    fn the_button_walks_to_the_next_and_round_the_end() {
        assert_eq!(next(0), 1);
        assert_eq!(next(COUNT - 1), 0);
    }

    #[test]
    fn the_first_quotation_is_the_one_of_the_reference() {
        assert_eq!(QUOTES[0].text, "纸上得来终觉浅，绝知此事要躬行。");
        assert_eq!(QUOTES[0].author, "陆游");
        assert_eq!(QUOTES[0].source, "《冬夜读书示子聿》");
        assert_eq!(QUOTES[0].category, QuoteCategory::Reading);
        assert_eq!(QUOTES[0].by(), "陆游《冬夜读书示子聿》");
    }

    #[test]
    fn active_pool_all_random_excludes_hidden() {
        let hidden = [2, 3];
        let pool = active_pool(
            QuoteMode::AllRandom,
            |_| false,
            |id| hidden.contains(&id),
            None,
            &QUOTES,
        );
        assert_eq!(pool.len(), COUNT - 2);
        assert!(pool.iter().all(|q| q.id != 2 && q.id != 3));
    }

    #[test]
    fn active_pool_all_random_falls_back_when_all_hidden() {
        let pool = active_pool(QuoteMode::AllRandom, |_| false, |_| true, None, &QUOTES);
        assert_eq!(pool.len(), COUNT);
    }

    #[test]
    fn active_pool_favorites_random_filters_and_falls_back() {
        let favs = [1, 5, 10];
        let pool = active_pool(
            QuoteMode::FavoritesRandom,
            |id| favs.contains(&id),
            |id| id == 5,
            None,
            &QUOTES,
        );
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].id, 1);
        assert_eq!(pool[1].id, 10);

        // Fallback when all favorites are hidden
        let pool_empty_favs = active_pool(
            QuoteMode::FavoritesRandom,
            |_| false,
            |id| id == 1,
            None,
            &QUOTES,
        );
        assert_eq!(pool_empty_favs.len(), COUNT - 1);
    }

    #[test]
    fn active_pool_fixed_returns_fixed_quote_or_fallback() {
        let pool = active_pool(QuoteMode::Fixed, |_| false, |_| false, Some(19), &QUOTES);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].id, 19);

        // Fixed quote is hidden -> fallback to first visible
        let pool_hidden = active_pool(
            QuoteMode::Fixed,
            |_| false,
            |id| id == 19 || id == 1,
            Some(19),
            &QUOTES,
        );
        assert_eq!(pool_hidden.len(), 1);
        assert_eq!(pool_hidden[0].id, 2);
    }

    #[test]
    fn quote_for_day_and_next_in_pool() {
        let favs = [10, 20, 30];
        let q0 = quote_for_day(
            QuoteMode::FavoritesRandom,
            |id| favs.contains(&id),
            |_| false,
            None,
            &QUOTES,
            0,
        );
        assert_eq!(q0.id, 10);
        let q1 = quote_for_day(
            QuoteMode::FavoritesRandom,
            |id| favs.contains(&id),
            |_| false,
            None,
            &QUOTES,
            1,
        );
        assert_eq!(q1.id, 20);

        let pool = active_pool(
            QuoteMode::FavoritesRandom,
            |id| favs.contains(&id),
            |_| false,
            None,
            &QUOTES,
        );
        let n1 = next_in_pool(10, &pool);
        assert_eq!(n1.id, 20);
        let n2 = next_in_pool(20, &pool);
        assert_eq!(n2.id, 30);
        let n3 = next_in_pool(30, &pool);
        assert_eq!(n3.id, 10);
    }

    #[test]
    fn test_parse_quotes_txt() {
        let raw = r#"
            纸上得来终觉浅|陆游|《冬夜读书示子聿》
            读书破万卷|杜甫
            只输入一句话没有任何竖线
            
            |空句子应该被过滤|出处
            带多余竖线的句子|作者|出处第一部分|出处第二部分
        "#;
        let parsed = parse_quotes_txt(raw);
        assert_eq!(parsed.len(), 4);

        assert_eq!(parsed[0].text, "纸上得来终觉浅");
        assert_eq!(parsed[0].author, "陆游");
        assert_eq!(parsed[0].source, "《冬夜读书示子聿》");

        assert_eq!(parsed[1].text, "读书破万卷");
        assert_eq!(parsed[1].author, "杜甫");
        assert_eq!(parsed[1].source, "");

        assert_eq!(parsed[2].text, "只输入一句话没有任何竖线");
        assert_eq!(parsed[2].author, "");
        assert_eq!(parsed[2].source, "");

        assert_eq!(parsed[3].text, "带多余竖线的句子");
        assert_eq!(parsed[3].author, "作者");
        assert_eq!(parsed[3].source, "出处第一部分|出处第二部分");
    }

    #[test]
    fn test_dedup_file_quotes() {
        let existing = ["纸上得来终觉浅，绝知此事要躬行。", "已存在的句子"];
        let parsed = vec![
            ParsedQuote {
                text: "纸上得来终觉浅，绝知此事要躬行。".into(),
                author: "陆游".into(),
                source: "《示子聿》".into(),
            },
            ParsedQuote {
                text: "新句子一".into(),
                author: "作者一".into(),
                source: "出处一".into(),
            },
            ParsedQuote {
                text: "新句子一".into(),
                author: "重复作者".into(),
                source: "重复出处".into(),
            },
            ParsedQuote {
                text: "新句子二".into(),
                author: "".into(),
                source: "".into(),
            },
        ];

        let deduped = dedup_file_quotes(existing.iter().copied(), parsed);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].text, "新句子一");
        assert_eq!(deduped[1].text, "新句子二");
    }

    #[test]
    fn test_allocate_custom_id() {
        assert_eq!(allocate_custom_id([]), 10000);
        assert_eq!(allocate_custom_id([1, 2, 99]), 10000);
        assert_eq!(allocate_custom_id([10000]), 10001);
        assert_eq!(allocate_custom_id([10000, 10005, 10002]), 10006);
        // File quote IDs (50000+) should not inflate custom ID allocation
        assert_eq!(allocate_custom_id([10000, 10001, 50000, 50005]), 10002);
    }

    #[test]
    fn test_assign_file_quote_ids() {
        let quotes = vec![
            ParsedQuote {
                text: "句子一".into(),
                author: "作者一".into(),
                source: "出处一".into(),
            },
            ParsedQuote {
                text: "句子二".into(),
                author: "作者二".into(),
                source: "出处二".into(),
            },
        ];
        let items = assign_file_quote_ids(quotes, 50000);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 50000);
        assert_eq!(items[0].text, "句子一");
        assert_eq!(items[0].category, QuoteCategory::Reading);
        assert_eq!(items[1].id, 50001);
        assert_eq!(items[1].text, "句子二");
    }
}
