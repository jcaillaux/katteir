//! Decoder thread, ported from the AV1 video spike (`spikes/av1-video` on
//! the `experiment` branch): feeds IVF temporal units to dav1d and hands
//! decoded pictures to the UI thread through a small bounded channel. Order:
//! the entry clip once, then the loop clip until the `Decoder` is dropped.
//!
//! The channel does the pacing: the thread blocks while the queue is full.
//! The UI takes a new frame only after the previous one was drawn, so a
//! hidden or throttled window stops decoding instead of burning CPU.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;

use super::{Clip, FrameData};

pub const FRAME_QUEUE_DEPTH: usize = 3;
/// Bound on loop repetitions: 10 000 plays of an 8 s loop is over 22 hours.
pub const MAX_LOOP_PLAYS: u32 = 10_000;
/// Bound on send/receive round-trips for a single temporal unit.
const MAX_DRAIN_STEPS: usize = 8;

pub struct Frame {
    pub picture: dav1d::Picture,
    /// How many UI ticks (at the decoder's base rate) this frame stays up.
    pub hold_ticks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stopped,
}

/// A running decoder. Dropping it stops the thread and waits for it.
pub struct Decoder {
    frames: Option<Receiver<Frame>>,
    thread: Option<JoinHandle<Result<(), dav1d::Error>>>,
    base_fps: u32,
}

impl Decoder {
    /// Starts decoding. The two clips' frame rates must both divide the
    /// faster one (e.g. 30 and 15), because the UI ticks at that rate.
    pub fn start(entry: Clip, looped: Clip) -> std::io::Result<Self> {
        let base_fps = entry.fps().max(looped.fps());
        assert!(
            base_fps.is_multiple_of(entry.fps()) && base_fps.is_multiple_of(looped.fps()),
            "clip frame rates must divide the base rate"
        );
        let (sender, receiver) = sync_channel(FRAME_QUEUE_DEPTH);
        let thread = std::thread::Builder::new()
            .name("decode".into())
            .spawn(move || run(&entry, &looped, base_fps, &sender))?;
        Ok(Self { frames: Some(receiver), thread: Some(thread), base_fps })
    }

    pub fn base_fps(&self) -> u32 {
        self.base_fps
    }

    /// The next decoded frame, if one is ready.
    pub fn try_next(&self) -> Result<Frame, TryRecvError> {
        let Some(frames) = &self.frames else {
            return Err(TryRecvError::Disconnected);
        };
        frames.try_recv()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // Dropping the receiver makes a blocked `send` fail, so the thread exits.
        drop(self.frames.take());
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::warn!("decoder stopped with an error: {error}"),
                Err(_) => log::error!("decoder thread panicked"),
            }
        }
    }
}

/// True when two clips can play back to back (see `Decoder::start`).
pub fn rates_compatible(entry: &Clip, looped: &Clip) -> bool {
    let base = entry.fps().max(looped.fps());
    base.is_multiple_of(entry.fps()) && base.is_multiple_of(looped.fps())
}

fn run(entry: &Clip, looped: &Clip, base_fps: u32, sender: &SyncSender<Frame>) -> Result<(), dav1d::Error> {
    let mut settings = dav1d::Settings::new();
    // One thread: a 720p frame takes ~5 ms, well inside a 33 ms frame.
    settings.set_n_threads(1);
    settings.set_max_frame_delay(1);
    let mut decoder = dav1d::Decoder::with_settings(&settings)?;
    // Reused for every temporal unit: no per-frame growth after the first.
    let mut out = Vec::with_capacity(MAX_DRAIN_STEPS + 1);
    if play(&mut decoder, entry, base_fps / entry.fps(), sender, &mut out)? == Flow::Stopped {
        return Ok(());
    }
    for _ in 0..MAX_LOOP_PLAYS {
        if play(&mut decoder, looped, base_fps / looped.fps(), sender, &mut out)? == Flow::Stopped {
            break;
        }
    }
    Ok(())
}

fn play(
    decoder: &mut dav1d::Decoder,
    clip: &Clip,
    hold_ticks: u32,
    sender: &SyncSender<Frame>,
    out: &mut Vec<dav1d::Picture>,
) -> Result<Flow, dav1d::Error> {
    assert!(hold_ticks >= 1);
    for frame_index in 0..clip.frame_count() {
        out.clear();
        feed(decoder, clip.frame_data(frame_index), out)?;
        debug_assert!(out.len() <= 1, "one shown frame per temporal unit expected");
        for picture in out.drain(..) {
            if sender.send(Frame { picture, hold_ticks }).is_err() {
                return Ok(Flow::Stopped);
            }
        }
    }
    Ok(Flow::Continue)
}

/// Sends one temporal unit and collects the pictures it produced into `out`.
fn feed(decoder: &mut dav1d::Decoder, data: FrameData, out: &mut Vec<dav1d::Picture>) -> Result<(), dav1d::Error> {
    assert!(!data.as_ref().is_empty());
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

fn take_picture(decoder: &mut dav1d::Decoder, out: &mut Vec<dav1d::Picture>) -> Result<(), dav1d::Error> {
    match decoder.get_picture() {
        Ok(picture) => {
            out.push(picture);
            Ok(())
        }
        Err(error) if error.is_again() => Ok(()),
        Err(error) => Err(error),
    }
}
