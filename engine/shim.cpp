/* 墨阅 · koreader/crengine C 接口 shim 实现。
 *
 * 许可证：AGPL-3.0-or-later。
 */

#include "shim.h"
#include "crsetup.h"

#include <crengine.h>
#include <lvtinydom.h>
#include <lvarray.h>
#include <lvdocviewprops.h>
#include <lvfnt.h>
#include <lvfntman.h>
#include <lvdrawbuf.h>
#include <lvpagesplitter.h>
#include <lvrend.h>
#include <lvstring.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

/* 追踪文档排版过程的回调。
 *
 * crengine 在执行完整排版/重排时会调用 OnFormatStart()；
 * 命中排版缓存（样式哈希、视口宽高完全一致）跳过重排时则不会调用。
 * 用它向外导出「本次打开或改排版是否真正执行了重排」的准确标志。
 * 同时根据 OnLoadFileProgress、OnNodeStylesUpdateProgress、OnFormatProgress
 * 三个阶段的耗时权重合成 0..100 的排版进度并回调调用方。 */
class DocCallback : public LVDocViewCallback {
public:
    LVDocView *view = nullptr;
    cr_progress_cb progress_cb = nullptr;
    void *user_data = nullptr;
    bool formatted = false;
    bool is_open = true;
    int last_progress = 0;

    void reset(bool open, cr_progress_cb cb, void *data) {
        progress_cb = cb;
        user_data = data;
        formatted = false;
        is_open = open;
        last_progress = 0;
    }

    void emit_progress(int p) {
        if (p < last_progress) {
            p = last_progress;
        }
        if (p > 100) {
            p = 100;
        }
        if (p > last_progress) {
            last_progress = p;
            if (progress_cb != nullptr) {
                progress_cb(p, user_data);
            }
        }
    }

    void OnLoadFileProgress(int percent) override {
        // 如果文档正在从缓存反序列化（hasCacheFile 为真），
        // 说明命中 DOM 缓存（耗时仅数十毫秒），此时不上报文件加载进度，
        // 避免在免重排命中时泄漏中间进度（APP-006 要求命中缓存时不经过中间值）。
        if (view != nullptr && view->getDocument() != nullptr && view->getDocument()->hasCacheFile()) {
            return;
        }
        if (is_open) {
            // 开书冷解析阶段：权重 20% (0% -> 20%)
            int p = (percent * 20) / 100;
            emit_progress(p);
        }
    }

    void OnNodeStylesUpdateProgress(int percent) override {
        if (is_open) {
            // 开书样式计算阶段：权重 10% (20% -> 30%)
            int p = 20 + (percent * 10) / 100;
            emit_progress(p);
        } else {
            // 改排版样式计算阶段：权重 15% (0% -> 15%)
            int p = (percent * 15) / 100;
            emit_progress(p);
        }
    }

    void OnFormatStart() override {
        formatted = true;
    }

    void OnFormatProgress(int percent) override {
        if (is_open) {
            // 开书页面排版切页阶段：权重 70% (30% -> 100%)
            int p = 30 + (percent * 70) / 100;
            emit_progress(p);
        } else {
            // 改排版页面排版切页阶段：权重 85% (15% -> 100%)
            int p = 15 + (percent * 85) / 100;
            emit_progress(p);
        }
    }

    void finish() {
        // 结束时无论是否命中缓存，最终进度都到达 100
        emit_progress(100);
    }
};

/* 已打开的文档。 */
struct CrDoc {
    LVDocView *view = nullptr;
    LVGrayDrawBuf *buf = nullptr;
    DocCallback callback;
    int width = 0;
    int height = 0;
    /* 页边距：页面坐标与文档流坐标的换算要用。 */
    int margin_left = 0;
    int margin_top = 0;
    int margin_right = 0;
    int margin_bottom = 0;
    /* 正文行高（像素）：字号 × 行高百分比。整行划线在文字行不足时用它
     * 推算空白行槽的网格。 */
    int line_height_px = 0;
    /* 精排开关与字重。
     *
     * crengine 的 DPI、字距模式（kerning）与基准字重是进程级全局设置：
     * 换一本书、甚至另一个线程上的另一个文档都会改掉它们。渲染前要按这里
     * 存的再设一次，否则同一份文档会因为别的文档改过全局设置而画出不同的
     * 字形（与回退字体是同一个问题）。 */
    bool fine_typesetting = true;
    int font_weight = 400;
    int render_dpi = 300;
    /* 这一版书的回退字体（face 名，分号隔开）。
     *
     * 回退字体在 crengine 里是进程级的设置，渲染前要按这里存的再设一次：
     * 换一本书会改掉全局设置，不设回来的话这一版会拿别的书的回退字体排。 */
    std::string fallback_font_faces;
    /* 打开时是否命中文档缓存（DOM 来源于磁盘缓存文件）。 */
    bool opened_from_cache = false;
};

