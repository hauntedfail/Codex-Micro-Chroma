#[cfg(target_os = "macos")]
mod platform {
    use std::{
        fs::{self, File, OpenOptions},
        io,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };

    use fs2::FileExt;
    use hidapi::{HidApi, HidDevice};
    use serde_json::Value;
    use thiserror::Error;

    use crate::protocol::{
        ambient_effect_request, frame_rpc_request, off_request, LightingEffect, RpcRequest,
        REPORT_SIZE,
    };

    const VENDOR_ID: u16 = 0x303A;
    const PRODUCT_ID: u16 = 0x8360;
    const USAGE_PAGE: u16 = 0xFF00;
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);
    const LOCK_TIMEOUT: Duration = Duration::from_secs(20);

    #[derive(Debug, Error)]
    pub enum DeviceError {
        #[error("could not initialize HID: {0}")]
        Initialize(#[source] hidapi::HidError),
        #[error("expected exactly one Codex Micro HID interface; found {0}")]
        InterfaceCount(usize),
        #[error("could not open Codex Micro: {0}")]
        Open(#[source] hidapi::HidError),
        #[error("could not write Codex Micro report: {0}")]
        Write(#[source] hidapi::HidError),
        #[error("Codex Micro accepted only {written} of {expected} report bytes")]
        ShortWrite { written: usize, expected: usize },
        #[error("could not read Codex Micro response: {0}")]
        Read(#[source] hidapi::HidError),
        #[error("timed out waiting for Codex Micro RPC response")]
        ResponseTimeout,
        #[error("Codex Micro RPC failed: {0}")]
        Rpc(String),
        #[error("could not prepare the HID write lock: {0}")]
        LockIo(#[source] io::Error),
        #[error("timed out waiting for another Codex Micro LED writer")]
        LockTimeout,
        #[error("Codex Micro connection failed ({initial}); reconnect attempt failed: {retry}")]
        Reconnect {
            initial: String,
            #[source]
            retry: Box<DeviceError>,
        },
        #[error(transparent)]
        Protocol(#[from] crate::protocol::ProtocolError),
    }

    pub struct DeviceSummary {
        pub product: Option<String>,
        pub serial: Option<String>,
    }

    pub struct Controller {
        _lock: HidWriteLock,
        _api: HidApi,
        device: HidDevice,
    }

    impl Controller {
        pub fn open() -> Result<Self, DeviceError> {
            let lock = HidWriteLock::acquire(LOCK_TIMEOUT)?;
            let (api, device, _summary) = open_device()?;
            Ok(Self {
                _lock: lock,
                _api: api,
                device,
            })
        }

        pub fn set_ambient(
            &mut self,
            id: u16,
            effect: LightingEffect,
            packed_rgb: u32,
            brightness: f32,
            speed: f32,
            magic: f32,
        ) -> Result<(), DeviceError> {
            let request = ambient_effect_request(id, effect, packed_rgb, brightness, speed, magic)?;
            self.send_with_reconnect(id, &request)
        }

        pub fn clear(&mut self, id: u16) -> Result<(), DeviceError> {
            self.send_with_reconnect(id, &off_request(id))
        }

        fn send_with_reconnect(
            &mut self,
            id: u16,
            request: &RpcRequest,
        ) -> Result<(), DeviceError> {
            match send_request(&self.device, id, request) {
                Ok(()) => Ok(()),
                Err(error) if error.is_transport_failure() => {
                    let initial = error.to_string();
                    self.reopen()
                        .and_then(|()| send_request(&self.device, id, request))
                        .map_err(|retry| DeviceError::Reconnect {
                            initial,
                            retry: Box::new(retry),
                        })
                }
                Err(error) => Err(error),
            }
        }

        fn reopen(&mut self) -> Result<(), DeviceError> {
            let (api, device, _summary) = open_device()?;
            self.device = device;
            self._api = api;
            Ok(())
        }
    }

    impl DeviceError {
        fn is_transport_failure(&self) -> bool {
            matches!(
                self,
                Self::Open(_)
                    | Self::Write(_)
                    | Self::ShortWrite { .. }
                    | Self::Read(_)
                    | Self::ResponseTimeout
            )
        }
    }

    struct HidWriteLock {
        file: File,
    }

    impl HidWriteLock {
        fn acquire(timeout: Duration) -> Result<Self, DeviceError> {
            let directory = lock_directory();
            fs::create_dir_all(&directory).map_err(DeviceError::LockIo)?;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(DeviceError::LockIo)?;
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(directory.join("hid-write.lock"))
                .map_err(DeviceError::LockIo)?;
            let deadline = Instant::now() + timeout;

            loop {
                match FileExt::try_lock_exclusive(&file) {
                    Ok(()) => return Ok(Self { file }),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(DeviceError::LockTimeout);
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(error) => return Err(DeviceError::LockIo(error)),
                }
            }
        }
    }

    impl Drop for HidWriteLock {
        fn drop(&mut self) {
            let _ = FileExt::unlock(&self.file);
        }
    }

    fn lock_directory() -> PathBuf {
        // SAFETY: geteuid has no preconditions and only returns the current process identity.
        let user_id = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("codex-micro-chroma-{user_id}"))
    }

    fn open_device() -> Result<(HidApi, HidDevice, DeviceSummary), DeviceError> {
        let api = HidApi::new().map_err(DeviceError::Initialize)?;
        api.set_open_exclusive(false);

        let matches = api
            .device_list()
            .filter(|device| {
                device.vendor_id() == VENDOR_ID
                    && device.product_id() == PRODUCT_ID
                    && device.usage_page() == USAGE_PAGE
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(DeviceError::InterfaceCount(matches.len()));
        }

        let summary = DeviceSummary {
            product: matches[0].product_string().map(str::to_owned),
            serial: matches[0].serial_number().map(str::to_owned),
        };
        let handle = matches[0].open_device(&api).map_err(DeviceError::Open)?;
        Ok((api, handle, summary))
    }

    pub fn probe() -> Result<DeviceSummary, DeviceError> {
        let (_api, _handle, summary) = open_device()?;
        Ok(summary)
    }

    pub fn set_ambient(
        id: u16,
        effect: LightingEffect,
        packed_rgb: u32,
        brightness: f32,
        speed: f32,
        magic: f32,
    ) -> Result<(), DeviceError> {
        Controller::open()?.set_ambient(id, effect, packed_rgb, brightness, speed, magic)
    }

    pub fn clear(id: u16) -> Result<(), DeviceError> {
        Controller::open()?.clear(id)
    }

    fn send_request(device: &HidDevice, id: u16, request: &RpcRequest) -> Result<(), DeviceError> {
        for report in frame_rpc_request(request)? {
            let written = device.write(&report).map_err(DeviceError::Write)?;
            if written != REPORT_SIZE {
                return Err(DeviceError::ShortWrite {
                    written,
                    expected: REPORT_SIZE,
                });
            }
        }
        wait_for_response(device, id)
    }

    fn wait_for_response(device: &HidDevice, id: u16) -> Result<(), DeviceError> {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let mut pending = Vec::<u8>::new();
        let mut report = [0_u8; REPORT_SIZE];

        while Instant::now() < deadline {
            let read = device
                .read_timeout(&mut report, 100)
                .map_err(DeviceError::Read)?;
            if read < 3 || report[1] != 0x02 {
                continue;
            }
            let length = usize::from(report[2]).min(read.saturating_sub(3));
            pending.extend_from_slice(&report[3..3 + length]);

            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let line = pending.drain(..=newline).collect::<Vec<_>>();
                let Ok(response) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                let response_id = response
                    .get("id")
                    .or_else(|| response.get("i"))
                    .and_then(Value::as_u64);
                if response_id != Some(u64::from(id)) {
                    continue;
                }
                if let Some(error) = response.get("error") {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown device error");
                    return Err(DeviceError::Rpc(message.to_owned()));
                }
                return Ok(());
            }
        }

        Err(DeviceError::ResponseTimeout)
    }

    #[cfg(test)]
    mod tests {
        use super::DeviceError;

        #[test]
        fn reconnect_policy_is_limited_to_transport_failures() {
            assert!(DeviceError::ResponseTimeout.is_transport_failure());
            assert!(!DeviceError::Rpc("rejected".into()).is_transport_failure());
            assert!(!DeviceError::LockTimeout.is_transport_failure());
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use thiserror::Error;

    use crate::protocol::LightingEffect;

    #[derive(Debug, Error)]
    #[error("Codex Micro HID support is only available on macOS")]
    pub struct DeviceError;

    pub struct DeviceSummary {
        pub product: Option<String>,
        pub serial: Option<String>,
    }

    pub struct Controller;

    impl Controller {
        pub fn open() -> Result<Self, DeviceError> {
            Err(DeviceError)
        }

        pub fn set_ambient(
            &mut self,
            _id: u16,
            _effect: LightingEffect,
            _packed_rgb: u32,
            _brightness: f32,
            _speed: f32,
            _magic: f32,
        ) -> Result<(), DeviceError> {
            Err(DeviceError)
        }

        pub fn clear(&mut self, _id: u16) -> Result<(), DeviceError> {
            Err(DeviceError)
        }
    }

    pub fn probe() -> Result<DeviceSummary, DeviceError> {
        Err(DeviceError)
    }

    pub fn set_ambient(
        _id: u16,
        _effect: LightingEffect,
        _packed_rgb: u32,
        _brightness: f32,
        _speed: f32,
        _magic: f32,
    ) -> Result<(), DeviceError> {
        Err(DeviceError)
    }

    pub fn clear(_id: u16) -> Result<(), DeviceError> {
        Err(DeviceError)
    }
}

pub use platform::*;
