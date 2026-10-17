//! H.264 from a live simulator screen through VideoToolbox (spec §4).
//!
//! One named thread per stream owns every VideoToolbox and CoreVideo object: the
//! compression session, the rotation session and its pixel-buffer pool. It waits for the
//! screen's frame signal (and, while a key frame or a failed encode is pending, every
//! [`RETRY_INTERVAL`], so a still screen recovers), at most one frame per
//! [`MIN_FRAME_INTERVAL`], wraps the current
//! framebuffer IOSurface in a `CVPixelBuffer`, turns it into the UI orientation with
//! `VTPixelRotationSession` when the UI is not portrait (the surface itself always stays
//! portrait), and encodes it. A change of output size (a rotation or a new surface) tears
//! the compression session down and builds a new one, whose first frame is a key frame
//! that [`EncoderState`] sends after a fresh config unit.
//!
//! VideoToolbox calls [`on_output`] on its own thread. It only converts the sample and
//! `try_send`s it into a channel of [`UNIT_QUEUE`] units; a unit that does not fit asks
//! for a key frame and the stream skips to it. Nothing there touches the Tokio runtime.
//! Logs carry sizes and status codes, never pixels.

use std::{
    ffi::{c_int, c_void},
    panic::AssertUnwindSafe,
    pin::pin,
    ptr::{NonNull, null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreateWithIOSurface, CVPixelBufferPool, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_io_surface::IOSurfaceRef;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTPixelRotationSession, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTPixelRotationPropertyKey_Rotation, kVTProfileLevel_H264_Main_AutoLevel, kVTRotation_180,
    kVTRotation_CCW90, kVTRotation_CW90,
};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::{
    coresim::Screen,
    lock,
    pixels::{IosOrientation, Turn},
};
use crate::device::{
    DeviceError,
    media::VideoSource,
    video::{UnitKind, VideoUnit},
};

/// At most 60 frames a second.
const MIN_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// How often the encoder thread wakes without a signal to look for a pending key frame or a
/// failed encode; either encodes the current surface again, so a still screen recovers.
const RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// The frame rate the encoder plans for, so its automatic level covers it.
const EXPECTED_FRAME_RATE: i64 = 60;
/// Units waiting for the media hub; a full queue drops to the next key frame.
const UNIT_QUEUE: usize = 8;
/// Bits per pixel per second: 0.1 bit per pixel at 60 fps.
const BITS_PER_PIXEL_SECOND: u64 = 6;
const MIN_BITRATE: u64 = 2_000_000;
const MAX_BITRATE: u64 = 12_000_000;
const MAX_KEY_FRAME_INTERVAL: i32 = 120;
const MAX_KEY_FRAME_INTERVAL_SECONDS: f64 = 2.0;
/// Encode failures in a row after which the stream ends as failed.
const MAX_FAILURES: u32 = 3;
const START_CODE: [u8; 4] = [0, 0, 0, 1];
/// `pts_us` is the presentation time in microseconds since the stream started.
const MICROSECONDS: i32 = 1_000_000;

fn protocol_error(message: impl Into<String>) -> DeviceError {
    DeviceError::Protocol(message.into())
}

// ---------------------------------------------------------------------------------------------
// Pure conversion

/// AVCC sample (4-byte big-endian lengths) to Annex B.
pub(crate) fn avcc_to_annex_b(sample: &[u8]) -> Result<Vec<u8>, DeviceError> {
    let mut annex_b = Vec::with_capacity(sample.len());
    let mut rest = sample;
    while !rest.is_empty() {
        let (length, tail) = rest
            .split_first_chunk::<4>()
            .ok_or_else(|| protocol_error("an encoded sample ends inside a NAL length"))?;
        let length = usize::try_from(u32::from_be_bytes(*length))
            .map_err(|_| protocol_error("an encoded NAL unit is too long"))?;
        if length > tail.len() {
            return Err(protocol_error("an encoded NAL unit runs past its sample"));
        }
        let (unit, tail) = tail.split_at(length);
        annex_b.extend_from_slice(&START_CODE);
        annex_b.extend_from_slice(unit);
        rest = tail;
    }
    Ok(annex_b)
}

/// Whether an AVCC sample holds an IDR slice (NAL type 5), which makes it a key frame.
fn has_idr(sample: &[u8]) -> bool {
    let mut rest = sample;
    while let Some((length, tail)) = rest.split_first_chunk::<4>() {
        let length = u32::from_be_bytes(*length) as usize;
        if length > tail.len() {
            return false;
        }
        let (unit, tail) = tail.split_at(length);
        if unit.first().is_some_and(|header| header & 0x1F == 5) {
            return true;
        }
        rest = tail;
    }
    false
}

/// SPS and PPS to one Annex B config unit.
pub(crate) fn config_unit(sps: &[u8], pps: &[u8]) -> VideoUnit {
    let mut data = Vec::with_capacity(2 * START_CODE.len() + sps.len() + pps.len());
    data.extend_from_slice(&START_CODE);
    data.extend_from_slice(sps);
    data.extend_from_slice(&START_CODE);
    data.extend_from_slice(pps);
    VideoUnit {
        kind: UnitKind::Config,
        pts_us: 0,
        data: Bytes::from(data),
    }
}

/// Bitrate for a pixel count (0.1 bit/px at 60 fps, clamped 2–12 Mbit/s).
pub(crate) fn bitrate_for(width: u32, height: u32) -> u32 {
    let bits = u64::from(width) * u64::from(height) * BITS_PER_PIXEL_SECOND;
    u32::try_from(bits.clamp(MIN_BITRATE, MAX_BITRATE)).unwrap_or(u32::MAX)
}

/// The encoder's output size for a portrait surface shown in `turn`.
fn rotated_size((width, height): (u32, u32), turn: Turn) -> (u32, u32) {
    match turn {
        Turn::None | Turn::Half => (width, height),
        Turn::Clockwise | Turn::CounterClockwise => (height, width),
    }
}

/// The picture size an H.264 SPS describes, after cropping. Accepts the NAL unit itself
/// or an Annex B config unit that starts with it.
#[doc(hidden)]
pub fn sps_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let nal = strip_start_code(data);
    // The NAL unit ends at the next start code; a four-byte one leaves a zero byte behind,
    // which the parse never reaches.
    let end = nal
        .windows(3)
        .position(|window| window == [0, 0, 1])
        .unwrap_or(nal.len());
    let nal = &nal[..end];
    if nal.first()? & 0x1F != 7 {
        return None;
    }
    // Remove emulation-prevention bytes (00 00 03 → 00 00).
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in &nal[1..] {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        rbsp.push(byte);
    }
    let mut bits = Bits { data: &rbsp, at: 0 };
    let profile = bits.read(8)?;
    bits.read(16)?; // constraint flags, level
    bits.ue()?; // seq_parameter_set_id
    let mut chroma_format = 1;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format = bits.ue()?;
        if chroma_format == 3 {
            bits.read(1)?; // separate_colour_plane_flag
        }
        bits.ue()?; // bit_depth_luma_minus8
        bits.ue()?; // bit_depth_chroma_minus8
        bits.read(1)?; // qpprime_y_zero_transform_bypass_flag
        if bits.read(1)? == 1 {
            let lists = if chroma_format == 3 { 12 } else { 8 };
            for list in 0..lists {
                if bits.read(1)? == 1 {
                    let size = if list < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i64, 8i64);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + bits.se()? + 256) % 256;
                        }
                        last = if next == 0 { last } else { next };
                    }
                }
            }
        }
    }
    bits.ue()?; // log2_max_frame_num_minus4
    match bits.ue()? {
        0 => {
            bits.ue()?;
        }
        1 => {
            bits.read(1)?;
            bits.se()?;
            bits.se()?;
            for _ in 0..bits.ue()? {
                bits.se()?;
            }
        }
        _ => {}
    }
    bits.ue()?; // max_num_ref_frames
    bits.read(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = bits.ue()? + 1;
    let height_units = bits.ue()? + 1;
    let frame_mbs_only = bits.read(1)?;
    if frame_mbs_only == 0 {
        bits.read(1)?;
    }
    bits.read(1)?; // direct_8x8_inference_flag
    let (mut left, mut right, mut top, mut bottom) = (0, 0, 0, 0);
    if bits.read(1)? == 1 {
        (left, right, top, bottom) = (bits.ue()?, bits.ue()?, bits.ue()?, bits.ue()?);
    }
    let (crop_x, crop_y) = match chroma_format {
        0 => (1, 2 - frame_mbs_only),
        1 => (2, 2 * (2 - frame_mbs_only)),
        2 => (2, 2 - frame_mbs_only),
        _ => (1, 2 - frame_mbs_only),
    };
    let width = (width_mbs * 16).checked_sub(crop_x * (left + right))?;
    let height = ((2 - frame_mbs_only) * height_units * 16).checked_sub(crop_y * (top + bottom))?;
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

fn strip_start_code(data: &[u8]) -> &[u8] {
    data.strip_prefix(&START_CODE)
        .or_else(|| data.strip_prefix(&[0, 0, 1]))
        .unwrap_or(data)
}

/// An RBSP bit reader for [`sps_dimensions`].
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, count: u32) -> Option<u64> {
        let mut value = 0;
        for _ in 0..count {
            let byte = self.data.get(self.at / 8)?;
            value = (value << 1) | u64::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Some(value)
    }

    fn ue(&mut self) -> Option<u64> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros > 32 {
                return None;
            }
        }
        Some((1 << zeros) - 1 + self.read(zeros)?)
    }

    fn se(&mut self) -> Option<i64> {
        let value = i64::try_from(self.ue()?).ok()?;
        Some(if value % 2 == 1 {
            (value + 1) / 2
        } else {
            -value / 2
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The output state machine

/// One encoded sample, converted to Annex B.
#[derive(Debug)]
pub(crate) struct Sample {
    /// The encoded picture size.
    size: (u32, u32),
    key: bool,
    /// SPS and PPS; only read for a key sample.
    params: Option<(Vec<u8>, Vec<u8>)>,
    data: Vec<u8>,
    pts_us: u64,
}

#[cfg(test)]
impl Sample {
    pub(crate) fn key(size: (u32, u32), sps: &[u8], pps: &[u8], data: &[u8]) -> Self {
        Self {
            size,
            key: true,
            params: Some((sps.to_vec(), pps.to_vec())),
            data: data.to_vec(),
            pts_us: 0,
        }
    }

    pub(crate) fn delta(size: (u32, u32), data: &[u8]) -> Self {
        Self {
            size,
            key: false,
            params: None,
            data: data.to_vec(),
            pts_us: 0,
        }
    }
}

/// The config the receiver was last sent.
#[derive(Debug, PartialEq)]
struct SentConfig {
    size: (u32, u32),
    sps: Vec<u8>,
    pps: Vec<u8>,
}

/// What the output callback sends for each sample: a config unit before a key unit whose
/// size or parameter sets changed (or after a reset or a loss), and nothing for deltas
/// after a lost unit until the next key.
#[derive(Debug, Default)]
pub(crate) struct EncoderState {
    sent: Option<SentConfig>,
    /// A unit was lost: deltas cannot decode until the next key.
    broken: bool,
}

impl EncoderState {
    pub(crate) fn on_sample(&mut self, sample: Sample) -> Vec<VideoUnit> {
        let Sample {
            size,
            key,
            params,
            data,
            pts_us,
        } = sample;
        if !key {
            if self.broken {
                return Vec::new();
            }
            return vec![VideoUnit {
                kind: UnitKind::Delta,
                pts_us,
                data: Bytes::from(data),
            }];
        }
        self.broken = false;
        let mut units = Vec::with_capacity(2);
        if let Some((sps, pps)) = params {
            let config = SentConfig { size, sps, pps };
            if self.sent.as_ref() != Some(&config) {
                units.push(config_unit(&config.sps, &config.pps));
                self.sent = Some(config);
            }
        }
        units.push(VideoUnit {
            kind: UnitKind::Key,
            pts_us,
            data: Bytes::from(data),
        });
        units
    }

    /// A unit [`on_sample`](Self::on_sample) returned did not reach the receiver: deltas
    /// are dropped until the next key, which carries a config again.
    pub(crate) fn on_lost(&mut self) {
        self.sent = None;
        self.broken = true;
    }

    /// The next key carries a config again, as a reset asks.
    pub(crate) fn resend_config(&mut self) {
        self.sent = None;
    }
}

// ---------------------------------------------------------------------------------------------
// The output callback

/// Requests from the media hub to the encoder thread and the output callback.
#[derive(Default)]
struct Requests {
    /// The next encoded frame is a key frame.
    force_key: AtomicBool,
    /// The next key unit is preceded by a config unit.
    resend_config: AtomicBool,
}

/// What the output callback reaches through its refcon. The encoder thread owns it and
/// drops it only after every session that points at it has been invalidated.
struct Output {
    units: mpsc::Sender<VideoUnit>,
    state: Mutex<EncoderState>,
    requests: Arc<Requests>,
    /// Set once a sample failed to convert, so the warning is logged once.
    warned: AtomicBool,
}

impl Output {
    fn deliver(&self, status: i32, flags: VTEncodeInfoFlags, sample: *mut CMSampleBuffer) {
        if flags.contains(VTEncodeInfoFlags::FrameDropped) {
            // The encoder skipped this frame; later frames reference earlier ones only.
            return;
        }
        let sample = match NonNull::new(sample) {
            Some(sample) if status == 0 => sample,
            _ => {
                tracing::debug!(status, "the H.264 encoder returned no sample");
                self.lost();
                return;
            }
        };
        // SAFETY: VideoToolbox passes a live sample buffer that stays valid for the
        // duration of the callback.
        let sample = match read_sample(unsafe { sample.as_ref() }) {
            Ok(sample) => sample,
            Err(error) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(%error, "could not read an encoded simulator frame");
                }
                self.lost();
                return;
            }
        };
        let mut state = lock(&self.state);
        if sample.key && self.requests.resend_config.swap(false, Ordering::AcqRel) {
            state.resend_config();
        }
        for unit in state.on_sample(sample) {
            if self.units.try_send(unit).is_err() {
                state.on_lost();
                self.requests.force_key.store(true, Ordering::Release);
                break;
            }
        }
    }

    fn lost(&self) {
        lock(&self.state).on_lost();
        self.requests.force_key.store(true, Ordering::Release);
    }
}

/// `VTCompressionOutputCallback`. Runs on a VideoToolbox thread: it converts the sample
/// and `try_send`s it, and a panic stops here instead of unwinding into VideoToolbox.
unsafe extern "C-unwind" fn on_output(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let delivered = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `refcon` is `Arc::as_ptr` of the `Output` the encoder thread keeps alive
        // until every session created with it has completed its frames and been
        // invalidated, so no callback outlives it.
        let output = unsafe { &*refcon.cast_const().cast::<Output>() };
        output.deliver(status, flags, sample);
    }));
    if delivered.is_err() {
        tracing::error!("the H.264 output callback panicked; dropping the frame");
    }
}

