//! The meeting's sound, kept: one stereo Ogg Opus file (RFC 7845) with "Me" (the microphone) on
//! the left and "Them" (the computer's sound) on the right. One file plays in the window's
//! <audio> element (WebKitGTK/GStreamer, WebView2, WKWebView), and the two tracks still come
//! back apart: to transcribe them again, or to tell the voices apart. The idea (Ogg Opus, a page
//! each second so that a crash loses little) is from Yogurt (MIT).
//!
//! The two sources send 16 kHz mono blocks on their own threads, each at its position on the
//! meeting clock. The encoder takes 16 kHz directly (Opus accepts 8/12/16/24/48 kHz), so there
//! is no resampler; the Ogg granule positions are always at 48 kHz (3 per input sample).
//! A frame (20 ms) is encoded when both channels have reached its end, or when one channel is
//! more than 3 s ahead of the other: then the late channel gets silence, and a late block for
//! time already encoded is dropped. So memory stays at about 3 s per channel.
//!
//! 36 kbit/s stereo, VOIP (speech), about 16 MB per hour of talk. Measured with two different
//! sounds on left and right: each leaks into the other channel at -68 dB or less at 36 kbit/s,
//! -58 dB at 32, but -34 dB at 28 and -25 dB at 24 (the encoder makes the stereo narrower when
//! bits are few). 36 keeps a margin above that knee. Two separate mono streams (mapping family
//! 1) would have no leak at all, but fewer players know them.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ogg::reading::PacketReader;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Decoder, Encoder};

/// Samples per second of each channel, in and out.
pub const RATE: usize = 16_000;
/// 20 ms: the usual Opus frame for speech.
const FRAME: usize = RATE / 50;
/// Ogg Opus counts the granule positions at 48 kHz, whatever the input rate.
const GRANULE_PER_SAMPLE: u64 = 48_000 / RATE as u64;
const BITRATE: i32 = 36_000;
/// A page each second: a crash loses at most about one second (plus what is still buffered).
const FRAMES_PER_PAGE: u32 = 50;
/// A channel this far ahead of the other makes the encoder go on; the other gets silence.
const AHEAD: usize = 3 * RATE;
/// Silence for a gap is added in pieces of this size, and frames are encoded after each piece.
/// Thus a long gap never fills memory.
const PIECE: usize = RATE;
/// The longest meeting. A position past it is bad input, not hours of silence to encode.
pub const MAX_S: usize = 12 * 3600;
/// The largest Opus packet is 1275 bytes per frame.
const MAX_PACKET: usize = 1500;
/// The largest Opus packet decodes to 120 ms.
const MAX_DECODED: usize = RATE * 120 / 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The microphone: the left channel.
    Me,
    /// The computer's sound: the right channel.
    Them,
}

