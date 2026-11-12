use inkread_platform::{
    check_present, merge_rects, Display, Error, GrayBuffer, Input, InputEvent, KeyCode, Power,
    Rect, RefreshMode, RefreshRequest, Result, TouchPhase, BLACK, WHITE,
};

// ============================================================================
// 1. 0x0 缓冲区与零尺寸边界测试 (Zero-sized buffer & zero-size operations)
// ============================================================================

#[test]
fn zero_sized_buffer_properties_and_dimensions() {
    let buf0 = GrayBuffer::new(0, 0);
    assert_eq!(buf0.width(), 0);
    assert_eq!(buf0.height(), 0);
    assert!(buf0.is_empty());
    assert!(buf0.pixels().is_empty());

    let buf_w0 = GrayBuffer::new(0, 10);
    assert_eq!(buf_w0.width(), 0);
    assert_eq!(buf_w0.height(), 10);
    assert!(buf_w0.is_empty());
    assert!(buf_w0.pixels().is_empty());

    let buf_h0 = GrayBuffer::new(10, 0);
    assert_eq!(buf_h0.width(), 10);
    assert_eq!(buf_h0.height(), 0);
    assert!(buf_h0.is_empty());
    assert!(buf_h0.pixels().is_empty());
}

#[test]
fn zero_sized_buffer_operations_are_safe_noops() {
    let mut buf = GrayBuffer::new(0, 0);

    // Reading or writing pixels must gracefully fail without panic.
    assert_eq!(buf.get_pixel(0, 0), None);
    assert_eq!(buf.get_pixel(-1, -1), None);
    assert_eq!(buf.get_pixel(1, 1), None);
    assert!(!buf.set_pixel(0, 0, BLACK));

    // fill and fill_rect must not panic.
    buf.fill(BLACK);
    assert!(buf.pixels().is_empty());

    buf.fill_rect(Rect::new(0, 0, 10, 10), BLACK);
    buf.fill_rect(Rect::new(-5, -5, 10, 10), BLACK);
    assert!(buf.pixels().is_empty());
}

#[test]
fn zero_sized_buffer_blit_scenarios() {
    let mut zero_buf = GrayBuffer::new(0, 0);
    let mut normal_buf = GrayBuffer::new(10, 10);

    // Blit from zero-sized buffer to normal buffer: should be a no-op.
    normal_buf.blit(&zero_buf, Rect::new(0, 0, 0, 0), 0, 0);
    assert!(normal_buf.pixels().iter().all(|&p| p == WHITE));

    // Blit from normal buffer to zero-sized buffer: should be a no-op.
    zero_buf.blit(&normal_buf, Rect::new(0, 0, 5, 5), 0, 0);
    assert!(zero_buf.pixels().is_empty());

    // Blit between two zero-sized buffers: should be a no-op.
    let zero_src = GrayBuffer::new(0, 0);
    zero_buf.blit(&zero_src, Rect::new(0, 0, 10, 10), 0, 0);
    assert!(zero_buf.pixels().is_empty());
}

// ============================================================================
// 2. fill_rect 负坐标与边界裁剪测试 (fill_rect negative coords & edge clipping)
// ============================================================================

#[test]
fn fill_rect_negative_origin_partial_overlap() {
    let mut buf = GrayBuffer::new(8, 8);

    // Rectangle begins at (-4, -4) with size 8x8.
    // Overlapping portion in the buffer must be (0..4, 0..4).
    buf.fill_rect(Rect::new(-4, -4, 8, 8), 100);

    for y in 0..8 {
        for x in 0..8 {
            let expected = if x < 4 && y < 4 { 100 } else { WHITE };
            assert_eq!(
                buf.get_pixel(x, y),
                Some(expected),
                "Mismatch at ({x}, {y})"
            );
        }
    }
}

#[test]
fn fill_rect_completely_negative_coordinates() {
    let mut buf = GrayBuffer::new(8, 8);

    // Fully in negative region.
    buf.fill_rect(Rect::new(-20, -20, 10, 10), BLACK);
    buf.fill_rect(Rect::new(-10, 0, 5, 5), BLACK);
    buf.fill_rect(Rect::new(0, -10, 5, 5), BLACK);

    assert!(buf.pixels().iter().all(|&p| p == WHITE));
}

#[test]
fn fill_rect_negative_origin_spanning_across_entire_buffer() {
    let mut buf = GrayBuffer::new(8, 8);

    // Starts outside on negative side and ends outside on positive side.
    buf.fill_rect(Rect::new(-10, -10, 50, 50), 42);

    assert!(
        buf.pixels().iter().all(|&p| p == 42),
        "Entire buffer should be filled when rect encloses it"
    );
}

