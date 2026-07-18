from pipe_patch_common import replace_once

# Expose pipe diagnostics within the kernel crate.
replace_once(
    "kernel/src/process/mod.rs",
    "mod pipe;\n",
    "pub(crate) mod pipe;\n",
)

# Boot image artifacts.
replace_once(
    "userspace/Cargo.toml",
    '''[[bin]]
name = "readline"
path = "src/bin/readline.rs"
test = false
bench = false
''',
    '''[[bin]]
name = "readline"
path = "src/bin/readline.rs"
test = false
bench = false

[[bin]]
name = "pipe_producer"
path = "src/bin/pipe_producer.rs"
test = false
bench = false

[[bin]]
name = "pipe_consumer"
path = "src/bin/pipe_consumer.rs"
test = false
bench = false
''',
)
replace_once(
    "build.rs",
    '''    let userspace_readline = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_readline")
            .expect("userspace readline artifact path was not set"),
    );
''',
    '''    let userspace_readline = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_readline")
            .expect("userspace readline artifact path was not set"),
    );
    let userspace_pipe_producer = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_pipe_producer")
            .expect("userspace pipe-producer artifact path was not set"),
    );
    let userspace_pipe_consumer = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_pipe_consumer")
            .expect("userspace pipe-consumer artifact path was not set"),
    );
''',
)
replace_once(
    "build.rs",
    '''    image.set_file(String::from("readline"), userspace_readline);
    image.set_file(String::from("hello.txt"), hello_text);
''',
    '''    image.set_file(String::from("readline"), userspace_readline);
    image.set_file(String::from("pipe-producer"), userspace_pipe_producer);
    image.set_file(String::from("pipe-consumer"), userspace_pipe_consumer);
    image.set_file(String::from("hello.txt"), hello_text);
''',
)

# Deterministic boot validation and startup status.
replace_once(
    "kernel/src/main.rs",
    '''    serial_println!(
        "userspace terminal verified: pid={}, blocked_reads={}, wakeups={}, bytes_read={}, exit_code=0",
        terminal_result.process_id,
        terminal_result.blocked_read_count,
        terminal_snapshot.wakeups,
        terminal_result.terminal_bytes_read
    );

    println!("GalacticOS");
''',
    '''    serial_println!(
        "userspace terminal verified: pid={}, blocked_reads={}, wakeups={}, bytes_read={}, exit_code=0",
        terminal_result.process_id,
        terminal_result.blocked_read_count,
        terminal_snapshot.wakeups,
        terminal_result.terminal_bytes_read
    );

    const PIPE_TEST_BYTES: u64 = 42;
    let pipe_before = userspace_runtime.pipe_snapshot();
    let pipeline_result = match userspace_runtime.pipeline(
        "/pipe-producer",
        &[],
        "/pipe-consumer",
        &[],
    ) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace pipe validation failed: {error}");
            hlt_loop();
        }
    };
    let pipe_after = userspace_runtime.pipe_snapshot();
    let pipe_verified = pipeline_result.producer.exit_code() == Some(0)
        && pipeline_result.consumer.exit_code() == Some(0)
        && pipeline_result.producer.pipe_write_count >= 1
        && pipeline_result.producer.pipe_bytes_written == PIPE_TEST_BYTES
        && pipeline_result.consumer.pipe_read_count >= 2
        && pipeline_result.consumer.pipe_bytes_read == PIPE_TEST_BYTES
        && pipeline_result.consumer.blocked_pipe_read_count >= 1
        && pipe_after.total_reader_wakeups > pipe_before.total_reader_wakeups
        && pipe_after.active_pipes == 0;
    if !pipe_verified {
        serial_println!(
            "userspace pipe verification failed: producer_exit={:?}, consumer_exit={:?}, writes={}, written={}, reads={}, read={}, blocked_reads={}, wakeups_before={}, wakeups_after={}, active={}",
            pipeline_result.producer.exit_code(),
            pipeline_result.consumer.exit_code(),
            pipeline_result.producer.pipe_write_count,
            pipeline_result.producer.pipe_bytes_written,
            pipeline_result.consumer.pipe_read_count,
            pipeline_result.consumer.pipe_bytes_read,
            pipeline_result.consumer.blocked_pipe_read_count,
            pipe_before.total_reader_wakeups,
            pipe_after.total_reader_wakeups,
            pipe_after.active_pipes
        );
        hlt_loop();
    }
    serial_println!(
        "userspace pipe verified: pipe={}, bytes={}, producer_writes={}, consumer_reads={}, blocked_reads={}, reader_wakeups={}, active_pipes=0",
        pipeline_result.pipe_id,
        pipeline_result.consumer.pipe_bytes_read,
        pipeline_result.producer.pipe_write_count,
        pipeline_result.consumer.pipe_read_count,
        pipeline_result.consumer.blocked_pipe_read_count,
        pipe_after.total_reader_wakeups
    );

    println!("GalacticOS");
''',
)
replace_once(
    "kernel/src/main.rs",
    '''    if terminal_verified {
        println!("Blocking userspace terminal verified");
    } else {
        println!("Userspace terminal unavailable");
    }
    println!("Interactive shell initialized");
''',
    '''    if terminal_verified {
        println!("Blocking userspace terminal verified");
    } else {
        println!("Userspace terminal unavailable");
    }
    if pipe_verified {
        println!("Blocking userspace pipes verified");
    } else {
        println!("Userspace pipes unavailable");
    }
    println!("Interactive shell initialized");
''',
)

