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
