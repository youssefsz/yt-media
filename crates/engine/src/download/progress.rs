//! Bounded private yt-dlp and `FFmpeg` progress protocols.

use std::collections::{BTreeMap, VecDeque};

use crate::process::ProcessEvent;

use super::{JobProgress, JobStage};

const MAX_PROTOCOL_LINE_BYTES: usize = 2_048;
const MAX_PROTOCOL_LINES: usize = 20_000;
const YTDLP_PREFIX: &str = "yt-media-progress|";
const ETA_SAMPLE_WINDOW: usize = 3;

pub(crate) struct ProtocolLines {
    pending: Vec<u8>,
    observed_lines: usize,
    invalid: Option<String>,
}

impl ProtocolLines {
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
            observed_lines: 0,
            invalid: None,
        }
    }

    pub(crate) fn push(&mut self, event: &ProcessEvent) -> Vec<String> {
        if self.invalid.is_some() {
            return Vec::new();
        }
        self.pending.extend_from_slice(&event.bytes);
        if self.pending.len() > MAX_PROTOCOL_LINE_BYTES.saturating_mul(2) {
            self.invalid = Some("progress record exceeded the bounded line buffer".to_owned());
            self.pending.clear();
            return Vec::new();
        }
        let mut lines = Vec::new();
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let bytes = self.pending.drain(..=position).collect::<Vec<_>>();
            self.observed_lines = self.observed_lines.saturating_add(1);
            if self.observed_lines > MAX_PROTOCOL_LINES {
                self.invalid = Some("too many progress records".to_owned());
                self.pending.clear();
                break;
            }
            let line = bytes
                .strip_suffix(b"\n")
                .unwrap_or(&bytes)
                .strip_suffix(b"\r")
                .unwrap_or_else(|| bytes.strip_suffix(b"\n").unwrap_or(&bytes));
            if line.len() > MAX_PROTOCOL_LINE_BYTES {
                self.invalid = Some("progress record exceeded 2048 bytes".to_owned());
                self.pending.clear();
                break;
            }
            if let Ok(value) = std::str::from_utf8(line) {
                lines.push(value.to_owned());
            } else {
                self.invalid = Some("progress record was not valid UTF-8".to_owned());
                self.pending.clear();
                break;
            }
        }
        lines
    }

    pub(crate) fn invalid_reason(&self) -> Option<&str> {
        self.invalid.as_deref()
    }
}

pub(crate) fn parse_ytdlp_progress(line: &str) -> Option<JobProgress> {
    let fields = line
        .strip_prefix(YTDLP_PREFIX)?
        .split('|')
        .collect::<Vec<_>>();
    if fields.len() != 6 {
        return None;
    }
    let status = fields[0].trim();
    if !matches!(status, "downloading" | "finished") {
        return None;
    }
    let completed = parse_bounded_u64(fields[1])?;
    let exact_total = parse_optional_u64(fields[2]);
    let estimated_total = parse_optional_u64(fields[3]);
    let total = exact_total.or(estimated_total).filter(|value| *value > 0);
    let bytes_per_second = parse_optional_f64_to_u64(fields[4]);
    let eta_seconds = parse_optional_f64_to_u64(fields[5]);
    Some(make_progress(
        JobStage::Downloading,
        completed,
        total,
        (status == "downloading")
            .then_some(bytes_per_second)
            .flatten(),
        (status == "downloading").then_some(eta_seconds).flatten(),
    ))
}

pub(crate) struct FfmpegProgress {
    duration_micros: u64,
    completed_micros: u64,
}

impl FfmpegProgress {
    pub(crate) fn new(duration_millis: u64) -> Self {
        Self {
            duration_micros: duration_millis.saturating_mul(1_000),
            completed_micros: 0,
        }
    }

    pub(crate) fn parse_line(&mut self, line: &str, stage: JobStage) -> Option<JobProgress> {
        let value = line.strip_prefix("out_time_us=")?;
        let completed = parse_bounded_u64(value)?;
        self.completed_micros = self
            .completed_micros
            .max(completed.min(self.duration_micros));
        Some(make_progress(
            stage,
            self.completed_micros,
            Some(self.duration_micros),
            None,
            None,
        ))
    }
}

pub(crate) fn make_progress(
    stage: JobStage,
    completed: u64,
    total: Option<u64>,
    bytes_per_second: Option<u64>,
    eta_seconds: Option<u64>,
) -> JobProgress {
    let percent = total.filter(|total| *total > 0).map(|total| {
        let scale = total.saturating_div(u64::from(u32::MAX)).saturating_add(1);
        let scaled_total = u32::try_from(total.saturating_div(scale)).unwrap_or(u32::MAX);
        let scaled_completed =
            u32::try_from(completed.min(total).saturating_div(scale)).unwrap_or(u32::MAX);
        (f64::from(scaled_completed) / f64::from(scaled_total)) * 100.0
    });
    JobProgress {
        stage,
        completed,
        total,
        percent,
        bytes_per_second,
        eta_seconds,
    }
}

