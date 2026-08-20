#ifdef __APPLE__

#import <Foundation/Foundation.h>
#import <IOBluetooth/IOBluetooth.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

// IOBluetooth's RFCOMM writes are not cancellable in-process. On current
// macOS, writeSync: can remain in CFWriteStreamWrite forever when the peer
// stops granting flow-control credit. writeAsync: only moves that same
// unbounded call into a block on the main dispatch queue, where it wedges the
// host's CFRunLoop instead. Even the deprecated write:length:sleep:NO method
// is a runtime trampoline to writeSync: (the sleep argument is ignored).
//
// The hard boundary is therefore a helper *process*. The Rust parent spawns a
// signed executable containing this constructor with the private environment
// sentinel below. This constructor runs before main, owns every IOBluetooth
// object, exposes stdin/stdout as raw serial byte streams, and exits. If
// IOBluetooth wedges, the parent remains responsive and SIGKILLs this process
// during timeout or cleanup. Command-line clients can re-execute themselves;
// sandboxed applications provide a separately signed inheriting helper.

#define BT_HELPER_SENTINEL_ENV "THD75_BT_HELPER_PROCESS_V1"
#define BT_HELPER_SENTINEL_VALUE "4d7f29c8b35a"
#define BT_HELPER_DEVICE_ENV "THD75_BT_HELPER_DEVICE"
#define BT_HELPER_CHANNEL_ENV "THD75_BT_HELPER_CHANNEL"
#define BT_HELPER_CONTROL_ENV "THD75_BT_HELPER_CONTROL_MODE"
#define BT_HELPER_TEST_ENV "THD75_BT_HELPER_TEST_MODE"
#define BT_HELPER_LIVENESS_FD_ENV "THD75_BT_HELPER_LIVENESS_FD"
#define BT_HELPER_PRE_READY_CAPACITY 4096
#define BT_HELPER_MAX_PAIRED_DEVICES 64

static const uint8_t kReadyMagic[] = "THD75BT-READY-v1";

// The selected identifier matched more than one paired device name. Keep
// this distinct from 71, whose process-local open race is retried once.
#define BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME 87
#define BT_HELPER_EXIT_TOO_MANY_PAIRED_DEVICES 88

// Opt-in shim tracing. Set THD75_BT_TRACE=1 on the parent. The helper
// inherits stderr, so every line carries its PID and can be correlated with
// the Rust transport log without contaminating the raw stdout byte stream.
static _Atomic int g_bt_trace = -1;

static int bt_trace_enabled(void) {
    int value = g_bt_trace;
    if (value < 0) {
        const char *env = getenv("THD75_BT_TRACE");
        value = (env && env[0] && env[0] != '0') ? 1 : 0;
        g_bt_trace = value;
    }
    return value;
}

static void bt_trace(const char *format, ...)
    __attribute__((format(printf, 1, 2)));
static void bt_trace(const char *format, ...) {
    if (!bt_trace_enabled()) return;
    struct timeval time;
    gettimeofday(&time, NULL);
    struct tm broken_down;
    gmtime_r(&time.tv_sec, &broken_down);
    fprintf(stderr,
            "[bt-helper pid=%d] %02d:%02d:%02d.%06d ",
            getpid(), broken_down.tm_hour, broken_down.tm_min,
            broken_down.tm_sec, (int)time.tv_usec);
    va_list arguments;
    va_start(arguments, format);
    vfprintf(stderr, format, arguments);
    va_end(arguments);
    fputc('\n', stderr);
    fflush(stderr);
}

// Referenced by Rust solely to force this Objective-C object (and therefore
// its constructor) out of the static archive into every using executable.
void bt_helper_link_anchor(void) {}

// std::process pipes do not expose a stable Rust API for O_NONBLOCK. Keep the
// platform constant and fcntl call in the native shim.
int bt_fd_set_nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0) return -1;
    return fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