// ============================================================================
// 3. i32::MAX / i32::MIN 极端坐标与极大宽高 (Extreme coords & huge dimensions)
// ============================================================================

#[test]
fn pixel_access_at_i32_limits() {
    let mut buf = GrayBuffer::new(10, 10);

    assert_eq!(buf.get_pixel(i32::MIN, 0), None);
    assert_eq!(buf.get_pixel(i32::MAX, 0), None);
    assert_eq!(buf.get_pixel(0, i32::MIN), None);
    assert_eq!(buf.get_pixel(0, i32::MAX), None);
    assert_eq!(buf.get_pixel(i32::MIN, i32::MIN), None);
    assert_eq!(buf.get_pixel(i32::MAX, i32::MAX), None);

    assert!(!buf.set_pixel(i32::MIN, 0, BLACK));
    assert!(!buf.set_pixel(i32::MAX, 0, BLACK));
    assert!(!buf.set_pixel(0, i32::MIN, BLACK));
    assert!(!buf.set_pixel(0, i32::MAX, BLACK));
    assert!(!buf.set_pixel(i32::MIN, i32::MIN, BLACK));
    assert!(!buf.set_pixel(i32::MAX, i32::MAX, BLACK));

    assert!(buf.pixels().iter().all(|&p| p == WHITE));
}

#[test]
fn fill_rect_at_i32_limits_does_not_panic() {
    let mut buf = GrayBuffer::new(8, 8);

    // Rectangles at i32::MIN / i32::MAX.
    buf.fill_rect(Rect::new(i32::MIN, i32::MIN, 10, 10), BLACK);
    buf.fill_rect(Rect::new(i32::MAX, i32::MAX, 10, 10), BLACK);
    buf.fill_rect(Rect::new(i32::MIN, 0, 10, 10), BLACK);
    buf.fill_rect(Rect::new(0, i32::MIN, 10, 10), BLACK);
    buf.fill_rect(Rect::new(i32::MAX, 0, 10, 10), BLACK);
    buf.fill_rect(Rect::new(0, i32::MAX, 10, 10), BLACK);

    // Huge width/height starting at i32::MIN (span [-2147483648, -1]).
    buf.fill_rect(Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX), BLACK);

    assert!(
        buf.pixels().iter().all(|&p| p == WHITE),
        "Buffer should remain unchanged for out-of-bounds rects"
    );
}

#[test]
fn fill_rect_with_huge_dimensions_clipped_safely() {
    let mut buf = GrayBuffer::new(8, 8);

    // Huge dimensions starting inside buffer.
    buf.fill_rect(Rect::new(4, 4, i32::MAX, i32::MAX), 55);

    for y in 0..8 {
        for x in 0..8 {
            let expected = if x >= 4 && y >= 4 { 55 } else { WHITE };
            assert_eq!(
                buf.get_pixel(x, y),
                Some(expected),
                "Mismatch at ({x}, {y})"
            );
        }
    }
}

// ============================================================================
// 4. blit 源与目标各种越界情况 (blit source & destination out-of-bounds)
// ============================================================================

#[test]
fn blit_both_source_and_destination_completely_out_of_bounds() {
    let src = GrayBuffer::filled(4, 4, BLACK);
    let mut dst = GrayBuffer::new(8, 8);

    // Source rect is completely outside src, dest is completely outside dst.
    dst.blit(&src, Rect::new(-100, -100, 4, 4), 100, 100);
    assert!(dst.pixels().iter().all(|&p| p == WHITE));

    // Using extreme coordinates.
    dst.blit(
        &src,
        Rect::new(i32::MIN, i32::MIN, 4, 4),
        i32::MAX,
        i32::MAX,
    );
    assert!(dst.pixels().iter().all(|&p| p == WHITE));

    dst.blit(
        &src,
        Rect::new(i32::MAX, i32::MAX, 4, 4),
        i32::MIN,
        i32::MIN,
    );
    assert!(dst.pixels().iter().all(|&p| p == WHITE));
}

