//! Native macOS Bluetooth RFCOMM transport.
//!
//! Bypasses the broken `IOUserBluetoothSerialDriver` serial port driver
//! and talks directly to the radio via `IOBluetoothRFCOMMChannel`.
//! Connections can be closed and reopened without restarting `bluetoothd`.
//!
//! This module is only available on macOS (`cfg(target_os = "macos")`).

#[cfg(any(target_os = "macos", doc))]
#[allow(unsafe_code)]
mod inner {
    use std::io;

    use crate::error::TransportError;
    use crate::transport::Transport;

    unsafe extern "C" {
        fn bt_rfcomm_open(device_name: *const i8, channel: u8) -> *mut std::ffi::c_void;
        fn bt_rfcomm_write(handle: *mut std::ffi::c_void, data: *const u8, len: usize) -> i32;
        fn bt_rfcomm_read_fd(handle: *mut std::ffi::c_void) -> i32;
        fn bt_rfcomm_close(handle: *mut std::ffi::c_void);
        fn bt_pump_runloop();
    }

    /// The RFCOMM channel for the TH-D75's SPP (Serial Port) service.
    const SPP_CHANNEL: u8 = 2;

    /// Default device name for BT discovery.
    const DEFAULT_DEVICE_NAME: &str = "TH-D75";

    /// Native macOS Bluetooth transport using `IOBluetooth` RFCOMM.
    pub struct BluetoothTransport {
        handle: *mut std::ffi::c_void,
        read_fd: i32,
    }

    unsafe impl Send for BluetoothTransport {}
    unsafe impl Sync for BluetoothTransport {}

    impl std::fmt::Debug for BluetoothTransport {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BluetoothTransport")
                .field("handle", &self.handle)
                .field("read_fd", &self.read_fd)
                .finish()
        }
    }

    impl BluetoothTransport {
        /// Connect to a TH-D75 radio via Bluetooth RFCOMM.
        ///
        /// # Errors
        ///
        /// Returns [`TransportError::NotFound`] if no device found or RFCOMM fails.
        pub fn open(device_name: Option<&str>) -> Result<Self, TransportError> {
            let name = device_name.unwrap_or(DEFAULT_DEVICE_NAME);
            tracing::info!(device = %name, channel = SPP_CHANNEL, "opening Bluetooth RFCOMM");

            let c_name = std::ffi::CString::new(name).map_err(|_| TransportError::NotFound)?;
            let handle = unsafe { bt_rfcomm_open(c_name.as_ptr(), SPP_CHANNEL) };
            if handle.is_null() {
                return Err(TransportError::NotFound);
            }

            let read_fd = unsafe { bt_rfcomm_read_fd(handle) };
            if read_fd < 0 {
                unsafe { bt_rfcomm_close(handle) };
                return Err(TransportError::NotFound);
            }

            tracing::info!(device = %name, "Bluetooth RFCOMM connected");
            Ok(Self { handle, read_fd })
        }
    }

    impl Transport for BluetoothTransport {
        async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            tracing::debug!(bytes = data.len(), "BT write");
            let ret = unsafe { bt_rfcomm_write(self.handle, data.as_ptr(), data.len()) };
            if ret != 0 {
                return Err(TransportError::Write(io::Error::other(
                    "RFCOMM write failed",
                )));
            }
            unsafe { bt_pump_runloop() };
            Ok(())
        }

        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            loop {
                unsafe { bt_pump_runloop() };

                let r = unsafe { libc_read(self.read_fd, buf.as_mut_ptr(), buf.len()) };
                if r > 0 {
                    tracing::debug!(bytes = r, "BT read");
                    #[allow(clippy::cast_sign_loss)]
                    return Ok(r as usize);
                }
                if r == 0 {
                    return Err(TransportError::Read(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "BT pipe closed",
                    )));
                }
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    continue;
                }
                return Err(TransportError::Read(err));
            }
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            tracing::info!("closing Bluetooth RFCOMM");
            if !self.handle.is_null() {
                unsafe { bt_rfcomm_close(self.handle) };
                self.handle = std::ptr::null_mut();
                self.read_fd = -1;
            }
            Ok(())
        }
    }

    impl Drop for BluetoothTransport {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe { bt_rfcomm_close(self.handle) };
            }
        }
    }

    /// Raw read syscall (avoids `libc` dependency).
    unsafe fn libc_read(fd: i32, buf: *mut u8, len: usize) -> isize {
        unsafe extern "C" {
            fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
        }
        unsafe { read(fd, buf, len) }
    }
}

#[cfg(any(target_os = "macos", doc))]
pub use inner::BluetoothTransport;