/// Converts one encoded sample.
fn read_sample(sample: &CMSampleBuffer) -> Result<Sample, DeviceError> {
    // SAFETY: `sample` is a live sample buffer; the getters return retained objects.
    let (block, format) = unsafe { (sample.data_buffer(), sample.format_description()) };
    let block = block.ok_or_else(|| protocol_error("an encoded frame has no data"))?;
    let format = format.ok_or_else(|| protocol_error("an encoded frame has no format"))?;
    // SAFETY: `block` is a live block buffer.
    let length = unsafe { block.data_length() };
    if length == 0 {
        return Err(protocol_error("an encoded frame is empty"));
    }
    let mut avcc = vec![0u8; length];
    let destination = NonNull::from(&mut avcc[..]).cast::<c_void>();
    // SAFETY: `destination` points at `length` writable bytes, all the block holds.
    let status = unsafe { block.copy_data_bytes(0, length, destination) };
    if status != 0 {
        return Err(protocol_error(format!(
            "CMBlockBufferCopyDataBytes failed: {status}"
        )));
    }
    let (sps, header_length) = parameter_set(&format, 0)?;
    if header_length != 4 {
        return Err(protocol_error(format!(
            "the encoder used {header_length}-byte NAL lengths"
        )));
    }
    let (pps, _) = parameter_set(&format, 1)?;
    // SAFETY: `format` is a video format description (the session encodes H.264).
    let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&format) };
    let size = (
        u32::try_from(dimensions.width).unwrap_or(0),
        u32::try_from(dimensions.height).unwrap_or(0),
    );
    // SAFETY: as above.
    let pts = unsafe { sample.presentation_time_stamp() };
    let key = has_idr(&avcc);
    Ok(Sample {
        size,
        key,
        params: key.then_some((sps, pps)),
        data: avcc_to_annex_b(&avcc)?,
        pts_us: micros(pts),
    })
}

