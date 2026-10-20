// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Optional file-contained demuxing. This is not a codec sandbox: selected
//! demuxers may omit unsupported/external tracks, including MOV drefs.
use crate::{Error, Result, ffi};
use ffmpeg_the_third as ffmpeg;
use std::{
    ffi::CString,
    ops::{Deref, DerefMut, RangeBounds},
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

/// How a demuxer may open its input resources. Old entry points remain unrestricted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputPolicy {
    /// Preserve the original libavformat input behaviour.
    #[default]
    Unrestricted,
    /// One already copied ordinary local file, a closed demuxer set and no
    /// additional container I/O. This does not promise all declared tracks survive.
    SelfContainedLocal,
}

const FORMATS: &std::ffi::CStr =
    c"mov,matroska,webm,aac,mp3,wav,aiff,flac,ogg,png_pipe,jpeg_pipe,webp_pipe";

#[derive(Default)]
struct IoState {
    denied: AtomicBool,
    #[cfg(test)]
    events: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
}
#[cfg(test)]
impl Drop for IoState {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("state dropped");
    }
}

// The callback never dereferences a URL or calls I/O, allocates, panics or unwinds.
unsafe extern "C" fn deny_io(
    context: *mut ffmpeg::sys::AVFormatContext,
    output: *mut *mut ffmpeg::sys::AVIOContext,
    _url: *const libc::c_char,
    _flags: libc::c_int,
    _options: *mut *mut ffmpeg::sys::AVDictionary,
) -> libc::c_int {
    // SAFETY: FFmpeg supplies a live format context. Its opaque points at the
    // stable Box<IoState> owned until after every format context is closed.
    // Nested demux contexts share opaque, per AVFormatContext.io_open's contract.
    unsafe {
        if !output.is_null() {
            *output = ptr::null_mut();
        }
        if !context.is_null() && !(*context).opaque.is_null() {
            let state = &*((*context).opaque.cast::<IoState>());
            state.denied.store(true, Ordering::Relaxed);
        }
    }
    -libc::EACCES
}

struct PrimaryIo {
    pointer: *mut ffmpeg::sys::AVIOContext,
    state: Box<IoState>,
}
// SAFETY: a primary AVIO and its FFmpeg Input are moved together, never accessed
// concurrently. IoState contains only an atomic; callbacks borrow its stable Box.
// Like ffmpeg::format::context::Input, this owner is Send, deliberately not Sync.
unsafe impl Send for PrimaryIo {}
impl Drop for PrimaryIo {
    fn drop(&mut self) {
        // SAFETY: this pointer was opened only by avio_open2, has exactly one
        // owner, and the format Input was already closed before this destructor.
        unsafe {
            if !self.pointer.is_null() {
                ffmpeg::sys::avio_closep(&mut self.pointer);
            }
        }
        #[cfg(test)]
        self.state.events.lock().unwrap().push("primary closed");
    }
}

/// Input must drop before its custom primary AVIO and callback state. Use an
/// Option so this order is explicit on all success and failure paths.
pub(crate) struct Input {
    inner: Option<ffmpeg::format::context::Input>,
    primary: Option<PrimaryIo>,
    path: PathBuf,
}
impl Drop for Input {
    fn drop(&mut self) {
        drop(self.inner.take());
        #[cfg(test)]
        if let Some(io) = &self.primary {
            io.state.events.lock().unwrap().push("context closed");
        }
        drop(self.primary.take());
    }
}
impl Deref for Input {
    type Target = ffmpeg::format::context::Input;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().expect("live input owner")
    }
}
impl DerefMut for Input {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().expect("live input owner")
    }
}

struct Dictionary(*mut ffmpeg::sys::AVDictionary);
impl Dictionary {
    fn new() -> Self {
        Self(ptr::null_mut())
    }
    fn set(&mut self, name: &std::ffi::CStr, value: &std::ffi::CStr, path: &Path) -> Result<()> {
        // SAFETY: av_dict_set copies both live NUL-terminated strings. Dictionary
        // owns the resulting pointer, including partially built error cases.
        let result =
            unsafe { ffmpeg::sys::av_dict_set(&mut self.0, name.as_ptr(), value.as_ptr(), 0) };
        if result < 0 {
            return Err(ffi::fail(
                "input options",
                path,
                ffmpeg::Error::from(result),
            ));
        }
        Ok(())
    }
}
impl Drop for Dictionary {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of the remaining options dictionary.
        unsafe {
            ffmpeg::sys::av_dict_free(&mut self.0);
        }
    }
}