// Create the dedicated parent-liveness pipe with close-on-exec on both ends.
// The Rust child's pre-exec hook explicitly duplicates only the read end onto
// its fixed descriptor; the helper therefore cannot accidentally inherit a
// writer that would mask parent death.
int bt_liveness_pipe_create(int *read_fd, int *write_fd) {
    if (!read_fd || !write_fd) {
        errno = EINVAL;
        return -1;
    }
    int fds[2];
    if (pipe(fds) != 0) return -1;
    int read_flags = fcntl(fds[0], F_GETFD);
    int write_flags = fcntl(fds[1], F_GETFD);
    if (read_flags < 0 || write_flags < 0 ||
        fcntl(fds[0], F_SETFD, read_flags | FD_CLOEXEC) != 0 ||
        fcntl(fds[1], F_SETFD, write_flags | FD_CLOEXEC) != 0) {
        int saved_errno = errno;
        close(fds[0]);
        close(fds[1]);
        errno = saved_errno;
        return -1;
    }
    *read_fd = fds[0];
    *write_fd = fds[1];
    return 0;
}

// Called only by Command::pre_exec. Keep this async-signal-safe: dup2 and
// fcntl are the only operations performed between fork and exec.
int bt_helper_prepare_liveness_fd(int source_fd, int target_fd) {
    if (source_fd < 0 || target_fd < 0) {
        errno = EBADF;
        return -1;
    }
    if (source_fd != target_fd && dup2(source_fd, target_fd) < 0) return -1;
    int flags = fcntl(target_fd, F_GETFD);
    if (flags < 0) return -1;
    return fcntl(target_fd, F_SETFD, flags & ~FD_CLOEXEC);
}

@class RfcommDelegate;

typedef struct {
    IOBluetoothDevice *device;
    IOBluetoothRFCOMMChannel *channel;
    RfcommDelegate *delegate;
    int output_fd;
    _Atomic int state;
    uint8_t pre_ready[BT_HELPER_PRE_READY_CAPACITY];
    size_t pre_ready_length;
} RfcommContext;

@interface RfcommDelegate : NSObject <IOBluetoothRFCOMMChannelDelegate>
@property(nonatomic, assign) RfcommContext *ctx;
@end

static pthread_mutex_t g_context_mutex = PTHREAD_MUTEX_INITIALIZER;
static int write_all(int fd, const uint8_t *bytes, size_t length);

static double monotonic_seconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0.0;
    return (double)now.tv_sec + ((double)now.tv_nsec / 1000000000.0);
}

static void *parent_liveness_watchdog(void *argument) {
    int fd = (int)(intptr_t)argument;
    uint8_t byte;
    for (;;) {
        ssize_t count = read(fd, &byte, sizeof(byte));
        if (count > 0) continue;
        if (count < 0 && errno == EINTR) continue;
        // EOF proves every copy of the parent's write endpoint is gone. Use
        // _exit because the helper main thread may be wedged in IOBluetooth
        // and no Objective-C or stdio cleanup is safe from this watchdog.
        _exit(83);
    }
}

static int start_parent_liveness_watchdog(int fd) {
    if (fd < 0 || fcntl(fd, F_GETFD) < 0) return -1;
    pthread_t thread;
    int result = pthread_create(
        &thread, NULL, parent_liveness_watchdog, (void *)(intptr_t)fd
    );
    if (result != 0) {
        errno = result;
        return -1;
    }
    result = pthread_detach(thread);
    if (result != 0) {
        errno = result;
        return -1;
    }
    return 0;
}

@implementation RfcommDelegate
- (void)rfcommChannelOpenComplete:(IOBluetoothRFCOMMChannel *)channel
                           status:(IOReturn)status {
    (void)channel;
    bt_trace("RFCOMM open complete status=0x%08x", (unsigned)status);
    pthread_mutex_lock(&g_context_mutex);
    if (_ctx) {
        if (status == kIOReturnSuccess && _ctx->state >= 0) {
            _ctx->state = 1;
        } else {
            _ctx->state = -1;
        }
    }
    pthread_mutex_unlock(&g_context_mutex);
}