#[test]
fn blit_source_partial_and_destination_partial_simultaneously() {
    let mut src = GrayBuffer::filled(4, 4, 10);
    src.set_pixel(0, 0, 99); // top-left
    src.set_pixel(1, 1, 88);

    let mut dst = GrayBuffer::new(4, 4);

    // src_rect = (-1, -1, 4, 4) clips src to (0, 0, 3, 3).
    // dst coordinates = (-1, -1).
    // The relative offset from src_rect(-1, -1) to src_area(0, 0) is (+1, +1).
    // So dest rect starts at (-1 + 1, -1 + 1) = (0, 0) with size (3, 3).
    // All 3x3 clipped pixels land inside dst at (0, 0) through (2, 2).
    dst.blit(&src, Rect::new(-1, -1, 4, 4), -1, -1);

    assert_eq!(dst.get_pixel(0, 0), Some(99));
    assert_eq!(dst.get_pixel(1, 1), Some(88));
    assert_eq!(dst.get_pixel(0, 1), Some(10));
    assert_eq!(dst.get_pixel(2, 2), Some(10));
    // Pixel (3, 3) must be untouched (WHITE).
    assert_eq!(dst.get_pixel(3, 3), Some(WHITE));
}

#[test]
fn blit_with_extreme_coordinates_no_panic() {
    let src = GrayBuffer::filled(4, 4, 70);
    let mut dst = GrayBuffer::new(4, 4);

    // Various extreme combinations.
    dst.blit(&src, Rect::new(0, 0, 4, 4), i32::MIN, 0);
    dst.blit(&src, Rect::new(0, 0, 4, 4), i32::MAX, 0);
    dst.blit(&src, Rect::new(0, 0, 4, 4), 0, i32::MIN);
    dst.blit(&src, Rect::new(0, 0, 4, 4), 0, i32::MAX);

    dst.blit(&src, Rect::new(i32::MIN, 0, 4, 4), 0, 0);
    dst.blit(&src, Rect::new(i32::MAX, 0, 4, 4), 0, 0);
    dst.blit(&src, Rect::new(0, i32::MIN, 4, 4), 0, 0);
    dst.blit(&src, Rect::new(0, i32::MAX, 4, 4), 0, 0);

    assert!(dst.pixels().iter().all(|&p| p == WHITE));
}

// ============================================================================
// 5. merge_rects 空输入/重叠/相邻/包含 (merge_rects full matrix)
// ============================================================================

#[test]
fn merge_rects_empty_inputs_matrix() {
    assert_eq!(merge_rects(&[]), None);
    assert_eq!(merge_rects(&[Rect::default()]), None);
    assert_eq!(
        merge_rects(&[
            Rect::new(0, 0, 0, 0),
            Rect::new(10, 10, 0, 5),
            Rect::new(5, 5, 5, 0),
            Rect::new(-1, -1, -5, -5),
        ]),
        None
    );
}

#[test]
fn merge_rects_skips_empty_among_valid_rects() {
    let r1 = Rect::new(5, 5, 10, 10);
    let r2 = Rect::new(20, 20, 5, 5);
    let empty1 = Rect::default();
    let empty2 = Rect::new(0, 0, 0, 100);

    let result = merge_rects(&[empty1, r1, empty2, r2]).expect("merges valid rects");
    assert_eq!(result, Rect::new(5, 5, 20, 20));
}

#[test]
fn merge_rects_overlapping_multiple() {
    let r1 = Rect::new(0, 0, 10, 10);
    let r2 = Rect::new(5, 5, 10, 10);
    let r3 = Rect::new(8, 8, 12, 12);

    let merged = merge_rects(&[r1, r2, r3]).expect("merges");
    assert_eq!(merged, Rect::new(0, 0, 20, 20));
    assert!(merged.contains(&r1));
    assert!(merged.contains(&r2));
    assert!(merged.contains(&r3));
}

#[test]
fn merge_rects_adjacent_cases() {
    // Horizontally adjacent (sharing boundary at x = 10).
    let left = Rect::new(0, 0, 10, 10);
    let right = Rect::new(10, 0, 10, 10);
    assert_eq!(merge_rects(&[left, right]), Some(Rect::new(0, 0, 20, 10)));

    // Vertically adjacent (sharing boundary at y = 10).
    let top = Rect::new(0, 0, 10, 10);
    let bottom = Rect::new(0, 10, 10, 10);
    assert_eq!(merge_rects(&[top, bottom]), Some(Rect::new(0, 0, 10, 20)));

    // Diagonally adjacent (corner-to-corner touching at (10, 10)).
    let diag1 = Rect::new(0, 0, 10, 10);
    let diag2 = Rect::new(10, 10, 10, 10);
    assert_eq!(merge_rects(&[diag1, diag2]), Some(Rect::new(0, 0, 20, 20)));
}

