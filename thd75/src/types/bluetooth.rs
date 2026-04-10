//! Bluetooth types for the TH-D75's wireless interface.
//!
//! The TH-D75 supports Bluetooth 3.0, Class 2 with two profiles
//! (per Operating Tips §5.12 and §6.2 specifications):
//!
//! - **HSP** (Headset Profile): audio via BT headsets (PTT still on radio).
//! - **SPP** (Serial Port Profile): serial data for CAT control, APRS apps,
//!   MCP-D75, and Reflector Terminal Mode.
//!
//! HFP (Hands-Free Profile) and BLE (Bluetooth Low Energy) are **not**
//! supported. The radio only works with HSP + SPP compatible devices.
//!
//! # Hardware (per service manual §2.1.8.9, Fig.12)
//!
//! The BT/GPS combo IC is IC2044, connected to the MAIN MPU (IC2005)
//! via three interfaces:
//!
//! - **HCI UART** (UART2): BT control/data at 115.2 kbps (access) /
//!   3.569 Mbps (normal), level-shifted 3.3V↔1.8V through IC2046.
//! - **AI2 UART** (UART1): GPS NMEA data at 115.2 kbps, level-shifted
//!   through IC2036.
//! - **PCM audio** (McASP): digital audio I2S for BT headset audio,
//!   level-shifted 3.3V↔1.8V through IC2048.
//!
//! IC2044 runs on dedicated 3.3V (IC2041) and 1.8V (IC2038) regulators,
//! with a 19.2 MHz TCXO (X2003) and 32.768 kHz RTC crystal (X2004).
//! The MAIN MPU controls BT power via `/BT_SHUTDOWN` (active low reset).
//!
//! Per User Manual Chapter 18:
//!
//! - Menu No. 930: Bluetooth on/off (default: Off).
//! - Menu No. 931: Connect to a paired device from the device list.
//! - Menu No. 932: Device search (pairing with new headset).
//! - Menu No. 933: Disconnect from a Bluetooth device.
//! - Menu No. 934: Pairing mode (for PC connections, 60-second countdown).
//! - Menu No. 935: Device information / name (up to 19 characters).
//! - Menu No. 936: Auto connect on power-on (default: On). Does not
//!   support automatic connection with a PC.
//!
//! HSP audio bandwidth is limited to 4 kHz and below, so FM radio
//! reception may sound different from speakers/earphones.
//!
//! Headset volume cannot be adjusted via the radio's `[VOL]` control;
//! use the headset's own volume control.
//!
//! Transfer rate: USB up to 12 Mbps, Bluetooth up to 128 kbps.
//! When connecting to a PC via Bluetooth 2.0 or earlier, the PIN code
//! is `0000`.
//!
//! Per User Manual Chapter 28 (specifications): Bluetooth output power
//! is -6 to +4 dBm, carrier frequency drift ±25 kHz (one slot),
//! ±40 kHz (three/five slot).
//!
//! This module is intentionally empty pending hardware capture data.
