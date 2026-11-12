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
#include <lvstring.h>

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
    if (layout.line_spacing_percent > 0) {
        view->setDefaultInterlineSpace(layout.line_spacing_percent);
    }
    // 文档已经建好了，这里要重新打一次标记：换排版参数时可能改了开关。
    apply_embedded_styles(view, layout.embedded_styles != 0);
    // lvRect(left, top, right, bottom) 记的是页边距，不是页面矩形。
    view->setPageMargins(lvRect(layout.margin_left, layout.margin_top, layout.margin_right,
                                 layout.margin_bottom));
    // 回退字体要在渲染之前定下来：字形是渲染时现找的。
    apply_fallback_faces(layout.fallback_font_faces);
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
    if (width > kMaxPageDimension || height > kMaxPageDimension) {
        set_error(err, err_len, "page size exceeds limit");
        return nullptr;
    }
    if (layout == nullptr) {
        set_error(err, err_len, "missing layout");
        return nullptr;
    }
    if (fontMan == nullptr) {
        set_error(err, err_len, "font manager not initialized");
        return nullptr;
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
    doc->margin_left = layout->margin_left;
    doc->margin_top = layout->margin_top;
    doc->fallback_font_faces = fallback_faces_of(*layout);

    doc->callback.view = doc->view;
    doc->callback.reset(true, progress_cb, user_data);
    view->setCallback(&doc->callback);

    if (!view->LoadDocument(path)) {
        set_error(err, err_len, "LoadDocument failed");
        return nullptr;
    }

    // 段首全角空格是内容里的缩进标记，统一去掉；首行缩进由 CSS 控制。
    if (ldomDocument *dom = view->getDocument()) {
        strip_leading_ideographic_spaces(dom->getRootNode());
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
                                  int dst_height, int invert_images, char *err, size_t err_len) {
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
    // 回退字体是进程级的设置，换一本书就会改掉全局那份。渲染前按这一版书
    // 自己存的名单再设一次：不设回来的话，字形会拿别的书的回退字体排。
    apply_fallback_faces(doc->fallback_font_faces.c_str());
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
    doc->margin_left = layout->margin_left;
    doc->margin_top = layout->margin_top;
    doc->fallback_font_faces = fallback_faces_of(*layout);
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
        // 链接的矩形：跨行时每行一个。
        LVArray<lvRect> rects;
        link_range->getSegmentRects(rects);
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