namespace {

/* 页面状态栏位置：0 = 不画。与 lvdocview.cpp 里的 PAGE_HEADER_POS_NONE 一致。 */
constexpr int kPageHeaderPosNone = 0;

/* 灰度缓冲位深：8 位灰度，与 GrayBuffer 一致。 */
constexpr int kGrayBits = 8;

/* 页面宽高上限（像素）。Kindle 是 32 位 ARM，size_t 的乘法会回绕，
 * 超大尺寸会直接让 LVGrayDrawBuf 越界写。8192 远超任何真实墨水屏。 */
constexpr int kMaxPageDimension = 8192;

/* 页面宽高下限（像素），与 crengine 的 SCREEN_SIZE_MIN 一致（crsetup.h）。
 * LVDocView::Resize() 会把更小的尺寸静默钳到 80：放进来就会变成「引擎按 80
 * 排版、缓冲按调用方尺寸解读」，整页错行。Rust 层有同一套校验，这里再挡一道，
 * 保证 C 边界单独被调用时也不会出现不一致的尺寸。 */
constexpr int kMinPageDimension = 80;

/* 现代精排的 block rendering 标志。
 *
 * 头文件里没有叫 BLOCK_RENDERING_FLAGS_WEB 的宏：KOReader 的「Web 盒模型」
 * 就是 lvrend.h 里的 BLOCK_RENDERING_FULL_FEATURED（0x7FFFFFFF），它把
 * W3C 盒模型、外边距折叠、float/inline-block 等增强渲染全部打开。
 * crengine 的 DEF_RENDER_BLOCK_RENDERING_FLAGS 也是这个值；这里显式写出来，
 * 让「精排开/关」不依赖库的默认值。 */
constexpr lUInt32 kFineBlockRenderingFlags = BLOCK_RENDERING_FULL_FEATURED;
static_assert((kFineBlockRenderingFlags & BLOCK_RENDERING_USE_W3C_BOX_MODEL) != 0,
              "fine typesetting must include the W3C box model");

/* 精排时的默认渲染 DPI（以及合法范围）。
 *
 * crengine 的 1 CSS px 在 BASE_CSS_DPI（96）下等于 1 屏幕像素；CSS 里的
 * pt / mm / in 按面板真实 DPI（如 167 / 212 / 300 ppi）换算才能与物理尺寸
 * 一致。注意只改 CSS 绝对单位的换算（gRenderDPI），不打开
 * PROP_RENDER_SCALE_FONT_WITH_DPI：字号是调用方按设备像素算好的，再乘
 * dpi/96 会放大数倍。 */
constexpr int kDefaultFineRenderDpi = 300;
constexpr int kMinRenderDpi = 72;
constexpr int kMaxRenderDpi = 600;

int resolve_render_dpi(bool fine, int requested_dpi) {
    if (!fine) {
        return DEF_RENDER_DPI;
    }
    if (requested_dpi <= 0) {
        return kDefaultFineRenderDpi;
    }
    return std::clamp(requested_dpi, kMinRenderDpi, kMaxRenderDpi);
}

/* 引导线的几何：2 像素线宽；点线 3 像素实、3 像素空。
 *
 * 墨水屏没有灰度渐变，线太细会被栅格化吃掉，点线的空档太大又会看起来
 * 断断续续。3/3 在 300 ppi 上接近 0.25 mm 的点径，缩小看是一条灰线，
 * 放大看仍能数出点。 */
constexpr int kUnderlineThickness = 2;
constexpr int kDottedRun = 3;
constexpr int kDottedGap = 3;

/* 引导线的横向范围。与 shim.h / inkread_engine::UnderlineScope 一致：
 * 0 = TextOnly（文字宽度），1 = FullLine（整行）。 */
constexpr int kUnderlineScopeFullLine = 1;

/* C++ 异常时返回的错误码。与正常错误的 1..6 区分开。 */
constexpr int kErrorException = 99;

/* crengine 的字体管理器（fontMan）是进程级全局对象，内部没有加锁。
 * 所有碰到它的入口都要先拿这把锁，否则多线程下会踩坏堆。 */
std::mutex &font_mutex() {
    static std::mutex mutex;
    return mutex;
}

/* 已注册 face 的名称缓存。
 *
 * fontMan 是进程级全局对象：cr_fonts_init 注册新字体后 face 列表会变，
 * 缓存必须失效重建，否则按旧下标取名字会拿到错位的结果。
 *
 * filled 为 false 表示缓存已失效，下次 cr_fonts_face_name 时重建。 */
struct FaceNameCache {
    std::vector<std::string> names;
    bool filled = false;
};

FaceNameCache &face_cache() {
    static FaceNameCache cache;
    return cache;
}

/* 丢掉 face 名称缓存：注册了新字体，名字列表要重新取。 */
void invalidate_face_name_cache() {
    FaceNameCache &cache = face_cache();
    cache.names.clear();
    cache.filled = false;
}

void set_error(char *err, size_t err_len, const std::string &msg) {
    if (err == nullptr || err_len == 0) {
        return;
    }
    const size_t n = msg.size() < err_len - 1 ? msg.size() : err_len - 1;
    std::memcpy(err, msg.data(), n);
    err[n] = '\0';
}

/* 把 lString8 拷进 C 字符串缓冲，超长时截断。 */
void copy_out(const lString8 &src, char *out, size_t out_len) {
    if (out == nullptr || out_len == 0) {
        return;
    }
    const size_t len = static_cast<size_t>(src.length());
    const size_t n = len < out_len - 1 ? len : out_len - 1;
    std::memcpy(out, src.c_str(), n);
    out[n] = '\0';
}

/* 复制一段文本成新分配的 UTF-8 C 字符串。分配失败返回 NULL。
 *
 * 出来的字符串归调用方所有，用 std::free 释放。 */
char *dup_string(const lString32 &src) {
    lString8 utf8 = UnicodeToUtf8(src);
    const size_t len = static_cast<size_t>(utf8.length());
    char *out = static_cast<char *>(std::malloc(len + 1));
    if (out == nullptr) {
        return nullptr;
    }
    std::memcpy(out, utf8.c_str(), len);
    out[len] = '\0';
    return out;
}

/* 本页在文档流里的起始 y。
 *
 * crengine 的排版坐标是「文档流坐标」：y 跨页连续累加，x 以正文区左边缘
 * 为原点。渲染时它做的偏移是（见 lvdocview.cpp 的 drawPageTo()）：
 *   屏幕 x = 流 x + 左边距
 *   屏幕 y = 流 y - 本页起始流坐标 + 上边距
 * 那一页的起点在 LVRendPageInfo::start 里。 */
int page_flow_start(const CrDoc *doc, int page) {
    LVRendPageList *pages = doc->view->getPageList();
    if (pages == nullptr || page < 0 || page >= pages->length()) {
        return 0;
    }
    LVRendPageInfo *info = (*pages)[page];
    return info == nullptr ? 0 : info->start;
}

/* 本页在文档流里的结束 y（不含）。
 *
 * 用它把「其实在下一页」的元素剔掉：crengine 取本页范围时，末尾是按
 * 「从页面底部往上扫」找的第一个位置，扫到的可能是下一页的第一行。 */
int page_flow_end(const CrDoc *doc, int page) {
    LVRendPageList *pages = doc->view->getPageList();
    if (pages == nullptr || page < 0 || page >= pages->length()) {
        return 0;
    }
    LVRendPageInfo *info = (*pages)[page];
    return info == nullptr ? 0 : info->start + info->height;
}

/* 把文档流坐标的矩形换算成页面坐标。
 *
 * 页面坐标与 cr_doc_render_page 画出来的画面同一坐标系：左上角是原点，
 * 右边与下边是开区间。 */
CrRect to_page_rect(const lvRect &rc, int page_start, int margin_left, int margin_top) {
    CrRect out;
    out.x = rc.left + margin_left;
    out.y = rc.top - page_start + margin_top;
    out.width = rc.width();
    out.height = rc.height();
    return out;
}

/* 画一段引导线像素。
 *
 * 只负责页面坐标：调用方已经把文档流坐标换算好了。`y` 是线的第一行；
 * 线宽是 kUnderlineThickness 像素。所有坐标都做严格边界校验：墨水屏的
 * 页面缓冲是裸指针，任何一次 0 <= py < height、0 <= px < width 之外的写
 * 都是越界写（KO2-UNDERLINE-001 的教训）。 */
void draw_underline_pixels(uint8_t *dst, int width, int height, int y, int x0, int x1, int style,
                           lUInt8 gray) {
    if (dst == nullptr || width <= 0 || height <= 0 || style <= 0) {
        return;
    }
    if (x0 < 0) {
        x0 = 0;
    }
    if (x1 > width) {
        x1 = width;
    }
    if (x0 >= x1 || y >= height || y <= -kUnderlineThickness) {
        return;
    }
    for (int py = y; py < y + kUnderlineThickness; py++) {
        if (py < 0 || py >= height) {
            continue;
        }
        uint8_t *row = dst + static_cast<size_t>(py) * static_cast<size_t>(width);
        if (style == 2) {
            // 点状：3 像素实、3 像素空。
            for (int px = x0; px < x1;) {
                const int run_end = px + kDottedRun < x1 ? px + kDottedRun : x1;
                for (int i = px; i < run_end; i++) {
                    if (i >= 0 && i < width) {
                        row[i] = gray;
                    }
                }
                px = run_end + kDottedGap;
            }
        } else {
            for (int px = x0; px < x1; px++) {
                if (px >= 0 && px < width) {
                    row[px] = gray;
                }
            }
        }
    }
}

/* 画一行文字下方的引导线（文档流坐标转页面坐标后落在页面缓冲里）。
 *
 * 只画文字宽度：x 范围来自这一行的文字矩形。 */
void draw_underline_line(const CrDoc *doc, uint8_t *dst, int width, int height, int flow_top,
                         int flow_bottom, int flow_left, int flow_right, int page_start,
                         int style, lUInt8 gray) {
    const int margin_left = std::clamp(doc->margin_left, 0, width);
    const int margin_top = std::clamp(doc->margin_top, 0, height);
    const int top = flow_top - page_start + margin_top;
    const int bottom = flow_bottom - page_start + margin_top;
    const int y = bottom + 2;
    if (y <= top) {
        return; // 与文字重叠，不画
    }
    draw_underline_pixels(dst, width, height, y, flow_left + margin_left,
                          flow_right + margin_left, style, gray);
}

/* 画一整行引导线：横跨正文区 margin_left..width-margin_right。 */
void draw_underline_full_line(const CrDoc *doc, uint8_t *dst, int width, int height, int y,
                              int style, lUInt8 gray) {
    const int margin_left = std::clamp(doc->margin_left, 0, width);
    const int margin_right = std::clamp(doc->margin_right, 0, width);
    const int margin_top = std::clamp(doc->margin_top, 0, height);
    if (y < margin_top) {
        return; // 不进上边距
    }
    const int body_bottom = height - std::clamp(doc->margin_bottom, 0, height);
    if (y + kUnderlineThickness > body_bottom) {
        return; // 不进下边距
    }
    draw_underline_pixels(dst, width, height, y, margin_left, width - margin_right, style, gray);
}

/* 正文每行下方的引导线。
 *
 * 用的是 crengine 自己的行几何：getPageDocumentRange() 拿到本页的文档
 * 范围，getSegmentRects() 给出每个文本行一个矩形（默认不含图片）。
 * 所以文字划线只落在正文文字行的下面，图片、页眉页脚都不会被画到。
 *
 * FullLine 模式下线条横跨正文区（margin_left..width-margin_right），并且
 * 按行高网格把段间空白、短段落下方和页面底部的空行槽补满，形成横格纸的
 * 效果。行高优先用相邻文字行的间距（中位数，标题等不同字号不会把网格
 * 带偏），行数不足时用排版参数算出的 line_height_px。
 *
 * 引导线画进 cr_doc_render_page 的页面缓冲里，和文字同一次渲染完成，
 * 翻页路径上不额外调用引擎。 */
void draw_underline_guides(const CrDoc *doc, int page, uint8_t *dst, int width, int height,
                           int style, int gray, int scope) {
    if (style <= 0 || doc->view == nullptr || dst == nullptr || width <= 0 || height <= 0) {
        return;
    }
    const lUInt8 g = static_cast<lUInt8>(std::clamp(gray, 0, 255));
    const bool full_line = scope == kUnderlineScopeFullLine;

    LVRef<ldomXRange> range = doc->view->getPageDocumentRange(page);
    std::vector<lvRect> rows;
    if (!range.isNull()) {
        LVArray<lvRect> rects;
        range->getSegmentRects(rects); // 每个文本行一个矩形，图片不在其中
        const int page_end = page_flow_end(doc, page);
        const int count = rects.length();
        for (int r = 0; r < count; r++) {
            const lvRect &rc = rects[r];
            if (rc.top >= page_end) {
                break; // 其实在下一页
            }
            if (!rows.empty() && rows.back().top == rc.top) {
                // 同一行可能有多个片段（双向文字），合并成一段，
                // 免得一行被切成两截短线。
                lvRect &row = rows.back();
                if (rc.bottom > row.bottom) {
                    row.bottom = rc.bottom;
                }
                if (rc.left < row.left) {
                    row.left = rc.left;
                }
                if (rc.right > row.right) {
                    row.right = rc.right;
                }
                continue;
            }
            rows.push_back(rc);
        }
    }

    if (!full_line) {
        if (rows.empty()) {
            return;
        }
        const int page_start = page_flow_start(doc, page);
        for (const lvRect &row : rows) {
            draw_underline_line(doc, dst, width, height, row.top, row.bottom, row.left, row.right,
                                page_start, style, g);
        }
        return;
    }

    /* 行高（网格步长）：相邻文字行 top 的差（中位数）。
     *
     * 段间距、标题行高和正文不同，取中位数比取最小值稳：一两行标题不会
     * 把整页的网格带偏。 */
    std::vector<int> gaps;
    for (size_t i = 1; i < rows.size(); i++) {
        const int gap = rows[i].top - rows[i - 1].top;
        if (gap > 0) {
            gaps.push_back(gap);
        }
    }
    int step = 0;
    if (!gaps.empty()) {
        std::sort(gaps.begin(), gaps.end());
        step = gaps[gaps.size() / 2];
    }
    if (step <= 0) {
        step = doc->line_height_px;
    }
    if (step < 4) {
        step = 4;
    }

    const int page_start = page_flow_start(doc, page);
    const int margin_top = std::clamp(doc->margin_top, 0, height);
    const int body_bottom = height - std::clamp(doc->margin_bottom, 0, height);
    int last_y = margin_top;
    for (size_t i = 0; i < rows.size(); i++) {
        const lvRect &row = rows[i];
        const int y = row.bottom - page_start + margin_top + 2;
        if (y <= row.top - page_start + margin_top) {
            continue; // 与文字重叠，不画
        }
        draw_underline_full_line(doc, dst, width, height, y, style, g);
        // 上一行与这一行之间的空白（段间距、标题后的留白）按网格补满。
        if (i > 0 && y - last_y > step + step / 2) {
            for (int fill = last_y + step; fill + kUnderlineThickness <= y - step / 2;
                 fill += step) {
                draw_underline_full_line(doc, dst, width, height, fill, style, g);
            }
        }
        last_y = y;
    }

    // 短段落或整页空白：从最后一条线继续按网格补到正文区底部。
    if (rows.empty()) {
        last_y = margin_top;
    }
    for (int y = last_y + step; y + kUnderlineThickness <= body_bottom; y += step) {
        draw_underline_full_line(doc, dst, width, height, y, style, g);
    }
}

/* 释放一批目录项（含里面的字符串）。 */
void free_toc_items(CrTocItem *items, int count) {
    if (items == nullptr) {
        return;
    }
    for (int i = 0; i < count; i++) {
        std::free(items[i].title);
        std::free(items[i].xpointer);
    }
    std::free(items);
}

/* 释放一个目录项的字段。 */
void free_toc_entry(CrTocItem &entry) {
    std::free(entry.title);
    std::free(entry.xpointer);
    entry.title = nullptr;
    entry.xpointer = nullptr;
}

/* 释放整份目录项列表。 */
void free_toc_vector(std::vector<CrTocItem> &entries) {
    for (CrTocItem &entry : entries) {
        free_toc_entry(entry);
    }
    entries.clear();
}

/* 释放一批词（含里面的字符串与矩形数组）。 */
void free_words(CrWord *words, int count) {
    if (words == nullptr) {
        return;
    }
    for (int i = 0; i < count; i++) {
        std::free(words[i].text);
        std::free(words[i].start_xpointer);
        std::free(words[i].end_xpointer);
        std::free(words[i].rects);
    }
    std::free(words);
}

/* 释放一批链接（含里面的字符串与矩形数组）。 */
void free_links(CrLink *links, int count) {
    if (links == nullptr) {
        return;
    }
    for (int i = 0; i < count; i++) {
        std::free(links[i].href);
        std::free(links[i].target_xpointer);
        std::free(links[i].rects);
    }
    std::free(links);
}

/* 递归收集目录项。level 是 0 起的层级：顶层是 0。
 *
 * 定位不到页码的目录项连同它的子项一起跳过：这一节在书里是定位不到的，
 * 留着只会让「页属于哪一节」算错。
 *
 * 分配失败返回 false，已经收到的部分会被释放。 */
bool collect_toc(LVDocView *view, LVTocItem *item, int level, std::vector<CrTocItem> &out) {
    for (int i = 0; i < item->getChildCount(); i++) {
        LVTocItem *child = item->getChild(i);
        if (child == nullptr) {
            continue;
        }
        CrTocItem entry;
        entry.title = dup_string(child->getName());
        entry.level = level;
        entry.xpointer = dup_string(child->getPath());
        entry.page = child->getPage();
        if (entry.page < 0) {
            // 页码定位不到时自己再算一次：目录项带着 XPointer，
            // 可以直接换算成页码。
            ldomXPointer ptr = child->getXPointer();
            if (!ptr.isNull()) {
                entry.page = view->getBookmarkPage(ptr);
            }
        }
        if (entry.page < 0) {
            free_toc_entry(entry);
            continue;
        }
        if (entry.title == nullptr || entry.xpointer == nullptr) {
            free_toc_entry(entry);
            free_toc_vector(out);
            return false;
        }
        out.push_back(entry);
        if (!collect_toc(view, child, level + 1, out)) {
            return false;
        }
    }
    return true;
}

/* 设置书内样式开关。
 *
 * 对应 crengine 的 PROP_EMBEDDED_STYLES：
 * - 属性本身存在 LVDocView 的 m_props 里。LVDocView::createEmptyDocument()
 *   建文档时读它，决定要不要给文档打上 DOC_FLAG_ENABLE_INTERNAL_STYLES 标记，
 *   而这个标记决定 EPUB 解析时会不会把书里的 <style> 灌进文档
 *   （见 ldomdocumentwriter.cpp）。所以这个属性必须在 LoadDocument 之前设。
 * - 文档已经建好之后要改，还得直接给文档打标记，crengine 自己的
 *   DCMD_SET_INTERNAL_STYLES（lvdocview.cpp）就是这么做的。 */
void apply_embedded_styles(LVDocView *view, bool enabled) {
    view->propsGetCurrent()->setBool(PROP_EMBEDDED_STYLES, enabled);
    ldomDocument *doc = view->getDocument();
    if (doc != nullptr) {
        doc->setDocFlag(DOC_FLAG_ENABLE_INTERNAL_STYLES, enabled);
    }
}

/* 设置书内嵌字体开关。
 *
 * 对应 crengine 的 PROP_EMBEDDED_FONTS / DOC_FLAG_ENABLE_DOC_FONTS：
 * 解析期由 createEmptyDocument() 读取 m_props 落到 doc flags，已打开文档
 * 切换时同步更新 doc flag，使 @font-face 立即生效或回退到阅读器选定字体。 */
void apply_embedded_fonts(LVDocView *view, bool enabled) {
    view->propsGetCurrent()->setBool(PROP_EMBEDDED_FONTS, enabled);
    ldomDocument *doc = view->getDocument();
    if (doc != nullptr) {
        doc->setDocFlag(DOC_FLAG_ENABLE_DOC_FONTS, enabled);
    }
}

/* 把请求的基准字重换成字体管理器实际能画好的那一个。
 *
 * crengine 的字面选择是「最近字重」：字面只有 400/700 时，请求 600 会选中
 * 700 的粗体字面，然后用 FT_Outline_Embolden 的负强度把它削细。负强度会把
 * 笔画侵蚀掉，600 画出来反而比 500 还细（TYPE-101 在 macOS 宋体上实测）。
 *
 * 负强度不可靠，所以这里只在「请求值比两个字面的中点更靠近更重的那一侧，
 * 而且负强度已经大到接近 0.5 像素」时把基准降到中点：字体管理器会选较轻
 * 的字面，再向上合成加粗。字面本身有请求的字重（半粗字面或可变字体）时
 * 原样下发；负强度很小的字重（例如 650 落在 400/700 中间）保留原值，让
 * 粗体字面只被轻微调整。
 *
 * 例如宋体有 300/400/700/900：请求 600 会下发 550（400 加合成加粗），
 * 请求 500 / 650 / 690 原样下发。
 *
 * 调用方必须已持有 font_mutex()：这里要读字体管理器的字面列表，而
 * std::mutex 不可重入，所以本函数自己不加锁。 */
int resolve_base_weight_locked(LVDocView *view, int weight) {
    if (view == nullptr || fontMan == nullptr || weight <= 0) {
        return weight;
    }
    LVArray<int> weights;
    fontMan->GetAvailableFontWeights(weights, view->getDefaultFontFace());
    if (weights.length() <= 0) {
        return weight;
    }
    int lower = 0;
    int upper = 0;
    for (int i = 0; i < weights.length(); i++) {
        const int w = weights[i];
        if (w == weight) {
            return weight; // 字面本身就有这个字重
        }
        if (w < weight && w > lower) {
            lower = w;
        }
        if (w > weight && (upper == 0 || w < upper)) {
            upper = w;
        }
    }
    if (lower > 0 && upper > 0) {
        const int midpoint = (lower + upper) / 2;
        // 负强度按 (requested - 字面字重)/6400 em 计算：75 在 46px 字号上
        // 约 0.54 像素，再大就会把粗体笔画削得比常规字重还细。
        constexpr int kMinErodingWeight = 75;
        if (weight > midpoint && upper - weight >= kMinErodingWeight) {
            return midpoint;
        }
    }
    return weight;
}

/* 把现代精排开关与字重套到 LVDocView 上。
 *
 * 用 propsApply 而不是直接改文档：这些属性库里都有现成的应用逻辑，而且
 * 文档还没建好（LoadDocument 之前）时会把值存进 m_props，等
 * createEmptyDocument() 建文档时再落到 doc flags 上（DOM 版本、block
 * rendering 标志、内嵌字体都是解析期行为，必须在 LoadDocument 之前设，
 * 见 ENG-009）。文档已经建好时 propsApply 会直接改 doc flags 并请求重排。
 *
 * 只在打开文档和改排版时调。DPI、kerning、基准字重是进程级全局设置，
 * 别的文档会改掉它们，渲染前由 apply_render_globals() 直接恢复，不能再走
 * propsApply()（它会 requestRender，把已经翻到的页清掉）。
 *
 * font_weight 越界时钳到 100..900；<= 0 时按 400 处理。 */
void apply_fine_typesetting(LVDocView *view, bool fine, bool embedded_fonts, int font_weight,
                            int dpi) {
    if (view == nullptr) {
        return;
    }
    if (font_weight <= 0) {
        font_weight = 400;
    }
    font_weight = std::clamp(font_weight, 100, 900);
    {
        // 查字面列表要读进程级字体管理器，持锁范围只包住这次查询：
        // 后面的 propsApply 不需要这把锁，也不能在持锁时调用（不可重入）。
        std::lock_guard<std::mutex> guard(font_mutex());
        font_weight = resolve_base_weight_locked(view, font_weight);
    }
    const int render_dpi = resolve_render_dpi(fine, dpi);

    CRPropRef props = LVCreatePropsContainer();
    // Web 盒模型与增强 block rendering。关掉精排时传 0：crengine 会退回
    // 传统（box-sizing: border-box）的布局，不再设置 ENHANCED 位。
    props->setInt(PROP_RENDER_BLOCK_RENDERING_FLAGS,
                  static_cast<int>(fine ? kFineBlockRenderingFlags : 0));
    // DOM 版本固定用当前版本，不随精排开关切换。旧的归一化版本会改变
    // DocFragment 的节点结构，用户来回切换精排时，已保存的 XPointer 阅读
    // 进度和高亮会跟着错位；DOM 版本只能在解析前决定，事后再改只会重排。
    props->setInt(PROP_REQUESTED_DOM_VERSION, gDOMVersionCurrent);
    // 悬挂标点（crengine 里叫 floating punctuation）。
    props->setBool(PROP_FLOATING_PUNCTUATION, fine);
    // 书内嵌字体：仅在精排开启且用户允许书内字体时启用。
    props->setBool(PROP_EMBEDDED_FONTS, fine && embedded_fonts);
    // 页下注关闭：注释已收进阅读器弹窗中展示，不再在每页底部重复排印注释原文。
    props->setBool(PROP_FOOTNOTES, false);
    // 激活 CJK 语言管线：精排模式默认按 zh-CN 启用简中标点挤压表与 libunibreak
    // 中文避头尾禁则，并开启内嵌语言识别（书内 xml:lang="zh-TW"/"ja"/"en" 自动切换）。
    props->setString(PROP_TEXTLANG_MAIN_LANG, fine ? lString32(U"zh-CN") : lString32(U"en"));
    props->setBool(PROP_TEXTLANG_EMBEDDED_LANGS_ENABLED, fine);
    // 字偶距与 CJK 精排：使用 KERNING_MODE_FREETYPE（非 DISABLED 即激活 lvtextfm.cpp
    // 的全部 CJK 标点挤压、灵活宽度与中西文 1/4em 留白，同时保留西文 FreeType kerning
    // 并走单字符宽度缓存）。避免对整本几十万汉字走 KERNING_MODE_HARFBUZZ 逐段全量 shaping
    // 导致低主频 ARM 墨水屏首次开书耗时放大 3~5 倍。关掉精排时不启用。
    props->setInt(PROP_FONT_KERNING, fine ? KERNING_MODE_FREETYPE : KERNING_MODE_DISABLED);
    // 自动 hinting 是 crengine 的现代默认；传统模式用 FreeType 的字节码
    // 解释器。两者都不改变行宽（只影响字形栅格化）。
    props->setInt(PROP_FONT_HINTING,
                  fine ? HINTING_MODE_AUTOHINT : HINTING_MODE_BYTECODE_INTERPRETOR);
    // CSS 绝对单位按面板真实 DPI 换算。不缩放字号本身。
    props->setInt(PROP_RENDER_DPI, render_dpi);
    props->setInt(PROP_RENDER_SCALE_FONT_WITH_DPI, 0);
    // 基准字重：书里每个元素的字重都会加上 (weight - 400) 的偏移。
    props->setInt(PROP_FONT_BASE_WEIGHT, font_weight);
    view->propsApply(props);
}

/* 把回退字体名单交给字体管理器。
 *
 * 名单是分号隔开的 face 名（crengine 自己的写法），空串表示不用回退字体。
 * crengine 只认它找得到的 face 名：认不出来的会被丢掉，所以真正生效的是
 * 过滤后的那一份，cr_fonts_fallback_faces() 读的就是它。
 *
 * 回退字体是进程级的设置：字体管理器是全局对象，字体管理器的
 * SetFallbackFontFaces() 会清掉字形缓存。换书时会覆盖前一本的设置，
 * 所以渲染前要按文档自己存的那份再设一次。
 * 改名单不用重排版，字形是渲染时才去找的。 */
void apply_fallback_faces(const char *faces) {
    std::lock_guard<std::mutex> guard(font_mutex());
    if (fontMan == nullptr) {
        return;
    }
    fontMan->SetFallbackFontFaces(lString8(faces == nullptr ? "" : faces));
}

/* 排版参数里的回退字体名单。 */
std::string fallback_faces_of(const CrLayout &layout) {
    return layout.fallback_font_faces == nullptr ? std::string()
                                                  : std::string(layout.fallback_font_faces);
}

/* 把这一份文档自己的排版参数记在 CrDoc 上。
 *
 * 页边距用于文档流坐标换算；精排开关、字重、行高是进程级全局设置的
 * 「本档副本」，渲染前要按它们再设一次（别的文档会改掉全局那份）。 */
void remember_layout(CrDoc *doc, const CrLayout &layout) {
    // 页边距是页面坐标换算的输入，必须用 LVDocView 真正生效的那一份：
    // 取引擎实际生效边距，防止后续引擎版本引入钳制后坐标失配。当前 vendored
    // 版本的 setPageMargins() 是直接赋值（lvdocview.cpp:415-427），两边相同；
    // 读回来是为了不依赖这个实现细节。取不到时退回钳过界的原始参数。
    if (doc->view != nullptr) {
        const lvRect margins = doc->view->getPageMargins();
        doc->margin_left = margins.left;
        doc->margin_top = margins.top;
        doc->margin_right = margins.right;
        doc->margin_bottom = margins.bottom;
    } else {
        doc->margin_left = std::clamp(layout.margin_left, 0, doc->width);
        doc->margin_top = std::clamp(layout.margin_top, 0, doc->height);
        doc->margin_right = std::clamp(layout.margin_right, 0, doc->width);
        doc->margin_bottom = std::clamp(layout.margin_bottom, 0, doc->height);
    }
    doc->fine_typesetting = layout.fine_typesetting != 0;
    int weight = layout.font_weight <= 0 ? 400 : layout.font_weight;
    doc->font_weight = std::clamp(weight, 100, 900);
    doc->render_dpi = resolve_render_dpi(doc->fine_typesetting, layout.dpi);
    doc->fallback_font_faces = fallback_faces_of(layout);

    // 行高网格：字号 × 行高百分比。layout 里没给时用引擎当前值。
    int font_size = layout.font_size;
    if (font_size <= 0 && doc->view != nullptr) {
        font_size = doc->view->getFontSize();
    }
    font_size = std::clamp(font_size, 1, 4096);
    int spacing = layout.line_spacing_percent > 0 ? layout.line_spacing_percent : 100;
    spacing = std::clamp(spacing, 1, 1000);
    doc->line_height_px = std::max(4, (font_size * spacing + 50) / 100);
}

/* 去掉文本节点开头的全角空格（U+3000）。
 *
 * 中文 EPUB 常用段首两个 U+3000 表示首行缩进。它们是正文里的字形，
 * CSS 的 text-indent 管不到：设成 0 也仍会缩进两个字，设成 2 字则会
 * 叠加成四字。阅读器统一把段首的全角空格当作排版标记去掉，首行缩进
 * 完全交给 build_cjk_css() 的 text-indent（READER-001 C.2）。
 *
 * 只处理节点开头连续的全角空格，正文中间的空格保留；U+3000 出现在
 * 行内节点边界的情况极少见，按同样规则处理。 */
void strip_leading_ideographic_spaces(ldomNode *node) {
    if (node == nullptr) {
        return;
    }
    if (node->isText()) {
        lString32 text = node->getText();
        int i = 0;
        while (i < text.length() && text[i] == 0x3000) {
            i++;
        }
        if (i > 0) {
            node->setText(text.substr(i));
        }
        return;
    }
    for (int i = 0; i < node->getChildCount(); i++) {
        strip_leading_ideographic_spaces(node->getChildNode(i));
    }
}

/* 把一组排版参数套到 LVDocView 上。
 *
 * 顺序有讲究：
 * - 样式表要先设，Render() 里的 updateDocStyleSheet() 会把它灌进文档；
 * - 页边距要等 Resize() 之后设，setPageMargins() 依赖 m_dx/m_dy 算页面矩形；
 * - 字号、行距、字体都会触发 REQUEST_RENDER，最后统一 Render() 一次。 */
void apply_layout(LVDocView *view, const CrLayout &layout) {
    if (layout.css != nullptr && *layout.css != '\0') {
        view->setStyleSheet(lString8(layout.css));
    }
    if (layout.font_face != nullptr && *layout.font_face != '\0') {
        view->setDefaultFontFace(lString8(layout.font_face));
    }
    if (layout.font_size > 0) {
        view->setFontSize(layout.font_size);
    }
    if (layout.line_spacing_percent >= 0) {
        view->setDefaultInterlineSpace(layout.line_spacing_percent);
    }
    // 精排属性要在文档解析前就位（DOM 版本、block rendering 标志、内嵌
    // 字体），文档已建好时它们会触发重排。字重是全局设置，每次渲染前
    // 还会再按文档自己的值设一次。
    const bool fine = layout.fine_typesetting != 0;
    const bool allow_doc_fonts = fine && (layout.embedded_fonts != 0);
    apply_fine_typesetting(view, fine, allow_doc_fonts, layout.font_weight, layout.dpi);
    // 文档已经建好了，这里要重新打一次标记：换排版参数时可能改了开关。
    apply_embedded_styles(view, layout.embedded_styles != 0);
    apply_embedded_fonts(view, allow_doc_fonts);
    // lvRect(left, top, right, bottom) 记的是页边距，不是页面矩形。
    view->setPageMargins(lvRect(layout.margin_left, layout.margin_top, layout.margin_right,
                                 layout.margin_bottom));
    // 回退字体要在渲染之前定下来：字形是渲染时现找的。
    apply_fallback_faces(layout.fallback_font_faces);
}

/* 渲染前恢复进程级的字体设置。
 *
 * 这些设置（kerning / hinting / 基准字重 / 渲染 DPI）是全局的，别的文档会
 * 改掉它们；但渲染路径不能再用 propsApply() 重放一遍：propsApply 里的
 * 若干分支会 requestRender()，把已经翻到的页清掉，下一次 Draw 又从第 0 页
 * 画起（TYPE-101 实测：连渲染 4 页得到同一张图）。这里直接写全局变量，
 * 值没变就不碰字体缓存。
 *
 * fontMan 的读写整段在 font_mutex() 下完成，只调不加锁的
 * resolve_base_weight_locked()：std::mutex 不可重入，绝不能在这里再套一层。 */
void apply_render_globals(LVDocView *view, bool fine, int font_weight, int render_dpi) {
    if (view == nullptr) {
        return;
    }
    std::lock_guard<std::mutex> guard(font_mutex());
    if (fontMan == nullptr) {
        return;
    }
    const kerning_mode_t kerning = fine ? KERNING_MODE_FREETYPE : KERNING_MODE_DISABLED;
    if (fontMan->GetKerningMode() != kerning) {
        fontMan->SetKerningMode(kerning);
    }
    const hinting_mode_t hinting =
            fine ? HINTING_MODE_AUTOHINT : HINTING_MODE_BYTECODE_INTERPRETOR;
    if (fontMan->GetHintingMode() != hinting) {
        fontMan->SetHintingMode(hinting);
    }
    const int weight = std::clamp(font_weight <= 0 ? 400 : font_weight, 100, 900);
    const int base_weight = resolve_base_weight_locked(view, weight);
    if (LVRendGetBaseFontWeight() != base_weight) {
        LVRendSetBaseFontWeight(base_weight);
    }
    gRenderDPI = render_dpi > 0 ? render_dpi : (fine ? kDefaultFineRenderDpi : DEF_RENDER_DPI);
    // 字号始终按设备像素算，不跟着 DPI 再缩放一次。
    gRenderScaleFontWithDPI = false;
}

/* 异常守卫。
 *
 * C++ 异常不能穿过 extern "C" 边界：Rust 侧声明的是 extern "C"（不是
 * extern "C-unwind"），异常跨边界展开是 UB，进程会被直接 abort，
 * 调用方拿不到 Error::Open / Error::Render。一本格式异常的畸形电子书
 * 就能把整个阅读器干掉。
 *
 * 用法：
 *   extern "C" int cr_xxx(...) {
 *       CR_GUARD_BEGIN
 *       ...
 *       return 0;
 *       CR_GUARD_END(err, err_len, kErrorException)
 *   }
 */
#define CR_GUARD_BEGIN try {
#define CR_GUARD_END(err, err_len, ret)                      \
    }                                                         \
    catch (const std::exception &e) {                         \
        set_error((err), (err_len), e.what());                 \
        return (ret);                                         \
    }                                                         \
    catch (...) {                                             \
        set_error((err), (err_len), "unknown C++ exception"); \
        return (ret);                                         \
    }

