//! Phone audio: Opus from scrcpy-server, decoded with ffmpeg, played with cpal.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;

const CODEC_OPUS: u32 = 0x6f70_7573;
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// Above this much buffered audio, drop the oldest samples to keep latency low.
const MAX_BUFFERED: usize = (RATE as usize * CHANNELS as usize) * 120 / 1000;

/// Samples decoded so far, across sessions (for tests and diagnostics).
pub static DECODED_SAMPLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Loudest sample decoded so far, as f32 bits (0.0 = only silence).
pub static PEAK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn peak() -> f32 {
    f32::from_bits(PEAK.load(std::sync::atomic::Ordering::Relaxed))
}

/// Reads the audio socket until it closes, playing what arrives.
pub fn run(mut s: TcpStream) -> io::Result<()> {
    let mut codec = [0u8; 4];
    s.read_exact(&mut codec)?;
    match u32::from_be_bytes(codec) {
        CODEC_OPUS => {}
        // The device disabled audio (e.g. Android < 11 can't capture it).
        0 => return Ok(()),
        other => return Err(io::Error::other(format!("unexpected audio codec {other:#x}"))),
    }

    let buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(MAX_BUFFERED * 2)));
    // The stream must live on this thread (it isn't Send) for as long as we play.
    // DEX_AUDIO_SINK=null decodes without playing (tests stay silent).
    let _stream = match std::env::var("DEX_AUDIO_SINK").as_deref() {
        Ok("null") => None,
        _ => Some(open_output(buffer.clone())?),
    };

    let mut decoder: Option<ff::decoder::Audio> = None;
    let mut frame = ff::frame::Audio::empty();
    let mut header = [0u8; 12];
    let mut payload = Vec::new();
    loop {
        s.read_exact(&mut header)?;
        let flags = u64::from_be_bytes(header[..8].try_into().unwrap());
        let len = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
        payload.resize(len, 0);
        s.read_exact(&mut payload)?;
        if flags & (1 << 62) != 0 {
            // Config packet: the Opus header, which the decoder needs up front.
            decoder = Some(opus_decoder(&payload).map_err(io::Error::other)?);
            continue;
        }
        let Some(dec) = decoder.as_mut() else { continue };
        if dec.send_packet(&ff::Packet::copy(&payload)).is_err() {
            continue;
        }
        while dec.receive_frame(&mut frame).is_ok() {
            push_frame(&frame, &buffer);
        }
    }
}

fn opus_decoder(config: &[u8]) -> Result<ff::decoder::Audio, ff::Error> {
    crate::decode::init()?;
    let codec = ff::decoder::find(ff::codec::Id::OPUS).ok_or(ff::Error::DecoderNotFound)?;
    let mut ctx = ff::codec::Context::new_with_codec(codec);
    // SAFETY: setting fields of an unopened context; extradata is allocated
    // with av_mallocz (padded, as ffmpeg requires) and freed by the context.
    unsafe {
        let raw = ctx.as_mut_ptr();
        (*raw).sample_rate = RATE as i32;
        ff::ffi::av_channel_layout_default(&mut (*raw).ch_layout, i32::from(CHANNELS));
        let pad = ff::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        let extra = ff::ffi::av_mallocz(config.len() + pad) as *mut u8;
        if extra.is_null() {
            return Err(ff::Error::Other { errno: 12 }); // ENOMEM
        }
        std::ptr::copy_nonoverlapping(config.as_ptr(), extra, config.len());
        (*raw).extradata = extra;
        (*raw).extradata_size = config.len() as i32;
    }
    ctx.decoder().audio()
}

/// Appends a decoded frame as interleaved f32 stereo.
fn push_frame(frame: &ff::frame::Audio, buffer: &Mutex<VecDeque<f32>>) {
    use ff::format::Sample;
    let n = frame.samples();
    DECODED_SAMPLES.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    let mut out = buffer.lock().unwrap();
    match frame.format() {
        Sample::F32(ff::format::sample::Type::Planar) if frame.planes() >= 2 => {
            let (l, r) = (frame.plane::<f32>(0), frame.plane::<f32>(1));
            for i in 0..n {
                out.push_back(l[i]);
                out.push_back(r[i]);
            }
        }
        Sample::F32(ff::format::sample::Type::Planar) => {
            for &v in &frame.plane::<f32>(0)[..n] {
                out.push_back(v);
                out.push_back(v);
            }
        }
        Sample::F32(ff::format::sample::Type::Packed) => {
            // SAFETY: packed f32 stereo is n * 2 floats in plane 0.
            let data = frame.data(0);
            let floats = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const f32, n * 2) };
            out.extend(floats);
        }
        _ => return, // ffmpeg's Opus decoder only produces float output
    }
    let loudest = out.iter().rev().take(n * 2).fold(0f32, |m, v| m.max(v.abs()));
    if loudest > peak() {
        PEAK.store(loudest.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
    let excess = out.len().saturating_sub(MAX_BUFFERED);
    out.drain(..excess);
}

fn open_output(buffer: Arc<Mutex<VecDeque<f32>>>) -> io::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| io::Error::other("no audio output device"))?;
    let config = cpal::StreamConfig {
        channels: CHANNELS,
        sample_rate: RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [f32], _| {
                let mut buf = buffer.lock().unwrap();
                for sample in out.iter_mut() {
                    *sample = buf.pop_front().unwrap_or(0.0);
                }
            },
            |e| eprintln!("dex: audio output error: {e}"),
            None,
        )
        .map_err(io::Error::other)?;
    stream.play().map_err(io::Error::other)?;
    Ok(stream)
}