impl Channel {
    fn index(self) -> usize {
        match self {
            Channel::Me => 0,
            Channel::Them => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// The meeting's length: the end of the later channel.
    pub seconds: f64,
    /// The size of the file.
    pub bytes: u64,
}

/// Writes the meeting's file while it records. The two sources are on different threads: keep
/// the writer behind a lock. Dropped before `finish`, the file stays: it plays up to its last
/// page (the same as after a crash).
pub struct Writer {
    ogg: PacketWriter<'static, File>,
    enc: Encoder,
    serial: u32,
    /// The encoder's look-ahead at 48 kHz: the player drops this much at the start.
    pre_skip: u64,
    /// Per channel: the samples from `done` on that are not encoded yet.
    lanes: [VecDeque<f32>; 2],
    /// Samples (per channel) given to the encoder: always a whole number of frames.
    done: usize,
    /// The meeting's length so far: the end of the later channel, silence included.
    end: usize,
    in_page: u32,
    /// One frame, left and right interleaved, as the encoder wants it.
    frame: Vec<f32>,
    packet: Vec<u8>,
    path: PathBuf,
}

impl Writer {
    pub fn create(path: &Path) -> Result<Self> {
        let mut enc = Encoder::new(RATE as u32, Channels::Stereo, Application::Voip)?;
        enc.set_bitrate(Bitrate::Bits(BITRATE))?;
        // The look-ahead is given at the input rate; the header wants it at 48 kHz.
        let pre_skip = enc.get_lookahead()?.max(0) as u64 * GRANULE_PER_SAMPLE;
        let pre_skip_u16 = u16::try_from(pre_skip).map_err(|_| anyhow!("the encoder's look-ahead is too long"))?;

        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600); // a recording is private
        }
        let file = o.open(path).with_context(|| format!("cannot create {}", path.display()))?;
        let mut w = Self {
            ogg: PacketWriter::new(file),
            enc,
            serial: serial(),
            pre_skip,
            lanes: [VecDeque::with_capacity(AHEAD + PIECE), VecDeque::with_capacity(AHEAD + PIECE)],
            done: 0,
            end: 0,
            in_page: 0,
            frame: Vec::with_capacity(2 * FRAME),
            packet: vec![0; MAX_PACKET],
            path: path.to_path_buf(),
        };

        // RFC 7845 5.1: the identification header, alone on the first page.
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(2); // channels
        head.extend_from_slice(&pre_skip_u16.to_le_bytes());
        head.extend_from_slice(&(RATE as u32).to_le_bytes()); // the input's rate (information only)
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family 0: mono or stereo (left, right) in one stream
        w.ogg.write_packet(head, w.serial, PacketWriteEndInfo::EndPage, 0)?;

        // RFC 7845 5.2: the comment header. The tag tells a reader which side is which.
        let vendor = format!("Lowkey ({})", opus::version());
        let comments = ["LOWKEY_CHANNELS=left:me,right:them"];
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for c in comments {
            tags.extend_from_slice(&(c.len() as u32).to_le_bytes());
            tags.extend_from_slice(c.as_bytes());
        }
        w.ogg.write_packet(tags, w.serial, PacketWriteEndInfo::EndPage, 0)?;
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds a block of one channel at its position (samples on the meeting clock). As in
    /// `voices::Tape::write_at`: a gap before the block is silence, and a block that starts
    /// before the channel's end keeps only its new part. A block for time already encoded is
    /// dropped (the channel was more than 3 s late).
    pub fn write_at(&mut self, ch: Channel, block: &[f32], at: usize) -> Result<()> {
        let i = ch.index();
        let lane_end = self.done + self.lanes[i].len();
        let skip = lane_end.saturating_sub(at).min(block.len());
        let new = &block[skip..];
        let from = at.saturating_add(skip);
        let to = from.saturating_add(new.len());
        if to > MAX_S * RATE {
            bail!("the meeting is longer than {} hours", MAX_S / 3600);
        }
        if new.is_empty() {
            return Ok(());
        }
        let mut gap = from - lane_end; // `from` is never before `lane_end` when `new` is not empty
        while gap > 0 {
            let n = gap.min(PIECE);
            self.lanes[i].extend(std::iter::repeat(0.0).take(n));
            gap -= n;
            self.drain()?;
        }
        for piece in new.chunks(PIECE) {
            // A bad sample (NaN, infinite, too loud) must not reach the encoder.
            self.lanes[i].extend(piece.iter().map(|&s| if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 }));
            self.drain()?;
        }
        self.end = self.end.max(to);
        Ok(())
    }

    /// Encodes the frames that are ready: both channels have them, or one channel is too far ahead.
    fn drain(&mut self) -> Result<()> {
        loop {
            let (a, b) = (self.lanes[0].len(), self.lanes[1].len());
            let (hi, lo) = (a.max(b), a.min(b));
            if lo >= FRAME || (hi >= FRAME && hi - lo > AHEAD) {
                self.emit(None)?;
            } else {
                return Ok(());
            }
        }
    }

    /// Encodes one frame; a channel without samples gives silence. `last`: the granule position
    /// of the stream's end, for the last packet.
    fn emit(&mut self, last: Option<u64>) -> Result<()> {
        let Self { lanes, frame, .. } = self;
        frame.clear();
        for _ in 0..FRAME {
            frame.push(lanes[0].pop_front().unwrap_or(0.0));
            frame.push(lanes[1].pop_front().unwrap_or(0.0));
        }
        let n = self.enc.encode_float(&self.frame, &mut self.packet)?;
        self.done += FRAME;
        self.in_page += 1;
        // A page's granule position counts all the samples decoded up to its end, look-ahead
        // included. The last page's is smaller: so the player cuts off the padding at the end.
        let (info, granule) = match last {
            Some(g) => (PacketWriteEndInfo::EndStream, g),
            None if self.in_page >= FRAMES_PER_PAGE => (PacketWriteEndInfo::EndPage, self.done as u64 * GRANULE_PER_SAMPLE),
            None => (PacketWriteEndInfo::NormalPacket, self.done as u64 * GRANULE_PER_SAMPLE),
        };
        if info != PacketWriteEndInfo::NormalPacket {
            self.in_page = 0;
        }
        // The page goes straight to the file (no buffer of ours): after a crash it is there.
        self.ogg.write_packet(self.packet[..n].to_vec(), self.serial, info, granule)?;
        Ok(())
    }

