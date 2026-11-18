//! `adb shell input ...` backend.
//!
//! Cheap, always-available, but slow: every `input` invocation spawns
//! a fresh JVM on the device (~30–80 ms each), so the multi-event
//! `swipe_with_settle` path runs in single-digit seconds for a
//! full-page swipe and looks jittery while it does.
//!
//! Kept as the initialization fallback and explicit `MASH_TOUCH_BACKEND=adb` baseline.
//! Pinch is the one gesture `input` cannot express, so it drops to raw
//! multi-touch `sendevent` samples (see [`crate::adb::Adb::pinch`]).

use crate::adb::Adb;
use crate::touch::TouchBackend;

pub struct AdbInputBackend {
    adb: Adb,
    screen_size: (u32, u32),
}

impl AdbInputBackend {
    pub fn new(adb: Adb, screen_size: (u32, u32)) -> Self {
        Self { adb, screen_size }
    }
}

impl TouchBackend for AdbInputBackend {
    fn tap(&mut self, x: u32, y: u32) -> Result<(), String> {
        self.adb.tap(x, y)
    }

    fn swipe(&mut self, from: (u32, u32), to: (u32, u32), duration_ms: u32) -> Result<(), String> {
        self.adb.swipe(from, to, duration_ms)
    }

    fn swipe_with_settle(
        &mut self,
        from: (u32, u32),
        to: (u32, u32),
        swipe_ms: u32,
        settle_ms: u32,
    ) -> Result<(), String> {
        self.adb.swipe_with_settle(from, to, swipe_ms, settle_ms)
    }

    fn pinch(
        &mut self,
        center: (u32, u32),
        from_gap: u32,
        to_gap: u32,
        duration_ms: u32,
    ) -> Result<(), String> {
        self.adb
            .pinch(center, from_gap, to_gap, duration_ms, self.screen_size)
    }

    fn name(&self) -> &'static str {
        "adb-input"
    }
}