- (void)rfcommChannelData:(IOBluetoothRFCOMMChannel *)channel
                     data:(void *)data
                   length:(size_t)length {
    (void)channel;
    pthread_mutex_lock(&g_context_mutex);
    if (_ctx) {
        if (_ctx->output_fd >= 0) {
            // stdout deliberately remains blocking. If the parent stops
            // draining, only this disposable helper can stall; close/timeout
            // SIGKILLs it. Dropping or partially forwarding bytes here would
            // corrupt the CAT/MCP stream while pretending the link was healthy.
            if (write_all(_ctx->output_fd, data, length) != 0) {
                _ctx->state = -1;
            }
        } else if (_ctx->pre_ready_length <= BT_HELPER_PRE_READY_CAPACITY &&
                   length <=
                       BT_HELPER_PRE_READY_CAPACITY - _ctx->pre_ready_length) {
            // RFCOMM may deliver bytes in the same run-loop slice as its open
            // callback. Preserve them until READY is fully emitted so radio
            // ingress cannot corrupt the readiness handshake or disappear.
            memcpy(_ctx->pre_ready + _ctx->pre_ready_length, data, length);
            _ctx->pre_ready_length += length;
        } else {
            // Never pretend a stream is valid after losing ingress. The
            // helper exits and forces its parent to reopen from a clean link.
            _ctx->state = -1;
        }
    }
    pthread_mutex_unlock(&g_context_mutex);
}

- (void)rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)channel {
    (void)channel;
    pthread_mutex_lock(&g_context_mutex);
    if (_ctx) _ctx->state = 0;
    pthread_mutex_unlock(&g_context_mutex);
}
@end

static void destroy_rfcomm_context(RfcommContext *ctx) {
    if (!ctx) return;

    // A healthy helper owns the channel until close completion. Pumping the
    // helper run loop here mirrors the REPL's ownership lifecycle: release
    // this SPP session before another process attempts to open it. The Rust
    // parent bounds the whole helper and can still SIGKILL a framework call
    // that does not return.
    pthread_mutex_lock(&g_context_mutex);
    ctx->output_fd = -1;
    int channel_state = ctx->state;
    pthread_mutex_unlock(&g_context_mutex);
    BOOL close_completed = (channel_state == 0);
    if (ctx->channel && channel_state == 1) {
        bt_trace("RFCOMM close begin");
        [ctx->channel closeChannel];
        for (int attempt = 0; attempt < 50 && ctx->state == 1; attempt++) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, false);
        }
        close_completed = (ctx->state == 0);
        bt_trace("RFCOMM close end state=%d", ctx->state);
    }

    pthread_mutex_lock(&g_context_mutex);
    ctx->delegate.ctx = NULL;
    ctx->state = -1;
    pthread_mutex_unlock(&g_context_mutex);

    if (ctx->channel) {
        [ctx->channel setDelegate:nil];
        if (!close_completed) [ctx->channel closeChannel];
        ctx->channel = nil;
    }
    ctx->delegate = nil;
    ctx->device = nil;
    free(ctx);
}

static int is_ascii_hex_digit(char byte) {
    return (byte >= '0' && byte <= '9') ||
           (byte >= 'A' && byte <= 'F') ||
           (byte >= 'a' && byte <= 'f');
}

static int device_identifier_is_exact_address(const char *identifier) {
    if (!identifier || strlen(identifier) != 17) return 0;
    char separator = identifier[2];
    if (separator != '-' && separator != ':') return 0;
    for (size_t index = 0; index < 17; index++) {
        if ((index + 1) % 3 == 0) {
            if (identifier[index] != separator) return 0;
        } else if (!is_ascii_hex_digit(identifier[index])) {
            return 0;
        }
    }
    return 1;
}

// Shared by production selection and the Rust regression test. An address is
// identity-bearing even when another paired device was given that exact text
// as its display name, so it must never enter the name-matching path.
int bt_device_identifier_matches_display_name(const char *identifier,
                                               const char *display_name) {
    if (!identifier || !display_name ||
        device_identifier_is_exact_address(identifier)) {
        return 0;
    }
    return strcmp(identifier, display_name) == 0;
}