/// The H.264 parameter set at `index` and the NAL length-field size.
fn parameter_set(
    format: &CMFormatDescription,
    index: usize,
) -> Result<(Vec<u8>, c_int), DeviceError> {
    let mut pointer: *const u8 = null();
    let mut size = 0usize;
    let mut header_length: c_int = 0;
    // SAFETY: every out-pointer is valid; the count is not needed.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format,
            index,
            &raw mut pointer,
            &raw mut size,
            null_mut(),
            &raw mut header_length,
        )
    };
    if status != 0 || pointer.is_null() {
        return Err(protocol_error(format!(
            "the encoded format has no parameter set {index}: {status}"
        )));
    }
    // SAFETY: the pointer addresses `size` bytes inside `format`, which outlives this copy.
    let set = unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec();
    Ok((set, header_length))
}

/// A presentation time in microseconds; 0 when it is not a positive time.
fn micros(time: CMTime) -> u64 {
    if time.timescale <= 0 || time.value < 0 {
        return 0;
    }
    let micros = i128::from(time.value) * 1_000_000 / i128::from(time.timescale);
    u64::try_from(micros).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------------------------
// VideoToolbox sessions (owned by the encoder thread)

fn number(value: i64) -> CFRetained<CFNumber> {
    CFNumber::new_i64(value)
}

/// Sets one session property; a refusal is logged and the session carries on with its
/// default.
fn set_property(session: &CFType, key: &CFString, value: &CFType) {
    // SAFETY: `session` is a live VideoToolbox session and `value` has the type the key's
    // documentation names.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    if status != 0 {
        tracing::warn!(key = %key, status, "the H.264 encoder refused a setting");
    }
}

/// A compression session at one output size, invalidated on drop.
struct Compressor {
    session: CFRetained<VTCompressionSession>,
    size: (u32, u32),
}

impl Compressor {
    fn new(size: (u32, u32), output: &Arc<Output>) -> Result<Self, DeviceError> {
        let (width, height) = size;
        let as_i32 = |value: u32| {
            i32::try_from(value).map_err(|_| protocol_error("the simulator screen is too large"))
        };
        let mut raw: *mut VTCompressionSession = null_mut();
        // SAFETY: the dictionaries are absent, `on_output` matches
        // `VTCompressionOutputCallback`, its refcon is an `Output` that outlives the session
        // (see `on_output`), and `raw` is a valid out-pointer.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                as_i32(width)?,
                as_i32(height)?,
                kCMVideoCodecType_H264,
                None,
                None,
                None,
                Some(on_output),
                Arc::as_ptr(output).cast_mut().cast(),
                NonNull::from(&mut raw),
            )
        };
        let raw = NonNull::new(raw).filter(|_| status == 0).ok_or_else(|| {
            tracing::warn!(status, width, height, "VTCompressionSessionCreate failed");
            protocol_error(format!("the H.264 encoder did not start: {status}"))
        })?;
        // SAFETY: `VTCompressionSessionCreate` returned a +1 reference.
        let session = unsafe { CFRetained::from_raw(raw) };
        let compressor = Self { session, size };
        let session: &CFType = &compressor.session;
        // SAFETY: the keys and their values are the VideoToolbox statics.
        unsafe {
            set_property(
                session,
                kVTCompressionPropertyKey_RealTime,
                CFBoolean::new(true),
            );
            set_property(
                session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            );
            set_property(
                session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &number(i64::from(MAX_KEY_FRAME_INTERVAL)),
            );
            set_property(
                session,
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                &CFNumber::new_f64(MAX_KEY_FRAME_INTERVAL_SECONDS),
            );
            set_property(
                session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &number(EXPECTED_FRAME_RATE),
            );
            set_property(
                session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_Main_AutoLevel,
            );
            set_property(
                session,
                kVTCompressionPropertyKey_AverageBitRate,
                &number(i64::from(bitrate_for(width, height))),
            );
        }
        // SAFETY: a live session; preparing is optional and only logged when it fails.
        let status = unsafe { compressor.session.prepare_to_encode_frames() };
        if status != 0 {
            tracing::debug!(status, "VTCompressionSessionPrepareToEncodeFrames failed");
        }
        tracing::debug!(width, height, "H.264 encoder started for a simulator");
        Ok(compressor)
    }

    fn encode(
        &self,
        frame: &CVPixelBuffer,
        pts_us: i64,
        options: Option<&CFDictionary>,
    ) -> Result<(), DeviceError> {
        // SAFETY: CMTimeMake only builds a value.
        let pts = unsafe { CMTime::new(pts_us, MICROSECONDS) };
        // SAFETY: `frame` is a live pixel buffer of the session's size; `kCMTimeInvalid`
        // means "no duration"; `options` is nil or a frame-options dictionary; the refcon
        // and info pointers may be null.
        let status = unsafe {
            self.session
                .encode_frame(frame, pts, kCMTimeInvalid, options, null_mut(), null_mut())
        };
        if status != 0 {
            tracing::warn!(status, "VTCompressionSessionEncodeFrame failed");
            return Err(protocol_error(format!(
                "the H.264 encoder refused a frame: {status}"
            )));
        }
        Ok(())
    }
}

