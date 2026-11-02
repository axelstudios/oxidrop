//! Renderer settings for a light desktop GUI.
//!
//! Frames wait for the display's vertical sync, with at most one queued. On Windows, winit
//! repaints the window on every move event while it is being dragged, inside the system's drag
//! loop, and a vsync'd present can block that loop for a refresh. Presenting without vsync
//! (paced by a frame cap) was tried for that, but with a single queued frame and wgpu's default
//! backend, rather than DX12, dragging looked the same either way.
//!
//! The swapchain is created without vsync and switched to vsync on the first frame. Created
//! with vsync (Vulkan, NVIDIA, G-Sync set to full screen only), moving the cursor out of the
//! window made the cursor lag everywhere for a second or two; switched later, it doesn't.
//! Presumably the driver picks how to present a window when its first swapchain is created.

use std::time::{Duration, Instant};

use eframe::{
    egui_wgpu::{SurfaceConfig, WgpuConfiguration},
    wgpu,
};

/// The wgpu configuration at startup: the swapchain is created without vsync, and
/// [`surface_config`] switches it to vsync on the first frame.
#[must_use]
pub(super) fn wgpu_options() -> WgpuConfiguration {
    WgpuConfiguration {
        surface: SurfaceConfig {
            present_mode: wgpu::PresentMode::AutoNoVsync,
            ..surface_config()
        },
        ..WgpuConfiguration::default()
    }
}

/// The surface configuration frames use: vsync, and at most one queued frame, since the UI
/// draws very little and every queued frame adds latency. Cheap to apply every frame.
#[must_use]
pub(super) fn surface_config() -> SurfaceConfig {
    SurfaceConfig {
        // The `Auto*` modes fall back to what the surface supports, so they never fail.
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: Some(1),
    }
}

/// Frame timing statistics for `OXIDROP_FRAME_STATS=1`: the app repaints continuously and
/// prints frame rate and frame times once a second, to see how long presenting blocks.
#[derive(Debug)]
pub(super) struct FrameStats {
    window_start: Instant,
    last_frame: Option<Instant>,
    intervals: Vec<Duration>,
}

impl FrameStats {
    #[must_use]
    pub(super) fn from_env() -> Option<Self> {
        std::env::var_os("OXIDROP_FRAME_STATS").map(|_| Self {
            window_start: Instant::now(),
            last_frame: None,
            intervals: Vec::new(),
        })
    }

    /// Records the start of a frame; returns a report line once a second.
    pub(super) fn frame(&mut self, now: Instant) -> Option<String> {
        if let Some(last) = self.last_frame.replace(now) {
            self.intervals.push(now - last);
        }
        if now - self.window_start < Duration::from_secs(1) || self.intervals.is_empty() {
            return None;
        }
        self.window_start = now;
        let mut intervals = std::mem::take(&mut self.intervals);
        intervals.sort_unstable();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let mean =
            intervals.iter().sum::<Duration>() / u32::try_from(intervals.len()).unwrap_or(u32::MAX);
        Some(format!(
            "{} frames · frame time mean {:.2} ms · median {:.2} ms · max {:.2} ms",
            intervals.len(),
            ms(mean),
            ms(intervals[intervals.len() / 2]),
            ms(intervals[intervals.len() - 1]),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_without_vsync_then_presents_with_it() {
        let startup = wgpu_options().surface;
        assert_eq!(startup.present_mode, wgpu::PresentMode::AutoNoVsync);
        assert_eq!(startup.desired_maximum_frame_latency, Some(1));
        let frames = surface_config();
        assert_eq!(frames.present_mode, wgpu::PresentMode::AutoVsync);
        assert_eq!(frames.desired_maximum_frame_latency, Some(1));
    }

    #[test]
    fn stats_report_once_a_second() {
        let mut stats = FrameStats::from_env().unwrap_or(FrameStats {
            window_start: Instant::now(),
            last_frame: None,
            intervals: Vec::new(),
        });
        let start = stats.window_start;
        for i in 0..10 {
            assert_eq!(stats.frame(start + Duration::from_millis(i * 10)), None);
        }
        let report = stats
            .frame(start + Duration::from_millis(1000))
            .expect("a report");
        assert!(report.starts_with("10 frames"), "{report}");
    }
}