    /// Encodes the rest (the later channel's end is the meeting's end; the other gets silence),
    /// flushes the encoder's look-ahead and closes the stream.
    pub fn finish(mut self) -> Result<Stats> {
        let needed = self.pre_skip + self.end as u64 * GRANULE_PER_SAMPLE;
        loop {
            let after = (self.done + FRAME) as u64 * GRANULE_PER_SAMPLE;
            let last = after >= needed;
            self.emit(last.then_some(needed))?;
            if last {
                break;
            }
        }
        let file = self.ogg.inner_mut();
        file.sync_data()?;
        Ok(Stats { seconds: self.end as f64 / RATE as f64, bytes: file.metadata()?.len() })
    }
}

/// The stream's serial number. Any value works for a file with one stream; a different one per
/// file only helps a tool that joins files.
fn serial() -> u32 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (t as u32) ^ ((t >> 32) as u32) ^ std::process::id().rotate_left(16)
}

/// The OpusHead fields a reader needs: (channels, pre-skip at 48 kHz).
fn head(data: &[u8]) -> Result<(u8, u64)> {
    if data.len() < 19 || &data[..8] != b"OpusHead" {
        bail!("not an Ogg Opus file");
    }
    if data[8] >> 4 != 0 {
        bail!("an Ogg Opus version this reader does not know");
    }
    let (channels, family) = (data[9], data[18]);
    if family != 0 || !(1..=2).contains(&channels) {
        bail!("not a mono or stereo Ogg Opus file");
    }
    Ok((channels, u16::from_le_bytes([data[10], data[11]]) as u64))
}

/// Decodes a file at 16 kHz and gives (me, them) in pieces. A file a crash cut short gives what
/// its whole pages hold. A mono file gives the same samples on both sides.
pub fn read_with(path: &Path, mut sink: impl FnMut(&[f32], &[f32])) -> Result<()> {
    let mut r = PacketReader::new(BufReader::new(File::open(path).with_context(|| format!("cannot open {}", path.display()))?));
    let first = r.read_packet()?.ok_or_else(|| anyhow!("an empty file"))?;
    let (_, pre_skip) = head(&first.data)?;
    let serial = first.stream_serial();
    // Decoded at 16 kHz: the pre-skip and the end are in 48 kHz units, so divide by 3.
    let pre = (pre_skip / GRANULE_PER_SAMPLE) as usize;
    let mut dec = Decoder::new(RATE as u32, Channels::Stereo)?;
    let mut pcm = vec![0f32; 2 * MAX_DECODED];
    let (mut me, mut them) = (Vec::with_capacity(MAX_DECODED), Vec::with_capacity(MAX_DECODED));
    let mut decoded = 0usize; // samples out of the decoder, the pre-skip included
    let mut tags = false;
    // An error is a page cut short or damaged: the file ends there.
    while let Ok(Some(p)) = r.read_packet() {
        if p.stream_serial() != serial {
            continue;
        }
        if !tags {
            tags = true;
            if p.data.starts_with(b"OpusTags") {
                continue;
            }
        }
        let n = match dec.decode_float(&p.data, &mut pcm, false) {
            Ok(n) => n,
            Err(_) => break,
        };
        let start = decoded;
        decoded += n;
        let lo = pre.saturating_sub(start).min(n);
        let mut hi = n;
        if p.last_in_stream() && p.absgp_page() != u64::MAX {
            let end = (p.absgp_page() / GRANULE_PER_SAMPLE) as usize;
            hi = end.saturating_sub(start).min(n);
        }
        if hi > lo {
            me.clear();
            them.clear();
            for k in lo..hi {
                me.push(pcm[2 * k]);
                them.push(pcm[2 * k + 1]);
            }
            sink(&me, &them);
        }
        if p.last_in_stream() {
            break;
        }
    }
    Ok(())
}

