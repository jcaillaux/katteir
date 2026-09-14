//! Decoder thread: feeds IVF temporal units to dav1d and hands decoded
//! pictures to the UI thread through a small bounded channel.
//!
//! Playback order: the entry clip once, then the loop clip repeatedly.
//! The channel is the pacing mechanism: the thread blocks while the queue is
//! full, and exits as soon as the receiver is dropped.

use std::error::Error;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ivf::{self, IvfIndex};

pub const FRAME_QUEUE_DEPTH: usize = 3;
/// Bound on loop repetitions: 1000 plays of an 8 s loop is over two hours.
pub const MAX_LOOP_PLAYS: u32 = 1_000;
/// Bound on send/receive round-trips for a single temporal unit.
const MAX_DRAIN_STEPS: usize = 8;

pub struct Clip {
    /// Whole file, loaded once and kept for the process lifetime so dav1d can
    /// borrow frame slices without a copy (`send_data` needs `'static`).
    pub bytes: &'static [u8],
    pub index: IvfIndex,
}

impl Clip {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let bytes: &'static [u8] = Box::leak(std::fs::read(path)?.into_boxed_slice());
        let index = ivf::parse(bytes)?;
        assert!(!index.frames.is_empty());
        Ok(Self { bytes, index })
    }

    fn frame(&self, frame_index: usize) -> &'static [u8] {
        let span = self.index.frames[frame_index];
        &self.bytes[span.offset..span.offset + span.len]
    }

    /// Whole frames per second; the spike only accepts integer rates.
    pub fn fps(&self) -> u32 {
        let den = self.index.timebase_den;
        let num = self.index.timebase_num;
        assert!(num > 0 && den.is_multiple_of(num), "non-integer frame rate {den}/{num}");
        den / num
    }
}

pub struct Frame {
    pub picture: dav1d::Picture,
    /// How many UI ticks (at the base rate) this frame stays on screen.
    pub hold_ticks: u32,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DecodeStats {
    pub frames: u64,
    pub decode_time: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stopped,
}

pub type DecodeThread = JoinHandle<Result<DecodeStats, dav1d::Error>>;

pub fn spawn(
    entry: Clip,
    looped: Clip,
    base_fps: u32,
    threads: u32,
) -> std::io::Result<(Receiver<Frame>, DecodeThread)> {
    assert!(base_fps > 0);
    assert_eq!(base_fps % entry.fps(), 0);
    assert_eq!(base_fps % looped.fps(), 0);
    let (sender, receiver) = sync_channel(FRAME_QUEUE_DEPTH);
    let handle = std::thread::Builder::new()
        .name("av1-decode".into())
        .spawn(move || run(&entry, &looped, base_fps, threads, &sender))?;
    Ok((receiver, handle))
}

fn run(
    entry: &Clip,
    looped: &Clip,
    base_fps: u32,
    threads: u32,
    sender: &SyncSender<Frame>,
) -> Result<DecodeStats, dav1d::Error> {
    let mut settings = dav1d::Settings::new();
    settings.set_n_threads(threads);
    settings.set_max_frame_delay(1);
    let mut decoder = dav1d::Decoder::with_settings(&settings)?;
    let mut stats = DecodeStats::default();
    // Reused for every temporal unit: no per-frame growth after the first.
    let mut out = Vec::with_capacity(MAX_DRAIN_STEPS + 1);

    let entry_hold = base_fps / entry.fps();
    if play(&mut decoder, entry, entry_hold, sender, &mut out, &mut stats)? == Flow::Stopped {
        return Ok(stats);
    }
    let loop_hold = base_fps / looped.fps();
    for _ in 0..MAX_LOOP_PLAYS {
        if play(&mut decoder, looped, loop_hold, sender, &mut out, &mut stats)? == Flow::Stopped {
            break;
        }
    }
    Ok(stats)
}

fn play(
    decoder: &mut dav1d::Decoder,
    clip: &Clip,
    hold_ticks: u32,
    sender: &SyncSender<Frame>,
    out: &mut Vec<dav1d::Picture>,
    stats: &mut DecodeStats,
) -> Result<Flow, dav1d::Error> {
    assert!(hold_ticks >= 1);
    for frame_index in 0..clip.index.frames.len() {
        out.clear();
        let started = Instant::now();
        feed(decoder, clip.frame(frame_index), out)?;
        stats.decode_time += started.elapsed();
        debug_assert!(out.len() <= 1, "one shown frame per temporal unit expected");
        // Drain outside the timed section: `send` blocks while the queue is full.
        for picture in out.drain(..) {
            stats.frames += 1;
            if sender.send(Frame { picture, hold_ticks }).is_err() {
                return Ok(Flow::Stopped);
            }
        }
    }
    Ok(Flow::Continue)
}

/// Sends one temporal unit and collects the pictures it produced into `out`.
fn feed(
    decoder: &mut dav1d::Decoder,
    data: &'static [u8],
    out: &mut Vec<dav1d::Picture>,
) -> Result<(), dav1d::Error> {
    assert!(!data.is_empty());
    let mut sent = decoder.send_data(data, None, None, None);
    for _ in 0..MAX_DRAIN_STEPS {
        match sent {
            Ok(()) => break,
            // A finished picture blocks the input: take it out, then retry.
            Err(error) if error.is_again() => {
                take_picture(decoder, out)?;
                sent = decoder.send_pending_data();
            }
            Err(error) => return Err(error),
        }
    }
    sent?;
    take_picture(decoder, out)?;
    assert!(out.len() <= MAX_DRAIN_STEPS + 1);
    Ok(())
}

fn take_picture(
    decoder: &mut dav1d::Decoder,
    out: &mut Vec<dav1d::Picture>,
) -> Result<(), dav1d::Error> {
    match decoder.get_picture() {
        Ok(picture) => {
            out.push(picture);
            Ok(())
        }
        Err(error) if error.is_again() => Ok(()),
        Err(error) => Err(error),
    }
}
