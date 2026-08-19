// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#import <TargetConditionals.h>

#if TARGET_OS_OSX

// Compile the same audited helper constructor/native RFCOMM implementation
// used by kenwood-thd75 directly into the signed Lodestar executable. The
// private environment sentinel makes the constructor inert in the parent and
// takes over before SwiftUI main only in a child spawned below.
#include "../../../thd75/src/transport/bluetooth_mac.m"

#include <spawn.h>
#include <sys/wait.h>

extern char **environ;

static _Atomic pid_t g_lodestar_bt_helper_pid = 0;

static void release_lodestar_helper_slot(pid_t pid) {
    pid_t expected = pid;
    (void)atomic_compare_exchange_strong(
        &g_lodestar_bt_helper_pid, &expected, 0
    );
}

static int set_fd_flags(int fd, int command, int flag) {
    int flags = fcntl(fd, command);
    if (flags < 0) return -1;
    return fcntl(fd, command == F_GETFD ? F_SETFD : F_SETFL, flags | flag);
}

static int make_pipe(int descriptors[2]) {
    descriptors[0] = -1;
    descriptors[1] = -1;
    int raw[2];
    if (pipe(raw) != 0) return -1;

    // Keep every pipe endpoint away from stdin/stdout/stderr. This avoids
    // action-order aliasing even if the GUI process happened to start with a
    // standard descriptor closed. F_DUPFD_CLOEXEC also makes every original
    // parent endpoint disappear at exec unless a spawn action duplicates it.
    int read_end = fcntl(raw[0], F_DUPFD_CLOEXEC, 10);
    int write_end = fcntl(raw[1], F_DUPFD_CLOEXEC, 10);
    if (read_end < 0 || write_end < 0) {
        int saved_errno = errno;
        if (read_end >= 0) close(read_end);
        if (write_end >= 0) close(write_end);
        close(raw[0]);
        close(raw[1]);
        errno = saved_errno;
        return -1;
    }
    close(raw[0]);
    close(raw[1]);
    descriptors[0] = read_end;
    descriptors[1] = write_end;
    return 0;
}

static int key_matches(const char *entry, const char *key) {
    size_t length = strlen(key);
    return strncmp(entry, key, length) == 0 && entry[length] == '=';
}

static char *environment_assignment(const char *key, const char *value) {
    size_t key_length = strlen(key);
    size_t value_length = strlen(value);
    if (key_length > SIZE_MAX - value_length - 2) {
        errno = EOVERFLOW;
        return NULL;
    }
    char *assignment = malloc(key_length + value_length + 2);
    if (!assignment) return NULL;
    memcpy(assignment, key, key_length);
    assignment[key_length] = '=';
    memcpy(assignment + key_length + 1, value, value_length + 1);
    return assignment;
}

static void free_child_environment(char **environment, size_t inherited_count,
                                   size_t assignment_count) {
    if (!environment) return;
    for (size_t index = inherited_count;
         index < inherited_count + assignment_count; index++) {
        free(environment[index]);
    }
    free(environment);
}

static char **make_child_environment(const char *device,
                                     const char *control_mode,
                                     const char *test_mode,
                                     const char *liveness_descriptor,
                                     size_t *inherited_count_out,
                                     size_t *assignment_count_out) {
    static const char *keys[] = {
        BT_HELPER_SENTINEL_ENV,
        BT_HELPER_DEVICE_ENV,
        BT_HELPER_CHANNEL_ENV,
        BT_HELPER_LIVENESS_FD_ENV,
        BT_HELPER_CONTROL_ENV,
        BT_HELPER_TEST_ENV,
    };
    const char *values[] = {
        BT_HELPER_SENTINEL_VALUE,
        device,
        "2",
        liveness_descriptor,
        control_mode,
        test_mode,
    };
    const size_t required_key_count = 4;
    const size_t key_count = sizeof(keys) / sizeof(keys[0]);
    size_t inherited_count = 0;
    for (char **entry = environ; entry && *entry; entry++) {
        int replaced = 0;
        for (size_t key_index = 0; key_index < key_count; key_index++) {
            if (key_matches(*entry, keys[key_index])) {
                replaced = 1;
                break;
            }
        }
        if (!replaced) inherited_count++;
    }

    size_t assignment_count = required_key_count;
    if (control_mode[0]) assignment_count++;
    if (test_mode[0]) assignment_count++;
    char **result = calloc(
        inherited_count + assignment_count + 1, sizeof(char *)
    );
    if (!result) return NULL;

    size_t destination = 0;
    for (char **entry = environ; entry && *entry; entry++) {
        int replaced = 0;
        for (size_t key_index = 0; key_index < key_count; key_index++) {
            if (key_matches(*entry, keys[key_index])) {
                replaced = 1;
                break;
            }
        }
        if (!replaced) result[destination++] = *entry;
    }
    for (size_t key_index = 0; key_index < key_count; key_index++) {
        if (key_index >= required_key_count && !values[key_index][0]) {
            continue;
        }
        result[destination] = environment_assignment(
            keys[key_index], values[key_index]
        );
        if (!result[destination]) {
            free_child_environment(result, inherited_count,
                                   destination - inherited_count);
            return NULL;
        }
        destination++;
    }
    *inherited_count_out = inherited_count;
    *assignment_count_out = assignment_count;
    return result;
}