// Runs only in the helper process's main thread, which owns and pumps the
// CFRunLoop used for all IOBluetooth callbacks.
static RfcommContext *open_rfcomm(const char *device_identifier,
                                  uint8_t rfcomm_channel,
                                  int *ambiguous_device_name) {
    @autoreleasepool {
        *ambiguous_device_name = 0;
        NSString *identifier = [NSString
            stringWithUTF8String:device_identifier];
        if (!identifier) return NULL;
        int exact_address_selector =
            device_identifier_is_exact_address(device_identifier);
        IOBluetoothDevice *device = nil;
        IOBluetoothDevice *name_match = nil;
        NSUInteger name_match_count = 0;
        for (IOBluetoothDevice *paired_device in
             [IOBluetoothDevice pairedDevices]) {
            // An exact address is globally identifying and takes precedence
            // even when an earlier device happened to share the same name.
            NSString *paired_address = paired_device.addressString;
            if (paired_address &&
                [paired_address caseInsensitiveCompare:identifier]
                    == NSOrderedSame) {
                device = paired_device;
                break;
            }
            if (bt_device_identifier_matches_display_name(
                    device_identifier, paired_device.name.UTF8String)) {
                name_match = paired_device;
                name_match_count++;
            }
        }
        // An exact-address request is identity-bearing. If that paired
        // address disappeared, never reinterpret the same bytes as a display
        // name and silently open a different device.
        if (!device && exact_address_selector) return NULL;
        if (!device && name_match_count > 1) {
            *ambiguous_device_name = 1;
            bt_trace("paired device name is ambiguous name=%s matches=%lu",
                     device_identifier, (unsigned long)name_match_count);
            return NULL;
        }
        if (!device) device = name_match;
        if (!device) return NULL;

        RfcommContext *ctx = calloc(1, sizeof(RfcommContext));
        if (!ctx) return NULL;
        ctx->state = 0;
        ctx->output_fd = -1;
        ctx->device = device;
        ctx->delegate = [[RfcommDelegate alloc] init];
        ctx->delegate.ctx = ctx;

        // A connected baseband is valid and may be shared by other Bluetooth
        // profiles. Never tear it down as an RFCOMM cleanup step. Keep the
        // REPL-proven nil-notification SDP query: on current macOS it wakes a
        // paired-but-disconnected D75, while delegate-gating the same query can
        // leave the baseband disconnected with no callback. The SPP channel is
        // fixed, so only baseband readiness is needed before RFCOMM open. SDP,
        // baseband readiness, and RFCOMM open share one absolute native
        // deadline, kept below the Rust parent's 22-second hard boundary.
        double open_deadline = monotonic_seconds() + 20.0;
        IOReturn query_result = [device performSDPQuery:nil];
        bt_trace("SDP start status=0x%08x connected=%d",
                 (unsigned)query_result, [device isConnected]);
        if (query_result != kIOReturnSuccess) {
            destroy_rfcomm_context(ctx);
            return NULL;
        }
        while (![device isConnected]) {
            double remaining = open_deadline - monotonic_seconds();
            if (remaining <= 0.0) break;
            CFRunLoopRunInMode(
                kCFRunLoopDefaultMode,
                remaining < 0.05 ? remaining : 0.05,
                false
            );
        }
        if (![device isConnected]) {
            bt_trace("SDP failed connected=%d", [device isConnected]);
            destroy_rfcomm_context(ctx);
            return NULL;
        }

        IOBluetoothRFCOMMChannel *channel = nil;
        IOReturn result =
            [device openRFCOMMChannelAsync:&channel
                             withChannelID:rfcomm_channel
                                  delegate:ctx->delegate];
        if (result != kIOReturnSuccess) {
            bt_trace("RFCOMM open start failed status=0x%08x",
                     (unsigned)result);
            destroy_rfcomm_context(ctx);
            return NULL;
        }

        while (ctx->state == 0) {
            double remaining = open_deadline - monotonic_seconds();
            if (remaining <= 0.0) break;
            CFRunLoopRunInMode(
                kCFRunLoopDefaultMode,
                remaining < 0.05 ? remaining : 0.05,
                false
            );
        }
        if (ctx->state != 1) {
            bt_trace("RFCOMM open timed out state=%d", ctx->state);
            if (channel) {
                [channel setDelegate:nil];
                [channel closeChannel];
            }
            destroy_rfcomm_context(ctx);
            return NULL;
        }

        ctx->channel = channel;
        return ctx;
    }
}

static int write_all(int fd, const uint8_t *bytes, size_t length) {
    size_t offset = 0;
    while (offset < length) {
        ssize_t count = write(fd, bytes + offset, length - offset);
        if (count > 0) {
            offset += (size_t)count;
            continue;
        }
        if (count < 0 && errno == EINTR) continue;
        return -1;
    }
    return 0;
}