#[test]
fn merge_rects_contained_cases() {
    let outer = Rect::new(0, 0, 50, 50);
    let inner = Rect::new(10, 10, 20, 20);
    let shared_top_left = Rect::new(0, 0, 15, 15);
    let shared_bottom_right = Rect::new(25, 25, 25, 25);

    assert_eq!(merge_rects(&[outer, inner]), Some(outer));
    assert_eq!(merge_rects(&[outer, shared_top_left]), Some(outer));
    assert_eq!(merge_rects(&[outer, shared_bottom_right]), Some(outer));
    assert_eq!(merge_rects(&[inner, outer]), Some(outer));
    assert_eq!(merge_rects(&[outer, outer]), Some(outer));
}

#[test]
fn merge_rects_with_negative_coordinates() {
    let r1 = Rect::new(-30, -30, 20, 20); // [-30, -10)
    let r2 = Rect::new(-15, -15, 10, 10); // [-15, -5)
    let r3 = Rect::new(10, 10, 10, 10); // [10, 20)

    let merged = merge_rects(&[r1, r2, r3]).expect("merges");
    // x0 = -30, y0 = -30, right = 20, bottom = 20 -> width = 50, height = 50.
    assert_eq!(merged, Rect::new(-30, -30, 50, 50));
    assert!(merged.contains(&r1));
    assert!(merged.contains(&r2));
    assert!(merged.contains(&r3));
}

// ============================================================================
// 6. Display / check_present / RefreshRequest 边界测试
// ============================================================================

#[test]
fn check_present_boundary_and_zero_sizes() {
    let frame_zero = GrayBuffer::new(0, 0);
    let req_zero = RefreshRequest::new(Rect::new(0, 0, 0, 0), RefreshMode::Full);
    // Since CORE-003 an empty rectangle is never a valid refresh region, so a
    // 0x0 panel has no acceptable request at all.
    assert!(matches!(
        check_present((0, 0), &frame_zero, &req_zero),
        Err(Error::OutOfBounds { .. })
    ));

    let frame = GrayBuffer::new(1264, 1680);

    // Zero-sized request placed at the bottom-right outer edge.
    let corner_zero = RefreshRequest::new(Rect::new(1264, 1680, 0, 0), RefreshMode::Partial);
    assert!(matches!(
        check_present((1264, 1680), &frame, &corner_zero),
        Err(Error::OutOfBounds { .. })
    ));

    // Request touches exact panel boundaries.
    let full_req = RefreshRequest::full(1264, 1680);
    assert!(check_present((1264, 1680), &frame, &full_req).is_ok());

    // Request exceeds right by 1 pixel.
    let right_out = RefreshRequest::new(Rect::new(1, 0, 1264, 1680), RefreshMode::Partial);
    assert!(matches!(
        check_present((1264, 1680), &frame, &right_out),
        Err(Error::OutOfBounds { .. })
    ));

    // Request exceeds bottom by 1 pixel.
    let bottom_out = RefreshRequest::new(Rect::new(0, 1, 1264, 1680), RefreshMode::Partial);
    assert!(matches!(
        check_present((1264, 1680), &frame, &bottom_out),
        Err(Error::OutOfBounds { .. })
    ));

    // Negative coordinate request.
    let neg_req = RefreshRequest::new(Rect::new(-1, 0, 10, 10), RefreshMode::Partial);
    assert!(matches!(
        check_present((1264, 1680), &frame, &neg_req),
        Err(Error::OutOfBounds { .. })
    ));
}

#[test]
fn display_trait_default_check_method() {
    struct TestDisplay {
        size: (u32, u32),
    }
    impl Display for TestDisplay {
        fn size(&self) -> (u32, u32) {
            self.size
        }
        fn present(&mut self, frame: &GrayBuffer, request: &RefreshRequest) -> Result<()> {
            self.check(frame, request)?;
            Ok(())
        }
    }

    let mut d = TestDisplay { size: (10, 10) };
    let frame = GrayBuffer::new(10, 10);
    let req = RefreshRequest::full(10, 10);
    assert!(d.present(&frame, &req).is_ok());

    let wrong_frame = GrayBuffer::new(8, 8);
    assert!(matches!(
        d.present(&wrong_frame, &req),
        Err(Error::SizeMismatch { .. })
    ));
}

// ============================================================================
// 7. Input & Power 接口行为验证 (Input & Power traits)
// ============================================================================

#[test]
fn input_touch_coordinates_accept_negative_values() {
    // Touch on bezel outside active display area can produce negative values.
    let touch = InputEvent::Touch {
        x: -5,
        y: -10,
        phase: TouchPhase::Press,
    };
    if let InputEvent::Touch { x, y, phase } = touch {
        assert_eq!(x, -5);
        assert_eq!(y, -10);
        assert_eq!(phase, TouchPhase::Press);
    } else {
        panic!("expected Touch variant");
    }
}

