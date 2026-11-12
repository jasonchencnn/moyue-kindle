# 墨阅同步契约测试用例（Sync Fixtures v2）

本目录包含墨阅原生 Kindle 客户端与墨阅云端（Readest 服务端）之间同步协议的通用测试用例（Fixtures）。
契约基准版本：**`docs/sync-contract.md` v2**。

## 适用对象
- **Kindle 客户端（Rust）**：`core::sync` 模块（任务 SYNC-003）的单元测试与集成测试；
- **墨阅 Web / App 端（TypeScript）**：`apps/readest-app` 侧的同步合并逻辑测试（跨端复用）。

---

## 字段规范
所有 JSON 数据结构严格遵守 `apps/readest-app/src/types/records.ts` 的模型定义：
- **`DBBook`**：包含 `book_hash`, `title`, `author`, `format`, `progress`, `reading_status`, `reading_status_updated_at`, `cover_hash`, `cover_updated_at`, `metadata`, `metadata_updated_at`, `group_id`, `group_name`, `group_updated_at`, `updated_at`, `deleted_at` 等。
- **`DBBookConfig`**：包含 `book_hash`, `location`, `xpointer`, `progress` (JSON 字符串), `updated_at`, `deleted_at` 等。
- **`DBBookNote`**：包含 `book_hash`, `id`, `type`, `cfi`, `xpointer0`, `xpointer1`, `page`, `text`, `style`, `color`, `note`, `updated_at`, `deleted_at` 等。

---

## 用例清单

| 文件名 | 测试场景 | 核心验证规则 |
|---|---|---|
| `lww_basic.json` | 基础整行 LWW | 比较 `updated_at`：本地晚于远端则本地胜出；远端晚于本地则远端胜出。 |
| `lww_ties.json` | 完全同刻平局处理 | 1. 整行平局服务端（remote）胜；<br>2. `reading_status` 平局客户端胜；<br>3. `metadata` 平局跟随整行胜者；<br>4. `group` 平局有分组的一侧胜出。 |
| `tombstones.json` | 墓碑机制与复活 | 1. `deleted_at` 参与整行胜负判定（`clientDeletedAt > serverDeletedAt \|\| clientUpdatedAt > serverUpdatedAt`）；<br>2. 相同毫秒竞争时墓碑优先胜出，防止笔记被意外复活；<br>3. 带有更新时间戳的合法导入允许复活已删除书籍。 |
| `field_level_lww.json` | 字段级独立 LWW 合并 | 1. 翻页刷新整行 `updated_at` 不覆盖对端更新的 `reading_status`；<br>2. 独立封面时间戳 `cover_updated_at`；<br>3. `metadata` 空值保全（winner 的 null 不覆盖 loser 的有效简介）；<br>4. 分组独立时间戳 `group_updated_at`。 |
| `notes_merge.json` | 笔记按 id 去重与合并 | 1. 不同 id 的笔记取两端并集；<br>2. 相同 id 比较 `max(updated_at, deleted_at)`；<br>3. 删除墓碑传播并覆盖旧笔记。 |
| `pagination_boundary.json` | 分页拉取与边界完整性 | 1. `type=books` 分页拉取时，末尾同毫秒记录自动补齐，防止 `> cursor` 漏拉；<br>2. 首拉（`since < 1000`）空书库时服务端注入 Dummy Book。 |

---

## 使用方式（Rust 单元测试示例）

```rust
use serde_json::Value;

#[test]
fn test_lww_basic_fixtures() {
    let fixture_str = include_str!("../../../tests/fixtures/sync/lww_basic.json");
    let fixture: Value = serde_json::from_str(fixture_str).unwrap();
    assert_eq!(fixture["contract_version"], "v2");
    
    for case in fixture["cases"].as_array().unwrap() {
        println!("Running case: {}", case["name"].as_str().unwrap());
        // 调用 core::sync::merge_row(local, remote) 并比对 case["expected"]
    }
}
```