static int run_helper(const char *device_name, uint8_t channel_id) {
    int ambiguous_device_name = 0;
    RfcommContext *ctx = open_rfcomm(
        device_name, channel_id, &ambiguous_device_name
    );
    if (!ctx) {
        if (ambiguous_device_name) {
            return BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME;
        }
        bt_trace("RFCOMM open failed device=%s channel=%u",
                 device_name, channel_id);
        return 71;
    }

    BluetoothRFCOMMMTU mtu = [ctx->channel getMTU];
    if (mtu == 0) {
        destroy_rfcomm_context(ctx);
        return 74;
    }

    // READY is the only non-radio data on stdout. Any ingress received during
    // open was retained in the bounded pre-ready buffer; flush it only after
    // the complete prefix so the parent can consume an unambiguous handshake.
    if (write_all(STDOUT_FILENO, kReadyMagic, sizeof(kReadyMagic) - 1) != 0) {
        destroy_rfcomm_context(ctx);
        return 72;
    }
    if (bt_fd_set_nonblocking(STDIN_FILENO) != 0) {
        destroy_rfcomm_context(ctx);
        return 73;
    }
    int ingress_result = 0;
    pthread_mutex_lock(&g_context_mutex);
    if (ctx->state != 1) {
        ingress_result = -1;
    } else if (ctx->pre_ready_length > 0 &&
               write_all(STDOUT_FILENO, ctx->pre_ready,
                         ctx->pre_ready_length) != 0) {
        ctx->state = -1;
        ingress_result = -1;
    } else {
        ctx->pre_ready_length = 0;
        ctx->output_fd = STDOUT_FILENO;
    }
    pthread_mutex_unlock(&g_context_mutex);
    if (ingress_result != 0) {
        destroy_rfcomm_context(ctx);
        return 84;
    }

    bt_trace("ready device=%s channel=%u mtu=%u",
             device_name, channel_id, (unsigned)mtu);

    // PIPE_BUF is 512 bytes on macOS. The parent submits at most that much
    // per atomic pipe write; cap reads to the smaller of PIPE_BUF and the
    // negotiated RFCOMM MTU. writeSync: may wedge indefinitely, which is
    // safe here because this entire process is the cancellation boundary.
    uint8_t bytes[512];
    size_t read_limit = (size_t)mtu < sizeof(bytes)
        ? (size_t)mtu : sizeof(bytes);
    int exit_code = 0;
    while (ctx->state == 1) {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, false);

        ssize_t count = read(STDIN_FILENO, bytes, read_limit);
        if (count > 0) {
            bt_trace("writeSync enter bytes=%zd", count);
            IOReturn result = [ctx->channel writeSync:bytes
                                                  length:(UInt16)count];
            bt_trace("writeSync exit status=0x%08x", (unsigned)result);
            if (result != kIOReturnSuccess) {
                exit_code = 75;
                break;
            }
            continue;
        }
        if (count == 0) break;
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) {
            exit_code = 76;
            break;
        }
        usleep(1000);
    }

    destroy_rfcomm_context(ctx);
    return exit_code;
}

