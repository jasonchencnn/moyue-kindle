/* 墨阅 · crengine-ng C 接口 shim
 *
 * 把 crengine-ng 的 C++ API（LVLDocView / LVGrayDrawBuf）包成极小的
 * extern "C" 接口，供 Rust 侧 FFI 调用。
 *
 * 内存约定
 * --------
 * - 只读的入参字符串由调用方准备（Rust 侧用 CString），shim 不保存。
 * - 出参有两种：写进调用方给的缓冲区的（如 cr_doc_title），和 shim 新分配
 *   的列表（如 cr_doc_toc、cr_doc_page_words、cr_doc_page_links）。
 * - shim 新分配的列表一律带一个对应的 cr_*_free，由调用方释放整个列表；
 *   列表里的字符串与数组由 free 一并回收，调用方不要单独 free 字段。
 * - Rust 侧的做法是：拷进自己的类型 → 立刻调 cr_*_free。两边都不持有
 *   对方的内存，不会有泄漏，也不会出现悬垂指针。
 *
 * 许可证：与本仓库一致，AGPL-3.0-or-later。
 * crengine-ng 本身是 GPLv2+，与 AGPL 兼容。
 */

#ifndef INKREAD_CRENGINE_SHIM_H
#define INKREAD_CRENGINE_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* 已打开的文档。 */
typedef struct CrDoc CrDoc;

/* 排版参数。
 *
 * 页边距单位是设备像素，四边可以不同。
 * font_face 为 NULL 或空串时用引擎默认字体；
 * font_size <= 0 时用引擎默认字号；
 * line_spacing_percent <= 0 时用引擎默认行高（100 表示单倍行距）。
 * css 为 NULL 或空串时不注入样式表。
 * embedded_styles 非 0 表示保留书内自带样式（<style>、外链 CSS、行内 style），
 * 0 表示只认注入的 css。对应 crengine 的 PROP_EMBEDDED_STYLES。
 * embedded_fonts 非 0 表示精排模式下允许书内 @font-face 自带字体生效，
 * 0 表示忽略书内嵌字体、一律使用阅读器选定的字体。对应 crengine 的
 * PROP_EMBEDDED_FONTS（仅当 fine_typesetting 与 embedded_styles 均为非 0 时生效）。
 * fine_typesetting 非 0 表示启用现代精排：Web 盒模型与完整 block rendering
 * 标志、HarfBuzz kerning/shaping、悬挂标点、按面板 DPI 缩放 CSS 绝对单位。
 * 0 表示退回 crengine 的传统行为。DOM 版本不随开关切换，
 * 两种状态都是 gDOMVersionCurrent：切换 DOM 版本会让已保存的 XPointer
 * 阅读进度/高亮错位。
 * font_weight 是正文字重（CSS font-weight 的 100..900）：超出范围时钳到
 * 边界，<= 0 时用 400。对应 crengine 的 PROP_FONT_BASE_WEIGHT。
 * fallback_font_faces 是回退字体的 face 名列表，分号隔开（crengine 自己的
 * 写法，如 "Noto Sans SC"）。主字体里没有的字形交给它们：正文用 Literata，
 * 汉字交给中文字体。为 NULL 或空串时不用回退字体。
 * 回退字体在引擎里是进程级的设置（字体管理器是全局对象）：渲染前每本书会
 * 按自己的排版参数再设一次。 */
typedef struct {
    int margin_left;
    int margin_top;
    int margin_right;
    int margin_bottom;
    const char *font_face;
    int font_size;
    int line_spacing_percent;
    const char *css;
    int embedded_styles;
    int embedded_fonts;
    const char *fallback_font_faces;
    int fine_typesetting;
    int font_weight;
    int dpi;
} CrLayout;

/* 像素矩形。
 *
 * 坐标相对页面左上角，与 cr_doc_render_page 画出来的画面同一坐标系：
 * x 向右、y 向下，右边与下边是开区间（width = right - left）。
 */
typedef struct {
    int x;
    int y;
    int width;
    int height;
} CrRect;

/* 目录项。
 *
 * level 是层级，0 起：顶层目录项是 0，每往里嵌套一层加 1。
 * page 是所在页码，0 起，按当前排版算。
 * title 与 xpointer 是 UTF-8、以 NUL 结尾的字符串，由 cr_toc_free 释放。
 *
 * 定位不到页码的目录项不会出现在列表里（连带它的子项）：这一节在书里
 * 就是定位不到的。 */
