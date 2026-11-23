//! 渲染一页 EPUB 并写成 8 位灰度 PNG。
//!
//! 用途：生成 `tests/golden/` 下的黄金图，肉眼检查排版效果，以及在设备上
//! 验证引擎（Kindle 上跑的就是这个程序）。
//!
//! ```text
//! cargo run -p inkread-engine --features crengine --example render_page -- \
//!     fixtures/sample.epub out.png [--page N] [--font-size N] [--margin N]
//! ```
//!
//! `--face` 是正文字体，`--fallback-face` 是回退字体（可以给多次）：主字体
//! 里没有的字形交给回退字体，比如正文用 Literata、汉字交给中文字体。
//!
//! `--weight N` 是正文字重（100..900，预设 300/400/600），`--no-fine` 关闭
//! 现代精排，`--underline-scope text|full` 选文字划线或整行划线。
//!
//! `--cache DIR` 打开文档缓存：第一次打开照常排版，排完把结果写进 DIR；
//! 之后用同样的排版参数再打开同一本书时直接读缓存。程序会打印这一份书
//! 是不是从缓存里读出来的，用来对比首次和第二次打开。
//!
//! 设备上的用法（字体由 UI-003 打进设备包）：
//!
//! ```text
//! render_page --font /mnt/us/aristotle/data/fonts/DejaVuSans-Symbols.ttf \
//!     book.epub 0 out.png
//! ```
//!
//! 黄金图的生成命令见 `docs/tasks/ENG-002/notes.md`。