# Shell pipeline command and diagnostics.
replace_once(
    "kernel/src/shell.rs",
    '''            "process" | "userspace" => self.print_userspace(),
            "terminal" | "tty" => self.print_terminal(),
            "spawn" | "run" => {
''',
    '''            "process" | "userspace" => self.print_userspace(),
            "terminal" | "tty" => self.print_terminal(),
            "pipes" => self.print_pipes(),
            "pipe" => self.run_pipeline(command_line),
            "spawn" | "run" => {
''',
)
replace_once(
    "kernel/src/shell.rs",
    '''    fn wait_process(&mut self, process_id: u64) {
        match self.runtime.wait(process_id) {
            Ok(result) => print_process_result(&result),
            Err(error) => shell_println!("wait: {error}"),
        }
    }

    fn print_userspace(&self) {
''',
    '''    fn wait_process(&mut self, process_id: u64) {
        match self.runtime.wait(process_id) {
            Ok(result) => print_process_result(&result),
            Err(error) => shell_println!("wait: {error}"),
        }
    }

    fn run_pipeline(&mut self, command_line: &str) {
        let pipeline = command_line
            .strip_prefix("pipe")
            .unwrap_or_default()
            .trim();
        let Some((producer, consumer)) = pipeline.split_once('|') else {
            shell_println!("usage: pipe <producer> [args...] | <consumer> [args...]");
            return;
        };
        let mut producer_words = producer.split_whitespace();
        let Some(producer_path) = producer_words.next() else {
            shell_println!("pipe: producer path is missing");
            return;
        };
        let producer_arguments: Vec<&str> = producer_words.collect();
        let mut consumer_words = consumer.split_whitespace();
        let Some(consumer_path) = consumer_words.next() else {
            shell_println!("pipe: consumer path is missing");
            return;
        };
        let consumer_arguments: Vec<&str> = consumer_words.collect();

        match self.runtime.pipeline(
            producer_path,
            &producer_arguments,
            consumer_path,
            &consumer_arguments,
        ) {
            Ok(result) => {
                shell_println!("pipeline {} completed", result.pipe_id);
                print_process_result(&result.producer);
                print_process_result(&result.consumer);
            }
            Err(error) => shell_println!("pipe: {error}"),
        }
    }

    fn print_userspace(&self) {
''',
)
replace_once(
    "kernel/src/shell.rs",
    '''            shell_println!(
                "  terminal: reads={}, bytes={}, blocked reads={}; frames reclaimed={}",
                result.terminal_read_count,
                result.terminal_bytes_read,
                result.blocked_read_count,
                result.frames_reclaimed
            );
''',
    '''            shell_println!(
                "  terminal: reads={}, bytes={}, blocked reads={}",
                result.terminal_read_count,
                result.terminal_bytes_read,
                result.blocked_read_count
            );
            shell_println!(
                "  pipes: reads={}, writes={}, bytes={}/{}, blocked={}/{}; frames reclaimed={}",
                result.pipe_read_count,
                result.pipe_write_count,
                result.pipe_bytes_read,
                result.pipe_bytes_written,
                result.blocked_pipe_read_count,
                result.blocked_pipe_write_count,
                result.frames_reclaimed
            );
''',
)
replace_once(
    "kernel/src/shell.rs",
    '''    fn print_interrupts(&self) {
''',
    '''    fn print_pipes(&self) {
        let pipes = self.runtime.pipe_snapshot();
        shell_println!(
            "pipes: active={}, created={}, destroyed={}, capacity={} bytes",
            pipes.active_pipes,
            pipes.total_created,
            pipes.total_destroyed,
            crate::process::pipe::PIPE_CAPACITY_BYTES
        );
        shell_println!(
            "I/O: reads={}, writes={}, bytes={}/{}, blocked={}/{}, wakeups={}/{}",
            pipes.total_read_calls,
            pipes.total_write_calls,
            pipes.total_bytes_read,
            pipes.total_bytes_written,
            pipes.total_blocked_reads,
            pipes.total_blocked_writes,
            pipes.total_reader_wakeups,
            pipes.total_writer_wakeups
        );
        for pipe in &pipes.pipes {
            shell_println!(
                "pipe {}: buffered={}, readers={}, writers={}, bytes={}/{}",
                pipe.id,
                pipe.buffered_bytes,
                pipe.readers,
                pipe.writers,
                pipe.bytes_read,
                pipe.bytes_written
            );
        }
    }

    fn print_interrupts(&self) {
''',
)
replace_once(
    "kernel/src/shell.rs",
    '''    shell_println!(
        "  terminal: reads={}, bytes={}, blocked reads={}; frames reclaimed={}",
        result.terminal_read_count,
        result.terminal_bytes_read,
        result.blocked_read_count,
        result.frames_reclaimed
    );
''',
    '''    shell_println!(
        "  terminal: reads={}, bytes={}, blocked reads={}",
        result.terminal_read_count,
        result.terminal_bytes_read,
        result.blocked_read_count
    );
    shell_println!(
        "  pipes: reads={}, writes={}, bytes={}/{}, blocked={}/{}; frames reclaimed={}",
        result.pipe_read_count,
        result.pipe_write_count,
        result.pipe_bytes_read,
        result.pipe_bytes_written,
        result.blocked_pipe_read_count,
        result.blocked_pipe_write_count,
        result.frames_reclaimed
    );
''',
)
replace_once(
    "kernel/src/shell.rs",
    '''    shell_println!("  terminal         show canonical terminal and wakeup statistics");
    shell_println!("  spawn <path> [args...]  launch a userspace process");
''',
    '''    shell_println!("  terminal         show canonical terminal and wakeup statistics");
    shell_println!("  pipes            show pipe buffers, blocking, and wakeups");
    shell_println!("  pipe <a> | <b>   run a userspace pipeline");
    shell_println!("  spawn <path> [args...]  launch a userspace process");
''',
)