#[test]
fn input_trait_and_key_code_variants() {
    struct MockInput {
        events: Vec<InputEvent>,
    }
    impl Input for MockInput {
        fn poll(&mut self) -> Option<InputEvent> {
            if self.events.is_empty() {
                None
            } else {
                Some(self.events.remove(0))
            }
        }
    }

    let mut input = MockInput {
        events: vec![
            InputEvent::Key {
                code: KeyCode::PageDown,
                pressed: true,
            },
            InputEvent::Key {
                code: KeyCode::Unknown(999),
                pressed: false,
            },
        ],
    };

    assert_eq!(
        input.poll(),
        Some(InputEvent::Key {
            code: KeyCode::PageDown,
            pressed: true,
        })
    );
    assert_eq!(
        input.poll(),
        Some(InputEvent::Key {
            code: KeyCode::Unknown(999),
            pressed: false,
        })
    );
    assert_eq!(input.poll(), None);
}

#[test]
fn power_trait_contract() {
    struct MockPower {
        percent: u8,
        prevented: bool,
    }
    impl Power for MockPower {
        fn battery_percent(&mut self) -> Result<u8> {
            Ok(self.percent)
        }
        fn prevent_sleep(&mut self, prevent: bool) -> Result<()> {
            self.prevented = prevent;
            Ok(())
        }
    }

    let mut p = MockPower {
        percent: 100,
        prevented: false,
    };
    assert_eq!(p.battery_percent().unwrap(), 100);
    p.prevent_sleep(true).unwrap();
    assert!(p.prevented);
}

// ============================================================================
// 8. 缺陷回归测试（QA-001 发现、CORE-003 已修复）
// ============================================================================

/// QA-001 Bug 1: 当 Rect::x 为 i32::MAX 时，right() 溢出饱和为 i32::MAX。
/// 导致 right() == x，intersection() 计算时判定 x1 <= x0，错误返回 Rect::default()。
/// 两个相同且非空的矩形求交集结果应等于其自身，intersects() 也应为 true。
#[test]
fn defect_rect_intersection_at_i32_max_returns_empty() {
    let r = Rect::new(i32::MAX, 0, 1, 1);
    assert!(!r.is_empty(), "Rectangle with width 1 is not empty");
    assert!(
        r.intersects(&r),
        "A non-empty rectangle must intersect itself"
    );
    assert_eq!(
        r.intersection(&r),
        r,
        "Self-intersection should yield the identical rectangle"
    );
}

/// QA-001 Bug 2: 当 inner 的右边界超过 i32::MAX 时，saturating_add 导致 inner.right() 饱和为 i32::MAX。
/// contains() 比较 inner.right() <= outer.right()（i32::MAX <= i32::MAX）判定为真，
/// 误报包含，而实际上 inner 已经超出 outer 的范围。
#[test]
fn defect_rect_contains_overflow_false_positive() {
    let outer = Rect::new(0, 0, i32::MAX, 10);
    let inner = Rect::new(100, 0, i32::MAX, 10);
    // inner starts 100 pixels to the right of outer with equal width, so its right edge extends 100 pixels past outer.
    assert!(
        !outer.contains(&inner),
        "outer cannot contain inner which extends beyond outer"
    );
}

/// QA-001 Bug 3: check_present 允许起点已经位于或超出显示区域边缘的 0 尺寸刷新矩形。
/// 例如在 (1264, 1680) 屏幕上，Rect::new(1264, 1680, 0, 0) 的 x == w, y == h。
/// 由于 rect.width == 0，rect.right() == 1264 <= 1264 成立，check_present 错误地返回 Ok(())。
#[test]
fn defect_check_present_accepts_zero_sized_rect_outside_boundary() {
    let frame = GrayBuffer::new(1264, 1680);
    let req = RefreshRequest::new(Rect::new(1264, 1680, 0, 0), RefreshMode::Partial);
    assert!(
        check_present((1264, 1680), &frame, &req).is_err(),
        "A request placed at or outside the display boundary must be rejected"
    );
}

/// QA-001 Bug 4: Rect 结构体字段公开（pub width: i32, pub height: i32）。
/// 若调用方直接实例化包含负宽高的 Rect，Rect::is_empty() 仅判断 `width == 0 || height == 0`，
/// 对负数返回 false（认为非空），与文档「Whether the rectangle covers no pixel」不符。
#[test]
fn defect_rect_is_empty_false_for_negative_dimensions() {
    let r = Rect {
        x: 0,
        y: 0,
        width: -5,
        height: 10,
    };
    assert!(
        r.is_empty(),
        "Rect with negative width covers no pixels and must report is_empty() == true"
    );
}