fn parse_optional_u64(value: &str) -> Option<u64> {
    if value.is_empty() || value == "NA" || value == "None" {
        None
    } else {
        parse_bounded_u64(value)
    }
}

fn parse_bounded_u64(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.len() > 24 || value.starts_with('-') {
        return None;
    }
    value.parse::<u64>().ok()
}

fn parse_optional_f64_to_u64(value: &str) -> Option<u64> {
    if value.is_empty() || value == "NA" || value == "None" || value.len() > 32 {
        return None;
    }
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    let integer = parse_bounded_u64(integer)?;
    let round_up = fraction
        .as_bytes()
        .first()
        .is_some_and(|digit| *digit >= b'5');
    integer.checked_add(u64::from(round_up))
}

pub(crate) struct DownloadProgressAggregator {
    expected_sources: usize,
    estimated_total: Option<u64>,
    sources: BTreeMap<u8, JobProgress>,
    displayed_percent: Option<f64>,
    eta_samples: VecDeque<u64>,
}

impl DownloadProgressAggregator {
    pub(crate) fn new(expected_sources: usize, estimated_total: Option<u64>) -> Self {
        Self {
            expected_sources,
            estimated_total: estimated_total.filter(|total| *total > 0),
            sources: BTreeMap::new(),
            displayed_percent: None,
            eta_samples: VecDeque::with_capacity(ETA_SAMPLE_WINDOW),
        }
    }

    pub(crate) fn update(&mut self, source: u8, progress: JobProgress) -> JobProgress {
        let latest_eta = progress.eta_seconds;
        self.sources.insert(source, progress);
        let completed = self
            .sources
            .values()
            .fold(0_u64, |sum, value| sum.saturating_add(value.completed));
        let all_sources_reported = self.sources.len() == self.expected_sources;
        let reported_total = all_sources_reported
            .then(|| {
                self.sources.values().try_fold(0_u64, |sum, value| {
                    value.total.map(|total| sum.saturating_add(total))
                })
            })
            .flatten();
        let total = reported_total.or(self.estimated_total);
        let speed = {
            let mut speeds = self
                .sources
                .values()
                .filter_map(|value| value.bytes_per_second);
            speeds
                .next()
                .map(|first| speeds.fold(first, u64::saturating_add))
        };
        let aggregate_eta = total
            .zip(speed)
            .and_then(|(total, speed)| remaining_seconds(total, completed, speed))
            .or(latest_eta.filter(|_| self.expected_sources == 1));
        let eta = self.stable_eta(aggregate_eta);
        let mut aggregate = make_progress(JobStage::Downloading, completed, total, speed, eta);
        if let Some(percent) = aggregate.percent {
            // An analyzed size is an estimate. Do not claim that the download stage
            // is complete until every source has supplied its own current total.
            let percent = if reported_total.is_none() {
                percent.min(99.0)
            } else {
                percent
            };
            let percent = self
                .displayed_percent
                .map_or(percent, |displayed| displayed.max(percent));
            self.displayed_percent = Some(percent);
            aggregate.percent = Some(percent);
        }
        aggregate
    }

    fn stable_eta(&mut self, eta: Option<u64>) -> Option<u64> {
        let Some(eta) = eta else {
            self.eta_samples.clear();
            return None;
        };
        self.eta_samples.push_back(eta);
        if self.eta_samples.len() > ETA_SAMPLE_WINDOW {
            self.eta_samples.pop_front();
        }
        if self.eta_samples.len() < ETA_SAMPLE_WINDOW {
            return None;
        }
        let mut samples = self.eta_samples.iter().copied().collect::<Vec<_>>();
        samples.sort_unstable();
        samples.get(ETA_SAMPLE_WINDOW / 2).copied()
    }
}

fn remaining_seconds(total: u64, completed: u64, bytes_per_second: u64) -> Option<u64> {
    let remaining = total.saturating_sub(completed);
    (remaining > 0 && bytes_per_second > 0).then(|| {
        remaining
            .saturating_add(bytes_per_second.saturating_sub(1))
            .saturating_div(bytes_per_second)
    })
}

#[cfg(test)]
mod tests {
    use super::{DownloadProgressAggregator, FfmpegProgress, make_progress, parse_ytdlp_progress};
    use crate::download::JobStage;