/* 不带错误缓冲的入口（返回值本身就是错误信号）用这一对。 */
#define CR_GUARD_END_NOERR(ret) \
    }                           \
    catch (...) {               \
        return (ret);           \
    }

/* void 返回的入口用这一对。 */
#define CR_GUARD_END_VOID() \
    }                       \
    catch (...) {           \
        return;             \
    }

} // namespace

namespace {

/* cr_doc_open 过程中的临时所有者：中途抛异常时把已经分配好的
 * LVDocView / LVGrayDrawBuf 一起回收，不留泄漏。 */
struct CrDocOwner {
    CrDoc *doc = nullptr;
    ~CrDocOwner() {
        if (doc != nullptr) {
            cr_doc_close(doc);
            doc = nullptr;
        }
    }
};

/* cr_doc_set_viewport 的事务守卫。
 *
 * 改视口要连输出缓冲、尺寸元数据、局部重排开关一起动。中途任何一步抛异常
 * （新缓冲分配之后的 Resize / apply_layout / Render），CR_GUARD 只能把异常
 * 转成错误码，没法恢复文档状态；0 页回退也要走同一条恢复路径。统一放在
 * 析构里：没有 commit() 就说明这次改尺寸没成功，把几何换回旧的。
 *
 * 只回滚几何（视口、输出缓冲、宽高、局部重排开关）。排版参数已经套到 view
 * 上了，和 cr_doc_set_layout 失败时一样保留：调用方拿到错误后，用有效参数
 * 再调一次即可继续用。 */
struct ViewportTxn {
    CrDoc *doc = nullptr;
    LVGrayDrawBuf *old_buf = nullptr;
    LVGrayDrawBuf *new_buf = nullptr;
    int old_width = 0;
    int old_height = 0;
    ldomDocument *dom = nullptr;
    bool restore_partial = false;
    bool committed = false;

