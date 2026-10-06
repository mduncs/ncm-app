use crate::TranscriptAdapter;
use anyhow::{bail, Context, Result};
use ncm_model::{SessionRef, TalkEvent};
use serde_json::Value;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::PathBuf,
};

/// Byte-offset JSONL transport. Original files are reopened read-only to detect replacement.
#[derive(Debug)]
pub struct TranscriptFollower {
    path: PathBuf,
    offset: u64,
    identity: Option<(u64, u64)>,
    boundary: Vec<u8>,
    observed: Option<(u64, Option<std::time::SystemTime>)>,
    waiting: bool,
    decision: UnansweredDecision,
    /// Maximum lines per poll; streaming initial loads call poll_adapter until caught up.
    pub max_records: usize,
    /// Maximum allocated line bytes; oversize complete lines are counted and skipped.
    pub max_line_bytes: usize,
    /// Cumulative malformed/oversize complete lines, including across file resets.
    pub malformed_lines: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptBatch {
    pub records: Vec<Value>,
    /// Caller must clear old events and adapter state before consuming this batch.
    pub reset: bool,
}

/// Streaming alternative to raw batches: keeps only emitted text and compact adapter state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptEvents {
    pub events: Vec<TalkEvent>,
    pub reset: bool,
    pub records_read: usize,
    pub lines_read: usize,
}

/// Body-free attention metadata, retained independently of presentation history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnansweredDecision {
    pub count: usize,
    pub oldest: Option<UnansweredRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnansweredRecord {
    pub timestamp: Option<ncm_model::Timestamp>,
    pub timestamp_text: Option<String>,
    pub uuid: Option<String>,
    pub record_type: String,
    pub origin_kind: Option<String>,
    pub reason: &'static str,
}