impl Input {
    pub(crate) fn open(path: &Path, policy: InputPolicy) -> Result<Self> {
        ffi::init();
        if policy == InputPolicy::Unrestricted {
            return Ok(Self {
                inner: Some(
                    ffmpeg::format::input(path).map_err(|error| ffi::fail("open", path, error))?,
                ),
                primary: None,
                path: path.into(),
            });
        }
        Self::open_local(path, false, None)
    }

    // `test_drefs` is never exposed through a public API. Production always sets
    // enable_drefs=0. Tests can force a demuxer attempt to exercise the deny hook.
    fn open_local(
        path: &Path,
        test_drefs: bool,
        test_format: Option<&std::ffi::CStr>,
    ) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path).map_err(|source| Error::Io {
            path: path.into(),
            source,
        })?;
        if !path.is_absolute() || !metadata.file_type().is_file() {
            return Err(Error::InputPolicy {
                path: path.into(),
                detail: "strict input must be an absolute ordinary file, without a final symlink"
                    .into(),
            });
        }
        let filename =
            CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error::InputPolicy {
                path: path.into(),
                detail: "input path contains a NUL byte".into(),
            })?;
        let mut primary = PrimaryIo {
            pointer: ptr::null_mut(),
            state: Box::default(),
        };
        let mut io_options = Dictionary::new();
        io_options.set(c"protocol_whitelist", c"file", path)?;
        // SAFETY: all pointers refer to live owned storage. Only this explicit
        // trusted path is opened; later demuxer opens use deny_io instead.
        let opened = unsafe {
            ffmpeg::sys::avio_open2(
                &mut primary.pointer,
                filename.as_ptr(),
                ffmpeg::sys::AVIO_FLAG_READ,
                ptr::null(),
                &mut io_options.0,
            )
        };
        if opened < 0 {
            return Err(ffi::fail(
                "open primary input",
                path,
                ffmpeg::Error::from(opened),
            ));
        }
        // A periodic PCM WAVE can outscore WAV as MPEG-TS during FFmpeg's
        // generic probe. Inspect only the already opened primary bytes; never
        // trust its extension or permit a different external input.
        let mut prefix = [0_u8; 12];
        // SAFETY: primary owns this live AVIO; prefix is writable for its length.
        let read = unsafe {
            ffmpeg::sys::avio_read(primary.pointer, prefix.as_mut_ptr(), prefix.len() as i32)
        };
        if read < 0 {
            return Err(ffi::fail(
                "read primary signature",
                path,
                ffmpeg::Error::from(read),
            ));
        }
        // SAFETY: this is the same sole-owned, ordinary-file AVIO. Rewind before
        // transferring it to the demuxer, including for short primary files.
        let rewound = unsafe { ffmpeg::sys::avio_seek(primary.pointer, 0, libc::SEEK_SET) };
        if rewound != 0 {
            return Err(ffi::fail(
                "rewind primary signature",
                path,
                ffmpeg::Error::from(if rewound < 0 {
                    rewound as i32
                } else {
                    -libc::EIO
                }),
            ));
        }
        let wave = read == prefix.len() as i32
            && matches!(&prefix[..4], b"RIFF" | b"RF64")
            && &prefix[8..] == b"WAVE";
        let mut options = Dictionary::new();
        options.set(c"protocol_whitelist", c"file", path)?;
        options.set(c"format_whitelist", FORMATS, path)?;
        options.set(c"enable_drefs", if test_drefs { c"1" } else { c"0" }, path)?;
        options.set(
            c"use_absolute_path",
            if test_drefs { c"1" } else { c"0" },
            path,
        )?;
        // SAFETY: allocation is checked before dereference. From this point
        // avformat_open_input owns raw: on failure it frees it and writes NULL.
        // CUSTOM_IO keeps primary.pointer owned by PrimaryIo even on failure.
        let mut raw = unsafe { ffmpeg::sys::avformat_alloc_context() };
        if raw.is_null() {
            return Err(ffi::fail(
                "allocate input",
                path,
                ffmpeg::Error::from(-libc::ENOMEM),
            ));
        }
        unsafe {
            (*raw).pb = primary.pointer;
            (*raw).flags |= ffmpeg::sys::AVFMT_FLAG_CUSTOM_IO;
            (*raw).opaque = (&mut *primary.state as *mut IoState).cast();
            (*raw).io_open = Some(deny_io);
        }
        // Only the explicit WAVE byte signature overrides generic probing.
        // Tests may additionally force image2 to exercise the same whitelist.
        let preferred = test_format.or(wave.then_some(c"wav"));
        let format = preferred.map_or(ptr::null(), |name| unsafe {
            ffmpeg::sys::av_find_input_format(name.as_ptr())
        });
        if preferred.is_some() && format.is_null() {
            // SAFETY: open_input has not taken ownership yet; raw owns no AVIO
            // because CUSTOM_IO leaves the primary with its existing guard.
            unsafe { ffmpeg::sys::avformat_free_context(raw) };
            return Err(ffi::fail(
                "select input demuxer",
                path,
                ffmpeg::Error::from(-libc::ENOSYS),
            ));
        }
        let status = unsafe {
            ffmpeg::sys::avformat_open_input(&mut raw, filename.as_ptr(), format, &mut options.0)
        };
        if status < 0 {
            // SAFETY: close only a context still returned by FFmpeg. Never
            // dereference its original address after a failed open.
            unsafe {
                if !raw.is_null() {
                    ffmpeg::sys::avformat_close_input(&mut raw);
                }
            }
            if primary.state.denied.load(Ordering::Relaxed) {
                return Err(Self::denied(path));
            }
            return Err(ffi::fail(
                "open strict input",
                path,
                ffmpeg::Error::from(status),
            ));
        }
        // SAFETY: successful open returns a live format context; wrap transfers
        // its sole ownership to Input. primary outlives this context and all I/O.
        let owner = Self {
            inner: Some(unsafe { ffmpeg::format::context::Input::wrap(raw) }),
            primary: Some(primary),
            path: path.into(),
        };
        owner.check_policy()?;
        // SAFETY: raw is still owned by owner; no concurrent caller uses it.
        let status = unsafe { ffmpeg::sys::avformat_find_stream_info(raw, ptr::null_mut()) };
        owner.check_policy()?;
        if status < 0 {
            return Err(ffi::fail("stream info", path, ffmpeg::Error::from(status)));
        }
        Ok(owner)
    }

    fn denied(path: &Path) -> Error {
        Error::InputPolicy {
            path: path.into(),
            detail: "demuxer attempted additional input I/O".into(),
        }
    }
    pub(crate) fn check_policy(&self) -> Result<()> {
        if self
            .primary
            .as_ref()
            .is_some_and(|io| io.state.denied.load(Ordering::Relaxed))
        {
            return Err(Self::denied(&self.path));
        }
        Ok(())
    }
    /// False is genuine EOF; a sticky denial is always returned before EOF.
    pub(crate) fn read_packet(&mut self, packet: &mut ffmpeg::Packet) -> Result<bool> {
        self.check_policy()?;
        let result = packet.read(self.inner.as_mut().expect("live input owner"));
        self.check_policy()?;
        match result {
            Ok(()) => Ok(true),
            Err(ffmpeg::Error::Eof) => Ok(false),
            Err(e) => Err(ffi::fail("read", &self.path, e)),
        }
    }
    pub(crate) fn seek_checked<R: RangeBounds<i64>>(&mut self, time: i64, range: R) -> Result<()> {
        self.check_policy()?;
        let result = self
            .inner
            .as_mut()
            .expect("live input owner")
            .seek(time, range);
        self.check_policy()?;
        result.map_err(|e| ffi::fail("seek", &self.path, e))
    }
    #[cfg(test)]
    pub(crate) fn open_with_test_drefs(path: &Path) -> Result<Self> {
        ffi::init();
        Self::open_local(path, true, None)
    }
    #[cfg(test)]
    pub(crate) fn open_with_test_image2(path: &Path) -> Result<Self> {
        ffi::init();
        Self::open_local(path, false, Some(c"image2"))
    }
    #[cfg(test)]
    pub(crate) fn drop_events(&self) -> std::sync::Arc<std::sync::Mutex<Vec<&'static str>>> {
        self.primary.as_ref().unwrap().state.events.clone()
    }
    #[cfg(test)]
    pub(crate) fn deny_for_test(&mut self) {
        // SAFETY: all callback inputs are taken from our live owned context.
        unsafe {
            let mut pb = ptr::null_mut();
            deny_io(
                self.as_mut_ptr(),
                &mut pb,
                c"file:forbidden".as_ptr(),
                ffmpeg::sys::AVIO_FLAG_READ,
                ptr::null_mut(),
            );
        }
    }
}