    ViewportTxn(CrDoc *d, LVGrayDrawBuf *nb, ldomDocument *dom_, bool restore_partial_)
        : doc(d), old_buf(d->buf), new_buf(nb), old_width(d->width), old_height(d->height),
          dom(dom_), restore_partial(restore_partial_) {}

    ~ViewportTxn() {
        if (!committed) {
            // 回调里存的是调用方栈上的闭包指针，函数返回后不能留着。
            doc->callback.reset(false, nullptr, nullptr);
            // Resize 本身也可能抛异常；它失败也不能漏掉元数据和模式的恢复，
            // 所以每一步分别兜底。析构不能抛异常。
            try {
                if (doc->view != nullptr) {
                    doc->view->Resize(old_width, old_height);
                }
            } catch (...) {
            }
            doc->buf = old_buf;
            doc->width = old_width;
            doc->height = old_height;
            if (dom != nullptr && restore_partial) {
                try {
                    dom->enablePartialRerendering(true);
                } catch (...) {
                }
            }
        }
        // 提交后 new_buf 归文档管（commit 里已置空），这里不能再删。
        delete new_buf;
    }

    /* 把新缓冲和尺寸交给文档。渲染失败时由析构负责换回去。 */
    void publish(int width, int height) {
        if (new_buf != nullptr) {
            doc->buf = new_buf;
        }
        doc->width = width;
        doc->height = height;
    }

