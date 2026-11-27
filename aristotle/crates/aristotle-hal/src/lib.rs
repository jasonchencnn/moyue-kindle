// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hardware layer for Aristotle on the Kindle Paperwhite 4 and the Oasis 2.

pub mod audio;
pub mod bluetooth;
pub mod display;
pub mod framebuffer;
mod framework;
pub mod input;
mod kobo;
mod lipc_command;
pub mod platform;
pub mod power;
pub mod powerd;
pub mod screensaver;
pub mod system;
pub mod wifi;