typedef struct {
    char *title;
    int level;
    char *xpointer;
    int page;
} CrTocItem;

/* 目录列表。
 *
 * items 由 shim 分配，调用方必须用 cr_toc_free 释放。 */
typedef struct {
    int count;
    CrTocItem *items;
} CrTocList;

/* 页内的一个词。
 *
 * text 是词本身（UTF-8）。断词规则由 crengine 决定：西文按空格与标点断词，
 * 汉字一个字算一个词。
 * start_xpointer / end_xpointer 是词首与词尾的位置（UTF-8），可以拿去在
 * 别的排版下重新定位。
 * rects 是这个词在页面上的矩形：通常只有一个，跨行时每行一个。
 * 所有指针由 cr_words_free 释放。 */
typedef struct {
    char *text;
    char *start_xpointer;
    char *end_xpointer;
    int rect_count;
    CrRect *rects;
} CrWord;

/* 页内的词列表。
 *
 * words 由 shim 分配，调用方必须用 cr_words_free 释放。 */
typedef struct {
    int count;
    CrWord *words;
} CrWordList;

/* 页内的一个链接。
 *
 * href 是链接目标原文（UTF-8）。
 * is_internal 非 0 表示是书内锚点：target_xpointer 是目标位置，
 * target_page 是目标所在页码（0 起）。
 * is_internal 为 0 表示是书外链接：target_xpointer 为 NULL，
 * target_page 为 -1，href 就是外部 URL。
 * 所有指针由 cr_links_free 释放。 */
typedef struct {
    char *href;
    int is_internal;
    char *target_xpointer;
    int target_page;
    int rect_count;
    CrRect *rects;
} CrLink;

/* 页内的链接列表。
 *
 * links 由 shim 分配，调用方必须用 cr_links_free 释放。 */
typedef struct {
    int count;
    CrLink *links;
} CrLinkList;

/* 初始化字体管理器并注册字体文件。
 *
 * font_paths 是 UTF-8 路径数组，font_count 是数组长度。至少成功注册一个
 * 字体时返回 0，否则返回非 0（此时 err 里是原因）。 */
int cr_fonts_init(const char *const *font_paths, int font_count, char *err, size_t err_len);

/* 已注册字体（face）数量。 */
int cr_fonts_face_count(void);

/* 第 index 个 face 的名称（UTF-8），越界或未初始化返回 NULL。
 * 返回的指针在下次 cr_fonts_init 之前一直有效。 */
const char *cr_fonts_face_name(int index);

/* 生效中的回退字体（face 名，分号隔开），写入 out（以 NUL 结尾，超长时截断）。
 *
 * 没有设置回退字体时 out 是空串。crengine 只认它找得到的 face 名：名单里
 * 认不出来的会被丢掉，所以这里返回的是真正生效的那一份。
 *
 * 成功返回 0，失败返回非 0。 */
int cr_fonts_fallback_faces(char *out, size_t out_len);

/* 打开文档缓存（进程级）。
 *
 * crengine 的文档缓存把解析好的 DOM（有时还有排版结果）存到磁盘：下次打开
 * 同一本书时直接读缓存，不用再解析一遍。整个进程只有一个缓存实例，
 * 再调一次会把上一个换掉。
 *
 * 排版结果能不能一起复用由 crengine 自己判断（渲染上下文要对得上）：
 * 对不上就只省下解析，排版照旧重做。
 *
 * dir 是缓存目录（不存在会创建），max_size 是容量上限（字节）：超了会从最
 * 旧的缓存文件开始删。目录建不出来、索引写不进去时返回非 0：缓存没开，
 * 引擎照常排版，不会崩。
 *
 * 不调这个函数时，引擎行为和没有缓存时完全一样。
 *
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_cache_init(const char *dir, unsigned long max_size, char *err, size_t err_len);

/* 缓存是否已经打开（cr_cache_init 成功过）。没开返回 0。 */
int cr_cache_enabled(void);

/* 清掉缓存目录里的缓存文件。
 *
 * 缓存没开时返回非 0。成功返回 0。 */
int cr_cache_clear(char *err, size_t err_len);

/* 这一份文档是不是从缓存里读出来的。没开缓存时返回 0。 */
int cr_doc_from_cache(const CrDoc *doc);

/* 这一份文档在本次打开或修改排版时是否真正执行了排版/重排。
 *
 * 命中排版缓存时返回 0（直接复用缓存里的分页和排版结果，免重排）；
 * 首次打开、无缓存、或排版参数改变触发重排时返回 1。
 * 文档指针无效时返回 0。 */