    /* 成功：旧缓冲可以释放了，析构不再回滚。 */
    void commit() {
        committed = true;
        // 同尺寸时没有换缓冲，doc->buf 还是 old_buf，不能删。
        if (doc->buf != old_buf) {
            delete old_buf;
        }
        old_buf = nullptr;
        new_buf = nullptr;
    }
};

} // namespace

extern "C" int cr_fonts_init(const char *const *font_paths, int font_count, char *err, size_t err_len) {
    CR_GUARD_BEGIN
    std::lock_guard<std::mutex> guard(font_mutex());
    if (fontMan == nullptr) {
        // 不扫描系统字体，只用显式注册的字体，方便复现。
        if (!InitFontManager(lString8())) {
            set_error(err, err_len, "InitFontManager failed");
            return 1;
        }
    }
    if (font_paths == nullptr || font_count <= 0) {
        set_error(err, err_len, "no font path given");
        return 2;
    }
    int registered = 0;
    for (int i = 0; i < font_count; i++) {
        const char *path = font_paths[i];
        if (path == nullptr || *path == '\0') {
            continue;
        }
        if (fontMan->RegisterFont(lString8(path))) {
            registered++;
        } else {
            std::string msg = "failed to register font: ";
            msg += path;
            set_error(err, err_len, msg);
        }
    }
    if (registered == 0) {
        if (err != nullptr && err_len > 0 && err[0] == '\0') {
            set_error(err, err_len, "no font registered");
        }
        return 3;
    }
    // 注册了新字体，face 列表变了：名字缓存必须失效，下次取名字时重建。
    invalidate_face_name_cache();
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_fonts_face_count(void) {
    CR_GUARD_BEGIN
    std::lock_guard<std::mutex> guard(font_mutex());
    if (fontMan == nullptr) {
        return 0;
    }
    lString32Collection list;
    fontMan->getFaceList(list);
    return list.length();
    CR_GUARD_END(nullptr, 0, 0)
}

extern "C" const char *cr_fonts_face_name(int index) {
    CR_GUARD_BEGIN
    std::lock_guard<std::mutex> guard(font_mutex());
    if (fontMan == nullptr) {
        return nullptr;
    }
    // 缓存挂在全局状态里：cr_fonts_init 注册新字体后会把它置为失效，
    // 这里发现失效就重建一次（QA-002 Sugg-2）。
    FaceNameCache &cache = face_cache();
    if (!cache.filled) {
        lString32Collection list;
        fontMan->getFaceList(list);
        cache.names.reserve(static_cast<size_t>(list.length()));
        for (int i = 0; i < list.length(); i++) {
            cache.names.push_back(UnicodeToUtf8(list[i]).c_str());
        }
        cache.filled = true;
    }
    if (index < 0 || index >= static_cast<int>(cache.names.size())) {
        return nullptr;
    }
    // 名字存在全局缓存里：下次 cr_fonts_init 会把它清掉，
    // 在那之前指针一直有效（shim.h 里承诺过）。Rust 侧只在初始化后立刻
    // 拷走，不会跨 init 持有。由 font_mutex() 保护，多线程同时取也不会踩坏。
    return cache.names[static_cast<size_t>(index)].c_str();
    CR_GUARD_END_NOERR(nullptr)
}

extern "C" int cr_fonts_fallback_faces(char *out, size_t out_len) {
    CR_GUARD_BEGIN
    if (out == nullptr || out_len == 0) {
        return 1;
    }
    std::lock_guard<std::mutex> guard(font_mutex());
    if (fontMan == nullptr) {
        out[0] = '\0';
        return 0;
    }
    copy_out(fontMan->GetFallbackFontFaces(), out, out_len);
    return 0;
    CR_GUARD_END_NOERR(1)
}

extern "C" int cr_cache_init(const char *dir, unsigned long max_size, char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (dir == nullptr || *dir == '\0') {
        set_error(err, err_len, "empty cache directory");
        return 1;
    }
    // crengine 的容量上限是 32 位无符号数（lvsize_t），超过 4 GB 没有意义。
    const lUInt32 limit =
        max_size > 0xFFFFFFFFul ? static_cast<lUInt32>(0xFFFFFFFFul) : static_cast<lUInt32>(max_size);
    // init 失败时 crengine 自己会把实例删掉，缓存等于没开：引擎照常排版。
    if (!ldomDocCache::init(Utf8ToUnicode(dir), limit)) {
        set_error(err, err_len, "cannot initialize document cache");
        return 2;
    }
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_cache_enabled(void) {
    CR_GUARD_BEGIN
    return ldomDocCache::enabled() ? 1 : 0;
    CR_GUARD_END_NOERR(0)
}

extern "C" int cr_cache_clear(char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (!ldomDocCache::enabled()) {
        set_error(err, err_len, "document cache is not initialized");
        return 1;
    }
    if (!ldomDocCache::clear()) {
        set_error(err, err_len, "cannot clear document cache");
        return 2;
    }
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_doc_from_cache(const CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr) {
        return 0;
    }
    return doc->opened_from_cache ? 1 : 0;
    CR_GUARD_END_NOERR(0)
}

extern "C" int cr_doc_reformatted(const CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr) {
        return 0;
    }
    return doc->callback.formatted ? 1 : 0;
    CR_GUARD_END_NOERR(0)
}

static float g_storage_size_factor = 2.0f;

extern "C" void cr_storage_size_factor_set(float factor) {
    if (factor > 0.0f) {
        g_storage_size_factor = factor;
        setStorageMaxUncompressedSizeFactor(factor);
    }
}

extern "C" float cr_storage_size_factor_get(void) {
    return g_storage_size_factor;
}

extern "C" int cr_doc_can_partial_rerender(const CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        return 0;
    }
    ldomDocument *dom = doc->view->getDocument();
    if (dom == nullptr) {
        return 0;
    }
    return dom->canBePartiallyRerendered() ? 1 : 0;
    CR_GUARD_END_NOERR(0)
}

