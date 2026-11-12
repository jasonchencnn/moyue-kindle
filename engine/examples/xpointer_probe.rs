//! KOReader 与墨阅排版引擎 XPointer 互相定位实测探针（SYNC-005）。
//!
//! 提供四种模式：
//! - `dump`：对指定书籍进行排版，提取关键位置（含页首、章节中、词中间、链接/脚注、图片附近），
//!   输出 `XPointer\t该处起 20 个字\t页码`。
//! - `locate`：从标准输入或文件读取一组 XPointer，解析其落点页码（1 起）与落点处前 20 个字，
//!   输出 `页码\t该处起 20 个字`。
//! - `range-dump`：对指定书籍导出 ≥8 处跨节点/选区高亮端点（pos0, pos1），
//!   输出 `pos0\tpos1\tdesc\t选区文字`。
//! - `range-locate`：从标准输入或文件读取跨节点选区端点（pos0\tpos1\tdesc...），还原选区文字，
//!   输出 `pos0\tpos1\tdesc\t选区文字`。

#[cfg(not(feature = "crengine"))]
fn main() {
    eprintln!("xpointer_probe 需要启用 crengine 特性：--features crengine");
    std::process::exit(1);
}

#[cfg(feature = "crengine")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;

    use inkread_engine::crengine::{configure_storage_size_factor, CrengineEngine};
    use inkread_engine::{Engine, LayoutSettings};

    // 设置存储因子与 KOReader 对齐 (40.0)，防止大书解析告警
    configure_storage_size_factor(40.0);

    const PAGE_WIDTH: u32 = 1264;
    const PAGE_HEIGHT: u32 = 1680;

    let mut mode = String::from("dump");
    let mut book_path: Option<PathBuf> = None;
    let mut input_file: Option<PathBuf> = None;
    let mut fonts: Vec<PathBuf> = Vec::new();
    let mut layout = LayoutSettings::default();
    let mut dump_count: usize = 25;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "dump" => mode = "dump".to_string(),
            "locate" => mode = "locate".to_string(),
            "range-dump" => mode = "range-dump".to_string(),
            "range-locate" => mode = "range-locate".to_string(),
            "--mode" => {
                mode = args
                    .next()
                    .ok_or("--mode 需要参数 (dump|locate|range-dump|range-locate)")?;
            }
            "--font" => {
                fonts.push(PathBuf::from(args.next().ok_or("--font 需要参数")?));
            }
            "--face" => {
                layout.font_face = args.next().ok_or("--face 需要参数")?;
            }
            "--fallback-face" => {
                layout
                    .fallback_font_faces
                    .push(args.next().ok_or("--fallback-face 需要参数")?);
            }
            "--font-size" => {
                layout.font_size_px = args.next().ok_or("--font-size 需要参数")?.parse()?;
            }
            "--line-spacing" => {
                layout.line_spacing_percent =
                    args.next().ok_or("--line-spacing 需要参数")?.parse()?;
            }
            "--margin" => {
                let m: u32 = args.next().ok_or("--margin 需要参数")?.parse()?;
                layout.margins.left = m;
                layout.margins.right = m;
                layout.margins.top = m;
                layout.margins.bottom = m;
            }
            "--count" => {
                dump_count = args.next().ok_or("--count 需要参数")?.parse()?;
            }
            "--input" => {
                input_file = Some(PathBuf::from(args.next().ok_or("--input 需要参数")?));
            }
            other if !other.starts_with('-') => {
                book_path = Some(PathBuf::from(other));
            }
            unknown => {
                return Err(format!("未知参数：{unknown}").into());
            }
        }
    }

    let book_path = book_path.ok_or("请指定 EPUB 书籍文件路径")?;
    if !book_path.exists() {
        return Err(format!("书籍文件不存在：{}", book_path.display()).into());
    }

    let engine = if fonts.is_empty() {
        CrengineEngine::with_default_fonts()?
    } else {
        CrengineEngine::new(fonts)?
    };

    let doc = engine.open(&book_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)?;

    match mode.as_str() {
        "dump" => {
            run_dump(&*doc, dump_count)?;
        }
        "locate" => {
            let reader: Box<dyn BufRead> = match input_file {
                Some(path) => Box::new(BufReader::new(std::fs::File::open(path)?)),
                None => Box::new(BufReader::new(std::io::stdin())),
            };
            run_locate(&*doc, reader)?;
        }
        "range-dump" => {
            run_range_dump(&*doc, &book_path)?;
        }
        "range-locate" => {
            let reader: Box<dyn BufRead> = match input_file {
                Some(path) => Box::new(BufReader::new(std::fs::File::open(path)?)),
                None => Box::new(BufReader::new(std::io::stdin())),
            };
            run_range_locate(&*doc, reader)?;
        }
        other => return Err(format!("不支持的模式：{other}").into()),
    }

    Ok(())
}

