from pipe_patch_common import replace_once

# QEMU smoke marker.
replace_once(
    "src/main.rs",
    '''const USER_TERMINAL_TEST_MARKER: &str = "userspace terminal verified:";
const QEMU_TEST_TIMEOUT: Duration = Duration::from_secs(75);
''',
    '''const USER_TERMINAL_TEST_MARKER: &str = "userspace terminal verified:";
const USER_PIPE_TEST_MARKER: &str = "userspace pipe verified:";
const QEMU_TEST_TIMEOUT: Duration = Duration::from_secs(75);
''',
)
replace_once(
    "src/main.rs",
    '''        "  --test      Verify hardware, storage, VFS, process isolation, file I/O, and terminal input"
''',
    '''        "  --test      Verify hardware, storage, VFS, processes, terminal input, and pipes"
''',
)
replace_once(
    "src/main.rs",
    '''        let mut user_terminal_ready = false;

        for line in BufReader::new(serial_output).lines() {
''',
    '''        let mut user_terminal_ready = false;
        let mut user_pipe_ready = false;

        for line in BufReader::new(serial_output).lines() {
''',
)
replace_once(
    "src/main.rs",
    '''            user_terminal_ready |= line.contains(USER_TERMINAL_TEST_MARKER);

            if heap_ready
''',
    '''            user_terminal_ready |= line.contains(USER_TERMINAL_TEST_MARKER);
            user_pipe_ready |= line.contains(USER_PIPE_TEST_MARKER);

            if heap_ready
''',
)
replace_once(
    "src/main.rs",
    '''                && user_file_io_ready
                && user_terminal_ready
            {
''',
    '''                && user_file_io_ready
                && user_terminal_ready
                && user_pipe_ready
            {
''',
)

print("userspace pipe milestone applied")