extern "C" int cr_doc_enable_partial_rerender(CrDoc *doc, int enable, char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    ldomDocument *dom = doc->view->getDocument();
    if (dom == nullptr) {
        set_error(err, err_len, "document DOM not loaded");
        return 2;
    }
    if (enable) {
        if (!dom->canBePartiallyRerendered()) {
            set_error(err, err_len, "document does not support partial rerendering");
            return 3;
        }
        dom->enablePartialRerendering(true);
    } else {
        dom->enablePartialRerendering(false);
    }
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_doc_is_partial_rerender_enabled(const CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        return 0;
    }
    ldomDocument *dom = doc->view->getDocument();
    if (dom == nullptr) {
        return 0;
    }
    return dom->isPartialRerenderingEnabled() ? 1 : 0;
    CR_GUARD_END_NOERR(0)
}

extern "C" int cr_doc_is_rerender_delayed(const CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        return 0;
    }
    ldomDocument *dom = doc->view->getDocument();
    if (dom == nullptr) {
        return 0;
    }
    return dom->isRerenderingDelayed(false) ? 1 : 0;
    CR_GUARD_END_NOERR(0)
}

extern "C" int cr_doc_catchup_render(CrDoc *doc, cr_progress_cb progress_cb, void *user_data,
                                     char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    ldomDocument *dom = doc->view->getDocument();
    if (dom == nullptr) {
        set_error(err, err_len, "document DOM not loaded");
        return 2;
    }
    bool was_enabled = dom->isPartialRerenderingEnabled();
    dom->enablePartialRerendering(false);
    doc->callback.reset(false, progress_cb, user_data);
    doc->view->Render();
    if (doc->view->getPageCount() <= 0) {
        set_error(err, err_len, "document has no pages after catchup render");
        return 3;
    }
    doc->callback.finish();
    if (was_enabled) {
        dom->enablePartialRerendering(true);
    }
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" CrDoc *cr_doc_open(const char *path, int width, int height, const CrLayout *layout,
                              cr_progress_cb progress_cb, void *user_data,
                              char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (path == nullptr || *path == '\0') {
        set_error(err, err_len, "empty document path");
        return nullptr;
    }
    if (width <= 0 || height <= 0) {
        set_error(err, err_len, "invalid page size");
        return nullptr;
    }
    if (width < kMinPageDimension || height < kMinPageDimension) {
        set_error(err, err_len, "page size below minimum");
        return nullptr;
    }
    if (width > kMaxPageDimension || height > kMaxPageDimension) {
        set_error(err, err_len, "page size exceeds limit");
        return nullptr;
    }
    if (layout == nullptr) {
        set_error(err, err_len, "missing layout");
        return nullptr;
    }
    {
        // fontMan 可能在另一个线程上初始化（cr_fonts_init 持同一把锁），
        // 读这个全局指针本身也要在锁内。
        std::lock_guard<std::mutex> guard(font_mutex());
        if (fontMan == nullptr) {
            set_error(err, err_len, "font manager not initialized");
            return nullptr;
        }
    }

    setStorageMaxUncompressedSizeFactor(g_storage_size_factor);
    CrDocOwner owner;
    owner.doc = new CrDoc();
    CrDoc *doc = owner.doc;
    doc->width = width;
    doc->height = height;
    doc->view = new LVDocView(kGrayBits, true);
    doc->buf = new LVGrayDrawBuf(width, height, kGrayBits);

    LVDocView *view = doc->view;
    view->setViewMode(DVM_PAGES, 1);
    view->setStatusMode(kPageHeaderPosNone, false, false, false, false, false, false, false, false);
    view->setShowCover(false);
    // 灰度缓冲下 crengine 的默认底色是 4bpp 的 3，8bpp 必须显式给 0xFFFFFF/0x000000。
    view->setBackgroundColor(0xFFFFFF);
    view->setTextColor(0x000000);

    // 排版参数与视口尺寸必须在 LoadDocument 之前设置（ENG-009）：
    // crengine 读缓存时，LoadDocument 内部的 openFromCache -> updateLoadedStyles
    // 会依据当前的默认字体、字号、边距及视口尺寸来解析节点字体与级联样式。
    // 如果在 LoadDocument 之后才 Resize 和 apply_layout，读缓存时就会使用默认属性，
    // 导致计算出的样式与字体哈希对不上当初写缓存时的值，致使 checkRenderContext() 校验失败并重排。
    // KOReader 亦是先设置全部排版参数后再加载文档。
    view->Resize(width, height);
    apply_layout(view, *layout);
    remember_layout(doc, *layout);

    doc->callback.view = doc->view;
    doc->callback.reset(true, progress_cb, user_data);
    view->setCallback(&doc->callback);

    if (!view->LoadDocument(path)) {
        set_error(err, err_len, "LoadDocument failed");
        return nullptr;
    }

    // 仅在关闭精排（强制统一极简排版）时剥离段首全角空格 U+3000；
    // 精排模式下保留原书全角空格，避免破坏古诗、信件、落款、对联等刻意排版。
    if (!doc->fine_typesetting) {
        if (ldomDocument *dom = view->getDocument()) {
            strip_leading_ideographic_spaces(dom->getRootNode());
        }
    }

    doc->opened_from_cache = (view->getDocument() != nullptr && view->getDocument()->isTocFromCacheValid());

    view->Render();

    if (view->getPageCount() <= 0) {
        set_error(err, err_len, "document has no pages");
        return nullptr;
    }

    doc->callback.finish();

    // 排完了，把文档写进缓存：下次打开同一本书、同一套排版参数时直接读缓存，
    // 不用再排一遍。写不进去只说明这次没缓存上，不影响这一次打开。
    // 书太小（不到 30 KB）时 crengine 自己会跳过。
    if (ldomDocCache::enabled()) {
        view->swapToCache();
    }
    owner.doc = nullptr;
    return doc;
    CR_GUARD_END(err, err_len, nullptr)
}