// Returns 0 after writing one record, 1 when the framework record is unusable,
// and -1 on stdout failure.
static int write_paired_device_record(IOBluetoothDevice *device) {
    NSData *address = [[device addressString]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSString *display_name = device.name ?: device.addressString;
    NSData *name = [display_name dataUsingEncoding:NSUTF8StringEncoding];
    if (!address || !name || address.length == 0 ||
        address.length > UINT16_MAX || name.length == 0 ||
        name.length > UINT16_MAX) {
        return 1;
    }
    uint8_t header[4] = {
        (uint8_t)(address.length >> 8),
        (uint8_t)address.length,
        (uint8_t)(name.length >> 8),
        (uint8_t)name.length,
    };
    if (write_all(STDOUT_FILENO, header, sizeof(header)) != 0 ||
        write_all(STDOUT_FILENO, address.bytes, address.length) != 0 ||
        write_all(STDOUT_FILENO, name.bytes, name.length) != 0) {
        return -1;
    }
    return 0;
}

// Production helper control modes use the same early constructor as the raw
// radio stream. Enumeration returns every bounded paired device using only its
// exact address and display name. Radio identity is established later, after
// the user or same-radio recovery policy chooses an exact address.
static int run_control_helper(const char *mode) {
    if (write_all(STDOUT_FILENO, kReadyMagic, sizeof(kReadyMagic) - 1) != 0) {
        return 79;
    }
    if (strcmp(mode, "paired") == 0) {
        @autoreleasepool {
            NSArray *devices = [IOBluetoothDevice pairedDevices];
            NSUInteger emitted = 0;
            for (IOBluetoothDevice *device in devices) {
                if (emitted >= BT_HELPER_MAX_PAIRED_DEVICES) {
                    return BT_HELPER_EXIT_TOO_MANY_PAIRED_DEVICES;
                }
                int result = write_paired_device_record(device);
                if (result < 0) return 86;
                if (result == 0) emitted++;
            }
            const uint8_t terminator[4] = {0, 0, 0, 0};
            return write_all(STDOUT_FILENO, terminator,
                             sizeof(terminator)) == 0 ? 0 : 86;
        }
    }
    return 82;
}

// No-radio lifecycle probes. The exact helper sentinel is still required, so
// an ambient test-mode variable alone has no effect.
static int run_test_helper(const char *mode) {
    if (write_all(STDOUT_FILENO, kReadyMagic, sizeof(kReadyMagic) - 1) != 0) {
        return 79;
    }
    if (strcmp(mode, "echo-v1") == 0) {
        uint8_t bytes[512];
        for (;;) {
            ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
            if (count > 0) {
                if (write_all(STDOUT_FILENO, bytes, (size_t)count) != 0) {
                    return 80;
                }
                continue;
            }
            if (count == 0) return 0;
            if (errno != EINTR) return 81;
        }
    }
    if (strcmp(mode, "hang-v1") == 0) {
        for (;;) pause();
    }
    return 82;
}

__attribute__((constructor))
static void bluetooth_helper_constructor(void) {
    const char *sentinel = getenv(BT_HELPER_SENTINEL_ENV);
    if (!sentinel || strcmp(sentinel, BT_HELPER_SENTINEL_VALUE) != 0) return;

    // A dead parent should yield EPIPE rather than terminating in a signal
    // while the helper is unwinding its channel.
    signal(SIGPIPE, SIG_IGN);
    // Interactive clients own Ctrl-C/Ctrl-\ and may use them to leave a
    // monitor without ending the radio connection. The helper shares the
    // parent's foreground process group, so it must not inherit the default
    // terminal-signal actions. Parent-liveness EOF and explicit TERM/KILL
    // cleanup remain authoritative.
    signal(SIGINT, SIG_IGN);
    signal(SIGQUIT, SIG_IGN);

    const char *liveness_env = getenv(BT_HELPER_LIVENESS_FD_ENV);
    if (!liveness_env || !liveness_env[0]) _exit(85);
    errno = 0;
    char *liveness_end = NULL;
    long parsed_liveness = strtol(liveness_env, &liveness_end, 10);
    if (errno == ERANGE || !liveness_end || liveness_end == liveness_env ||
        *liveness_end != '\0' || parsed_liveness < 0 ||
        parsed_liveness > INT_MAX ||
        start_parent_liveness_watchdog((int)parsed_liveness) != 0) {
        _exit(85);
    }

    const char *control_mode = getenv(BT_HELPER_CONTROL_ENV);
    if (control_mode && control_mode[0]) _exit(run_control_helper(control_mode));

    const char *test_mode = getenv(BT_HELPER_TEST_ENV);
    if (test_mode && test_mode[0]) _exit(run_test_helper(test_mode));

    const char *device_env = getenv(BT_HELPER_DEVICE_ENV);
    const char *channel_env = getenv(BT_HELPER_CHANNEL_ENV);
    if (!device_env || !device_env[0] || !channel_env) _exit(77);

    char *end = NULL;
    long parsed_channel = strtol(channel_env, &end, 10);
    if (!end || *end != '\0' || parsed_channel < 1 || parsed_channel > 30) {
        _exit(78);
    }

    int result = run_helper(device_env, (uint8_t)parsed_channel);
    _exit(result);
}

#endif