static int add_dup_and_close(posix_spawn_file_actions_t *actions,
                             int source, int destination) {
    int result = posix_spawn_file_actions_adddup2(
        actions, source, destination
    );
    if (result != 0 || source == destination) return result;
    return posix_spawn_file_actions_addclose(actions, source);
}

static const char *control_mode_for_spawn_mode(int mode) {
    return mode == 1 ? "paired-v2" : "";
}

static const char *test_mode_for_spawn_mode(int mode) {
    return mode == 2 ? "echo-v1" : mode == 3 ? "hang-v1" : "";
}

static const char *child_environment_value(char **environment,
                                           const char *key) {
    for (char **entry = environment; entry && *entry; entry++) {
        if (key_matches(*entry, key)) {
            return *entry + strlen(key) + 1;
        }
    }
    return NULL;
}

// Verifies that production control and no-radio test modes remain separated
// when the Swift test target exercises the wrapper without invoking Bluetooth.
int lodestar_bt_helper_environment_protocol_probe(void) {
    static const char *expected_control[] = {NULL, "paired-v2", NULL, NULL};
    static const char *expected_test[] = {NULL, NULL, "echo-v1", "hang-v1"};
    for (int mode = 0; mode <= 3; mode++) {
        size_t inherited_count = 0;
        size_t assignment_count = 0;
        char **environment = make_child_environment(
            "-", control_mode_for_spawn_mode(mode),
            test_mode_for_spawn_mode(mode), "123",
            &inherited_count, &assignment_count
        );
        if (!environment) return 0;
        const char *control = child_environment_value(
            environment, BT_HELPER_CONTROL_ENV
        );
        const char *test = child_environment_value(
            environment, BT_HELPER_TEST_ENV
        );
        int control_matches = expected_control[mode]
            ? control && strcmp(control, expected_control[mode]) == 0
            : !control;
        int test_matches = expected_test[mode]
            ? test && strcmp(test, expected_test[mode]) == 0
            : !test;
        free_child_environment(
            environment, inherited_count, assignment_count
        );
        if (!control_matches || !test_matches) return 0;
    }
    return 1;
}