int cr_doc_reformatted(const CrDoc *doc);

/* 查询当前文档是否支持局部重排。
 *
 * 需满足：启用文档缓存（ldomDocCache），排版过至少一次，且文档包含 2 个及以上章节（DocFragment）。
 * 支持返回 1，不支持返回 0。文档无效时返回 0。 */
int cr_doc_can_partial_rerender(const CrDoc *doc);

/* 开启或关闭局部重排模式。
 *
 * enable 非 0 开启，0 关闭。
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_enable_partial_rerender(CrDoc *doc, int enable, char *err, size_t err_len);

/* 当前文档是否开启了局部重排。
 *
 * 开启返回 1，未开启返回 0。文档无效时返回 0。 */
int cr_doc_is_partial_rerender_enabled(const CrDoc *doc);

/* 修改排版参数后，文档是否处于延迟重排状态（_rerendering_delayed）。
 *
 * 处于延迟重排状态返回 1，否则返回 0。文档无效时返回 0。 */
int cr_doc_is_rerender_delayed(const CrDoc *doc);

/* 排版进度回调。
 *
 * progress 取值 0..100。
 * 注意：回调在持有全局引擎锁的线程里同步执行，回调内部不能调用任何 crengine 接口。 */
typedef void (*cr_progress_cb)(int progress, void *user_data);

/* 局部重排后在后台补完全书排版（计算所有剩余章节的分页并校准总页数）。
 *
 * progress_cb 可为 NULL。
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_catchup_render(CrDoc *doc, cr_progress_cb progress_cb, void *user_data,
                          char *err, size_t err_len);

/* 设置或获取文档存储大小因子（cre_storage_size_factor）。
 *
 * 默认为 2.0f。增大因子可扩展文本节点、元素、矩形与样式数据的最大允许解压内存空间，
 * 防止超大书籍（如《紅樓夢》）在解析或排版时触发 storage reached max allowed uncompressed size 告警与频繁 compact/swap。
 * 对小书无额外内存开销（按需按 chunk 分配）。 */
void cr_storage_size_factor_set(float factor);
float cr_storage_size_factor_get(void);

/* 打开文档并按 width x height 排版。
 *
 * layout 不能为 NULL。progress_cb 可为 NULL（不上报进度）。
 * 失败返回 NULL，err 里是原因。
 *
 * 缓存开着时，排版结束后会把文档写进缓存（写不进去不算失败）。 */
CrDoc *cr_doc_open(const char *path, int width, int height, const CrLayout *layout,
                   cr_progress_cb progress_cb, void *user_data, char *err,
                   size_t err_len);

/* 关闭文档并释放资源。可传 NULL。 */
void cr_doc_close(CrDoc *doc);

/* 文档总页数；文档为空时返回 0。 */
int cr_doc_page_count(const CrDoc *doc);

/* 渲染第 page 页（0 起）到 8 位灰度缓冲。
 *
 * dst 是 row-major 的 width * height 字节，每字节一个像素，0 黑 255 白，
 * 与 inkread_platform::GrayBuffer 的内存布局一致。dst_len 必须不小于
 * width * height。
 *
 * dst_width / dst_height 是调用方缓冲的实际宽高，必须与文档的页面尺寸
 * 完全一致：像素总数相同但宽高对调时，按文档行宽写入会让画面整页错行。
 * 不一致时返回非 0，err 里是原因。
 *
 * invert_images 为非 0 时仅对图片区域执行反色（供夜间模式抵消全屏反色使用）。
 *
 * underline 是正文每行下方的引导线：0 不画、1 实线、2 点状虚线。
 * 引导线用引擎自己的行几何画在正文文字行下面，图片、页眉页脚不画。
 * underline_gray 是引导线的灰度（0 黑、255 白），越界时钳到 0..255。
 * underline_scope 是引导线的横向范围：0 只画文字宽度（TextOnly）、
 * 1 画满正文区一整行（FullLine，短段落下方与空白行槽也按行高网格补满）。
 *
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_render_page(CrDoc *doc, int page, uint8_t *dst, size_t dst_len, int dst_width,
                        int dst_height, int invert_images, int underline, int underline_gray,
                        int underline_scope, char *err, size_t err_len);

/* 换一套排版参数并重新排版。
 *
 * progress_cb 可为 NULL。
 * 重新排版后页数与页码都可能变。成功返回 0，失败返回非 0。 */
