//! Native macOS Bluetooth RFCOMM transport.
//!
//! Bypasses the broken `IOUserBluetoothSerialDriver` serial port driver
//! and talks directly to the radio via `IOBluetoothRFCOMMChannel`.
//! Connections can be closed and reopened without restarting `bluetoothd`.
//!
//! This module is only available on macOS (`cfg(target_os = "macos")`).

#[cfg(any(target_os = "macos", doc))]
#[expect(
    unsafe_code,
    reason = "The workspace forbids unsafe; this module overrides to allow it because \
              IOBluetoothRFCOMMChannel is a C API with no safe Rust alternative (the built-in \
              `IOUserBluetoothSerialDriver` is broken for stale RFCOMM cleanup per the thd75 \
              BT notes). Safety invariants for each `unsafe` block are documented inline."
)]
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

    // SAFETY: `BluetoothTransport` holds an opaque `*mut c_void` handle that is only
    // ever dereferenced by the `bt_rfcomm_*` FFI functions. The macOS IOBluetooth
    // framework synchronizes its own internal state across threads (the RFCOMM channel
    // is owned by a CFRunLoop-pumped delegate object), so this handle is safe to move
    // between threads.
    unsafe impl Send for BluetoothTransport {}
    // SAFETY: The `bt_rfcomm_*` FFI functions serialize all access to their per-handle
    // state through the IOBluetooth framework's internal locking. `read_fd` is a POSIX
    // file descriptor shared via a pipe the native layer writes into; read/write to a
    // pipe FD is atomic up to PIPE_BUF (4 KiB) bytes, which exceeds any CAT/MCP frame
    // we send. Concurrent `read`/`write` on the same handle from multiple threads is
    // therefore safe.
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
            // SAFETY: `bt_rfcomm_open` takes a NUL-terminated C string pointer (which
            // `CString::as_ptr` guarantees for the `CString`'s lifetime — valid through
            // this call) and a channel number. Returns a handle or NULL. We check for
            // NULL immediately below.
            let handle = unsafe { bt_rfcomm_open(c_name.as_ptr(), SPP_CHANNEL) };
            if handle.is_null() {
                return Err(TransportError::NotFound);
            }

            // SAFETY: `handle` is non-null (checked above) and was produced by the
            // paired `bt_rfcomm_open` above — guaranteed to be a valid handle for the
            // lifetime until we call `bt_rfcomm_close`. `bt_rfcomm_read_fd` returns
            // -1 on failure; we check immediately.
            let read_fd = unsafe { bt_rfcomm_read_fd(handle) };
            if read_fd < 0 {
                // SAFETY: `handle` is non-null (checked above) and was produced by
                // `bt_rfcomm_open` in this same function — the pairing invariant is
                // trivially preserved because we have not yet escaped this function.
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
            // SAFETY: `self.handle` was produced by `bt_rfcomm_open` (in `open()`) and
            // is still live — `close()` is the only path that nulls it, and Rust's
            // `&mut self` bound prevents `close()` from racing with us. The buffer
            // `(data.as_ptr(), data.len())` is valid and in-bounds for reading during
            // the call (the `&[u8]` borrow outlives the FFI call).
            let ret = unsafe { bt_rfcomm_write(self.handle, data.as_ptr(), data.len()) };
            if ret != 0 {
                return Err(TransportError::Write(io::Error::other(
                    "RFCOMM write failed",
                )));
            }
            // SAFETY: `bt_pump_runloop` is a parameterless tick of the CFRunLoop owned
            // by the native layer; it takes no inputs from us and cannot violate any
            // Rust invariant. Idempotent and safe to call repeatedly.
            unsafe { bt_pump_runloop() };
            Ok(())
        }

        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            loop {
                // SAFETY: Parameterless CFRunLoop tick — see the corresponding block in
                // `write()` above. Pumped inside the loop so IOBluetooth callbacks
                // deliver bytes into the pipe before we attempt to read from it.
                unsafe { bt_pump_runloop() };

                // SAFETY: `self.read_fd` was set from a successful `bt_rfcomm_read_fd`
                // call in `open()` and is still live (close() sets it to -1, but Rust's
                // `&mut self` bound prevents close() from racing with us). The buffer
                // `(buf.as_mut_ptr(), buf.len())` is valid and in-bounds for writing
                // during the call.
                let r = unsafe { libc_read(self.read_fd, buf.as_mut_ptr(), buf.len()) };
                if r > 0 {
                    tracing::debug!(bytes = r, "BT read");
                    #[expect(
                        clippy::cast_sign_loss,
                        reason = "`libc::read` returns `ssize_t` where the positive branch \
                                  (`r > 0`) is guaranteed to fit in usize by the POSIX spec — \
                                  it cannot exceed the caller's buffer length. Guarded by the \
                                  preceding `if r > 0` branch."
                    )]
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
                // SAFETY: `self.handle` was produced by `bt_rfcomm_open` (in `open()`)
                // and is non-null (checked above). After close we null it out so no
                // subsequent method will call FFI with a dangling pointer.
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
                // SAFETY: `self.handle` was produced by `bt_rfcomm_open` and has not
                // yet been closed (its nulling in `close()` would prevent reaching
                // this branch). Drop is the final owner, so this is the terminal use
                // and cannot race.
                unsafe { bt_rfcomm_close(self.handle) };
            }
        }
    }

    /// Raw read syscall (avoids `libc` dependency).
    ///
    /// # Safety
    ///
    /// `fd` must be an open file descriptor and `(buf, len)` must describe a valid
    /// writable buffer for at least `len` bytes.
    unsafe fn libc_read(fd: i32, buf: *mut u8, len: usize) -> isize {
        unsafe extern "C" {
            fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
        }
        // SAFETY: Forwarded from the function's own safety contract — the caller
        // guaranteed `fd` is valid and `(buf, len)` is a writable buffer of `len`
        // bytes.
        unsafe { read(fd, buf, len) }
    }
}

#[cfg(any(target_os = "macos", doc))]
pub use inner::BluetoothTransport;
