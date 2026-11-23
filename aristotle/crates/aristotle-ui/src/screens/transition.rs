// SPDX-License-Identifier: AGPL-3.0-or-later
//! Feedback while the launcher, reader and stock interface exchange the screen.

use crate::geom::Insets;
use crate::text::{Align, Paragraph};
use crate::theme::{Styles, INK, INK_MID, PAPER, RULE};
use crate::tree::{Cross, Kind, Node, Tree};
use aristotle_i18n::{tr, Msg};

/// A completed boundary in the launch or shutdown sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The stock interface has been asked to stop.
    Prepare,
    /// The reader is loading its library and interface.
    Loading,
    /// Fonts and the library are loaded, before the first interactive frame.
    Ready,
    /// The reader is saving its state.
    Saving,
    /// State is saved and the stock interface is being restarted.
    Restoring,
    /// The stock framework is running, before its first usable screen.
    Waiting,
    /// The stock interface has exceeded its normal startup budget.
    Slow,
}

impl Stage {
    /// The compact command used by the launch scripts.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "prepare" => Some(Self::Prepare),
            "loading" => Some(Self::Loading),
            "ready" => Some(Self::Ready),
            "saving" => Some(Self::Saving),
            "restoring" => Some(Self::Restoring),
            "waiting" => Some(Self::Waiting),
            "slow" => Some(Self::Slow),
            _ => None,
        }
    }

    fn entering(self) -> bool {
        matches!(self, Self::Prepare | Self::Loading | Self::Ready)
    }

    fn step(self) -> usize {
        match self {
            Self::Prepare | Self::Saving => 1,
            Self::Loading | Self::Restoring => 2,
            Self::Ready | Self::Waiting | Self::Slow => 3,
        }
    }

    fn message(self) -> Msg {
        match self {
            Self::Prepare => Msg::TransitionPrepare,
            Self::Loading => Msg::TransitionLoading,
            Self::Ready => Msg::TransitionReady,
            Self::Saving => Msg::TransitionSaving,
            Self::Restoring => Msg::TransitionRestoring,
            Self::Waiting => Msg::TransitionWaiting,
            Self::Slow => Msg::TransitionSlow,
        }
    }
}

/// A centered logo, milestone label and three-part stage indicator.
pub fn build(styles: &Styles, stage: Stage) -> Tree {
    let mut tree = Tree::new();
    let logo = crate::status::brand_mark(&mut tree);
    let logo = tree.align(0.5, 0.5, logo);
    let logo = tree.fixed(None, Some(64), Some(logo));
    let title = tree.text(Paragraph::plain(
        tr(if stage.entering() {
            Msg::TransitionEnter
        } else {
            Msg::TransitionReturn
        }),
        styles.font_lg().bold(),
        Align::Center,
    ));
    let title = tree.fixed(None, Some(100), Some(title));
    let detail = tree.text(Paragraph::plain(
        tr(stage.message()),
        styles.font_base(),
        Align::Center,
    ));
    let detail = tree.fixed(None, Some(132), Some(detail));
    let mut segments = Vec::new();
    for index in 0..3 {
        let color = if index < stage.step() { INK } else { RULE };
        let segment = tree.add(Node::column(0).bg(color).border(0, color, 6).flex(1), []);
        segments.push(segment);
    }
    let progress = tree.add(Node::row(12).cross(Cross::Stretch), segments);
    let progress = tree.fixed(None, Some(12), Some(progress));
    let hint = tree.text(Paragraph::plain(
        tr(Msg::TransitionHint),
        styles.font_xs().gray(INK_MID),
        Align::Center,
    ));
    let space = tree.gap(0, 32);
    let content = tree.add(
        Node::column(28).cross(Cross::Stretch),
        [logo, space, title, detail, progress, hint],
    );
    let content = tree.fixed(Some(760), None, Some(content));
    let content = tree.pad(Insets::all(48), content);
    let root = tree.add(
        Node::new(Kind::Align { x: 0.5, y: 0.45 }).bg(PAPER),
        [content],
    );
    tree.set_root(root);
    tree
}

/// Ownership of the current framebuffer while waiting for the stock interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameState {
    Own,
    Blank,
    Other,
}

/// Compares visible pixels while ignoring framebuffer stride padding.
pub fn frame_state(pixels: &[u8], stride: usize, canvas: &crate::Canvas) -> FrameState {
    let width = canvas.width as usize;
    let height = canvas.height as usize;
    if stride < width || pixels.len() < stride.saturating_mul(height) {
        return FrameState::Other;
    }
    let mut same = true;
    let mut blank = true;
    for y in 0..height {
        let row = &pixels[y * stride..y * stride + width];
        let expected = &canvas.pixels[y * canvas.stride as usize..][..width];
        same &= row == expected;
        blank &= row.iter().all(|&pixel| pixel >= 250);
    }
    if same {
        FrameState::Own
    } else if blank {
        FrameState::Blank
    } else {
        FrameState::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_distinguishes_its_frame_blank_and_stock_content_with_stride_padding() {
        let mut canvas = crate::Canvas::new(2, 2);
        canvas.pixels[0] = 0;
        let mut pixels = vec![255; 8];
        pixels[0] = 0;
        assert_eq!(frame_state(&pixels, 4, &canvas), FrameState::Own);
        pixels[0] = 255;
        assert_eq!(frame_state(&pixels, 4, &canvas), FrameState::Blank);
        pixels[4] = 0;
        assert_eq!(frame_state(&pixels, 4, &canvas), FrameState::Other);
    }
}