#[cfg(feature = "crengine")]
fn parse_xpointer_offset(xpointer: &str) -> (&str, usize) {
    if let Some(pos) = xpointer.rfind('.') {
        let node = &xpointer[..pos];
        let offset = xpointer[pos + 1..].parse::<usize>().unwrap_or(0);
        (node, offset)
    } else {
        (xpointer, 0)
    }
}

/// 归一化节点路径：在 crengine XPath 中，`text()[1]` 与 `text()` 等价。
#[cfg(feature = "crengine")]
fn normalize_node(node: &str) -> &str {
    node.strip_suffix("[1]").unwrap_or(node)
}

#[cfg(feature = "crengine")]
fn sanitize_text(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(max_chars * 4);
    for c in text.chars() {
        if out.chars().count() >= max_chars {
            break;
        }
        if c == '\t' || c == '\n' || c == '\r' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(feature = "crengine")]
fn extract_text_from_page_words(
    words: &[inkread_engine::Word],
    target_node: &str,
    target_offset: usize,
    max_chars: usize,
) -> Option<String> {
    if words.is_empty() {
        return None;
    }

    let norm_target = normalize_node(target_node);
    let mut start_idx = None;
    let mut offset_in_word = 0;

    for (i, word) in words.iter().enumerate() {
        let (w_node, w_start_off) = parse_xpointer_offset(&word.start_xpointer);
        let (_, w_end_off) = parse_xpointer_offset(&word.end_xpointer);
        let norm_w = normalize_node(w_node);

        if norm_w == norm_target {
            if target_offset >= w_start_off && target_offset < w_end_off {
                start_idx = Some(i);
                offset_in_word = target_offset - w_start_off;
                break;
            } else if target_offset <= w_start_off {
                start_idx = Some(i);
                offset_in_word = 0;
                break;
            }
        }
    }

    // 若 target_node 是父容器（如 /body/DocFragment[1] 或 /body/.../p[1]），找第一个属于该容器的词
    if start_idx.is_none() && !norm_target.is_empty() {
        for (i, word) in words.iter().enumerate() {
            let (w_node, _) = parse_xpointer_offset(&word.start_xpointer);
            if w_node.starts_with(target_node) {
                start_idx = Some(i);
                offset_in_word = 0;
                break;
            }
        }
    }

    let start_idx = start_idx?;

    let mut result = String::new();
    let mut last_end_node: Option<&str> = None;
    let mut last_end_offset: Option<usize> = None;

    for word in &words[start_idx..] {
        if result.chars().count() >= max_chars {
            break;
        }
        let (w_node, w_start_off) = parse_xpointer_offset(&word.start_xpointer);
        let (w_end_node, w_end_off) = parse_xpointer_offset(&word.end_xpointer);

        if let (Some(prev_node), Some(prev_end)) = (last_end_node, last_end_offset) {
            if prev_node == w_node {
                if w_start_off > prev_end {
                    result.push(' ');
                }
            } else {
                result.push(' ');
            }
        }

        let chars_to_take = max_chars.saturating_sub(result.chars().count());
        if offset_in_word > 0 {
            for c in word.text.chars().skip(offset_in_word).take(chars_to_take) {
                result.push(c);
            }
            offset_in_word = 0;
        } else {
            for c in word.text.chars().take(chars_to_take) {
                result.push(c);
            }
        }

        last_end_node = Some(w_end_node);
        last_end_offset = Some(w_end_off);
    }

    Some(result)
}

#[cfg(feature = "crengine")]
fn extract_text_for_xpointer(
    doc: &dyn inkread_engine::Document,
    xpointer: &str,
    max_chars: usize,
) -> (u32, String) {
    let page = match doc.page_of_xpointer(xpointer) {
        Ok(p) => p,
        Err(_) => return (0, String::new()),
    };
    let page_1based = page + 1;
    let (target_node, target_offset) = parse_xpointer_offset(xpointer);

    let words = doc.page_words(page).unwrap_or_default();
    let text = extract_text_from_page_words(&words, target_node, target_offset, max_chars)
        .unwrap_or_default();

    (page_1based, sanitize_text(&text, max_chars))
}

#[cfg(feature = "crengine")]
fn find_word_index_start(
    words: &[inkread_engine::Word],
    target_node: &str,
    target_offset: usize,
) -> (usize, usize) {
    let norm_target = normalize_node(target_node);
    for (i, word) in words.iter().enumerate() {
        let (w_node, w_start_off) = parse_xpointer_offset(&word.start_xpointer);
        let (_, w_end_off) = parse_xpointer_offset(&word.end_xpointer);
        let norm_w = normalize_node(w_node);

        if norm_w == norm_target {
            if target_offset >= w_start_off && target_offset < w_end_off {
                return (i, target_offset - w_start_off);
            } else if target_offset <= w_start_off {
                return (i, 0);
            }
        }
    }

    // 检查父容器匹配
    if !norm_target.is_empty() {
        for (i, word) in words.iter().enumerate() {
            let (w_node, _) = parse_xpointer_offset(&word.start_xpointer);
            if w_node.starts_with(target_node) {
                return (i, 0);
            }
        }
    }

    (0, 0)
}

#[cfg(feature = "crengine")]
fn find_word_index_end(
    words: &[inkread_engine::Word],
    target_node: &str,
    target_offset: usize,
) -> (usize, usize) {
    let norm_target = normalize_node(target_node);
    for (i, word) in words.iter().enumerate().rev() {
        let (w_node, w_start_off) = parse_xpointer_offset(&word.start_xpointer);
        let (_, w_end_off) = parse_xpointer_offset(&word.end_xpointer);
        let norm_w = normalize_node(w_node);

        if norm_w == norm_target {
            if target_offset > w_start_off && target_offset <= w_end_off {
                return (i, target_offset - w_start_off);
            } else if target_offset > w_end_off {
                return (i, word.text.chars().count());
            }
        }
    }

    if !norm_target.is_empty() {
        for (i, word) in words.iter().enumerate().rev() {
            let (w_node, _) = parse_xpointer_offset(&word.start_xpointer);
            if w_node.starts_with(target_node) {
                return (i, word.text.chars().count());
            }
        }
    }

    let last_idx = words.len().saturating_sub(1);
    let last_count = words.last().map(|w| w.text.chars().count()).unwrap_or(0);
    (last_idx, last_count)
}

/// 基于现有 Document trait (page_of_xpointer + page_words) 还原两个 XPointer 之间的选区文字
#[cfg(feature = "crengine")]
fn extract_text_for_range(
    doc: &dyn inkread_engine::Document,
    pos0: &str,
    pos1: &str,
) -> (u32, u32, String) {
    let p0 = match doc.page_of_xpointer(pos0) {
        Ok(p) => p,
        Err(_) => return (0, 0, String::new()),
    };
    let p1 = match doc.page_of_xpointer(pos1) {
        Ok(p) => p,
        Err(_) => return (0, 0, String::new()),
    };

    let (start_page, end_page, swapped) = if p0 <= p1 {
        (p0, p1, false)
    } else {
        (p1, p0, true)
    };
    let (act_pos0, act_pos1) = if !swapped { (pos0, pos1) } else { (pos1, pos0) };

    let (node0, off0) = parse_xpointer_offset(act_pos0);
    let (node1, off1) = parse_xpointer_offset(act_pos1);

    let mut result = String::new();

    for page in start_page..=end_page {
        let words = match doc.page_words(page) {
            Ok(w) => w,
            Err(_) => continue,
        };
        if words.is_empty() {
            continue;
        }

        let (start_idx, offset_in_first) = if page == start_page {
            find_word_index_start(&words, node0, off0)
        } else {
            (0, 0)
        };

        let (end_idx, chars_in_last) = if page == end_page {
            find_word_index_end(&words, node1, off1)
        } else {
            (
                words.len().saturating_sub(1),
                words.last().map(|w| w.text.chars().count()).unwrap_or(0),
            )
        };

        if start_idx <= end_idx && start_idx < words.len() {
            let slice_end = end_idx.min(words.len().saturating_sub(1));
            for (offset, w) in words[start_idx..=slice_end].iter().enumerate() {
                let i = start_idx + offset;
                let mut w_str = w.text.clone();

                if page == start_page && i == start_idx && offset_in_first > 0 {
                    w_str = w_str.chars().skip(offset_in_first).collect();
                }
                if page == end_page && i == end_idx && chars_in_last < w.text.chars().count() {
                    w_str = w_str
                        .chars()
                        .take(chars_in_last.saturating_sub(
                            if page == start_page && i == start_idx {
                                offset_in_first
                            } else {
                                0
                            },
                        ))
                        .collect();
                }

                if !w_str.is_empty() {
                    // 若前一个字符与当前字符均为 ASCII 字母数字，插入空格；否则直接拼接
                    if let (Some(last_c), Some(first_c)) =
                        (result.chars().last(), w_str.chars().next())
                    {
                        if (last_c.is_ascii_alphanumeric() && first_c.is_ascii_alphanumeric())
                            || last_c == '.'
                            || last_c == ','
                            || last_c == ';'
                        {
                            result.push(' ');
                        }
                    }
                    result.push_str(&w_str);
                }
            }
        }
    }

    (start_page + 1, end_page + 1, sanitize_text(&result, 500000))
}

#[cfg(feature = "crengine")]
fn get_predefined_ranges(book_name: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    if book_name.contains("interactive") {
        vec![
            (
                "/body/DocFragment[1]/body/h1/text().0",
                "/body/DocFragment[1]/body/h1/text().9",
                "同一文本节点(标题)",
            ),
            (
                "/body/DocFragment[1]/body/p[5]/text().14",
                "/body/DocFragment[1]/body/p[5]/text().39",
                "同一文本节点(词中到词中)",
            ),
            (
                "/body/DocFragment[1]/body/p[1]/text()[1].28",
                "/body/DocFragment[1]/body/p[1]/text()[2].10",
                "跨内联元素(跨脚注标记)",
            ),
            (
                "/body/DocFragment[1]/body/p[2]/text()[1].0",
                "/body/DocFragment[1]/body/p[2]/a[1]/text().9",
                "跨内联元素(跨跳转链接)",
            ),
            (
                "/body/DocFragment[1]/body/p[1]/text()[1].0",
                "/body/DocFragment[1]/body/p[2]/text()[1].13",
                "跨段落(p1到p2)",
            ),
            (
                "/body/DocFragment[1]/body/p[5]/text().138",
                "/body/DocFragment[2]/body/p[1]/text().12",
                "跨章节(frag1到frag2)",
            ),
            (
                "/body/DocFragment[1]/body/p[2]/text()[1].3",
                "/body/DocFragment[1]/body/p[3]/text().15",
                "词中偏移跨段落",
            ),
            (
                "/body/DocFragment[1]/body/p[2]/text()[1].5",
                "/body/DocFragment[1]/body/p[2]/a[1]/text().4",
                "词中偏移跨内联元素",
            ),
        ]
    } else if book_name.contains("alice") {
        vec![
            (
                "/body/DocFragment[2]/body/header/h2/text().0",
                "/body/DocFragment[2]/body/header/h2/text().20",
                "同一文本节点(前言)",
            ),
            (
                "/body/DocFragment[3]/body/div/p[1]/text().10",
                "/body/DocFragment[3]/body/div/p[1]/text().45",
                "同一文本节点(词中到词中)",
            ),
            (
                "/body/DocFragment[3]/body/div/p[3]/text()[1].0",
                "/body/DocFragment[3]/body/div/p[3]/text()[2].11",
                "跨内联斜体(跨very)",
            ),
            (
                "/body/DocFragment[3]/body/div/p[3]/i[1]/text().0",
                "/body/DocFragment[3]/body/div/p[3]/text()[2].15",
                "从内联斜体跨出到正文",
            ),
            (
                "/body/DocFragment[3]/body/div/p[1]/text().50",
                "/body/DocFragment[3]/body/div/p[2]/text().15",
                "跨段落(p1到p2)",
            ),
            (
                "/body/DocFragment[3]/body/div/p[18]/text().0",
                "/body/DocFragment[4]/body/div/h2/text()[1].10",
                "跨章节(第一章到第二章)",
            ),
            (
                "/body/DocFragment[3]/body/div/p[2]/text().5",
                "/body/DocFragment[3]/body/div/p[3]/text()[1].12",
                "词中偏移跨段落",
            ),
            (
                "/body/DocFragment[2]/body/table/tbody/tr[1]/td[1]/a/text().0",
                "/body/DocFragment[2]/body/table/tbody/tr[2]/td[1]/a/text().5",
                "跨目录表格行链接",
            ),
        ]
    } else {
        // hlm.epub
        vec![
            (
                "/body/DocFragment[2]/body/p[1]/text().0",
                "/body/DocFragment[2]/body/p[1]/text().21",
                "同一文本节点(第一回标题)",
            ),
            (
                "/body/DocFragment[2]/body/p[6]/text().1440",
                "/body/DocFragment[2]/body/p[6]/text().1470",
                "深层词中到词中",
            ),
            (
                "/body/DocFragment[2]/body/p[2]/text().50",
                "/body/DocFragment[2]/body/p[3]/text().15",
                "跨段落(p2到p3)",
            ),
            (
                "/body/DocFragment[2]/body/p[5]/text().0",
                "/body/DocFragment[2]/body/p[6]/text().30",
                "跨长段落(p5到p6)",
            ),
            (
                "/body/DocFragment[2]/body/p[27]/text().7900",
                "/body/DocFragment[3]/body/p[1]/text().20",
                "跨大章节(卷2到卷3)",
            ),
            (
                "/body/DocFragment[3]/body/p[6]/text()[1].100",
                "/body/DocFragment[4]/body/p[2]/text().50",
                "超长跨度跨章节(卷3到卷4)",
            ),
            (
                "/body/DocFragment[8]/body/p[11]/text().368",
                "/body/DocFragment[8]/body/p[12]/text().25",
                "深层字中跨段落",
            ),
            (
                "/body/DocFragment[2]/body/p[10]/text()[1].3400",
                "/body/DocFragment[2]/body/p[10]/text()[1].3450",
                "大偏移选区",
            ),
        ]
    }
}

#[cfg(feature = "crengine")]
fn run_dump(
    doc: &dyn inkread_engine::Document,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let page_count = doc.page_count();
    let mut xpointers: Vec<String> = Vec::new();

    // 1. 收集页面起点 (从每隔若干页选取，保证覆盖全书跨度)
    let step = (page_count as usize / (count / 2).max(1)).max(1);
    for p in (0..page_count).step_by(step) {
        if let Ok(xp) = doc.page_xpointer(p) {
            if !xpointers.contains(&xp) {
                xpointers.push(xp);
            }
        }
    }

    // 2. 收集 TOC 目录项中的 XPointer
    if let Ok(toc_entries) = doc.toc() {
        for entry in toc_entries.iter().take(5) {
            if !entry.xpointer.is_empty() && !xpointers.contains(&entry.xpointer) {
                xpointers.push(entry.xpointer.clone());
            }
        }
    }

    // 3. 收集正文页中的链接与脚注
    for p in 0..page_count.min(10) {
        if let Ok(links) = doc.page_links(p) {
            for link in links {
                if let inkread_engine::LinkTarget::Internal { xpointer, .. } = link.target {
                    if !xpointer.is_empty() && !xpointers.contains(&xpointer) {
                        xpointers.push(xpointer);
                    }
                }
            }
        }
    }

    // 4. 收集词中间位置 (Mid-word / Mid-sentence positions)
    for p in 0..page_count.min(10) {
        if let Ok(words) = doc.page_words(p) {
            for word in words.iter().filter(|w| w.text.chars().count() >= 2) {
                let (w_node, w_start_off) = parse_xpointer_offset(&word.start_xpointer);
                let mid_off = w_start_off + word.text.chars().count() / 2;
                let mid_xp = format!("{w_node}.{mid_off}");
                if !xpointers.contains(&mid_xp) {
                    xpointers.push(mid_xp);
                    if xpointers.len() >= count + 5 {
                        break;
                    }
                }
            }
        }
        if xpointers.len() >= count + 5 {
            break;
        }
    }

    // 限制总数到 count
    if xpointers.len() > count {
        xpointers.truncate(count);
    }

    // 输出 TSV: XPointer\t该处文字\t页码
    for xp in &xpointers {
        let (page, text) = extract_text_for_xpointer(doc, xp, 20);
        println!("{xp}\t{text}\t{page}");
    }

    Ok(())
}

#[cfg(feature = "crengine")]
fn run_locate(
    doc: &dyn inkread_engine::Document,
    mut reader: Box<dyn std::io::BufRead>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim();
        if !trimmed.is_empty() && trimmed.starts_with('/') {
            let xpointer = trimmed.split('\t').next().unwrap_or(trimmed);
            let (page, text) = extract_text_for_xpointer(doc, xpointer, 20);
            println!("{page}\t{text}");
        }
        line.clear();
    }
    Ok(())
}

#[cfg(feature = "crengine")]
fn run_range_dump(
    doc: &dyn inkread_engine::Document,
    book_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let filename = book_path.file_name().unwrap_or_default().to_string_lossy();
    let ranges = get_predefined_ranges(&filename);

    for (p0, p1, desc) in ranges {
        let (_, _, text) = extract_text_for_range(doc, p0, p1);
        println!("{p0}\t{p1}\t{desc}\t{text}");
    }
    Ok(())
}

#[cfg(feature = "crengine")]
fn run_range_locate(
    doc: &dyn inkread_engine::Document,
    mut reader: Box<dyn std::io::BufRead>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim();
        if !trimmed.is_empty() && trimmed.starts_with('/') {
            let parts: Vec<&str> = trimmed.split('\t').collect();
            if parts.len() >= 2 {
                let p0 = parts[0];
                let p1 = parts[1];
                let desc = if parts.len() >= 3 { parts[2] } else { "" };
                let (_, _, text) = extract_text_for_range(doc, p0, p1);
                println!("{p0}\t{p1}\t{desc}\t{text}");
            }
        }
        line.clear();
    }
    Ok(())
}