impl TranscriptFollower {
    pub fn new(session: &SessionRef) -> Self {
        Self {
            path: session.transcript_path.clone(),
            offset: 0,
            identity: None,
            boundary: Vec::new(),
            observed: None,
            waiting: false,
            decision: UnansweredDecision::default(),
            max_records: 1024,
            max_line_bytes: 16 * 1024 * 1024,
            malformed_lines: 0,
        }
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Compatibility API: raw values are transient and bounded by max_records/max_line_bytes.
    /// Malformed complete lines advance the offset; a partial final line waits for its newline.
    pub fn poll(&mut self) -> Result<TranscriptBatch> {
        let mut records = Vec::new();
        let progress = self.scan(false, |record| {
            if let Some(record) = record {
                records.push(record.clone());
            }
            Ok(())
        })?;
        Ok(TranscriptBatch {
            records,
            reset: progress.reset,
        })
    }

    /// Consume each raw value immediately instead of allocating a whole batch of raw records.
    /// Resets the adapter before any records from a truncated/replaced transcript are consumed.
    pub fn poll_adapter(
        &mut self,
        adapter: &mut dyn TranscriptAdapter,
    ) -> Result<TranscriptEvents> {
        self.poll_adapter_bounded(adapter, usize::MAX, usize::MAX, usize::MAX)
    }

    /// Bound presentation text during scanning, before a catch-up batch can accumulate.
    pub fn poll_adapter_bounded(
        &mut self,
        adapter: &mut dyn TranscriptAdapter,
        max_events: usize,
        max_bytes: usize,
        max_body: usize,
    ) -> Result<TranscriptEvents> {
        let mut events = std::collections::VecDeque::<TalkEvent>::new();
        let mut bytes = 0usize;
        let mut decision = self.decision.clone();
        let progress = self.scan(true, |record| {
            match record {
                Some(record) => {
                    let emitted = adapter.consume(record)?;
                    if adapter.answers_pending(record, &emitted) {
                        decision = UnansweredDecision::default();
                    }
                    for mut event in emitted {
                        if let TalkEvent::You { timestamp, .. } = &event {
                            decision.count += 1;
                            decision.oldest.get_or_insert_with(|| UnansweredRecord {
                                timestamp: *timestamp,
                                timestamp_text: record["attachment"]["timestamp"]
                                    .as_str()
                                    .or_else(|| record["timestamp"].as_str())
                                    .map(str::to_owned),
                                uuid: record["uuid"].as_str().map(str::to_owned),
                                record_type: record["type"].as_str().unwrap_or("unknown").into(),
                                origin_kind: record["origin"]["kind"]
                                    .as_str()
                                    .or_else(|| record["attachment"]["origin"]["kind"].as_str())
                                    .map(str::to_owned),
                                reason: if record["attachment"]["type"] == "queued_command" {
                                    "non-report queued command; no subsequent assistant text/tool activity"
                                } else {
                                    "human prompt; no subsequent assistant text/tool activity"
                                },
                            });
                        }
                        let body = match &mut event {
                            TalkEvent::You { body, .. }
                            | TalkEvent::Reply { body, .. }
                            | TalkEvent::Report { body, .. }
                            | TalkEvent::ReplyToReport { body, .. } => body,
                        };
                        if body.len() > max_body {
                            let mut end = max_body;
                            while !body.is_char_boundary(end) {
                                end -= 1;
                            }
                            body.truncate(end);
                            body.push_str("\n… display clipped; full text remains in transcript");
                            body.shrink_to_fit();
                        }
                        bytes = bytes.saturating_add(event.body().len());
                        events.push_back(event);
                        while events.len() > max_events || bytes > max_bytes {
                            let Some(old) = events.pop_front() else {
                                break;
                            };
                            bytes -= old.body().len();
                        }
                    }
                }
                None => {
                    adapter.reset();
                    events.clear();
                    bytes = 0;
                    decision = UnansweredDecision::default();
                }
            }
            Ok(())
        })?;
        self.decision = decision;
        Ok(TranscriptEvents {
            events: events.into(),
            ..progress
        })
    }

    /// Attention state survives eviction of old presentation text during catch-up.
    pub fn unanswered_at(&self, now: ncm_model::Timestamp) -> Option<crate::Unanswered> {
        self.decision
            .oldest
            .as_ref()
            .map(|record| crate::Unanswered {
                timestamp: record.timestamp,
                age: record.timestamp.map(|time| time.age_at(now)),
                replies_to_reports: 0,
            })
    }

    /// Metadata only: never includes transcript bodies or tool arguments.
    pub fn unanswered_decision(&self) -> &UnansweredDecision {
        &self.decision
    }

    fn scan(
        &mut self,
        projected: bool,
        mut visit: impl FnMut(Option<&Value>) -> Result<()>,
    ) -> Result<TranscriptEvents> {
        if self.max_records == 0 || self.max_line_bytes == 0 {
            bail!("transcript limits must be nonzero");
        }
        let mut file = File::open(&self.path)
            .with_context(|| format!("open transcript {}", self.path.display()))?;
        let metadata = file.metadata()?;
        let identity = file_identity(&metadata);
        let mut reset =
            self.identity.is_some_and(|old| old != identity) || metadata.len() < self.offset;
        let observed = (metadata.len(), metadata.modified().ok());
        if !reset && self.waiting && self.observed == Some(observed) {
            return Ok(TranscriptEvents::default());
        }
        if !reset && !self.boundary.is_empty() {
            file.seek(SeekFrom::Start(self.offset - self.boundary.len() as u64))?;
            let mut current = vec![0; self.boundary.len()];
            file.read_exact(&mut current)?;
            reset = current != self.boundary;
        }
        if reset {
            self.offset = 0;
            visit(None)?;
        }
        self.observed = Some(observed);
        self.waiting = false;
        self.identity = Some(identity);
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut progress = TranscriptEvents {
            reset,
            ..TranscriptEvents::default()
        };
        let mut line = Vec::new();
        for _ in 0..self.max_records {
            line.clear();
            // Small records use the fast slice parser. Large projected records are scanned
            // for their newline, then deserialized from the reader without allocating the line.
            let allocation_limit = if projected {
                self.max_line_bytes.saturating_add(1).min(256 * 1024)
            } else {
                self.max_line_bytes.saturating_add(1)
            };
            let count = (&mut reader)
                .take(allocation_limit as u64)
                .read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            let mut bytes = count as u64;
            let mut complete = line.last() == Some(&b'\n');
            let streamed = projected && !complete && count == allocation_limit;
            if !complete && count == allocation_limit {
                loop {
                    let buffer = reader.fill_buf()?;
                    if buffer.is_empty() {
                        break;
                    }
                    let newline = buffer.iter().position(|byte| *byte == b'\n');
                    let consumed = newline.map_or(buffer.len(), |index| index + 1);
                    bytes += consumed as u64;
                    reader.consume(consumed);
                    if newline.is_some() {
                        complete = true;
                        break;
                    }
                }
            }
            let oversize = bytes > self.max_line_bytes as u64;
            if !complete {
                break;
            }
            self.offset += bytes;
            progress.lines_read += 1;
            if oversize {
                self.malformed_lines += 1;
                continue;
            }
            if !streamed && line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let parsed = if streamed {
                reader.seek_relative(-(bytes as i64))?;
                let parsed = serde_json::from_reader::<_, crate::projection::Record>(
                    (&mut reader).take(bytes),
                )
                .and_then(serde_json::to_value);
                // Malformed records can fail before their newline; restore the committed end.
                reader.seek(SeekFrom::Start(self.offset))?;
                parsed
            } else if projected {
                serde_json::from_slice::<crate::projection::Record>(&line)
                    .and_then(serde_json::to_value)
            } else {
                serde_json::from_slice::<Value>(&line)
            };
            match parsed {
                Ok(record) => {
                    progress.records_read += 1;
                    visit(Some(&record))?;
                }
                Err(_) => self.malformed_lines += 1,
            }
        }
        let count = self.offset.min(64) as usize;
        self.waiting = progress.lines_read < self.max_records;
        self.boundary.resize(count, 0);
        if count > 0 {
            reader.seek(SeekFrom::Start(self.offset - count as u64))?;
            reader.read_exact(&mut self.boundary)?;
        }
        Ok(progress)
    }
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}
#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}
