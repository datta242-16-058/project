use crate::models::{RiskLevel, SystemEvent};
use log::{error, info, warn};

pub struct EventLogger;

impl EventLogger {
    pub fn log_event(event: &SystemEvent) {
        match event.severity {
            RiskLevel::Low => info!(
                "[LOW] PID:{} - {} ({:?})",
                event.pid, event.description, event.event_type
            ),
            RiskLevel::Medium => warn!(
                "[MEDIUM] PID:{} - {} ({:?})",
                event.pid, event.description, event.event_type
            ),
            RiskLevel::High => error!(
                "[HIGH] PID:{} - {} ({:?})",
                event.pid, event.description, event.event_type
            ),
            RiskLevel::Critical => error!(
                "[CRITICAL] PID:{} - {} ({:?})",
                event.pid, event.description, event.event_type
            ),
        }

        // In a real system, we'd append to a structural log file (JSON/SQLite) here.
    }
}
