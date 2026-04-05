//! System configuration types.
//!
//! Covers radio-wide settings including user preferences, frequency range
//! limits, I/O port control, and SD card operations.
//!
//! System setting types are defined in the [`settings`](super::settings) module:
//! [`SystemSettings`](super::settings::SystemSettings),
//! [`AudioSettings`](super::settings::AudioSettings), and
//! [`DisplaySettings`](super::settings::DisplaySettings).
//!
//! # Transceiver reset (per User Manual Chapter 12)
//!
//! Menu No. 999 or `[F]` + Power ON provides three reset types:
//!
//! - **VFO Reset**: initializes VFO and accompanying settings only.
//! - **Partial Reset**: initializes all settings except memory channels
//!   and DTMF memory channels.
//! - **Full Reset**: initializes all customized settings. Date and time
//!   are not reset. To enable voice guidance after full reset, press
//!   `[PF2]` + Power ON.
//!
//! # Firmware version (per User Manual Chapter 12)
//!
//! Menu No. 991 displays the current firmware version. Firmware updates
//! are applied by connecting to a PC via USB.
//!
//! # USB function (per User Manual Chapter 17)
//!
//! Menu No. 980: `COM+AF/IF Output` (virtual COM port + audio output)
//! or `Mass Storage` (microSD card access from PC). The radio is a
//! USB 2.0 device supporting CDC, ADC 1.0, and MSC device classes.
//! USB hub connections are not supported.
//!
//! This module is intentionally empty; system types live in
//! [`settings`](super::settings) to keep related types together.
