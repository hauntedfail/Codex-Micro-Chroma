#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::{c_char, c_void, CStr},
        ptr, slice,
        sync::{Arc, Mutex},
        thread::{self, JoinHandle},
    };

    use crossbeam_channel::{bounded, Sender};
    use thiserror::Error;

    use crate::audio::{AudioAnalyzer, AudioAnalyzerError, AudioFeatureFrame};

    const MAXIMUM_FRAMES: usize = 4_096;
    const ERROR_BUFFER_SIZE: usize = 1_024;

    type NativeAudioCallback =
        unsafe extern "C" fn(*const f32, *const f32, u32, f64, f64, *mut c_void);

    unsafe extern "C" {
        fn chroma_process_tap_start(
            callback: NativeAudioCallback,
            context: *mut c_void,
            sample_rate: *mut f64,
            error_buffer: *mut c_char,
            error_buffer_size: usize,
        ) -> *mut c_void;
        fn chroma_process_tap_stop(handle: *mut c_void);
    }

    #[derive(Debug, Error)]
    pub enum SystemAudioError {
        #[error("could not start Core Audio Process Tap: {0}")]
        Start(String),
        #[error("Core Audio Process Tap returned an invalid sample rate: {0}")]
        InvalidSampleRate(f64),
        #[error("could not create the audio analyzer worker: {0}")]
        Worker(#[source] std::io::Error),
        #[error("could not initialize the audio analyzer: {0}")]
        Analyzer(#[source] AudioAnalyzerError),
    }

    struct AudioPacket {
        frames: usize,
        sample_time: f64,
        left: [f32; MAXIMUM_FRAMES],
        right: [f32; MAXIMUM_FRAMES],
    }

    struct CallbackContext {
        sender: Sender<AudioPacket>,
    }

    pub struct SystemAudioSource {
        handle: *mut c_void,
        callback_context: Option<Box<CallbackContext>>,
        worker: Option<JoinHandle<()>>,
        latest: Arc<Mutex<Option<AudioFeatureFrame>>>,
        sample_rate: f32,
    }

    impl SystemAudioSource {
        pub fn start() -> Result<Self, SystemAudioError> {
            let (sender, receiver) = bounded::<AudioPacket>(8);
            let mut callback_context = Box::new(CallbackContext { sender });
            let mut error_buffer = [0_i8; ERROR_BUFFER_SIZE];
            let mut sample_rate = 0.0_f64;

            // SAFETY: callback_context stays boxed at the same address until the native tap has
            // been synchronously stopped in Drop. The bridge bounds each callback to 4096 frames.
            let handle = unsafe {
                chroma_process_tap_start(
                    receive_audio,
                    (&mut *callback_context as *mut CallbackContext).cast(),
                    &mut sample_rate,
                    error_buffer.as_mut_ptr(),
                    error_buffer.len(),
                )
            };
            if handle.is_null() {
                // SAFETY: the Objective-C bridge always NUL-terminates this fixed buffer.
                let message = unsafe { CStr::from_ptr(error_buffer.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                return Err(SystemAudioError::Start(if message.is_empty() {
                    "unknown error".into()
                } else {
                    message
                }));
            }
            if !sample_rate.is_finite() || sample_rate <= 0.0 || sample_rate > f32::MAX as f64 {
                // SAFETY: handle was returned by chroma_process_tap_start and has not been stopped.
                unsafe { chroma_process_tap_stop(handle) };
                return Err(SystemAudioError::InvalidSampleRate(sample_rate));
            }

            let sample_rate = sample_rate as f32;
            let mut analyzer = match AudioAnalyzer::new(sample_rate) {
                Ok(analyzer) => analyzer,
                Err(error) => {
                    // SAFETY: handle was returned by chroma_process_tap_start and has not been
                    // stopped. Stopping it synchronously keeps callback_context valid here.
                    unsafe { chroma_process_tap_stop(handle) };
                    return Err(SystemAudioError::Analyzer(error));
                }
            };
            let latest = Arc::new(Mutex::new(None));
            let worker_latest = Arc::clone(&latest);
            let worker = match thread::Builder::new()
                .name("codex-micro-chroma-audio".into())
                .spawn(move || {
                    while let Ok(packet) = receiver.recv() {
                        let timestamp = packet.sample_time / f64::from(sample_rate);
                        if let Some(frame) = analyzer.push_stereo(
                            &packet.left[..packet.frames],
                            &packet.right[..packet.frames],
                            timestamp,
                        ) {
                            if let Ok(mut guard) = worker_latest.lock() {
                                *guard = Some(frame);
                            }
                        }
                    }
                }) {
                Ok(worker) => worker,
                Err(error) => {
                    // SAFETY: handle was returned by chroma_process_tap_start and callbacks stop
                    // synchronously before callback_context is dropped on this return path.
                    unsafe { chroma_process_tap_stop(handle) };
                    return Err(SystemAudioError::Worker(error));
                }
            };

            Ok(Self {
                handle,
                callback_context: Some(callback_context),
                worker: Some(worker),
                latest,
                sample_rate,
            })
        }

        pub fn latest(&self) -> Option<AudioFeatureFrame> {
            self.latest.lock().ok().and_then(|guard| *guard)
        }

        pub fn sample_rate(&self) -> f32 {
            self.sample_rate
        }
    }

    impl Drop for SystemAudioSource {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                // SAFETY: the bridge stops callbacks before releasing the retained native object.
                unsafe { chroma_process_tap_stop(self.handle) };
                self.handle = ptr::null_mut();
            }
            self.callback_context.take();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    unsafe extern "C" fn receive_audio(
        left: *const f32,
        right: *const f32,
        frame_count: u32,
        _sample_rate: f64,
        sample_time: f64,
        context: *mut c_void,
    ) {
        if left.is_null() || right.is_null() || context.is_null() {
            return;
        }
        let frames = usize::try_from(frame_count)
            .unwrap_or(MAXIMUM_FRAMES)
            .min(MAXIMUM_FRAMES);
        if frames == 0 {
            return;
        }

        let mut packet = AudioPacket {
            frames,
            sample_time,
            left: [0.0; MAXIMUM_FRAMES],
            right: [0.0; MAXIMUM_FRAMES],
        };
        // SAFETY: the Objective-C bridge guarantees both buffers contain frame_count Float32
        // samples and caps frame_count at MAXIMUM_FRAMES for the duration of this callback.
        let left_samples = unsafe { slice::from_raw_parts(left, frames) };
        // SAFETY: same contract as left_samples.
        let right_samples = unsafe { slice::from_raw_parts(right, frames) };
        packet.left[..frames].copy_from_slice(left_samples);
        packet.right[..frames].copy_from_slice(right_samples);

        // SAFETY: context points to CallbackContext until the native tap is synchronously stopped.
        let callback_context = unsafe { &*(context.cast::<CallbackContext>()) };
        let _ = callback_context.sender.try_send(packet);
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use thiserror::Error;

    use crate::audio::AudioFeatureFrame;

    #[derive(Debug, Error)]
    #[error("Core Audio Process Tap is only available on macOS")]
    pub struct SystemAudioError;

    pub struct SystemAudioSource;

    impl SystemAudioSource {
        pub fn start() -> Result<Self, SystemAudioError> {
            Err(SystemAudioError)
        }

        pub fn latest(&self) -> Option<AudioFeatureFrame> {
            None
        }

        pub fn sample_rate(&self) -> f32 {
            0.0
        }
    }
}

pub use platform::*;
