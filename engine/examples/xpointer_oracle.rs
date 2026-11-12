//! 生成 Readest 的 crengine XPointer 预言机 JSON（SYNC-007）。
//!
//! 输出格式与 KOReader 的 `xpointer-oracle.lua` 一致：
//!
//! ```json
//! { "epub": "/abs/book.epub",
//!   "fragments": [ { "index": 0, "docfragment": 1, "word_count": 1234,
//!                    "words": [ { "xp": "...", "xp_end": "...", "text": "..." } ] } ] }
//! ```
//!
//! 每个 spine 项（crengine 的 `DocFragment[N]`）取样若干可见词。词、起止
//! XPointer 与文字全部来自本引擎的 `page_words()`（底层是
//! `ldomXRange::getRangeWords()`），与 KOReader 的 `getNextVisibleWordStart()`
//! 使用同一套 crengine 断词逻辑。
//!
//! 用法：
//!
//! ```text
//! cargo run -p inkread-engine --features crengine --example xpointer_oracle -- \
//!   book.epub out.json [--font FONT]... [--face NAME] [--fallback-face NAME] \
//!   [--font-size 46] [--line-spacing 170] [--margin 72] [--per-fragment 60]
//! ```

#[cfg(not(feature = "crengine"))]
fn main() {
    eprintln!("xpointer_oracle 需要启用 crengine 特性：--features crengine");
    std::process::exit(1);
}