/// The whole file at 16 kHz: (me, them). One hour is about 460 MB of samples; use `read_with`
/// to take it in pieces.
pub fn read(path: &Path) -> Result<(Vec<f32>, Vec<f32>)> {
    let (mut me, mut them) = (Vec::new(), Vec::new());
    read_with(path, |a, b| {
        me.extend_from_slice(a);
        them.extend_from_slice(b);
    })?;
    Ok((me, them))
}

/// The length in seconds, from the pages' granule positions (no decoding). A file a crash cut
/// short has the length of its last whole page.
pub fn duration(path: &Path) -> Result<f64> {
    let mut r = PacketReader::new(BufReader::new(File::open(path).with_context(|| format!("cannot open {}", path.display()))?));
    let first = r.read_packet()?.ok_or_else(|| anyhow!("an empty file"))?;
    let (_, pre_skip) = head(&first.data)?;
    let serial = first.stream_serial();
    let mut last = 0u64;
    while let Ok(Some(p)) = r.read_packet() {
        if p.stream_serial() == serial && p.last_in_page() && p.absgp_page() != u64::MAX {
            last = last.max(p.absgp_page());
        }
    }
    Ok(last.saturating_sub(pre_skip) as f64 / 48_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("lowkey-recording-{}-{name}-{}", std::process::id(), serial()));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn sine(hz: f32, amp: f32, n: usize, from: usize) -> Vec<f32> {
        (from..from + n).map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin()).collect()
    }
    fn rms(v: &[f32]) -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
    }
    /// The level of one frequency (Goertzel, as an amplitude).
    fn tone(v: &[f32], hz: f32) -> f32 {
        let w = 2.0 * std::f32::consts::PI * hz / RATE as f32;
        let (mut re, mut im) = (0f64, 0f64);
        for (i, &x) in v.iter().enumerate() {
            re += x as f64 * (w * i as f32).cos() as f64;
            im += x as f64 * (w * i as f32).sin() as f64;
        }
        (2.0 * (re * re + im * im).sqrt() / v.len() as f64) as f32
    }
    /// The first sample louder than `level`.
    fn onset(v: &[f32], level: f32) -> Option<usize> {
        v.iter().position(|x| x.abs() > level)
    }

    /// Writes both channels in blocks of `block` samples, `them` a little after `me` (as two threads do).
    fn write_both(w: &mut Writer, me: &[f32], them: &[f32], block: usize) {
        let n = me.len().max(them.len());
        let mut at = 0;
        while at < n {
            if at < me.len() {
                w.write_at(Channel::Me, &me[at..(at + block).min(me.len())], at).unwrap();
            }
            if at < them.len() {
                w.write_at(Channel::Them, &them[at..(at + block).min(them.len())], at).unwrap();
            }
            at += block;
        }
    }

    #[test]
    fn round_trip_keeps_the_channels_apart() {
        let d = dir("round");
        let p = d.0.join("m.ogg");
        let n = 5 * RATE + 123;
        let (me, them) = (sine(440.0, 0.3, n, 0), sine(1000.0, 0.2, n, 0));
        let mut w = Writer::create(&p).unwrap();
        write_both(&mut w, &me, &them, 512);
        let s = w.finish().unwrap();
        assert!((s.seconds - n as f64 / RATE as f64).abs() < 1e-9);
        assert_eq!(s.bytes, std::fs::metadata(&p).unwrap().len());
        let (a, b) = read(&p).unwrap();
        assert_eq!(a.len(), n, "exact length back");
        assert_eq!(b.len(), n);
        assert!((duration(&p).unwrap() - n as f64 / RATE as f64).abs() < 1e-6);
        let mid = RATE..4 * RATE;
        // levels: each tone on its side at about its amplitude
        let (me440, me1k) = (tone(&a[mid.clone()], 440.0), tone(&a[mid.clone()], 1000.0));
        let (th440, th1k) = (tone(&b[mid.clone()], 440.0), tone(&b[mid.clone()], 1000.0));
        assert!((me440 - 0.3).abs() < 0.05, "me 440 Hz: {me440}");
        assert!((th1k - 0.2).abs() < 0.04, "them 1 kHz: {th1k}");
        // apart: the other side's tone more than 40 dB down
        assert!(me1k < 0.2 * 0.01, "them in me: {me1k}");
        assert!(th440 < 0.3 * 0.01, "me in them: {th440}");
        // timing: the decoded wave lines up with the input (pre-skip handled) within a frame
        let lag = best_lag(&me[mid.clone()], &a, mid.start);
        assert!(lag.unsigned_abs() < FRAME as u64, "lag {lag}");
    }

    /// The shift (in samples) that best lines up `x` (taken at `at`) with `y`.
    fn best_lag(x: &[f32], y: &[f32], at: usize) -> i64 {
        let mut best = (f32::MIN, 0i64);
        for lag in -(FRAME as i64)..=(FRAME as i64) {
            let s = at as i64 + lag;
            if s < 0 || s as usize + x.len() > y.len() {
                continue;
            }
            let c: f32 = x.iter().zip(&y[s as usize..]).map(|(a, b)| a * b).sum();
            if c > best.0 {
                best = (c, lag);
            }
        }
        best.1
    }

    #[test]
    fn gaps_are_silence_and_overlaps_keep_the_new_part() {
        let d = dir("gaps");
        let p = d.0.join("m.ogg");
        let mut w = Writer::create(&p).unwrap();
        // Me: 0..1 s tone, nothing for 2 s, then 3..4 s tone. A block that overlaps by 100 samples
        // and one with a 7-sample hole, as the clock gives them.
        w.write_at(Channel::Me, &sine(440.0, 0.3, RATE, 0), 0).unwrap();
        w.write_at(Channel::Me, &sine(440.0, 0.3, RATE / 2, 3 * RATE), 3 * RATE).unwrap();
        w.write_at(Channel::Me, &sine(440.0, 0.3, RATE / 2 + 100, 3 * RATE + RATE / 2 - 100), 3 * RATE + RATE / 2 - 100).unwrap();
        w.write_at(Channel::Me, &sine(440.0, 0.3, 400, 4 * RATE + 7), 4 * RATE + 7).unwrap();
        // Them: a tone 2..3 s only.
        w.write_at(Channel::Them, &sine(1000.0, 0.2, RATE, 2 * RATE), 2 * RATE).unwrap();
        w.finish().unwrap();
        let (a, b) = read(&p).unwrap();
        assert_eq!(a.len(), 4 * RATE + 407);
        assert_eq!(b.len(), a.len());
        assert!(rms(&a[RATE / 4..3 * RATE / 4]) > 0.15);
        assert!(rms(&a[RATE + FRAME..3 * RATE - FRAME]) < 0.01, "the gap is silent");
        assert!(rms(&a[3 * RATE + FRAME..4 * RATE - FRAME]) > 0.15, "after the overlap");
        // no click from the overlap: the wave goes on in phase, so the level stays even
        let around = &a[3 * RATE + RATE / 2 - 400..3 * RATE + RATE / 2 + 400];
        assert!((rms(around) - 0.3 / 2f32.sqrt()).abs() < 0.03, "even level at the overlap: {}", rms(around));
        assert!(rms(&b[..2 * RATE - FRAME]) < 0.01);
        assert!(rms(&b[2 * RATE + FRAME..3 * RATE - FRAME]) > 0.1);
        let start = onset(&b, 0.05).unwrap();
        assert!(start.abs_diff(2 * RATE) < FRAME, "them starts at {start}");
    }

    #[test]
    fn one_channel_silent_for_a_long_time() {
        let d = dir("silent");
        let p = d.0.join("m.ogg");
        let mut w = Writer::create(&p).unwrap();
        // Them sends nothing for 10 minutes: Me goes on alone, memory stays bounded.
        let len = 600 * RATE;
        let mut at = 0;
        while at < len {
            let block = sine(440.0, 0.1, 1600, at);
            w.write_at(Channel::Me, &block, at).unwrap();
            assert!(w.lanes[0].len() <= AHEAD + FRAME + 1600, "memory {}", w.lanes[0].len());
            at += block.len();
        }
        // then Them sends a block for 5 s ago: more than 3 s behind Me
        w.write_at(Channel::Them, &sine(1000.0, 0.2, RATE, len - 5 * RATE), len - 5 * RATE).unwrap();
        let s = w.finish().unwrap();
        assert_eq!(s.seconds, 600.0);
        let (a, b) = read(&p).unwrap();
        assert_eq!(a.len(), len);
        // that time was already encoded: the block was dropped, not shifted
        assert!(rms(&b) < 0.005, "late block dropped");
        // Them back within 3 s: kept, at its time
        let p2 = d.0.join("m2.ogg");
        let mut w = Writer::create(&p2).unwrap();
        let mut at = 0;
        while at < 20 * RATE {
            w.write_at(Channel::Me, &sine(440.0, 0.1, 1600, at), at).unwrap();
            at += 1600;
        }
        // Me is at 20 s; Them's block at 18 s is 2 s behind: still in the buffer
        w.write_at(Channel::Them, &sine(1000.0, 0.2, RATE, 18 * RATE), 18 * RATE).unwrap();
        w.finish().unwrap();
        let (_, b) = read(&p2).unwrap();
        assert!(rms(&b[..17 * RATE]) < 0.005);
        assert!(rms(&b[18 * RATE + FRAME..19 * RATE - FRAME]) > 0.1);
        assert!(onset(&b, 0.05).unwrap().abs_diff(18 * RATE) < FRAME);
    }

    #[test]
    fn a_channel_far_behind_is_cut_to_silence() {
        let d = dir("behind");
        let p = d.0.join("m.ogg");
        let mut w = Writer::create(&p).unwrap();
        // Them delivers its blocks 5 s late, in a burst. The encoder went on after 3 s with
        // silence for Them; the late part of the burst is dropped, the rest is in its place.
        for k in 0..10 {
            w.write_at(Channel::Me, &sine(440.0, 0.2, RATE, k * RATE), k * RATE).unwrap();
        }
        for k in 0..10 {
            w.write_at(Channel::Them, &sine(1000.0, 0.2, RATE, k * RATE), k * RATE).unwrap();
        }
        w.finish().unwrap();
        let (a, b) = read(&p).unwrap();
        assert_eq!(a.len(), 10 * RATE);
        assert!(rms(&a[FRAME..10 * RATE - FRAME]) > 0.1);
        // At most 3 s (plus a frame) of Them was still open when it arrived.
        let kept = onset(&b, 0.05).unwrap();
        assert!((7 * RATE - 2 * FRAME..=7 * RATE + 2 * FRAME).contains(&kept), "them kept from {kept}");
        assert!(rms(&b[kept + FRAME..10 * RATE - FRAME]) > 0.1);
        // A block entirely in the past is fine (dropped), and memory stays bounded.
        let mut w = Writer::create(&d.0.join("m2.ogg")).unwrap();
        w.write_at(Channel::Me, &vec![0.1; 60 * RATE], 0).unwrap();
        assert!(w.lanes[0].len() <= AHEAD + PIECE, "memory {}", w.lanes[0].len());
        w.write_at(Channel::Them, &vec![0.1; RATE], 0).unwrap();
        assert!(w.lanes[1].is_empty());
    }

    #[test]
    fn a_crash_leaves_a_file_that_plays_to_its_last_page() {
        let d = dir("crash");
        let p = d.0.join("m.ogg");
        let mut w = Writer::create(&p).unwrap();
        let n = 5 * RATE + 777;
        write_both(&mut w, &sine(440.0, 0.3, n, 0), &sine(1000.0, 0.2, n, 0), 320 * 3 + 17);
        // never finished: what a crash leaves (the pages written so far are in the file)
        std::mem::forget(w);
        let s = duration(&p).unwrap();
        assert!((4.0..=5.0).contains(&s), "duration {s}");
        let (a, b) = read(&p).unwrap();
        assert!(a.len() as f64 / RATE as f64 >= 4.0, "read {}", a.len());
        assert!(rms(&a[RATE..2 * RATE]) > 0.15 && rms(&b[RATE..2 * RATE]) > 0.1);
        // and a file cut in the middle of a page: the whole pages before it still read
        let bytes = std::fs::read(&p).unwrap();
        for cut in [bytes.len() - 7, bytes.len() / 2, 100, 30, 0] {
            std::fs::write(&p, &bytes[..cut]).unwrap();
            let r = read(&p);
            let t = duration(&p);
            if cut >= 100 {
                // past both headers
                assert!(r.is_ok() && t.is_ok(), "cut at {cut}");
            }
            if cut == bytes.len() / 2 {
                let (a, _) = r.unwrap();
                assert!(a.len() > RATE, "half the file: {}", a.len());
            }
        }
    }

    #[test]
    fn nothing_recorded_is_a_valid_empty_file() {
        let d = dir("empty");
        let p = d.0.join("m.ogg");
        let s = Writer::create(&p).unwrap().finish().unwrap();
        assert_eq!(s.seconds, 0.0);
        let (a, b) = read(&p).unwrap();
        assert!(a.is_empty() && b.is_empty());
        assert_eq!(duration(&p).unwrap(), 0.0);
    }

    #[test]
    fn exact_length_whatever_the_frame_boundary() {
        let d = dir("lengths");
        for n in [1usize, 319, 320, 321, 16000, 16000 + 123] {
            let p = d.0.join(format!("{n}.ogg"));
            let mut w = Writer::create(&p).unwrap();
            w.write_at(Channel::Them, &sine(300.0, 0.2, n, 0), 0).unwrap();
            w.finish().unwrap();
            let (a, b) = read(&p).unwrap();
            assert_eq!((a.len(), b.len()), (n, n), "length {n}");
            assert!((duration(&p).unwrap() - n as f64 / RATE as f64).abs() < 1e-6);
        }
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        let d = dir("bad");
        let p = d.0.join("m.ogg");
        let mut w = Writer::create(&p).unwrap();
        assert!(w.write_at(Channel::Me, &[0.1; 10], usize::MAX - 3).is_err());
        assert!(w.write_at(Channel::Me, &[0.1; 10], MAX_S * RATE).is_err());
        w.write_at(Channel::Me, &[f32::NAN, f32::INFINITY, -9.0, 0.5], 0).unwrap();
        w.write_at(Channel::Me, &[], 5).unwrap();
        w.finish().unwrap();
        assert!(read(&p).is_ok());

        let x = d.0.join("x.ogg");
        std::fs::write(&x, b"hello").unwrap();
        assert!(read(&x).is_err() && duration(&x).is_err());
        assert!(read(&d.0.join("missing.ogg")).is_err());
        // random bytes after a good start: no panic
        let good = std::fs::read(&p).unwrap();
        let mut seed = 1u32;
        for _ in 0..50 {
            let mut v = good.clone();
            for _ in 0..20 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let k = (seed as usize >> 4) % v.len();
                v[k] = (seed >> 24) as u8;
            }
            std::fs::write(&x, &v).unwrap();
            let _ = read(&x);
            let _ = duration(&x);
        }
    }

    #[test]
    fn the_writer_can_go_to_another_thread() {
        fn send<T: Send>() {}
        send::<Writer>();
    }

    /// ffprobe and ffmpeg (if this machine has them) read the file as stereo Opus, with Me on the left.
    #[test]
    fn ffmpeg_reads_it() {
        let has = |tool: &str| std::process::Command::new(tool).arg("-version").output().is_ok();
        if !has("ffprobe") || !has("ffmpeg") {
            eprintln!("no ffprobe/ffmpeg: skipped");
            return;
        }
        let d = dir("ffmpeg");
        let p = d.0.join("m.ogg");
        let n = 3 * RATE;
        let mut w = Writer::create(&p).unwrap();
        write_both(&mut w, &sine(440.0, 0.3, n, 0), &sine(1000.0, 0.2, n, 0), 1000);
        w.finish().unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_name,channels,sample_rate:format=duration", "-of", "default=nw=1"])
            .arg(&p)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("codec_name=opus") && text.contains("channels=2"), "{text}");
        let dur: f64 = text.lines().find_map(|l| l.strip_prefix("duration=")).unwrap().parse().unwrap();
        assert!((dur - 3.0).abs() < 0.01, "ffprobe duration {dur}");
        // ffmpeg's own decoder (it uses the pre-skip and the end granule too)
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&p)
            .args(["-f", "f32le", "-ar", "16000", "-"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let s: Vec<f32> = out.stdout.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let (l, r): (Vec<f32>, Vec<f32>) = s.chunks_exact(2).map(|c| (c[0], c[1])).unzip();
        assert!(l.len().abs_diff(n) < 20, "ffmpeg length {}", l.len());
        assert!(tone(&l[RATE..2 * RATE], 440.0) > 0.25 && tone(&l[RATE..2 * RATE], 1000.0) < 0.005);
        assert!(tone(&r[RATE..2 * RATE], 1000.0) > 0.15 && tone(&r[RATE..2 * RATE], 440.0) < 0.005);
    }
}