// Spawn modes: 0 = production RFCOMM byte stream, 1 = paired-device list,
// 2 = no-radio echo test, 3 = no-radio wedged-helper test.
int lodestar_bt_helper_spawn(const char *executable, const char *device,
                             int mode, pid_t *pid_out, int *input_out,
                             int *output_out, int *liveness_out,
                             int *holds_slot_out) {
    if (!executable || !executable[0] || !device || !pid_out || !input_out ||
        !output_out || !liveness_out || !holds_slot_out ||
        mode < 0 || mode > 3) {
        errno = EINVAL;
        return -1;
    }

    // The wedged no-radio test mode deliberately takes the production slot
    // so tests cover exclusion and release only after confirmed reap.
    int holds_slot = mode == 0 || mode == 3;
    if (holds_slot) {
        pid_t expected = 0;
        if (!atomic_compare_exchange_strong(
                &g_lodestar_bt_helper_pid, &expected, -1)) {
            errno = EBUSY;
            return -1;
        }
    }

    int input_pipe[2] = {-1, -1};
    int output_pipe[2] = {-1, -1};
    int liveness_pipe[2] = {-1, -1};
    if (make_pipe(input_pipe) != 0 || make_pipe(output_pipe) != 0 ||
        make_pipe(liveness_pipe) != 0) {
        int saved_errno = errno;
        if (input_pipe[0] >= 0) close(input_pipe[0]);
        if (input_pipe[1] >= 0) close(input_pipe[1]);
        if (output_pipe[0] >= 0) close(output_pipe[0]);
        if (output_pipe[1] >= 0) close(output_pipe[1]);
        if (liveness_pipe[0] >= 0) close(liveness_pipe[0]);
        if (liveness_pipe[1] >= 0) close(liveness_pipe[1]);
        if (holds_slot) g_lodestar_bt_helper_pid = 0;
        errno = saved_errno;
        return -1;
    }

    // Reserve a descriptor that provably differs from all six pipe
    // endpoints. The spawn action dup2s the liveness read end onto this
    // descriptor, which also clears its close-on-exec flag in the child.
    // A fixed destination is unsafe in a high-FD app because it can alias a
    // parent pipe endpoint that a later file action must close.
    int child_liveness_fd = fcntl(
        liveness_pipe[0], F_DUPFD_CLOEXEC, 10
    );
    if (child_liveness_fd < 0) {
        int saved_errno = errno;
        close(input_pipe[0]);
        close(input_pipe[1]);
        close(output_pipe[0]);
        close(output_pipe[1]);
        close(liveness_pipe[0]);
        close(liveness_pipe[1]);
        if (holds_slot) g_lodestar_bt_helper_pid = 0;
        errno = saved_errno;
        return -1;
    }
    char child_liveness_text[32];
    int text_length = snprintf(
        child_liveness_text, sizeof(child_liveness_text),
        "%d", child_liveness_fd
    );
    if (text_length <= 0 || (size_t)text_length >= sizeof(child_liveness_text)) {
        close(child_liveness_fd);
        close(input_pipe[0]);
        close(input_pipe[1]);
        close(output_pipe[0]);
        close(output_pipe[1]);
        close(liveness_pipe[0]);
        close(liveness_pipe[1]);
        if (holds_slot) g_lodestar_bt_helper_pid = 0;
        errno = EOVERFLOW;
        return -1;
    }

    const char *control_mode = control_mode_for_spawn_mode(mode);
    const char *test_mode = test_mode_for_spawn_mode(mode);
    size_t inherited_count = 0;
    size_t assignment_count = 0;
    char **child_environment = make_child_environment(
        device, control_mode, test_mode, child_liveness_text,
        &inherited_count, &assignment_count
    );
    if (!child_environment) {
        int saved_errno = errno;
        close(input_pipe[0]);
        close(input_pipe[1]);
        close(output_pipe[0]);
        close(output_pipe[1]);
        close(liveness_pipe[0]);
        close(liveness_pipe[1]);
        close(child_liveness_fd);
        if (holds_slot) g_lodestar_bt_helper_pid = 0;
        errno = saved_errno;
        return -1;
    }

    posix_spawn_file_actions_t actions;
    int actions_initialized = 0;
    int spawn_error = posix_spawn_file_actions_init(&actions);
    if (spawn_error == 0) actions_initialized = 1;
    if (spawn_error == 0) {
        spawn_error = add_dup_and_close(
            &actions, input_pipe[0], STDIN_FILENO
        );
    }
    if (spawn_error == 0) {
        spawn_error = add_dup_and_close(
            &actions, output_pipe[1], STDOUT_FILENO
        );
    }
    if (spawn_error == 0) {
        spawn_error = add_dup_and_close(
            &actions, liveness_pipe[0], child_liveness_fd
        );
    }
    if (spawn_error == 0) {
        spawn_error = posix_spawn_file_actions_addclose(
            &actions, input_pipe[1]
        );
    }
    if (spawn_error == 0) {
        spawn_error = posix_spawn_file_actions_addclose(
            &actions, output_pipe[0]
        );
    }
    if (spawn_error == 0) {
        spawn_error = posix_spawn_file_actions_addclose(
            &actions, liveness_pipe[1]
        );
    }

    pid_t child_pid = -1;
    char *const arguments[] = {
        (char *)executable,
        (char *)"--lodestar-bluetooth-helper",
        NULL,
    };
    if (spawn_error == 0) {
        spawn_error = posix_spawn(
            &child_pid, executable, &actions, NULL,
            arguments, child_environment
        );
    }
    if (actions_initialized) {
        (void)posix_spawn_file_actions_destroy(&actions);
    }
    free_child_environment(
        child_environment, inherited_count, assignment_count
    );
    close(input_pipe[0]);
    close(output_pipe[1]);
    close(liveness_pipe[0]);
    close(child_liveness_fd);

    if (spawn_error != 0) {
        close(input_pipe[1]);
        close(output_pipe[0]);
        close(liveness_pipe[1]);
        if (holds_slot) g_lodestar_bt_helper_pid = 0;
        errno = spawn_error;
        return -1;
    }

    if (set_fd_flags(input_pipe[1], F_GETFL, O_NONBLOCK) != 0 ||
        set_fd_flags(output_pipe[0], F_GETFL, O_NONBLOCK) != 0) {
        int saved_errno = errno;
        close(input_pipe[1]);
        close(output_pipe[0]);
        close(liveness_pipe[1]);
        (void)kill(child_pid, SIGKILL);
        while (waitpid(child_pid, NULL, 0) < 0 && errno == EINTR) {}
        if (holds_slot) g_lodestar_bt_helper_pid = 0;
        errno = saved_errno;
        return -1;
    }

    if (holds_slot) g_lodestar_bt_helper_pid = child_pid;
    *pid_out = child_pid;
    *input_out = input_pipe[1];
    *output_out = output_pipe[0];
    *liveness_out = liveness_pipe[1];
    *holds_slot_out = holds_slot;
    return 0;
}

