// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
//
// Bridging header for the iPad target. The iOS SDK ships the IOKit
// user-client C API (public since iOS 16 for DriverKit apps) but no
// Swift module map, so `import IOKit` fails; the bridging header is
// the supported route to those declarations.

#ifndef IOKitShim_h
#define IOKitShim_h

#include <IOKit/IOKitLib.h>

#endif /* IOKitShim_h */