impl Drop for Compressor {
    fn drop(&mut self) {
        // SAFETY: a live session. Completing first delivers every pending frame, so no
        // callback runs after `invalidate` returns and its `Output` may then be dropped.
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}

/// Rotates the portrait surface into the UI orientation, into buffers from a pool at the
/// rotated size; invalidated on drop.
struct Rotator {
    session: CFRetained<VTPixelRotationSession>,
    pool: CFRetained<CVPixelBufferPool>,
    turn: Turn,
    size: (u32, u32),
}

impl Rotator {
    fn new(turn: Turn, size: (u32, u32)) -> Result<Self, DeviceError> {
        // SAFETY: the rotation constants are VideoToolbox statics.
        let rotation = unsafe {
            match turn {
                Turn::Clockwise => kVTRotation_CW90,
                Turn::CounterClockwise => kVTRotation_CCW90,
                Turn::Half => kVTRotation_180,
                Turn::None => return Err(protocol_error("a portrait frame needs no rotation")),
            }
        };
        let mut raw: *mut VTPixelRotationSession = null_mut();
        // SAFETY: `raw` is a valid out-pointer.
        let status = unsafe { VTPixelRotationSession::create(None, NonNull::from(&mut raw)) };
        let raw = NonNull::new(raw).filter(|_| status == 0).ok_or_else(|| {
            tracing::warn!(status, "VTPixelRotationSessionCreate failed");
            protocol_error(format!("the frame rotation did not start: {status}"))
        })?;
        // SAFETY: `VTPixelRotationSessionCreate` returned a +1 reference.
        let session = unsafe { CFRetained::from_raw(raw) };
        // A rotator without its pool still invalidates its session on the way out.
        let invalidate = Invalidate(&session);
        // SAFETY: a live session; the rotation key takes one of the `kVTRotation_` strings.
        unsafe {
            let status = VTSessionSetProperty(
                &session,
                kVTPixelRotationPropertyKey_Rotation,
                Some(rotation),
            );
            if status != 0 {
                tracing::warn!(status, "the frame rotation refused its angle");
                return Err(protocol_error(format!(
                    "the frame rotation refused its angle: {status}"
                )));
            }
        }
        let pool = pixel_buffer_pool(size)?;
        std::mem::forget(invalidate);
        Ok(Self {
            session,
            pool,
            turn,
            size,
        })
    }

    fn rotate(&self, source: &CVPixelBuffer) -> Result<CFRetained<CVPixelBuffer>, DeviceError> {
        let mut raw: *mut CVPixelBuffer = null_mut();
        // SAFETY: a live pool and a valid out-pointer.
        let status = unsafe {
            CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut raw))
        };
        let raw = NonNull::new(raw)
            .filter(|_| status == kCVReturnSuccess)
            .ok_or_else(|| {
                tracing::warn!(status, "CVPixelBufferPoolCreatePixelBuffer failed");
                protocol_error(format!("no buffer for a rotated frame: {status}"))
            })?;
        // SAFETY: `CVPixelBufferPoolCreatePixelBuffer` returned a +1 reference.
        let rotated = unsafe { CFRetained::from_raw(raw) };
        // SAFETY: both buffers are live; the destination has the rotated size the rotation
        // needs (the source's width and height swapped for a quarter turn).
        let status = unsafe { self.session.rotate_image(source, &rotated) };
        if status != 0 {
            tracing::warn!(status, "VTPixelRotationSessionRotateImage failed");
            return Err(protocol_error(format!("a frame did not rotate: {status}")));
        }
        Ok(rotated)
    }
}

impl Drop for Rotator {
    fn drop(&mut self) {
        // SAFETY: a live session.
        unsafe { self.session.invalidate() };
    }
}

/// Invalidates a rotation session that never became a [`Rotator`].
struct Invalidate<'a>(&'a VTPixelRotationSession);

impl Drop for Invalidate<'_> {
    fn drop(&mut self) {
        // SAFETY: a live session.
        unsafe { self.0.invalidate() };
    }
}

