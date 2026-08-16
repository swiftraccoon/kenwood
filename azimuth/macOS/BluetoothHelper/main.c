// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

// Referencing this symbol pulls the native TH-D75 helper constructor out of
// the Rust static archive. With the private parent launch sentinel present, that
// constructor owns the process before main and exposes stdin/stdout as the
// isolated Bluetooth byte stream.
extern void bt_helper_link_anchor(void);

int main(void) {
    bt_helper_link_anchor();
    // This executable is private implementation detail. A launch without the
    // parent's private launch sentinel performs no radio operation.
    return 86;
}