    #[test]
    fn parses_machine_ytdlp_progress_and_prefers_exact_total() {
        let progress = parse_ytdlp_progress("yt-media-progress|downloading|25|100|120|10.4|7.6");
        assert!(progress.is_some());
        if let Some(progress) = progress {
            assert_eq!(progress.completed, 25);
            assert_eq!(progress.total, Some(100));
            assert_eq!(progress.percent, Some(25.0));
            assert_eq!(progress.bytes_per_second, Some(10));
            assert_eq!(progress.eta_seconds, Some(8));
        }
    }

    #[test]
    fn finished_ytdlp_record_does_not_leak_zero_speed_or_eta() {
        let progress = parse_ytdlp_progress("yt-media-progress|finished|100|100|100|0|0");
        assert!(progress.is_some());
        if let Some(progress) = progress {
            assert_eq!(progress.percent, Some(100.0));
            assert_eq!(progress.bytes_per_second, None);
            assert_eq!(progress.eta_seconds, None);
        }
    }

    #[test]
    fn rejects_malformed_or_unbounded_progress_values() {
        assert!(parse_ytdlp_progress("download: 10%").is_none());
        assert!(
            parse_ytdlp_progress(
                "yt-media-progress|downloading|999999999999999999999999999|100||1|1"
            )
            .is_none()
        );
    }

    #[test]
    fn ffmpeg_progress_is_duration_bounded() {
        let mut parser = FfmpegProgress::new(1_000);
        let progress = parser.parse_line("out_time_us=2000000", JobStage::Converting);
        assert!(progress.is_some());
        if let Some(progress) = progress {
            assert_eq!(progress.completed, 1_000_000);
            assert_eq!(progress.percent, Some(100.0));
        }
        let late_lower_timestamp = parser.parse_line("out_time_us=500000", JobStage::Converting);
        assert_eq!(
            late_lower_timestamp.map(|progress| progress.completed),
            Some(1_000_000)
        );
    }

    #[test]
    fn multi_source_progress_without_analyzed_size_waits_for_every_total() {
        let mut aggregator = DownloadProgressAggregator::new(2, None);
        let first = aggregator.update(
            0,
            make_progress(JobStage::Downloading, 50, Some(100), Some(10), Some(5)),
        );
        assert_eq!(first.total, None);
        assert_eq!(first.percent, None);
        assert_eq!(first.eta_seconds, None);

        let combined = aggregator.update(
            1,
            make_progress(JobStage::Downloading, 25, Some(100), Some(20), Some(8)),
        );
        assert_eq!(combined.completed, 75);
        assert_eq!(combined.total, Some(200));
        assert_eq!(combined.bytes_per_second, Some(30));
        assert_eq!(combined.eta_seconds, None);
        assert_eq!(combined.percent, Some(37.5));
    }

    #[test]
    fn analyzed_size_seeds_split_stream_progress_and_reported_totals_replace_it() {
        let mut aggregator = DownloadProgressAggregator::new(2, Some(250));
        let first = aggregator.update(
            0,
            make_progress(JobStage::Downloading, 50, Some(100), Some(10), Some(5)),
        );
        assert_eq!(first.completed, 50);
        assert_eq!(first.total, Some(250));
        assert_eq!(first.percent, Some(20.0));
        assert_eq!(first.eta_seconds, None);

        let combined = aggregator.update(
            1,
            make_progress(JobStage::Downloading, 25, Some(100), Some(20), Some(8)),
        );
        assert_eq!(combined.completed, 75);
        assert_eq!(combined.total, Some(200));
        assert_eq!(combined.percent, Some(37.5));

        let changed_estimate = aggregator.update(
            1,
            make_progress(JobStage::Downloading, 30, Some(200), Some(20), Some(12)),
        );
        assert_eq!(changed_estimate.total, Some(300));
        assert_eq!(changed_estimate.percent, Some(37.5));
    }

    #[test]
    fn analyzed_size_never_claims_completion_before_all_sources_report() {
        let mut aggregator = DownloadProgressAggregator::new(2, Some(100));
        let first_finished = aggregator.update(
            0,
            make_progress(JobStage::Downloading, 100, Some(100), None, None),
        );
        assert_eq!(first_finished.percent, Some(99.0));
    }

    #[test]
    fn aggregate_eta_is_exposed_only_after_a_stable_sample_window() {
        let mut aggregator = DownloadProgressAggregator::new(1, None);
        for completed in [0, 10] {
            let progress = aggregator.update(
                0,
                make_progress(JobStage::Downloading, completed, Some(100), Some(10), None),
            );
            assert_eq!(progress.eta_seconds, None);
        }
        let stable = aggregator.update(
            0,
            make_progress(JobStage::Downloading, 20, Some(100), Some(10), None),
        );
        assert_eq!(stable.eta_seconds, Some(9));
    }
}
