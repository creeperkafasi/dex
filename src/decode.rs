//! H.264 -> RGBA with ffmpeg.

use ffmpeg_next as ff;
use ff::software::scaling;

/// A decoded frame, tightly packed RGBA.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Initializes ffmpeg once, keeping its log to real errors.
pub fn init() -> Result<(), ff::Error> {
    ff::init()?;
    ff::util::log::set_level(ff::util::log::Level::Error);
    Ok(())
}

pub struct Decoder {
    /// Time spent decoding and converting so far, in ns (for diagnostics).
    pub decode_ns: u64,
    pub convert_ns: u64,
    dec: ff::decoder::Video,
    scaler: Option<(scaling::Context, u32, u32, ff::format::Pixel)>,
    yuv: ff::frame::Video,
    rgb: ff::frame::Video,
}

impl Decoder {
    pub fn h264() -> Result<Self, ff::Error> {
        init()?;
        let codec = ff::decoder::find(ff::codec::Id::H264).ok_or(ff::Error::DecoderNotFound)?;
        let mut ctx = ff::codec::Context::new_with_codec(codec);
        // Lowest latency: no frame reordering delay, no frame threading.
        // SAFETY: plain field writes on a context that isn't opened yet.
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).flags |= ff::ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*raw).thread_type = ff::ffi::FF_THREAD_SLICE;
        }
        Ok(Self {
            decode_ns: 0,
            convert_ns: 0,
            dec: ctx.decoder().video()?,
            scaler: None,
            yuv: ff::frame::Video::empty(),
            rgb: ff::frame::Video::empty(),
        })
    }

    /// Feeds one access unit; returns the newest decoded frame, if any.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Frame>, ff::Error> {
        let start = std::time::Instant::now();
        self.dec.send_packet(&ff::Packet::copy(data))?;
        let mut out = None;
        while self.dec.receive_frame(&mut self.yuv).is_ok() {
            let decoded = std::time::Instant::now();
            out = Some(self.convert()?);
            self.convert_ns += decoded.elapsed().as_nanos() as u64;
        }
        self.decode_ns += start.elapsed().as_nanos() as u64;
        Ok(out)
    }

    fn convert(&mut self) -> Result<Frame, ff::Error> {
        let (w, h, fmt) = (self.yuv.width(), self.yuv.height(), self.yuv.format());
        let stale = !matches!(&self.scaler, Some((_, sw, sh, sf)) if (*sw, *sh, *sf) == (w, h, fmt));
        if stale {
            let ctx = scaling::Context::get(fmt, w, h, ff::format::Pixel::RGBA, w, h, scaling::Flags::BILINEAR)?;
            self.scaler = Some((ctx, w, h, fmt));
        }
        let (scaler, ..) = self.scaler.as_mut().unwrap();
        scaler.run(&self.yuv, &mut self.rgb)?;

        let stride = self.rgb.stride(0);
        let row = w as usize * 4;
        let src = self.rgb.data(0);
        let mut rgba = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            rgba.extend_from_slice(&src[y * stride..y * stride + row]);
        }
        Ok(Frame { width: w, height: h, rgba })
    }
}