#[cfg(feature = "crengine")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::PathBuf;

    use inkread_engine::crengine::{configure_storage_size_factor, CrengineEngine};
    use inkread_engine::{Engine, LayoutSettings};

    configure_storage_size_factor(40.0);

    const PAGE_WIDTH: u32 = 1264;
    const PAGE_HEIGHT: u32 = 1680;

    let mut book_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut fonts: Vec<PathBuf> = Vec::new();
    let mut layout = LayoutSettings::default();
    let mut per_fragment: usize = 60;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--font" => fonts.push(PathBuf::from(args.next().ok_or("--font 需要参数")?)),
            "--face" => layout.font_face = args.next().ok_or("--face 需要参数")?,
            "--fallback-face" => layout
                .fallback_font_faces
                .push(args.next().ok_or("--fallback-face 需要参数")?),
            "--font-size" => {
                layout.font_size_px = args.next().ok_or("--font-size 需要参数")?.parse()?
            }
            "--line-spacing" => {
                layout.line_spacing_percent =
                    args.next().ok_or("--line-spacing 需要参数")?.parse()?
            }
            "--margin" => {
                let m: u32 = args.next().ok_or("--margin 需要参数")?.parse()?;
                layout.margins.left = m;
                layout.margins.right = m;
                layout.margins.top = m;
                layout.margins.bottom = m;
            }
            "--per-fragment" => {
                per_fragment = args
                    .next()
                    .ok_or("--per-fragment 需要参数")?
                    .parse::<usize>()?
                    .max(1);
            }
            other if other.starts_with('-') => {
                return Err(format!("未知参数：{other}").into());
            }
            other => {
                if book_path.is_none() {
                    book_path = Some(PathBuf::from(other));
                } else if out_path.is_none() {
                    out_path = Some(PathBuf::from(other));
                } else {
                    return Err(format!("多余的参数：{other}").into());
                }
            }
        }
    }

    let book_path = book_path.ok_or("用法：xpointer_oracle <book.epub> <out.json>")?;
    let out_path = out_path.ok_or("用法：xpointer_oracle <book.epub> <out.json>")?;
    if !book_path.exists() {
        return Err(format!("书籍文件不存在：{}", book_path.display()).into());
    }
    let book_abs = std::fs::canonicalize(&book_path)?;

    let engine = if fonts.is_empty() {
        CrengineEngine::with_default_fonts()?
    } else {
        CrengineEngine::new(fonts)?
    };
    let doc = engine.open(&book_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)?;

    // 先按 DocFragment 序号确认 spine 项数量：crengine 为每个 <itemref>
    // 生成一个 DocFragment，读不到的序号就是结尾。
    let mut fragment_count = 0usize;
    loop {
        let xp = format!("/body/DocFragment[{}]", fragment_count + 1);
        if doc.page_of_xpointer(&xp).is_err() {
            break;
        }
        fragment_count += 1;
        if fragment_count > 100_000 {
            return Err("DocFragment 数量异常".into());
        }
    }

    // 单遍扫描所有页面，每个 fragment 用蓄水池抽样保留固定数量的词，
    // 大书（如百万字的《紅樓夢》）也不会把整本词表读进内存。
    struct Sample {
        seen: usize,
        kept: Vec<(usize, inkread_engine::Word)>,
        rng: u64,
    }
    let mut samples: BTreeMap<usize, Sample> = BTreeMap::new();
    let page_count = doc.page_count();
    for page in 0..page_count {
        for word in doc.page_words(page).unwrap_or_default() {
            let Some(fragment) = docfragment_index(&word.start_xpointer) else {
                continue;
            };
            let sample = samples.entry(fragment).or_insert_with(|| Sample {
                seen: 0,
                kept: Vec::with_capacity(per_fragment),
                // 固定种子：同样的输入每次得到同样的抽样。
                rng: 0x9e37_79b9_7f4a_7c15 ^ fragment as u64,
            });
            let ordinal = sample.seen;
            sample.seen += 1;
            if sample.kept.len() < per_fragment {
                sample.kept.push((ordinal, word));
            } else {
                // xorshift64：避免引入 rand 依赖。
                let mut x = sample.rng;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                sample.rng = x;
                let slot = (x % (ordinal as u64 + 1)) as usize;
                if slot < per_fragment {
                    sample.kept[slot] = (ordinal, word);
                }
            }
        }
    }

    let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path)?);
    writeln!(out, "{{")?;
    writeln!(
        out,
        "  \"epub\": {},",
        json_string(&book_abs.to_string_lossy())
    )?;
    writeln!(out, "  \"fragments\": [")?;
    let mut total_words = 0usize;
    for n in 1..=fragment_count {
        let mut sample = samples.remove(&n).unwrap_or(Sample {
            seen: 0,
            kept: Vec::new(),
            rng: 0,
        });
        sample.kept.sort_by_key(|(ordinal, _)| *ordinal);
        total_words += sample.kept.len();
        let comma = if n == fragment_count { "" } else { "," };
        writeln!(out, "    {{")?;
        writeln!(out, "      \"index\": {},", n - 1)?;
        writeln!(out, "      \"docfragment\": {},", n)?;
        writeln!(out, "      \"word_count\": {},", sample.seen)?;
        writeln!(out, "      \"words\": [")?;
        for (i, (_, word)) in sample.kept.iter().enumerate() {
            let comma = if i + 1 == sample.kept.len() { "" } else { "," };
            writeln!(
                out,
                "        {{ \"xp\": {}, \"xp_end\": {}, \"text\": {} }}{}",
                json_string(&word.start_xpointer),
                json_string(&word.end_xpointer),
                json_string(&word.text),
                comma
            )?;
        }
        writeln!(out, "      ]")?;
        writeln!(out, "    }}{comma}")?;
    }
    writeln!(out, "  ]")?;
    writeln!(out, "}}")?;
    out.flush()?;

    eprintln!(
        "wrote {}: {} fragments, {} pages, {} sampled words",
        out_path.display(),
        fragment_count,
        page_count,
        total_words
    );
    Ok(())
}

/// 从 XPointer 里取 `DocFragment[N]` 的序号；单 spine 书省略 `[N]`，按 1 算。
#[cfg(feature = "crengine")]
fn docfragment_index(xpointer: &str) -> Option<usize> {
    const PREFIX: &str = "/body/DocFragment";
    let rest = xpointer.strip_prefix(PREFIX)?;
    if let Some(rest) = rest.strip_prefix('[') {
        let end = rest.find(']')?;
        rest[..end].parse().ok()
    } else {
        Some(1)
    }
}

#[cfg(feature = "crengine")]
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
