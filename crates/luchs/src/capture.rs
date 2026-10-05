//! Snapshot scheduling stays in Rust so renderer-independent policy is testable.
use std::time::{Duration, Instant};

pub const IDLE_AFTER: Duration = Duration::from_secs(1);
pub const IDLE_INTERVAL: Duration = Duration::from_millis(500);

pub struct CapturePolicy {
    interval: Duration,
    active: Instant,
    next: Instant,
    pub visible: bool,
    pub scale: f64,
}

impl CapturePolicy {
    pub fn new(fps: u32, now: Instant) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / fps as f64),
            active: now,
            next: now,
            visible: true,
            scale: 1.0,
        }
    }
    pub fn wake(&mut self, now: Instant) {
        self.active = now;
        self.next = now;
    }
    pub fn due(&self, now: Instant) -> bool {
        self.visible && now >= self.next
    }
    pub fn wait(&self, now: Instant) -> Duration {
        if self.visible {
            self.next.saturating_duration_since(now)
        } else {
            IDLE_INTERVAL
        }
    }
    pub fn completed(&mut self, published: bool, now: Instant) {
        if published {
            self.active = now;
        }
        self.next = now
            + if now.duration_since(self.active) >= IDLE_AFTER {
                IDLE_INTERVAL
            } else {
                self.interval
            };
    }
    pub fn presentation(&mut self, visible: bool, scale: f64, now: Instant) {
        self.visible = visible;
        self.scale = scale;
        self.wake(now);
    }
}

/// Coalesce host resize bursts before changing WebKit and its capture allocation.
pub const PRESENTATION_DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub logical: (u32, u32),
    pub pixels: (u32, u32),
    pub scale: f64,
}
impl Viewport {
    pub fn resolve(
        default: (u32, u32),
        hint: &jackstay::affordances::Presentation,
        previous_scale: f64,
    ) -> Self {
        let mut requested = hint
            .preferred_size
            .as_ref()
            .map_or((f64::from(default.0), f64::from(default.1)), |s| {
                (s.width.round().max(1.), s.height.round().max(1.))
            });
        // Normalize very large finite hints before searching, preserving their
        // aspect ratio without requiring a tiny factor below the search precision.
        let normalize = (u32::MAX as f64 / requested.0.max(requested.1)).min(1.);
        requested = (
            (requested.0 * normalize).max(1.),
            (requested.1 * normalize).max(1.),
        );
        let dimensions = |factor: f64, scale: f64| {
            let logical = (
                (requested.0 * factor).floor().clamp(1., u32::MAX as f64) as u32,
                (requested.1 * factor).floor().clamp(1., u32::MAX as f64) as u32,
            );
            let pixels = (
                (f64::from(logical.0) * scale).round(),
                (f64::from(logical.1) * scale).round(),
            );
            (logical, pixels)
        };
        let valid = |pixels: (f64, f64)| {
            pixels.0 >= 1.
                && pixels.1 >= 1.
                && pixels.0 <= u32::MAX as f64
                && pixels.1 <= u32::MAX as f64
                && pixels.0 * pixels.1 * 4. <= crate::protocol::MAX_FRAME_BYTES as f64
        };
        let mut scale = hint.scale;
        // A scale that cannot represent even one logical pixel cannot be clamped.
        // With no preferred size retain the CLI viewport, as before.
        if !scale.is_finite()
            || scale <= 0.
            || (hint.preferred_size.is_none() && !valid(dimensions(1., scale).1))
            || (hint.preferred_size.is_some()
                && !valid((scale.round().max(1.), scale.round().max(1.))))
        {
            scale = previous_scale;
        }
        if hint.preferred_size.is_none() && !valid(dimensions(1., scale).1) {
            scale = 1.;
        }
        let mut resolved = dimensions(1., scale);
        if !valid(resolved.1) {
            // Binary search a proportional clamp, accounting for pixel rounding.
            let (mut low, mut high) = (0., 1.);
            for _ in 0..64 {
                let middle = (low + high) / 2.;
                let candidate = dimensions(middle, scale);
                if candidate.1.0 * candidate.1.1 * 4. <= crate::protocol::MAX_FRAME_BYTES as f64
                    && candidate.1.0 <= u32::MAX as f64
                    && candidate.1.1 <= u32::MAX as f64
                {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            resolved = dimensions(low, scale);
        }
        // A subpixel scale yielding an empty axis is ignored.
        if !valid(resolved.1) {
            let mut fallback = hint.clone();
            fallback.scale = 1.;
            return Self::resolve(default, &fallback, 1.);
        }
        Self {
            logical: resolved.0,
            pixels: (resolved.1.0 as u32, resolved.1.1 as u32),
            scale,
        }
    }
}