/// A pool of IOSurface-backed BGRA buffers at `size`, which the encoder reads without a
/// copy.
fn pixel_buffer_pool(
    (width, height): (u32, u32),
) -> Result<CFRetained<CVPixelBufferPool>, DeviceError> {
    let format = number(i64::from(kCVPixelFormatType_32BGRA));
    let width = number(i64::from(width));
    let height = number(i64::from(height));
    let surface_properties = CFDictionary::<CFString, CFType>::empty();
    // SAFETY: the CoreVideo key statics.
    let keys: [&CFString; 4] = unsafe {
        [
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferWidthKey,
            kCVPixelBufferHeightKey,
            kCVPixelBufferIOSurfacePropertiesKey,
        ]
    };
    let values: [&CFType; 4] = [&format, &width, &height, &surface_properties];
    let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    let mut raw: *mut CVPixelBufferPool = null_mut();
    // SAFETY: `attributes` is a pixel-buffer attributes dictionary and `raw` a valid
    // out-pointer.
    let status = unsafe {
        CVPixelBufferPool::create(
            None,
            None,
            Some(attributes.as_opaque()),
            NonNull::from(&mut raw),
        )
    };
    let raw = NonNull::new(raw)
        .filter(|_| status == kCVReturnSuccess)
        .ok_or_else(|| {
            tracing::warn!(status, "CVPixelBufferPoolCreate failed");
            protocol_error(format!("no buffers for rotated frames: {status}"))
        })?;
    // SAFETY: `CVPixelBufferPoolCreate` returned a +1 reference.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// `surface` as a `CVPixelBuffer`, without a copy.
fn wrap_surface(surface: &IOSurfaceRef) -> Result<CFRetained<CVPixelBuffer>, DeviceError> {
    let mut raw: *mut CVPixelBuffer = null_mut();
    // SAFETY: a live IOSurface, no attributes, and a valid out-pointer; the buffer retains
    // the surface.
    let status =
        unsafe { CVPixelBufferCreateWithIOSurface(None, surface, None, NonNull::from(&mut raw)) };
    let raw = NonNull::new(raw)
        .filter(|_| status == kCVReturnSuccess)
        .ok_or_else(|| {
            tracing::warn!(status, "CVPixelBufferCreateWithIOSurface failed");
            protocol_error(format!("the framebuffer could not be wrapped: {status}"))
        })?;
    // SAFETY: `CVPixelBufferCreateWithIOSurface` returned a +1 reference.
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// Everything the encoder thread owns. Fields drop in order: the sessions are invalidated
/// before `output`, which their callbacks point at, goes.
struct Pipeline {
    compressor: Option<Compressor>,
    rotator: Option<Rotator>,
    /// The current surface wrapped once; the buffer retains the surface, so its address
    /// identifies it.
    wrapped: Option<(*const IOSurfaceRef, CFRetained<CVPixelBuffer>)>,
    force_key_options: CFRetained<CFDictionary<CFString, CFBoolean>>,
    started: Instant,
    output: Arc<Output>,
}

impl Pipeline {
    fn new(output: Arc<Output>) -> Self {
        // SAFETY: the VideoToolbox key static.
        let key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
        Self {
            compressor: None,
            rotator: None,
            wrapped: None,
            force_key_options: CFDictionary::from_slices(&[key], &[CFBoolean::new(true)]),
            started: Instant::now(),
            output,
        }
    }

    /// Encodes the current frame of the portrait `surface`, turned for `orientation`.
    fn encode(
        &mut self,
        surface: &IOSurfaceRef,
        orientation: IosOrientation,
        force_key: bool,
    ) -> Result<(), DeviceError> {
        let turn = orientation.turn();
        let portrait = (
            u32::try_from(surface.width()).unwrap_or(0),
            u32::try_from(surface.height()).unwrap_or(0),
        );
        if portrait.0 == 0 || portrait.1 == 0 {
            return Err(protocol_error("the simulator framebuffer is empty"));
        }
        let size = rotated_size(portrait, turn);
        if self
            .compressor
            .as_ref()
            .is_none_or(|compressor| compressor.size != size)
        {
            // The old session completes its frames and goes before the new one starts.
            self.compressor = None;
            self.compressor = Some(Compressor::new(size, &self.output)?);
        }
        let address: *const IOSurfaceRef = surface;
        if self
            .wrapped
            .as_ref()
            .is_none_or(|(wrapped, _)| *wrapped != address)
        {
            self.wrapped = None;
            self.wrapped = Some((address, wrap_surface(surface)?));
        }
        let Some((_, source)) = &self.wrapped else {
            return Err(protocol_error("the framebuffer is not wrapped"));
        };
        let rotated = if turn == Turn::None {
            self.rotator = None;
            None
        } else {
            if self
                .rotator
                .as_ref()
                .is_none_or(|rotator| rotator.turn != turn || rotator.size != size)
            {
                self.rotator = None;
                self.rotator = Some(Rotator::new(turn, size)?);
            }
            let rotator = self
                .rotator
                .as_ref()
                .ok_or_else(|| protocol_error("no rotator"))?;
            Some(rotator.rotate(source)?)
        };
        let frame: &CVPixelBuffer = rotated.as_deref().unwrap_or(source);
        let pts_us = i64::try_from(self.started.elapsed().as_micros()).unwrap_or(i64::MAX);
        let options = force_key.then(|| self.force_key_options.as_opaque());
        let compressor = self
            .compressor
            .as_ref()
            .ok_or_else(|| protocol_error("no encoder"))?;
        compressor.encode(frame, pts_us, options)
    }
}

#[cfg(test)]
impl Pipeline {
    /// Waits until every frame passed in so far has reached the output callback.
    fn flush(&self) {
        if let Some(compressor) = &self.compressor {
            // SAFETY: a live session.
            unsafe { compressor.session.complete_frames(kCMTimeInvalid) };
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The source

/// Runs `future` on this (non-runtime) thread, parking between polls, for at most
/// `timeout`; `None` when it ran out. The futures here only wait on Tokio's
/// runtime-independent sync primitives.
fn block_on<F: Future>(future: F, timeout: Duration) -> Option<F::Output> {
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let deadline = Instant::now() + timeout;
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Some(output);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        thread::park_timeout(deadline - now);
    }
}

/// What the encoder thread waits on.
struct Signals {
    damage: watch::Receiver<u64>,
    orientation: watch::Receiver<IosOrientation>,
    wake: Arc<Notify>,
    stop: CancellationToken,
}

/// Encodes the current frame in an orientation, as a key frame when asked.
trait Encode: FnMut(IosOrientation, bool) -> Result<(), DeviceError> {}
impl<F: FnMut(IosOrientation, bool) -> Result<(), DeviceError>> Encode for F {}

/// The encoder loop: one frame per frame signal or reset, at most one per
/// [`MIN_FRAME_INTERVAL`]; while a key frame or a failed encode is pending, also every
/// [`RETRY_INTERVAL`] without a signal. Runs until stopped or [`MAX_FAILURES`] encodes in a
/// row fail. Reports whether the first frame encoded, then returns whether the stream
/// failed.
fn run_encoder(
    signals: &mut Signals,
    requests: &Requests,
    started: oneshot::Sender<Result<(), DeviceError>>,
    encode: &mut impl Encode,
) -> bool {
    let mut started = Some(started);
    let mut failures = 0;
    let mut last: Option<Instant> = None;
    // The first frame goes out at once.
    signals.damage.mark_changed();
    loop {
        // The wait wakes every `RETRY_INTERVAL` to see a key request the output callback
        // made after the last encode: the callback only `try_send`s and never wakes us.
        let woke = block_on(
            async {
                tokio::select! {
                    biased;
                    () = signals.stop.cancelled() => false,
                    changed = signals.damage.changed() => changed.is_ok(),
                    () = signals.wake.notified() => true,
                }
            },
            RETRY_INTERVAL,
        );
        match woke {
            Some(false) => return false,
            Some(true) => {}
            // No signal: encode the same frame again only for a pending key or a failure.
            None if failures > 0 || requests.force_key.load(Ordering::Acquire) => {}
            None => continue,
        }
        if let Some(last) = last {
            let next = last + MIN_FRAME_INTERVAL;
            let now = Instant::now();
            if next > now {
                thread::sleep(next - now);
            }
        }
        if signals.stop.is_cancelled() {
            return false;
        }
        signals.damage.borrow_and_update();
        last = Some(Instant::now());
        let orientation = *signals.orientation.borrow_and_update();
        let force_key = requests.force_key.swap(false, Ordering::AcqRel);
        match encode(orientation, force_key) {
            Ok(()) => {
                failures = 0;
                if let Some(started) = started.take() {
                    let _ = started.send(Ok(()));
                }
            }
            Err(error) => {
                if force_key {
                    requests.force_key.store(true, Ordering::Release);
                }
                failures += 1;
                if let Some(started) = started.take() {
                    let _ = started.send(Err(error));
                    return true;
                }
                if failures >= MAX_FAILURES {
                    tracing::warn!(%error, "the simulator's H.264 stream failed");
                    return true;
                }
                tracing::debug!(%error, failures, "a simulator frame did not encode; retrying");
            }
        }
    }
}

/// A live simulator's screen as H.264 units for the media hub.
pub(crate) struct SimulatorVideo {
    units: mpsc::Receiver<VideoUnit>,
    requests: Arc<Requests>,
    wake: Arc<Notify>,
    stop: CancellationToken,
    failed: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl SimulatorVideo {
    /// Starts the encoder thread and waits until its first frame encoded.
    pub(crate) async fn start(screen: Arc<Screen>) -> Result<Self, DeviceError> {
        let (sender, units) = mpsc::channel(UNIT_QUEUE);
        let requests = Arc::new(Requests::default());
        let output = Arc::new(Output {
            units: sender,
            state: Mutex::new(EncoderState::default()),
            requests: requests.clone(),
            warned: AtomicBool::new(false),
        });
        let (damage, orientation) = (screen.damage(), screen.orientation());
        let (video, first) = Self::launch(units, requests, damage, orientation, move || {
            // Built on the encoder thread, which owns every VideoToolbox object.
            let mut pipeline = Pipeline::new(output);
            move |orientation, force_key| {
                let surface = screen
                    .surface()
                    .ok_or_else(|| protocol_error("the simulator has no framebuffer"))?;
                pipeline.encode(surface.as_surface(), orientation, force_key)
            }
        })?;
        video.started(first).await
    }

    /// Spawns the encoder thread around the encoder `make_encode` builds there. The thread
    /// records a failure (an error or a panic) in `failed` before the encoder, and with it
    /// the units' sender, is dropped, so the hub sees `failed()` when the units end.
    fn launch<M, E>(
        units: mpsc::Receiver<VideoUnit>,
        requests: Arc<Requests>,
        damage: watch::Receiver<u64>,
        orientation: watch::Receiver<IosOrientation>,
        make_encode: M,
    ) -> Result<(Self, oneshot::Receiver<Result<(), DeviceError>>), DeviceError>
    where
        M: FnOnce() -> E + Send + 'static,
        E: Encode,
    {
        let wake = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let failed = Arc::new(AtomicBool::new(false));
        let mut signals = Signals {
            damage,
            orientation,
            wake: wake.clone(),
            stop: stop.clone(),
        };
        let (started, first) = oneshot::channel();
        let thread = {
            let requests = requests.clone();
            let failed = failed.clone();
            thread::Builder::new()
                .name("bibcode-ios-encoder".to_owned())
                .spawn(move || {
                    let mut encode = make_encode();
                    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        run_encoder(&mut signals, &requests, started, &mut encode)
                    }));
                    let failure = outcome.unwrap_or_else(|_| {
                        tracing::error!("the simulator's H.264 encoder thread panicked");
                        true
                    });
                    if failure {
                        failed.store(true, Ordering::Release);
                    }
                    // Only now do the sessions and the units' sender go.
                    drop(encode);
                })
                .map_err(|error| DeviceError::Io(error.to_string()))?
        };
        let video = Self {
            units,
            requests,
            wake,
            stop,
            failed,
            thread: Some(thread),
        };
        Ok((video, first))
    }

    /// Waits until the first frame encoded; stops the thread when it did not.
    async fn started(
        mut self,
        first: oneshot::Receiver<Result<(), DeviceError>>,
    ) -> Result<Self, DeviceError> {
        match first.await {
            Ok(Ok(())) => Ok(self),
            Ok(Err(error)) => {
                self.stop_thread().await;
                Err(error)
            }
            Err(_) => {
                self.stop_thread().await;
                Err(protocol_error(
                    "the H.264 encoder stopped before its first frame",
                ))
            }
        }
    }

    /// Stops the encoder thread and waits until it has invalidated its sessions.
    async fn stop_thread(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take()
            && tokio::task::spawn_blocking(move || thread.join())
                .await
                .is_err()
        {
            tracing::warn!("the simulator's H.264 encoder thread did not stop cleanly");
        }
    }
}

impl Drop for SimulatorVideo {
    fn drop(&mut self) {
        // A source dropped without `stop` still ends its thread, which cleans up alone.
        self.stop.cancel();
    }
}

impl VideoSource for SimulatorVideo {
    async fn next_unit(&mut self) -> Option<VideoUnit> {
        self.units.recv().await
    }

    /// The next frame is a key frame after a config unit, encoded at once even when the
    /// screen does not change.
    async fn reset_video(&mut self) -> Result<(), DeviceError> {
        self.requests.resend_config.store(true, Ordering::Release);
        self.requests.force_key.store(true, Ordering::Release);
        self.wake.notify_one();
        Ok(())
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    async fn stop(mut self, _device_ended: bool) {
        self.stop_thread().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::video::UnitKind;

    fn kinds(units: &[crate::device::video::VideoUnit]) -> Vec<UnitKind> {
        units.iter().map(|unit| unit.kind.clone()).collect()
    }

    #[test]
    fn avcc_lengths_become_start_codes() {
        let avcc = [0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 1, 0x41];
        assert_eq!(
            avcc_to_annex_b(&avcc).unwrap(),
            vec![0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41]
        );
        assert!(avcc_to_annex_b(&[0, 0, 0, 9, 0x65]).is_err());
    }

    #[test]
    fn config_unit_carries_sps_then_pps() {
        let unit = config_unit(&[0x67, 1], &[0x68, 2]);
        assert_eq!(unit.kind, UnitKind::Config);
        assert_eq!(&unit.data[..], &[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]);
    }

    #[test]
    fn bitrate_scales_and_clamps() {
        assert_eq!(bitrate_for(100, 100), 2_000_000);
        assert_eq!(bitrate_for(400, 800), 2_000_000); // 1.92 Mbit/s, clamped up
        assert_eq!(bitrate_for(1000, 1000), 6_000_000); // 0.1 bit/px × 60 fps
        assert_eq!(bitrate_for(1206, 2622), 12_000_000); // 18.97 Mbit/s, clamped down
        assert_eq!(bitrate_for(4000, 4000), 12_000_000);
    }

    #[test]
    fn size_change_emits_config_before_frames() {
        // Pure state machine extracted from the callback: `EncoderState::on_sample`.
        let mut state = EncoderState::default();
        let first = state.on_sample(Sample::key((1206, 2622), b"sps1", b"pps1", b"idr"));
        assert_eq!(kinds(&first), [UnitKind::Config, UnitKind::Key]);
        let delta = state.on_sample(Sample::delta((1206, 2622), b"p"));
        assert_eq!(kinds(&delta), [UnitKind::Delta]);
        let rotated = state.on_sample(Sample::key((2622, 1206), b"sps2", b"pps2", b"idr"));
        assert_eq!(kinds(&rotated), [UnitKind::Config, UnitKind::Key]);
    }

    #[test]
    fn a_lost_unit_skips_deltas_until_a_key_with_config() {
        let mut state = EncoderState::default();
        state.on_sample(Sample::key((8, 8), b"sps", b"pps", b"idr"));
        assert_eq!(
            kinds(&state.on_sample(Sample::key((8, 8), b"sps", b"pps", b"idr"))),
            [UnitKind::Key]
        );
        state.on_lost();
        assert!(state.on_sample(Sample::delta((8, 8), b"p")).is_empty());
        let key = state.on_sample(Sample::key((8, 8), b"sps", b"pps", b"idr"));
        assert_eq!(kinds(&key), [UnitKind::Config, UnitKind::Key]);
        assert_eq!(
            kinds(&state.on_sample(Sample::delta((8, 8), b"p"))),
            [UnitKind::Delta]
        );
        // A reset asks for the config again, even when nothing changed.
        state.resend_config();
        let key = state.on_sample(Sample::key((8, 8), b"sps", b"pps", b"idr"));
        assert_eq!(kinds(&key), [UnitKind::Config, UnitKind::Key]);
    }

    #[test]
    fn only_idr_slices_make_a_key() {
        assert!(has_idr(&[0, 0, 0, 1, 0x06, 0, 0, 0, 2, 0x65, 0x88]));
        assert!(!has_idr(&[0, 0, 0, 2, 0x41, 0x9A]));
        assert!(!has_idr(&[0, 0, 0, 9, 0x65]));
        // An empty NAL unit is skipped, not read as the next length's first byte.
        assert!(!has_idr(&[0, 0, 0, 0, 0x05, 0, 0, 1, 0x41]));
        assert!(has_idr(&[0, 0, 0, 0, 0, 0, 0, 1, 0x65]));
    }

    #[test]
    fn sps_dimensions_apply_cropping() {
        // Main profile, 1206x2622: 76x164 macroblocks cropped by 10 columns and 2 rows,
        // as VideoToolbox writes it for the iPhone 17 Pro framebuffer size
        // (recorded with `BIBCODE_RECORD_VT_FIXTURES=1`).
        let sps = include_bytes!("fixtures/vt-sps-1206x2622.bin");
        assert_eq!(sps_dimensions(sps), Some((1206, 2622)));
        let mut config = vec![0, 0, 0, 1];
        config.extend_from_slice(sps);
        config.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0x80]);
        assert_eq!(sps_dimensions(&config), Some((1206, 2622)));
        assert_eq!(sps_dimensions(&[0x68, 0xEE]), None);
    }

    #[test]
    fn a_recorded_key_sample_converts_to_annex_b() {
        let avcc = include_bytes!("fixtures/vt-key-128x256.bin");
        assert!(has_idr(avcc));
        let annex_b = avcc_to_annex_b(avcc).unwrap();
        assert_eq!(annex_b.len(), avcc.len());
        assert_eq!(&annex_b[..4], &START_CODE);
    }

    // -- VideoToolbox itself (public frameworks only; no simulator) ------------------------

    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress,
    };
    use objc2_io_surface::{
        IOSurfaceLockOptions, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat,
        kIOSurfaceWidth,
    };

    use crate::device::ios::pixels::{Bgra, upright_rgb};

    /// A BGRA surface whose pixel (x, y) is `pixel(x, y)`.
    fn surface(
        width: u32,
        height: u32,
        pixel: impl Fn(u32, u32) -> [u8; 4],
    ) -> CFRetained<IOSurfaceRef> {
        let values = [
            number(i64::from(width)),
            number(i64::from(height)),
            number(4),
            number(i64::from(kCVPixelFormatType_32BGRA)),
        ];
        // SAFETY: the IOSurface key statics.
        let keys: [&CFString; 4] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ]
        };
        let values: Vec<&CFNumber> = values.iter().map(|value| &**value).collect();
        let properties = CFDictionary::<CFString, CFNumber>::from_slices(&keys, &values);
        // SAFETY: a surface properties dictionary.
        let surface =
            unsafe { IOSurfaceRef::new(properties.as_opaque()) }.expect("IOSurfaceCreate");
        // SAFETY: locking for writing, with no seed.
        assert_eq!(
            unsafe { surface.lock(IOSurfaceLockOptions::empty(), null_mut()) },
            0
        );
        let bytes_per_row = surface.bytes_per_row();
        let base = surface.base_address().as_ptr().cast::<u8>();
        for y in 0..height {
            for x in 0..width {
                let at = y as usize * bytes_per_row + x as usize * 4;
                // SAFETY: (x, y) is inside the locked surface.
                unsafe { std::ptr::copy_nonoverlapping(pixel(x, y).as_ptr(), base.add(at), 4) };
            }
        }
        // SAFETY: locked above.
        assert_eq!(
            unsafe { surface.unlock(IOSurfaceLockOptions::empty(), null_mut()) },
            0
        );
        surface
    }

    /// The BGRA pixels of `buffer` as RGB, row after row.
    fn rgb_of(buffer: &CVPixelBuffer) -> (Vec<u8>, u32, u32) {
        let lock = CVPixelBufferLockFlags::ReadOnly;
        // SAFETY: a live buffer, locked for reading until the unlock below.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(buffer, lock) }, 0);
        let (width, height) = (
            CVPixelBufferGetWidth(buffer),
            CVPixelBufferGetHeight(buffer),
        );
        let bytes_per_row = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
        let mut rgb = Vec::new();
        for y in 0..height {
            for x in 0..width {
                // SAFETY: (x, y) is inside the locked buffer.
                let pixel =
                    unsafe { std::slice::from_raw_parts(base.add(y * bytes_per_row + x * 4), 4) };
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
        }
        // SAFETY: locked above.
        assert_eq!(unsafe { CVPixelBufferUnlockBaseAddress(buffer, lock) }, 0);
        (
            rgb,
            u32::try_from(width).unwrap(),
            u32::try_from(height).unwrap(),
        )
    }

    fn gradient(x: u32, y: u32) -> [u8; 4] {
        [(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8, 255]
    }

    #[test]
    fn rotation_turns_frames_like_the_jpeg_path() {
        let (width, height) = (24, 40);
        let source = surface(width, height, gradient);
        let wrapped = wrap_surface(&source).unwrap();
        let (portrait, ..) = rgb_of(&wrapped);
        let bgra: Vec<u8> = portrait
            .chunks(3)
            .flat_map(|rgb| [rgb[2], rgb[1], rgb[0], 255])
            .collect();
        let pixels = Bgra {
            pixels: &bgra,
            width,
            height,
            bytes_per_row: width as usize * 4,
        };
        for orientation in [
            IosOrientation::Landscape,
            IosOrientation::ReverseLandscape,
            IosOrientation::ReversePortrait,
        ] {
            let turn = orientation.turn();
            let size = rotated_size((width, height), turn);
            let rotator = Rotator::new(turn, size).unwrap();
            let rotated = rotator.rotate(&wrapped).unwrap();
            let expected = upright_rgb(&pixels, turn, size.0).unwrap();
            assert_eq!(rgb_of(&rotated), expected, "{orientation:?}");
        }
    }

    /// Units the callback queued so far.
    fn drain(units: &mut mpsc::Receiver<VideoUnit>) -> Vec<VideoUnit> {
        std::iter::from_fn(|| units.try_recv().ok()).collect()
    }

    #[test]
    fn videotoolbox_sends_config_before_key_and_again_after_a_turn() {
        let (sender, mut units) = mpsc::channel(64);
        let requests = Arc::new(Requests::default());
        let output = Arc::new(Output {
            units: sender,
            state: Mutex::new(EncoderState::default()),
            requests: requests.clone(),
            warned: AtomicBool::new(false),
        });
        let mut pipeline = Pipeline::new(output);
        let first = surface(128, 256, gradient);
        pipeline
            .encode(&first, IosOrientation::Portrait, false)
            .unwrap();
        pipeline.flush();
        let opened = drain(&mut units);
        assert_eq!(kinds(&opened), [UnitKind::Config, UnitKind::Key]);
        assert_eq!(sps_dimensions(&opened[0].data), Some((128, 256)));
        if std::env::var_os("BIBCODE_RECORD_VT_FIXTURES").is_some() {
            let mut avcc = Vec::new();
            let mut rest = &opened[1].data[..];
            while !rest.is_empty() {
                let next = rest[4..]
                    .windows(4)
                    .position(|w| w == START_CODE)
                    .map_or(rest.len(), |at| at + 4);
                avcc.extend_from_slice(&u32::try_from(next - 4).unwrap().to_be_bytes());
                avcc.extend_from_slice(&rest[4..next]);
                rest = &rest[next..];
            }
            std::fs::write(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/device/ios/fixtures/vt-key-128x256.bin"
                ),
                avcc,
            )
            .unwrap();
            let (sender, mut phone_units) = mpsc::channel(8);
            let mut phone = Pipeline::new(Arc::new(Output {
                units: sender,
                state: Mutex::new(EncoderState::default()),
                requests: Arc::default(),
                warned: AtomicBool::new(false),
            }));
            phone
                .encode(
                    &surface(1206, 2622, gradient),
                    IosOrientation::Portrait,
                    false,
                )
                .unwrap();
            phone.flush();
            let config = phone_units.try_recv().unwrap();
            let sps = &config.data[4..config.data[4..]
                .windows(4)
                .position(|w| w == START_CODE)
                .unwrap()
                + 4];
            std::fs::write(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/device/ios/fixtures/vt-sps-1206x2622.bin"
                ),
                sps,
            )
            .unwrap();
        }
        let changed = surface(128, 256, |x, y| gradient(y, x));
        pipeline
            .encode(&changed, IosOrientation::Portrait, false)
            .unwrap();
        pipeline.flush();
        let delta = drain(&mut units);
        assert_eq!(kinds(&delta), [UnitKind::Delta]);
        assert!(delta[0].pts_us > opened[1].pts_us);
        // A turn rebuilds the session at the turned size, then config before key.
        pipeline
            .encode(&changed, IosOrientation::Landscape, false)
            .unwrap();
        pipeline.flush();
        let turned = drain(&mut units);
        assert_eq!(kinds(&turned), [UnitKind::Config, UnitKind::Key]);
        assert_eq!(sps_dimensions(&turned[0].data), Some((256, 128)));
        // A forced key on the same session needs no config.
        pipeline
            .encode(&changed, IosOrientation::Landscape, true)
            .unwrap();
        pipeline.flush();
        assert_eq!(kinds(&drain(&mut units)), [UnitKind::Key]);
        // Back to portrait: the first surface is rewrapped and the size changes back.
        pipeline
            .encode(&first, IosOrientation::Portrait, false)
            .unwrap();
        pipeline.flush();
        let back = drain(&mut units);
        assert_eq!(kinds(&back), [UnitKind::Config, UnitKind::Key]);
        assert_eq!(sps_dimensions(&back[0].data), Some((128, 256)));
        drop(pipeline);
        assert!(
            units.try_recv().is_err(),
            "the sender went with the pipeline"
        );
    }

    #[test]
    fn a_full_queue_drops_to_the_next_key_frame() {
        let (sender, mut units) = mpsc::channel(2);
        let requests = Arc::new(Requests::default());
        let output = Arc::new(Output {
            units: sender,
            state: Mutex::new(EncoderState::default()),
            requests: requests.clone(),
            warned: AtomicBool::new(false),
        });
        let mut pipeline = Pipeline::new(output);
        let frame = surface(128, 256, gradient);
        let changed = surface(128, 256, |x, y| gradient(y, x));
        // Config and key fill the queue; nobody drains it.
        pipeline
            .encode(&frame, IosOrientation::Portrait, false)
            .unwrap();
        pipeline.flush();
        assert!(!requests.force_key.load(Ordering::Acquire));
        // The next delta does not fit: a key frame is requested.
        pipeline
            .encode(&changed, IosOrientation::Portrait, false)
            .unwrap();
        pipeline.flush();
        assert!(
            requests.force_key.load(Ordering::Acquire),
            "a key frame is requested"
        );
        assert_eq!(kinds(&drain(&mut units)), [UnitKind::Config, UnitKind::Key]);
        // Deltas after the loss cannot decode, so they are dropped.
        let force = requests.force_key.swap(false, Ordering::AcqRel);
        pipeline
            .encode(&frame, IosOrientation::Portrait, false)
            .unwrap();
        pipeline.flush();
        assert!(drain(&mut units).is_empty());
        // The requested key gets through, with its config again.
        pipeline
            .encode(&changed, IosOrientation::Portrait, force)
            .unwrap();
        pipeline.flush();
        assert_eq!(kinds(&drain(&mut units)), [UnitKind::Config, UnitKind::Key]);
    }

    // -- The encoder thread, with a scripted encoder ---------------------------------------

    struct Scripted {
        video: SimulatorVideo,
        damage: watch::Sender<u64>,
        _orientation: watch::Sender<IosOrientation>,
        /// The `force_key` of each encode, in order.
        calls: std::sync::mpsc::Receiver<bool>,
    }

    fn unit(kind: UnitKind) -> VideoUnit {
        VideoUnit {
            kind,
            pts_us: 0,
            data: Bytes::from_static(b"unit"),
        }
    }

    /// A source whose encoder sends one key unit per call while `script(call)` is `Ok`.
    async fn scripted(
        script: impl Fn(usize) -> Result<(), DeviceError> + Send + 'static,
    ) -> Scripted {
        let (sender, units) = mpsc::channel(UNIT_QUEUE);
        let (damage, damage_rx) = watch::channel(0);
        let (orientation, orientation_rx) = watch::channel(IosOrientation::Portrait);
        let (calls_tx, calls) = std::sync::mpsc::channel();
        let (video, first) = SimulatorVideo::launch(
            units,
            Arc::default(),
            damage_rx,
            orientation_rx,
            move || {
                let mut call = 0;
                move |_: IosOrientation, force_key: bool| -> Result<(), DeviceError> {
                    let _ = calls_tx.send(force_key);
                    call += 1;
                    script(call)?;
                    let _ = sender.try_send(unit(UnitKind::Key));
                    Ok(())
                }
            },
        )
        .unwrap();
        Scripted {
            video: video.started(first).await.unwrap(),
            damage,
            _orientation: orientation,
            calls,
        }
    }

    async fn next(video: &mut SimulatorVideo) -> Option<VideoUnit> {
        tokio::time::timeout(Duration::from_secs(5), VideoSource::next_unit(video))
            .await
            .expect("the source answered in time")
    }

    #[tokio::test]
    async fn an_encoder_failure_is_reported_when_the_units_end() {
        let mut source = scripted(|call| {
            if call == 1 {
                Ok(())
            } else {
                Err(protocol_error("the encoder refused a frame"))
            }
        })
        .await;
        assert!(next(&mut source.video).await.is_some());
        // One new frame fails; the retries fail too, without another signal.
        source.damage.send_modify(|frames| *frames += 1);
        assert!(next(&mut source.video).await.is_none());
        assert!(source.video.failed(), "failed() is set when the units end");
        VideoSource::stop(source.video, false).await;
    }

    #[tokio::test]
    async fn a_panicking_encoder_is_reported_as_failed() {
        let mut source = scripted(|call| {
            assert!(call == 1, "the encoder broke");
            Ok(())
        })
        .await;
        assert!(next(&mut source.video).await.is_some());
        source.damage.send_modify(|frames| *frames += 1);
        assert!(next(&mut source.video).await.is_none());
        assert!(source.video.failed());
        VideoSource::stop(source.video, false).await;
    }

    #[tokio::test]
    async fn a_still_screen_is_encoded_again_while_a_key_is_pending() {
        let source = scripted(|_| Ok(())).await;
        let calls = &source.calls;
        assert_eq!(calls.recv_timeout(Duration::from_secs(5)), Ok(false));
        // The output callback lost a unit and asked for a key; the screen stays still.
        source
            .video
            .requests
            .force_key
            .store(true, Ordering::Release);
        // The next damage-free wait times out and encodes a key frame.
        assert_eq!(calls.recv_timeout(Duration::from_secs(2)), Ok(true));
        // With nothing pending, a still screen is not encoded again.
        assert!(calls.recv_timeout(RETRY_INTERVAL * 3).is_err());
        VideoSource::stop(source.video, false).await;
    }

    #[tokio::test]
    async fn a_failed_encode_is_retried_without_a_new_frame() {
        let source = scripted(|call| {
            if call == 2 {
                Err(protocol_error("the encoder refused a frame"))
            } else {
                Ok(())
            }
        })
        .await;
        let calls = &source.calls;
        assert_eq!(calls.recv_timeout(Duration::from_secs(5)), Ok(false));
        source.damage.send_modify(|frames| *frames += 1);
        assert_eq!(calls.recv_timeout(Duration::from_secs(2)), Ok(false));
        // Retried after the interval with no new signal, and it succeeds.
        assert_eq!(calls.recv_timeout(Duration::from_secs(2)), Ok(false));
        assert!(!source.video.failed());
        VideoSource::stop(source.video, false).await;
    }
}