#[cfg(feature = "crengine")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    use std::time::Instant;

    use inkread_engine::crengine::{configure_cache, CrengineEngine};
    use inkread_engine::{Engine, LayoutSettings};
    use inkread_platform::GrayBuffer;

    /// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
    const PAGE_WIDTH: u32 = 1264;
    const PAGE_HEIGHT: u32 = 1680;

    /// 缓存目录的容量上限（字节）。
    const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

    let mut positional: Vec<PathBuf> = Vec::new();
    let mut page = 0u32;
    let mut layout = LayoutSettings::default();
    let mut fonts: Vec<PathBuf> = Vec::new();
    let mut cache: Option<PathBuf> = None;
    let mut partial_demo: Option<u32> = None;
    let mut new_face: Option<String> = None;
    let mut underline = inkread_engine::Underline::Off;
    let mut underline_scope = inkread_engine::UnderlineScope::TextOnly;
    let mut night = false;
    let mut new_fallback_face: Option<String> = None;
    let mut page_size = (PAGE_WIDTH, PAGE_HEIGHT);

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--underline" => {
                let v = args.next().ok_or("--underline 需要一个参数")?;
                underline = match v.as_str() {
                    "off" | "none" => inkread_engine::Underline::Off,
                    "solid" => inkread_engine::Underline::Solid,
                    "dotted" => inkread_engine::Underline::Dotted,
                    _ => return Err(format!("未知的引导线样式：{v}").into()),
                };
            }
            "--night" => {
                // 夜间模式：整页反色，和 App 在面板上做的事一样
                // （aristotle-render 的 Page::invert_images）。
                night = true;
            }
            "--partial" => {
                partial_demo = Some(56);
            }
            "--partial-font-size" | "--new-font-size" => {
                partial_demo = Some(args.next().ok_or("--new-font-size 需要一个参数")?.parse()?);
            }
            "--new-face" => {
                new_face = Some(args.next().ok_or("--new-face 需要一个参数")?.to_string());
                if partial_demo.is_none() {
                    partial_demo = Some(layout.font_size_px);
                }
            }
            "--new-fallback-face" => {
                new_fallback_face = Some(
                    args.next()
                        .ok_or("--new-fallback-face 需要一个参数")?
                        .to_string(),
                );
                if partial_demo.is_none() {
                    partial_demo = Some(layout.font_size_px);
                }
            }
            "--page" => {
                page = args.next().ok_or("--page 需要一个参数")?.parse()?;
            }
            "--cache" => {
                cache = Some(PathBuf::from(args.next().ok_or("--cache 需要一个参数")?));
            }
            "--width" => {
                let value: u32 = args.next().ok_or("--width 需要一个参数")?.parse()?;
                page_size.0 = value;
            }
            "--height" => {
                let value: u32 = args.next().ok_or("--height 需要一个参数")?.parse()?;
                page_size.1 = value;
            }
            "--font" => {
                fonts.push(PathBuf::from(args.next().ok_or("--font 需要一个参数")?));
            }
            "--font-size" => {
                layout.font_size_px = args.next().ok_or("--font-size 需要一个参数")?.parse()?;
            }
            "--line-spacing" => {
                layout.line_spacing_percent =
                    args.next().ok_or("--line-spacing 需要一个参数")?.parse()?;
            }
            "--weight" => {
                layout.font_weight = args.next().ok_or("--weight 需要一个参数")?.parse()?;
            }
            "--no-fine" => {
                layout.fine_typesetting = false;
            }
            "--underline-scope" => {
                let v = args.next().ok_or("--underline-scope 需要一个参数")?;
                underline_scope = match v.as_str() {
                    "text" | "text-only" => inkread_engine::UnderlineScope::TextOnly,
                    "full" | "full-line" => inkread_engine::UnderlineScope::FullLine,
                    _ => return Err(format!("未知的划线范围：{v}").into()),
                };
            }
            "--margin" => {
                let value: u32 = args.next().ok_or("--margin 需要一个参数")?.parse()?;
                layout.margins.left = value;
                layout.margins.right = value;
                layout.margins.top = value;
                layout.margins.bottom = value;
            }
            "--margin-left" => {
                layout.margins.left = args.next().ok_or("--margin-left 需要一个参数")?.parse()?;
            }
            "--margin-right" => {
                layout.margins.right = args.next().ok_or("--margin-right 需要一个参数")?.parse()?;
            }
            "--margin-top" => {
                layout.margins.top = args.next().ok_or("--margin-top 需要一个参数")?.parse()?;
            }
            "--margin-bottom" => {
                layout.margins.bottom =
                    args.next().ok_or("--margin-bottom 需要一个参数")?.parse()?;
            }
            "--face" => {
                layout.font_face = args.next().ok_or("--face 需要一个参数")?.to_string();
            }
            "--fallback-face" => {
                layout.fallback_font_faces.push(
                    args.next()
                        .ok_or("--fallback-face 需要一个参数")?
                        .to_string(),
                );
            }
            other => {
                positional.push(PathBuf::from(other));
            }
        }
    }

    // 位置参数有两种写法：`<输入> <输出>` 和 `<输入> <页码> <输出>`。
    let (input, output) = match positional.len() {
        1 => (positional[0].clone(), None),
        2 => (positional[0].clone(), Some(positional[1].clone())),
        3 => {
            page = positional[1]
                .to_str()
                .ok_or("页码不是合法的路径")?
                .parse()?;
            (positional[0].clone(), Some(positional[2].clone()))
        }
        _ => {
            return Err(
                "用法：render_page [选项] <输入.epub> <输出.png>\n      render_page [选项] <输入.epub> <页码> <输出.png>"
                    .into(),
            )
        }
    };
    let output = output.unwrap_or_else(|| PathBuf::from("out.png"));
    layout.validate()?;

    let engine = if fonts.is_empty() {
        CrengineEngine::with_default_fonts()?
    } else {
        CrengineEngine::new(fonts)?
    };
    println!("字体文件：");
    for font in engine.font_files() {
        println!("  {}", font.display());
    }
    println!("已注册 face：{}", engine.font_faces().join(", "));
    println!("回退字体：{}", layout.fallback_font_faces.join(", "));

    if let Some(dir) = &cache {
        // 缓存开不开都不影响这一次打开：目录建不出来就当没有缓存。
        match configure_cache(dir, CACHE_MAX_BYTES) {
            Ok(()) => println!("文档缓存：{}", dir.display()),
            Err(e) => println!("文档缓存开不了（{e}），照常排版"),
        }
    }

    let open_start = Instant::now();
    let mut doc = engine.open(&input, page_size, &layout)?;
    let open_elapsed = open_start.elapsed();
    let pages = doc.page_count();
    println!("书名：{}", doc.title());
    println!("作者：{}", doc.author());
    println!("共 {pages} 页");
    println!("从缓存读出：{}", doc.opened_from_cache());
    println!("本次已重排：{}", doc.reformatted());
    if page >= pages {
        return Err(format!("页码越界：请求第 {page} 页，文档只有 {pages} 页").into());
    }

    let mut buffer = GrayBuffer::new(page_size.0, page_size.1);
    let options = inkread_engine::RenderOptions {
        invert_images: false,
        underline,
        underline_gray: 0x20,
        underline_scope,
    };
    println!("正文引导线：{underline:?}");
    let render_elapsed;

    if let Some(new_font_size) = partial_demo {
        println!("\n=== 局部重排演示（--partial）===");
        let can_partial = doc.can_partial_rerender();
        println!("文档是否支持局部重排：{can_partial}");
        if can_partial {
            doc.enable_partial_rerender(true)?;
            println!("已启用局部重排模式：{}", doc.is_partial_rerender_enabled());
            let mut new_layout = layout.clone();
            new_layout.font_size_px = new_font_size;
            if let Some(ref face) = new_face {
                new_layout.font_face = face.clone();
            }
            if let Some(ref fb) = new_fallback_face {
                new_layout.fallback_font_faces = vec![fb.clone()];
            }
            let re_start = Instant::now();
            doc.set_layout(&new_layout)?;
            let re_elapsed = re_start.elapsed();
            println!(
                "修改排版参数（字号 {}px -> {}px, 正文 {:?} -> {:?}, 回退 {:?} -> {:?}）调用耗时：{:.2} ms (延迟重排中：{})",
                layout.font_size_px,
                new_layout.font_size_px,
                layout.font_face,
                new_layout.font_face,
                layout.fallback_font_faces,
                new_layout.fallback_font_faces,
                re_elapsed.as_secs_f64() * 1000.0,
                doc.is_rerender_delayed()
            );

            let render_start = Instant::now();
            doc.render_page_with_options(page, &mut buffer, &options)?;
            render_elapsed = render_start.elapsed();
            println!(
                "局部排版 + 渲染当前第 {page} 页耗时：{:.2} ms",
                render_elapsed.as_secs_f64() * 1000.0
            );

            let catchup_start = Instant::now();
            doc.catchup_render()?;
            let catchup_elapsed = catchup_start.elapsed();
            println!(
                "后台补完全书排版耗时：{:.2} ms（补完后总页数：{} 页）",
                catchup_elapsed.as_secs_f64() * 1000.0,
                doc.page_count()
            );
        } else {
            println!("当前文档不支持局部重排（需多分片且启用缓存），退回全量渲染");
            let render_start = Instant::now();
            doc.render_page_with_options(page, &mut buffer, &options)?;
            render_elapsed = render_start.elapsed();
            println!("渲染耗时：{:.1} ms", render_elapsed.as_secs_f64() * 1000.0);
        }
    } else {
        let render_start = Instant::now();
        doc.render_page_with_options(page, &mut buffer, &options)?;
        render_elapsed = render_start.elapsed();
        println!("渲染耗时：{:.1} ms", render_elapsed.as_secs_f64() * 1000.0);
    }

    if night {
        // 夜间模式：整页反色，和 App 在面板上做的事一样
        // （aristotle-render 的 Page::invert_images）。
        for p in buffer.pixels_mut() {
            *p = 255 - *p;
        }
    }
    write_png(&output, &buffer)?;

    let xpointer = doc.page_xpointer(page)?;
    println!("第 {page} 页 XPointer：{xpointer}");
    println!(
        "XPointer 反查页码：{}",
        doc.page_of_xpointer(&xpointer).unwrap_or_default()
    );
    println!(
        "已渲染第 {page} 页 → {}（{}x{}，8 位灰度）",
        output.display(),
        buffer.width(),
        buffer.height()
    );
    println!("打开耗时：{:.1} ms", open_elapsed.as_secs_f64() * 1000.0);
    println!("渲染耗时：{:.1} ms", render_elapsed.as_secs_f64() * 1000.0);
    Ok(())
}

#[cfg(not(feature = "crengine"))]
fn main() {
    eprintln!("这个例子需要 --features crengine");
    std::process::exit(1);
}

/// 把 8 位灰度缓冲写成 PNG。
#[cfg(feature = "crengine")]
fn write_png(path: &std::path::Path, buffer: &inkread_platform::GrayBuffer) -> Result<(), String> {
    let file =
        std::fs::File::create(path).map_err(|e| format!("创建 {} 失败：{e}", path.display()))?;
    let mut encoder = png::Encoder::new(file, buffer.width(), buffer.height());
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("写 PNG 头失败：{e}"))?;
    writer
        .write_image_data(buffer.pixels())
        .map_err(|e| format!("写 PNG 数据失败：{e}"))?;
    Ok(())
}
