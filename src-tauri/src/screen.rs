//! Python CV sidecar client and typed recognition results.
//!
//! Process lifecycle and transport live in `client`; individual CV requests
//! live in `operations`. Callers continue to use the stable exports here.

use std::path::Path;

mod client;
mod error;
mod input;
mod operations;
mod protocol;
mod recognition_diagnostics;
mod transport;
mod types;

pub use client::{SidecarClient, TemplateLoadSpec};
pub use error::CvError;
pub(crate) use input::{ScrcpyControl, ScrcpyTouchEvent};
pub(crate) use types::{BATTLE_DETECTION_SCREENS, EVENT_ONLY_DETECTION_SCREENS};
pub use types::*;

/// The CV operations the event scanner consumes, so the scanner can run
/// against any source that serves the same verbs — the live sidecar client,
/// or the headless event harness's own minimal client.
pub(crate) trait EventCvSource {
    fn detect(&mut self, image_path: Option<&Path>) -> Result<Screen, CvError>;
    fn read_map_labels(&mut self, image_path: Option<&Path>) -> Result<MapLabelsResult, CvError>;
    fn read_quest_card(&mut self, image_path: Option<&Path>) -> Result<QuestCardResult, CvError>;
    fn read_event_points(&mut self, image_path: Option<&Path>) -> Result<EventPointsResult, CvError>;
    fn frame_thumbnail(
        &mut self,
        image_path: Option<&Path>,
        width: u32,
        height: u32,
    ) -> Result<FrameThumbnail, CvError>;
}

impl EventCvSource for SidecarClient {
    fn detect(&mut self, image_path: Option<&Path>) -> Result<Screen, CvError> {
        SidecarClient::detect(self, image_path)
    }
    fn read_map_labels(&mut self, image_path: Option<&Path>) -> Result<MapLabelsResult, CvError> {
        SidecarClient::read_map_labels(self, image_path)
    }
    fn read_quest_card(&mut self, image_path: Option<&Path>) -> Result<QuestCardResult, CvError> {
        SidecarClient::read_quest_card(self, image_path)
    }
    fn read_event_points(
        &mut self,
        image_path: Option<&Path>,
    ) -> Result<EventPointsResult, CvError> {
        SidecarClient::read_event_points(self, image_path)
    }
    fn frame_thumbnail(
        &mut self,
        image_path: Option<&Path>,
        width: u32,
        height: u32,
    ) -> Result<FrameThumbnail, CvError> {
        SidecarClient::frame_thumbnail(self, image_path, width, height)
    }
}

pub const SIDECAR_STARTUP_REDOWNLOAD_MESSAGE: &str =
    "CV 运行时启动失败，请前往资源管理重新下载 CV 运行时。";

pub fn sidecar_startup_user_message(error: &str) -> Option<&'static str> {
    if error.contains("sidecar did not become ready")
        || error.contains("OpenCV bindings requires")
        || error.contains("invalid JSON from sidecar")
    {
        Some(SIDECAR_STARTUP_REDOWNLOAD_MESSAGE)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