int cr_doc_set_layout(CrDoc *doc, const CrLayout *layout,
                      cr_progress_cb progress_cb, void *user_data,
                      char *err, size_t err_len);

/* 换一套排版参数，并按新的页面尺寸重新排版。
 *
 * width / height 是新的页面尺寸（像素）。文档不重开：DOM、目录、书签、
 * 进度都在，只是按新尺寸重新分页。
 *
 * 尺寸变了以后，排好版的分片全部失效（它们是按旧宽度排的），必须整本重排。
 * 这里会临时关掉局部重排、排完再恢复，免得新旧两种分页混在一起。
 *
 * 尺寸非法（<= 0、小于 80 或超过上限）时直接报错，文档一个字节都没动。
 * 中途抛异常或重排出不了页时，视口、输出缓冲、宽高和局部重排开关一起回滚；
 * 已经套上的排版参数保留（与 cr_doc_set_layout 失败时一致），调用方可以用
 * 有效参数重试。成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_set_viewport(CrDoc *doc, int width, int height, const CrLayout *layout,
                        cr_progress_cb progress_cb, void *user_data,
                        char *err, size_t err_len);

/* 取第 page 页（0 起）起始位置的 XPointer（UTF-8）。
 *
 * 写入 out（以 NUL 结尾，超长时截断）。成功返回 0，失败返回非 0。 */
int cr_doc_page_xpointer(const CrDoc *doc, int page, char *out, size_t out_len, char *err,
                         size_t err_len);

/* 按 XPointer（UTF-8）反查它落在第几页（0 起），结果写入 out_page。
 *
 * XPointer 格式对但指不到位置时（比如书换了版本、目录指向被删掉的章节）
 * 返回非 0，err 里是原因，out_page 不动：这种位置不能当成第 0 页。
 *
 * 成功返回 0，失败返回非 0。 */
int cr_doc_page_of_xpointer(const CrDoc *doc, const char *xpointer, int *out_page, char *err,
                            size_t err_len);

/* 书名 / 作者（UTF-8，写入 out，以 NUL 结尾，超长时截断）。
 *
 * 成功返回 0，取不到时 out 是空串。 */
int cr_doc_title(const CrDoc *doc, char *out, size_t out_len);
int cr_doc_author(const CrDoc *doc, char *out, size_t out_len);

/* 取目录（TOC），扁平列表，顺序与书里的目录一致。
 *
 * out 由调用方准备，shim 往里写 count 与 items。items 里的每个字符串都是
 * shim 新分配的 UTF-8 字符串，调用方必须用 cr_toc_free 释放整个列表，
 * 不能单独 free 里面的字段。
 *
 * 没有目录的书返回 count = 0、items = NULL，这不算错误。
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_toc(const CrDoc *doc, CrTocList *out, char *err, size_t err_len);

/* 释放 cr_doc_toc 填出来的目录列表。传 NULL 或空列表都可以。 */
void cr_toc_free(CrTocList *list);

/* 取第 page 页（0 起）里的词。
 *
 * 断词由 crengine 决定：西文按空格与标点断词，汉字一个字算一个词。
 * 每个词的矩形是相对页面左上角的像素坐标，与 cr_doc_render_page 同一坐标系。
 *
 * out 由调用方准备，shim 往里写 count 与 words。words 里的字符串与 rects
 * 都是 shim 新分配的，调用方必须用 cr_words_free 释放整个列表。
 *
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_page_words(const CrDoc *doc, int page, CrWordList *out, char *err, size_t err_len);

/* 释放 cr_doc_page_words 填出来的词列表。传 NULL 或空列表都可以。 */
void cr_words_free(CrWordList *list);

/* 取第 page 页（0 起）里的链接。
 *
 * 书内锚点链接会给出目标 XPointer 与目标页码；书外链接给出 URL 原文。
 * 每个链接的矩形是相对页面左上角的像素坐标，跨行时每行一个。
 *
 * out 由调用方准备，shim 往里写 count 与 links。links 里的字符串与 rects
 * 都是 shim 新分配的，调用方必须用 cr_links_free 释放整个列表。
 *
 * 成功返回 0，失败返回非 0，err 里是原因。 */
int cr_doc_page_links(const CrDoc *doc, int page, CrLinkList *out, char *err, size_t err_len);

/* 释放 cr_doc_page_links 填出来的链接列表。传 NULL 或空列表都可以。 */
void cr_links_free(CrLinkList *list);

#ifdef __cplusplus
}
#endif

#endif /* INKREAD_CRENGINE_SHIM_H */