static int reap_if_exited(pid_t pid, int holds_slot) {
    int status = 0;
    pid_t result;
    do {
        result = waitpid(pid, &status, WNOHANG);
    } while (result < 0 && errno == EINTR);
    if (result == 0) return 0;
    if (result == pid || (result < 0 && errno == ECHILD)) {
        if (holds_slot) release_lodestar_helper_slot(pid);
        return 1;
    }
    return -1;
}

typedef struct {
    pid_t pid;
    int holds_slot;
} LodestarReaperContext;

static void *lodestar_detached_reaper(void *argument) {
    LodestarReaperContext *context = argument;
    pid_t result;
    do {
        result = waitpid(context->pid, NULL, 0);
    } while (result < 0 && errno == EINTR);
    if (context->holds_slot) release_lodestar_helper_slot(context->pid);
    free(context);
    return NULL;
}

// Owns liveness_fd. The caller must close the helper's stdin before asking
// for graceful teardown so EOF can drive channel-only cleanup.
int lodestar_bt_helper_terminate(pid_t pid, int liveness_fd,
                                int graceful, int holds_slot) {
    if (pid <= 0) {
        if (liveness_fd >= 0) close(liveness_fd);
        return 0;
    }

    double deadline = monotonic_seconds() + (graceful ? 0.6 : 0.0);
    for (;;) {
        int state = reap_if_exited(pid, holds_slot);
        if (state != 0) {
            if (liveness_fd >= 0) close(liveness_fd);
            return state < 0 ? -1 : 0;
        }
        if (!graceful || monotonic_seconds() >= deadline) break;
        usleep(1000);
    }

    if (liveness_fd >= 0) close(liveness_fd);
    if (kill(pid, SIGKILL) != 0 && errno != ESRCH) return -1;

    deadline = monotonic_seconds() + 0.1;
    while (monotonic_seconds() < deadline) {
        int state = reap_if_exited(pid, holds_slot);
        if (state != 0) return state < 0 ? -1 : 0;
        usleep(1000);
    }

    LodestarReaperContext *context = malloc(sizeof(*context));
    if (!context) {
        while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {}
        if (holds_slot) release_lodestar_helper_slot(pid);
        return 0;
    }
    context->pid = pid;
    context->holds_slot = holds_slot;
    pthread_t reaper;
    int thread_error = pthread_create(
        &reaper, NULL, lodestar_detached_reaper, context
    );
    if (thread_error != 0) {
        free(context);
        while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {}
        if (holds_slot) release_lodestar_helper_slot(pid);
        return 0;
    }
    (void)pthread_detach(reaper);
    return 1;
}

#endif
