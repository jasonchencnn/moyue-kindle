// SPDX-License-Identifier: AGPL-3.0-or-later
//! ARM performance benchmark for IME candidate updates and 500-book search index.
//!
//! Measures:
//! 1. Keystroke to candidate update latency (typing pinyin, candidate lookup, pagination).
//! 2. Opening search page & building SearchIndex for 500 books.
//! 3. Search query matching across 500 books for uncommitted pinyin, initials, and simplified/traditional characters.

use aristotle_ui::keyboard::pinyin::{candidates, lookup};
use aristotle_ui::keyboard::state::{InputMode, KeyboardState};
use aristotle_ui::screens::library::Book;
use aristotle_ui::screens::search::SearchIndex;
use std::time::Instant;

fn generate_500_books() -> Vec<Book> {
    let titles = [
        "紅樓夢",
        "西游记",
        "三国演义",
        "水浒传",
        "呐喊",
        "彷徨",
        "朝花夕拾",
        "围城",
        "骆驼祥子",
        "四世同堂",
        "边城",
        "繁花",
        "平凡的世界",
        "白鹿原",
        "活着",
        "许三观卖血记",
        "射雕英雄传",
        "神雕侠侣",
        "天龙八部",
        "笑傲江湖",
        "倚天屠龙记",
        "鹿鼎记",
        "雪山飞狐",
        "书剑恩仇录",
        "碧血剑",
        "飞狐外传",
        "白马啸西风",
        "鸳鸯刀",
        "连城诀",
        "越女剑",
        "傾城之戀",
        "金鎖記",
        "半生緣",
        "紅玫瑰與白玫瑰",
        "啼笑因缘",
        "家",
        "春",
        "秋",
        "子夜",
        "林家铺子",
        "雷雨",
        "日出",
        "北京人",
        "茶馆",
        "原野",
        "狂人日记",
        "阿Q正传",
        "孔乙己",
        "药",
        "故乡",
        "War and Peace",
        "Pride and Prejudice",
        "Moby Dick",
        "The Great Gatsby",
        "Crime and Punishment",
        "The Brothers Karamazov",
        "Anna Karenina",
        "Les Misérables",
        "The Count of Monte Cristo",
        "The Picture of Dorian Gray",
    ];

    let authors = [
        "曹雪芹",
        "吴承恩",
        "罗贯中",
        "施耐庵",
        "鲁迅",
        "钱钟书",
        "老舍",
        "沈从文",
        "金宇澄",
        "路遥",
        "陈忠实",
        "余华",
        "金庸",
        "张爱玲",
        "张恨水",
        "巴金",
        "茅盾",
        "曹禺",
        "Leo Tolstoy",
        "Jane Austen",
        "Herman Melville",
        "F. Scott Fitzgerald",
        "Fyodor Dostoevsky",
        "Victor Hugo",
        "Alexandre Dumas",
        "Oscar Wilde",
    ];

    let mut books = Vec::with_capacity(500);
    for i in 0..500 {
        let title = titles[i % titles.len()];
        let author = authors[i % authors.len()];
        let title_unique = if i >= titles.len() {
            format!("{title} · 第{}部", (i / titles.len()) + 1)
        } else {
            title.to_string()
        };
        books.push(Book::new(&title_unique, author, (i as f32 % 100.0) / 100.0));
    }
    books
}

fn bench_keystroke_to_candidates() {
    println!("=== 1. Keystroke to Candidate Update Benchmark ===");

    // Warmup
    for _ in 0..100 {
        let _ = lookup("hong");
        let _ = candidates("ho");
    }

    // Benchmark exact syllable lookup ("hong")
    let iters = 10_000;
    let t0 = Instant::now();
    for _ in 0..iters {
        let _ = lookup("hong");
    }
    let dur_lookup = t0.elapsed();
    let per_lookup_ns = dur_lookup.as_nanos() as f64 / iters as f64;
    println!("Exact syllable lookup (lookup('hong')): {per_lookup_ns:.2} ns/op (total {dur_lookup:?} for {iters} iters)");

    // Benchmark prefix candidate retrieval ("ho")
    let t0 = Instant::now();
    for _ in 0..iters {
        let _ = candidates("ho");
    }
    let dur_prefix = t0.elapsed();
    let per_prefix_ns = dur_prefix.as_nanos() as f64 / iters as f64;
    println!("Prefix candidates retrieval (candidates('ho')): {per_prefix_ns:.2} ns/op (total {dur_prefix:?} for {iters} iters)");

    // Benchmark full keystroke event in KeyboardState: typing 'h' -> 'o' -> 'n' -> 'g'
    let state_iters = 2_000;
    let t0 = Instant::now();
    for _ in 0..state_iters {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::Chinese);
        state.press_key('h');
        state.press_key('o');
        state.press_key('n');
        state.press_key('g');
        let _ = state.current_candidates();
        state.next_cand_page();
        let _ = state.current_candidates();
        state.pick_candidate(0);
    }
    let dur_state = t0.elapsed();
    let per_keystroke_us = (dur_state.as_micros() as f64) / (state_iters * 4) as f64;
    println!("Full IME state keystroke update (typing 4 keys + cand + pick): {per_keystroke_us:.2} µs/keystroke (total {dur_state:?})");
}

fn bench_index_build_and_search() {
    println!("\n=== 2. 500-Book SearchIndex Build & Query Benchmark ===");

    let books = generate_500_books();
    assert_eq!(books.len(), 500);

    // Warmup
    for _ in 0..5 {
        let _ = SearchIndex::build(&books);
    }

    // Benchmark index build for 500 books
    let build_iters = 100;
    let t0 = Instant::now();
    for _ in 0..build_iters {
        let _ = SearchIndex::build(&books);
    }
    let dur_build = t0.elapsed();
    let per_build_ms = (dur_build.as_secs_f64() * 1000.0) / build_iters as f64;
    println!("SearchIndex::build(500 books): {per_build_ms:.3} ms/build (total {dur_build:?} for {build_iters} builds)");

    let index = SearchIndex::build(&books);

    // Benchmark search queries across 500 books
    let queries = [
        ("hong", "uncommitted full pinyin prefix"),
        ("hlm", "pinyin initials"),
        ("honglou", "uncommitted 2-char pinyin"),
        ("hongl", "syllable + prefix initial"),
        ("红", "simplified candidate matching traditional book"),
        ("紅", "traditional candidate matching traditional book"),
        ("cxq", "author pinyin initials"),
        ("cao", "author pinyin prefix"),
        ("tolstoy", "latin author substring"),
    ];

    println!("\n=== 3. Search Matching Latency (500 Books) ===");
    for (q, desc) in queries {
        let iters = 1_000;
        let t0 = Instant::now();
        let mut count = 0;
        for _ in 0..iters {
            let res = index.search(q);
            count = res.len();
        }
        let dur = t0.elapsed();
        let per_query_us = (dur.as_micros() as f64) / iters as f64;
        println!("query '{q}' ({desc}): {per_query_us:.2} µs/query (hits: {count})");
    }
}

fn main() {
    println!("--- InkRead / Aristotle IME & Search Benchmark ---");
    bench_keystroke_to_candidates();
    bench_index_build_and_search();
    println!("--- Benchmark Completed ---");
}
