//! Coalesce text frames and prioritize them between complete image packets.
use crate::{
    metrics::Metrics,
    terminal::{DiffWriter, TtyWriter, STOP},
};
use anyhow::Result;
use ncm_model::Grid;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
struct Frame {
    grid: Grid,
    input: bool,
    selected: usize,
    quick: bool,
}
struct Packet {
    bytes: Vec<u8>,
    quick: bool,
}
#[derive(Default)]
struct Queue {
    frame: Option<Frame>,
    packets: VecDeque<Packet>,
    bytes: usize,
    invalidate: bool,
}
pub struct Output {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    pause_images: Arc<AtomicBool>,
    pause_all: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stopped: Arc<AtomicBool>,
    gate: Arc<Mutex<()>>,
}
impl Output {
    pub fn start(log: Option<&std::path::Path>) -> Result<Self> {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let shared = queue.clone();
        let pause_images = Arc::new(AtomicBool::new(false));
        let images_paused = pause_images.clone();
        let pause_all = Arc::new(AtomicBool::new(false));
        let all_paused = pause_all.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let gate = Arc::new(Mutex::new(()));
        let output_gate = gate.clone();
        let mut metrics = Metrics::new(false, log)?;
        let thread = std::thread::Builder::new()
            .name("ncm-output".into())
            .spawn(move || {
                #[cfg(test)]
                {
                    // Also exercise shutdown with a poisoned queue and a second
                    // panic in the main thread that was waiting to submit a frame.
                    let _guard = (std::env::var("NCM_TEST_PANIC").as_deref() == Ok("ncm-output"))
                        .then(|| shared.0.lock().unwrap());
                    crate::panic_log::inject("ncm-output");
                }
                let mut writer = DiffWriter::default();
                let mut first = true;
                while !stop.load(Ordering::Relaxed) && !STOP.load(Ordering::Relaxed) {
                    let guard = output_gate.lock().unwrap();
                    if all_paused.load(Ordering::Acquire) {
                        drop(guard);
                        let queue = shared.0.lock().unwrap();
                        let _wait = shared.1.wait_timeout(queue, Duration::from_millis(50));
                        continue;
                    }
                    let (frame, packet) = {
                        let mut queue = shared.0.lock().unwrap();
                        if queue.invalidate {
                            writer = DiffWriter::default();
                            queue.invalidate = false;
                        }
                        let frame = queue.frame.take();
                        let packet = if frame.is_none() && !images_paused.load(Ordering::Relaxed) {
                            queue.packets.pop_front()
                        } else {
                            None
                        };
                        if let Some(packet) = &packet {
                            queue.bytes -= packet.bytes.len();
                        }
                        (frame, packet)
                    };
                    if let Some(frame) = frame {
                        let started = Instant::now();
                        let bytes = match writer.draw(frame.grid, None, &[], &mut TtyWriter) {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                STOP.store(true, Ordering::Relaxed);
                                break;
                            }
                        };
                        metrics.trace(
                            if first { "first_frame" } else { "paint" },
                            &format!(
                                "duration_ms={:.3} bytes={bytes} images=0",
                                started.elapsed().as_secs_f64() * 1000.
                            ),
                        );
                        first = false;
                        if frame.input {
                            metrics.trace(
                                "input_painted",
                                &format!("selected={} quick={}", frame.selected, frame.quick),
                            );
                        }
                    } else if let Some(packet) = packet {
                        let started = Instant::now();
                        match TtyWriter.write_graphics(&packet.bytes) {
                            Ok(true) => {}
                            Ok(false) => {
                                metrics.trace("graphics_suppressed", "viewing client changed");
                                continue;
                            }
                            Err(_) => {
                                STOP.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        metrics.trace(
                            "image_flush",
                            &format!(
                                "bytes={} duration_ms={:.3}",
                                packet.bytes.len(),
                                started.elapsed().as_secs_f64() * 1000.
                            ),
                        );
                        // m=0 completes a strip. Trace the first actual picture, not its first chunk.
                        if packet.quick && packet.bytes.windows(3).any(|s| s == b"m=0") {
                            metrics
                                .trace("quick_picture", &format!("bytes={}", packet.bytes.len()));
                        }
                    } else {
                        drop(guard);
                        let queue = shared.0.lock().unwrap();
                        if queue.frame.is_none()
                            && (queue.packets.is_empty() || images_paused.load(Ordering::Relaxed))
                        {
                            let _wait = shared.1.wait_timeout(queue, Duration::from_millis(50));
                        }
                    }
                }
            })?;
        Ok(Self {
            queue,
            pause_images,
            pause_all,
            thread: Some(thread),
            stopped,
            gate,
        })
    }
    pub fn finish(&mut self, cleanup: &[u8]) {
        self.stopped.store(true, Ordering::Relaxed);
        self.queue.1.notify_all();
        STOP.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Best effort only: terminal mode cleanup must never wait for a dead/full tty.
        if crate::terminal::tty_alive() && !cleanup.is_empty() && std::env::var_os("TMUX").is_none()
        {
            unsafe {
                libc::write(libc::STDOUT_FILENO, cleanup.as_ptr().cast(), cleanup.len());
            }
        }
    }
    pub fn can_upload(&self) -> bool {
        self.queue.0.lock().unwrap().bytes < 16_384
    }
    pub fn pause(&self, hidden: bool, using: bool) {
        let images = self.pause_images.swap(hidden || using, Ordering::Relaxed);
        let all = self.pause_all.swap(using, Ordering::Relaxed);
        if images != (hidden || using) || all != using {
            self.queue.1.notify_one();
        }
    }
    /// Wait for a packet/frame already being written before releasing the terminal.
    pub fn suspend(&self) {
        self.pause(true, true);
        let _guard = self.gate.lock().unwrap();
        let mut queue = self.queue.0.lock().unwrap();
        queue.frame = None;
        queue.packets.clear();
        queue.bytes = 0;
        queue.invalidate = true;
    }
    pub fn invalidate(&self) {
        let mut queue = self.queue.0.lock().unwrap();
        queue.invalidate = true;
        queue.packets.clear();
        queue.bytes = 0;
        self.queue.1.notify_one();
    }
    pub fn submit(
        &self,
        grid: Option<Grid>,
        images: Vec<u8>,
        quick_bytes: usize,
        input: bool,
        state: &crate::input::State,
    ) {
        if grid.is_none() && images.is_empty() {
            return;
        }
        let mut queue = self.queue.0.lock().unwrap();
        if let Some(grid) = grid {
            let input = input || queue.frame.as_ref().is_some_and(|frame| frame.input);
            queue.frame = Some(Frame {
                grid,
                input,
                selected: state.selected,
                quick: state.quick,
            });
        }
        let prefix: &[u8] = b"\x1b_G";
        let mut boundaries = images
            .windows(prefix.len())
            .enumerate()
            .filter_map(|(i, packet)| (packet == prefix).then_some(i))
            .collect::<Vec<_>>();
        boundaries.push(images.len());
        let quick_start = images.len().saturating_sub(quick_bytes);
        for span in boundaries.windows(2) {
            let bytes = images[span[0]..span[1]].to_vec();
            queue.bytes += bytes.len();
            queue.packets.push_back(Packet {
                bytes,
                quick: quick_bytes > 0 && span[0] >= quick_start,
            });
        }
        self.queue.1.notify_one();
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.finish(&[]);
    }
}
