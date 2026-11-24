use ftm_core::protocol::Event;
use std::time::{Duration, Instant};

/// How often the event forwarder writes a summary to ftm-app.log while events flow.
pub const WINDOW: Duration = Duration::from_secs(600);

/// What the forwarder handed the page over one [`WINDOW`]: how many scripts (batches) and events, the busiest second,
/// how much pane output, and how often the whole tree (`workspaces`, which rebuilds the sidebar) or the task list came.
#[derive(Default)]
pub struct BatchStats {
    since: Option<Instant>,
    batches: u64,
    events: u64,
    max_batch: usize,
    output_bytes: u64,
    workspaces: u64,
    tasks: u64,
    second: Option<Instant>,
    second_events: u64,
    peak_per_second: u64,
}

impl BatchStats {
    /// Counts one emitted batch, and returns the window's summary once [`WINDOW`] has passed since it began.
    pub fn record(&mut self, batch: &[Event], now: Instant) -> Option<String> {
        let since = *self.since.get_or_insert(now);
        self.batches += 1;
        self.events += batch.len() as u64;
        self.max_batch = self.max_batch.max(batch.len());
        for event in batch {
            match event {
                Event::Output { data, .. } => self.output_bytes += data.len() as u64 * 3 / 4,
                Event::Workspaces { .. } => self.workspaces += 1,
                Event::Tasks { .. } => self.tasks += 1,
                _ => {},
            }
        }
        if self.second.is_none_or(|s| now.duration_since(s) >= Duration::from_secs(1)) {
            self.second = Some(now);
            self.second_events = 0;
        }
        self.second_events += batch.len() as u64;
        self.peak_per_second = self.peak_per_second.max(self.second_events);
        let elapsed = now.duration_since(since);
        if elapsed < WINDOW {
            return None;
        }
        let line = format!(
            "perf events {}s: {} batches, {} events (peak {}/s, max {} per batch), {} KB output, {} workspaces, {} tasks",
            elapsed.as_secs(),
            self.batches,
            self.events,
            self.peak_per_second,
            self.max_batch,
            self.output_bytes / 1024,
            self.workspaces,
            self.tasks,
        );
        *self = BatchStats::default();
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(bytes: usize) -> Event {
        Event::Output {
            pane: Default::default(),
            data: "A".repeat(bytes * 4 / 3),
            reset: false,
        }
    }

    #[test]
    fn summarises_once_per_window() {
        let mut stats = BatchStats::default();
        let start = Instant::now();
        assert_eq!(stats.record(&[output(3072), output(3072)], start), None);
        assert_eq!(
            stats.record(
                &[Event::Workspaces { workspaces: vec![] }],
                start + Duration::from_millis(500)
            ),
            None
        );
        assert_eq!(
            stats.record(&[Event::Tasks { tasks: vec![] }], start + Duration::from_secs(5)),
            None
        );
        let line = stats
            .record(&[output(3072)], start + WINDOW)
            .expect("a summary after the window");
        assert_eq!(
            line,
            "perf events 600s: 4 batches, 5 events (peak 3/s, max 2 per batch), 9 KB output, 1 workspaces, 1 tasks"
        );
        assert_eq!(
            stats.record(&[output(1024)], start + WINDOW + Duration::from_secs(1)),
            None,
            "a new window"
        );
    }
}