extern "C" void cr_doc_close(CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr) {
        return;
    }
    if (doc->view != nullptr) {
        doc->view->setCallback(nullptr);
    }
    delete doc->buf;
    delete doc->view;
    delete doc;
    CR_GUARD_END_VOID()
}

extern "C" int cr_doc_page_count(const CrDoc *doc) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        return 0;
    }
    return doc->view->getPageCount();
    CR_GUARD_END(nullptr, 0, 0)
}

extern "C" int cr_doc_render_page(CrDoc *doc, int page, uint8_t *dst, size_t dst_len, int dst_width,
                                  int dst_height, int invert_images, int underline,
                                  int underline_gray, int underline_scope, char *err,
                                  size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr || doc->buf == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (dst == nullptr) {
        set_error(err, err_len, "null destination buffer");
        return 2;
    }
    // 缓冲宽高必须与文档页面尺寸完全一致。只比字节数不够：宽高对调时
    // 总字节数一样，按文档行宽写入会让整页错行。
    if (dst_width != doc->width || dst_height != doc->height) {
        set_error(err, err_len, "destination buffer size does not match page size");
        return 3;
    }
    const size_t needed = static_cast<size_t>(doc->width) * static_cast<size_t>(doc->height);
    if (dst_len < needed) {
        set_error(err, err_len, "destination buffer too small");
        return 4;
    }
    const int pages = doc->view->getPageCount();
    if (page < 0 || page >= pages) {
        set_error(err, err_len, "page out of range");
        return 5;
    }

    LVDocView *view = doc->view;
    // 回退字体、精排开关（kerning/DPI）与字重都是进程级的设置，换一本书
    // 或另一个线程上的文档就会改掉全局那份。渲染前按这一版书自己存的参数
    // 再设一次：不设回来的话，字形会拿别的书的设置排。
    apply_fallback_faces(doc->fallback_font_faces.c_str());
    apply_render_globals(view, doc->fine_typesetting, doc->font_weight, doc->render_dpi);
    if (!view->goToPage(page)) {
        set_error(err, err_len, "goToPage failed");
        return 6;
    }
    doc->buf->setInvertImages(invert_images != 0);
    // 第二个参数是 autoResize：false 表示不要按页面尺寸改缓冲大小，
    // 我们自己的缓冲已经是页面尺寸了。
    view->Draw(*doc->buf, false);

    // 8bpp 灰度缓冲的行宽就是像素宽，与 GrayBuffer 的 row-major 布局一致。
    const int row_size = doc->buf->GetRowSize();
    if (row_size != doc->width) {
        set_error(err, err_len, "unexpected gray buffer row size");
        return 7;
    }
    for (int y = 0; y < doc->height; y++) {
        const lUInt8 *line = doc->buf->GetScanLine(y);
        if (line == nullptr) {
            set_error(err, err_len, "gray buffer scanline missing");
            return 7;
        }
        std::memcpy(dst + static_cast<size_t>(y) * static_cast<size_t>(doc->width), line,
                    static_cast<size_t>(doc->width));
    }
    // 正文每行下方的引导线：和文字同一次渲染画进页面缓冲，
    // 翻页时不用额外调用引擎。
    draw_underline_guides(doc, page, dst, doc->width, doc->height, underline, underline_gray,
                          underline_scope);
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_doc_set_layout(CrDoc *doc, const CrLayout *layout,
                                 cr_progress_cb progress_cb, void *user_data,
                                 char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (layout == nullptr) {
        set_error(err, err_len, "missing layout");
        return 2;
    }
    apply_layout(doc->view, *layout);
    remember_layout(doc, *layout);
    doc->callback.reset(false, progress_cb, user_data);
    doc->view->Render();
    if (doc->view->getPageCount() <= 0) {
        set_error(err, err_len, "document has no pages");
        return 3;
    }
    doc->callback.finish();
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

/* 把文档的页面尺寸改成 width x height，并按新尺寸重新排版。
 *
 * 要同时动三样东西，缺一不可：
 * - LVDocView 的视口（Resize）：决定正文区有多宽、多高，决定怎么分页；
 * - 输出缓冲（LVGrayDrawBuf）：渲染时按页面尺寸逐行写进它，尺寸不对就错行；
 * - 尺寸元数据（doc->width/height）：渲染时校验调用方给的缓冲尺寸。
 *
 * 尺寸变了以后，排好版的分片全废了（它们是按旧宽度排的），局部重排会把
 * 新旧两种分页混在一起，所以这里临时关掉局部重排、整本重排一次、排完恢复。
 * 这条路跟 cr_doc_catchup_render 走的是同一条。
 *
 * 失败时的处理：
 * - 尺寸非法：改之前就挡住，文档一个字节都没动，照旧可用；
 * - 新缓冲分配失败：分配在改动之前，文档还是原样；
 * - Resize / apply_layout / Render 抛异常，或重排出不了页：视口、输出缓冲、
 *   宽高、局部重排开关一起回滚到调用前（见 ViewportTxn）。
 *   排版参数已经套到 view 上，和 cr_doc_set_layout 失败时一样保留；调用方
 *   拿到错误后，用有效参数重试即可继续用。 */
extern "C" int cr_doc_set_viewport(CrDoc *doc, int width, int height, const CrLayout *layout,
                                   cr_progress_cb progress_cb, void *user_data,
                                   char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr || doc->buf == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (layout == nullptr) {
        set_error(err, err_len, "missing layout");
        return 2;
    }
    if (width <= 0 || height <= 0) {
        set_error(err, err_len, "invalid page size");
        return 3;
    }
    if (width < kMinPageDimension || height < kMinPageDimension) {
        set_error(err, err_len, "page size below minimum");
        return 3;
    }
    if (width > kMaxPageDimension || height > kMaxPageDimension) {
        set_error(err, err_len, "page size exceeds limit");
        return 4;
    }

    const int old_width = doc->width;
    const int old_height = doc->height;
    // 尺寸没变就是纯改排版参数：输出缓冲不用换，局部重排该延迟还是延迟。
    // 尺寸变了才必须整本重排。
    const bool size_changed = width != old_width || height != old_height;

    // 新缓冲先分配：分配失败时文档还是原样，不用回滚。
    LVGrayDrawBuf *new_buf =
        size_changed ? new LVGrayDrawBuf(width, height, kGrayBits) : nullptr;

    LVDocView *view = doc->view;
    ldomDocument *dom = view->getDocument();
    const bool partial_was_enabled = dom != nullptr && dom->isPartialRerenderingEnabled();
    // 只有尺寸变了才临时关掉局部重排。
    const bool partial_disabled = size_changed && partial_was_enabled;

    ViewportTxn txn(doc, new_buf, dom, partial_disabled);
    if (partial_disabled) {
        dom->enablePartialRerendering(false);
    }

    // 视口先改，再套排版参数：页边距是按视口算的（setPageMargins 依赖
    // m_dx/m_dy 算页面矩形）。顺序和 cr_doc_open 里一样。
    view->Resize(width, height);
    apply_layout(view, *layout);
    remember_layout(doc, *layout);
    txn.publish(width, height);

    doc->callback.reset(false, progress_cb, user_data);
    view->Render();
    if (view->getPageCount() <= 0) {
        set_error(err, err_len, "document has no pages");
        // 析构里回滚视口、缓冲、宽高和局部重排开关。
        return 5;
    }
    doc->callback.finish();
    if (partial_disabled) {
        dom->enablePartialRerendering(true);
    }
    txn.commit();
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_doc_page_xpointer(const CrDoc *doc, int page, char *out, size_t out_len, char *err,
                                    size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (out == nullptr || out_len == 0) {
        set_error(err, err_len, "null output buffer");
        return 2;
    }
    const int pages = doc->view->getPageCount();
    if (page < 0 || page >= pages) {
        set_error(err, err_len, "page out of range");
        return 3;
    }
    ldomXPointer ptr = doc->view->getPageBookmark(page);
    if (ptr.isNull()) {
        set_error(err, err_len, "no xpointer for page");
        return 4;
    }
    copy_out(UnicodeToUtf8(ptr.toString()), out, out_len);
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_doc_page_of_xpointer(const CrDoc *doc, const char *xpointer, int *out_page,
                                       char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (xpointer == nullptr || *xpointer == '\0') {
        set_error(err, err_len, "empty xpointer");
        return 2;
    }
    if (out_page == nullptr) {
        set_error(err, err_len, "null output page pointer");
        return 3;
    }
    ldomDocument *doc_dom = doc->view->getDocument();
    if (doc_dom == nullptr) {
        set_error(err, err_len, "document not loaded");
        return 4;
    }
    ldomXPointer ptr = doc_dom->createXPointer(Utf8ToUnicode(xpointer));
    if (ptr.isNull()) {
        set_error(err, err_len, "invalid xpointer");
        return 5;
    }
    // 先问引擎要页码：getBookmarkPage() 内部会保证文档已经排版过，
    // 下面的 toPoint() 才拿得到真实位置。
    const int page = doc->view->getBookmarkPage(ptr);
    // 定位不到时 getBookmarkPage() 返回 0，和「就在第 0 页」分不开：
    // 位置算不出来（节点不可见、没排进文档流）时它内部走的是 pt.y < 0
    // 那条分支，一样返回 0。自己看位置，把这种情况报成失败。
    if (ptr.toPoint().y < 0) {
        set_error(err, err_len, "xpointer does not point into this document");
        return 6;
    }
    *out_page = page;
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" int cr_doc_title(const CrDoc *doc, char *out, size_t out_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr || out == nullptr || out_len == 0) {
        return 1;
    }
    copy_out(UnicodeToUtf8(doc->view->getTitle()), out, out_len);
    return 0;
    CR_GUARD_END_NOERR(1)
}

extern "C" int cr_doc_author(const CrDoc *doc, char *out, size_t out_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr || out == nullptr || out_len == 0) {
        return 1;
    }
    copy_out(UnicodeToUtf8(doc->view->getAuthors()), out, out_len);
    return 0;
    CR_GUARD_END_NOERR(1)
}

extern "C" int cr_doc_toc(const CrDoc *doc, CrTocList *out, char *err, size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (out == nullptr) {
        set_error(err, err_len, "null output list pointer");
        return 2;
    }
    out->count = 0;
    out->items = nullptr;

    // getToc() 会在页码过期时重新算一遍页码（updatePageNumbers）。
    LVTocItem *root = doc->view->getToc();
    if (root == nullptr) {
        return 0; // 没有目录，不算错误
    }
    std::vector<CrTocItem> entries;
    if (!collect_toc(doc->view, root, 0, entries)) {
        free_toc_vector(entries);
        set_error(err, err_len, "out of memory");
        return 3;
    }
    if (entries.empty()) {
        return 0;
    }
    const size_t count = entries.size();
    CrTocItem *items =
            static_cast<CrTocItem *>(std::calloc(count, sizeof(CrTocItem)));
    if (items == nullptr) {
        free_toc_vector(entries);
        set_error(err, err_len, "out of memory");
        return 3;
    }
    for (size_t i = 0; i < count; i++) {
        items[i] = entries[i];
        // 所有权转移给 items，entries 里对应位置清空，避免重复释放。
        entries[i].title = nullptr;
        entries[i].xpointer = nullptr;
    }
    out->items = items;
    out->count = static_cast<int>(count);
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" void cr_toc_free(CrTocList *list) {
    CR_GUARD_BEGIN
    if (list == nullptr) {
        return;
    }
    free_toc_items(list->items, list->count);
    list->items = nullptr;
    list->count = 0;
    CR_GUARD_END_VOID()
}

extern "C" int cr_doc_page_words(const CrDoc *doc, int page, CrWordList *out, char *err,
                                 size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (out == nullptr) {
        set_error(err, err_len, "null output list pointer");
        return 2;
    }
    out->count = 0;
    out->words = nullptr;
    const int pages = doc->view->getPageCount();
    if (page < 0 || page >= pages) {
        set_error(err, err_len, "page out of range");
        return 3;
    }
    LVRef<ldomXRange> range = doc->view->getPageDocumentRange(page);
    if (range.isNull()) {
        return 0; // 空页
    }
    LVArray<ldomWord> words;
    range->getRangeWords(words);
    const int count = words.length();
    if (count <= 0) {
        return 0;
    }
    CrWord *items = static_cast<CrWord *>(std::calloc(static_cast<size_t>(count), sizeof(CrWord)));
    if (items == nullptr) {
        set_error(err, err_len, "out of memory");
        return 4;
    }
    const int page_start = page_flow_start(doc, page);
    const int page_end = page_flow_end(doc, page);
    int filled = 0;
    for (int i = 0; i < count; i++) {
        ldomWord &word = words[i];
        CrWord item;
        item.text = dup_string(word.getText());
        item.start_xpointer = dup_string(word.getStartXPointer().toString());
        item.end_xpointer = dup_string(word.getEndXPointer().toString());
        if (item.text == nullptr || item.start_xpointer == nullptr ||
            item.end_xpointer == nullptr) {
            std::free(item.text);
            std::free(item.start_xpointer);
            std::free(item.end_xpointer);
            free_words(items, filled);
            set_error(err, err_len, "out of memory");
            return 4;
        }
        // 词的矩形：跨行时每行一个。
        ldomXRange word_range(word);
        LVArray<lvRect> rects;
        word_range.getSegmentRects(rects);
        const int rect_count = rects.length();
        // 本页范围之外的词丢掉：它们其实排在下一页。
        if (rect_count <= 0 || rects[0].top >= page_end) {
            std::free(item.text);
            std::free(item.start_xpointer);
            std::free(item.end_xpointer);
            continue;
        }
        item.rect_count = rect_count;
        item.rects = nullptr;
        if (rect_count > 0) {
            item.rects = static_cast<CrRect *>(
                    std::calloc(static_cast<size_t>(rect_count), sizeof(CrRect)));
            if (item.rects == nullptr) {
                std::free(item.text);
                std::free(item.start_xpointer);
                std::free(item.end_xpointer);
                free_words(items, filled);
                set_error(err, err_len, "out of memory");
                return 4;
            }
            for (int r = 0; r < rect_count; r++) {
                item.rects[r] = to_page_rect(rects[r], page_start, doc->margin_left,
                                            doc->margin_top);
            }
        }
        items[filled++] = item;
    }
    out->words = items;
    out->count = filled;
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" void cr_words_free(CrWordList *list) {
    CR_GUARD_BEGIN
    if (list == nullptr) {
        return;
    }
    free_words(list->words, list->count);
    list->words = nullptr;
    list->count = 0;
    CR_GUARD_END_VOID()
}

extern "C" int cr_doc_page_links(const CrDoc *doc, int page, CrLinkList *out, char *err,
                                 size_t err_len) {
    CR_GUARD_BEGIN
    if (doc == nullptr || doc->view == nullptr) {
        set_error(err, err_len, "document not opened");
        return 1;
    }
    if (out == nullptr) {
        set_error(err, err_len, "null output list pointer");
        return 2;
    }
    out->count = 0;
    out->links = nullptr;
    const int pages = doc->view->getPageCount();
    if (page < 0 || page >= pages) {
        set_error(err, err_len, "page out of range");
        return 3;
    }
    // getCurrentPageLinks() 取的是「当前页」的链接，先翻到目标页。
    if (!doc->view->goToPage(page)) {
        set_error(err, err_len, "goToPage failed");
        return 4;
    }
    ldomXRangeList ranges;
    doc->view->getCurrentPageLinks(ranges);
    const int count = ranges.length();
    if (count <= 0) {
        return 0;
    }
    CrLink *items = static_cast<CrLink *>(std::calloc(static_cast<size_t>(count), sizeof(CrLink)));
    if (items == nullptr) {
        set_error(err, err_len, "out of memory");
        return 5;
    }
    const int page_start = page_flow_start(doc, page);
    const int page_end = page_flow_end(doc, page);
    int filled = 0;
    for (int i = 0; i < count; i++) {
        ldomXRange *link_range = ranges[i];
        if (link_range == nullptr) {
            continue;
        }
        lString32 href = link_range->getHRef();
        if (href.empty()) {
            continue;
        }
        CrLink item;
        item.href = dup_string(href);
        if (item.href == nullptr) {
            free_links(items, filled);
            set_error(err, err_len, "out of memory");
            return 5;
        }
        item.is_internal = 0;
        item.target_xpointer = nullptr;
        item.target_page = -1;
        // 链接的矩形：跨行时每行一个；includeImages=true 保证多看/微信读书等
        // 纯图片脚注图标（<a class="duokan-footnote"><img .../></a>）也能命中点击热区。
        LVArray<lvRect> rects;
        link_range->getSegmentRects(rects, true);
        const int rect_count = rects.length();
        // 本页范围之外的链接丢掉：它们其实排在下一页。
        if (rect_count <= 0 || rects[0].top >= page_end) {
            std::free(item.href);
            continue;
        }
        item.rect_count = rect_count;
        item.rects = nullptr;
        if (rect_count > 0) {
            item.rects = static_cast<CrRect *>(
                    std::calloc(static_cast<size_t>(rect_count), sizeof(CrRect)));
            if (item.rects == nullptr) {
                std::free(item.href);
                free_links(items, filled);
                set_error(err, err_len, "out of memory");
                return 5;
            }
            for (int r = 0; r < rect_count; r++) {
                item.rects[r] = to_page_rect(rects[r], page_start, doc->margin_left,
                                            doc->margin_top);
            }
        }
        // 目标：'#' 开头的是书内锚点，其余是书外 URL。
        // EPUB 的跨文件链接在解析时已经被改写成书内锚点（见
        // ldomdocumentfragmentwriter.cpp 的 convertHref），所以这里
        // 只需要处理这两种。
        if (href.length() > 1 && href[0] == '#') {
            item.is_internal = 1;
            ldomDocument *dom = doc->view->getDocument();
            ldomNode *dest =
                    dom == nullptr ? nullptr : dom->getElementById(href.substr(1).c_str());
            if (dest != nullptr) {
                ldomXPointer ptr(dest, 0);
                item.target_xpointer = dup_string(ptr.toString());
                if (item.target_xpointer == nullptr) {
                    item.target_page = -1;
                } else {
                    item.target_page = doc->view->getBookmarkPage(ptr);
                }
            }
        }
        items[filled++] = item;
    }
    out->links = items;
    out->count = filled;
    return 0;
    CR_GUARD_END(err, err_len, kErrorException)
}

extern "C" void cr_links_free(CrLinkList *list) {
    CR_GUARD_BEGIN
    if (list == nullptr) {
        return;
    }
    free_links(list->links, list->count);
    list->links = nullptr;
    list->count = 0;
    CR_GUARD_END_VOID()
}
