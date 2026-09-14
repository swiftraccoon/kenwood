//! Input framing, cancellation, and descriptor restoration without a radio.

use super::*;

type TestResult = AppResult<()>;

#[test]
fn terminal_selection_never_uses_rustyline_plain_fallback() {
    assert!(rich_terminal(true, true, Some("xterm-256color")));
    assert!(rich_terminal(true, true, None));
    for term in ["dumb", "DUMB", "cons25", "emacs"] {
        assert!(!rich_terminal(true, true, Some(term)));
    }
    assert!(!rich_terminal(false, true, Some("xterm-256color")));
    assert!(!rich_terminal(true, false, Some("xterm-256color")));
}

#[test]
fn terminal_ownership_selects_interactive_or_batch_completion() {
    assert_eq!(InputMode::from_terminal(true), InputMode::Interactive);
    assert_eq!(InputMode::from_terminal(false), InputMode::Batch);
}

#[test]
fn terminal_results_preserve_errors_and_bound_commands() -> TestResult {
    assert_eq!(
        terminal_event(Ok("status".to_owned()))?,
        Event::Line("status".to_owned())
    );
    assert_eq!(terminal_event(Err(ReadlineError::Eof))?, Event::Eof);
    assert_eq!(
        terminal_event(Err(ReadlineError::Interrupted))?,
        Event::Interrupted
    );
    #[cfg(unix)]
    assert_eq!(
        terminal_event(Err(ReadlineError::Signal(
            rustyline::error::Signal::Interrupt
        )))?,
        Event::Interrupted
    );
    let error = terminal_event(Err(ReadlineError::Io(io::Error::other(
        "injected input error",
    ))))
    .err()
    .ok_or("reader failure became normal EOF")?;
    assert!(error.to_string().contains("injected input error"));
    assert!(terminal_event(Ok("x".repeat(MAX_LINE_BYTES + 1))).is_err());
    assert!(terminal_event(Ok("x".repeat(MAX_LINE_BYTES))).is_ok());
    Ok(())
}

#[cfg(unix)]
mod unix {
    use std::io::Seek;
    use std::sync::mpsc;

    use tokio::io::AsyncWriteExt;
    use tokio::net::unix::pipe::{self, Sender};

    use super::*;

    fn input_pipe() -> AppResult<(Sender, File, File)> {
        let (writer, reader) = pipe::pipe()?;
        let file = File::from(reader.into_blocking_fd()?);
        let witness = file.try_clone()?;
        Ok((writer, file, witness))
    }

    #[tokio::test]
    async fn terminal_join_repeats_a_wake_lost_before_handler_installation() -> TestResult {
        let (wake, received) = mpsc::channel();
        let worker = std::thread::spawn(move || received.recv_timeout(Duration::from_secs(1)));
        let cancelled = AtomicBool::new(true);
        let mut attempts = 0;
        let (completed, failure) = join_terminal(worker, &cancelled, |_worker| {
            attempts += 1;
            if attempts == 2 {
                wake.send(()).map_err(io::Error::other)?;
            }
            Ok(())
        })
        .await?;
        completed?;
        assert!(failure.is_none());
        assert!(attempts >= 2);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_join_preserves_exposed_wake_failure_and_still_joins_the_reader() -> TestResult
    {
        let (wake, received) = mpsc::channel();
        let worker = std::thread::spawn(move || received.recv_timeout(Duration::from_secs(1)));
        let mut attempts = 0;
        let (completed, failure) = join_terminal(worker, &AtomicBool::new(true), |_worker| {
            attempts += 1;
            if attempts == 1 {
                return Err(io::Error::other("injected terminal wake failure"));
            }
            wake.send(()).map_err(io::Error::other)
        })
        .await?;
        completed?;
        assert!(
            failure
                .ok_or("independent wake failure was discarded")?
                .to_string()
                .contains("injected terminal wake failure")
        );
        Ok(())
    }

    #[tokio::test]
    async fn completed_terminal_worker_wins_before_another_interrupt() -> TestResult {
        let worker = std::thread::spawn(|| 42);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !worker.is_finished() {
                tokio::time::sleep(CANCELLATION_POLL).await;
            }
        })
        .await?;
        assert!(worker.is_finished());
        let mut attempts = 0;
        let (completed, failure) = join_terminal(worker, &AtomicBool::new(true), |_worker| {
            attempts += 1;
            Ok(())
        })
        .await?;
        assert_eq!(completed, 42);
        assert_eq!(attempts, 0);
        assert!(failure.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn an_uncancelled_terminal_worker_is_never_signalled() -> TestResult {
        let worker = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(30));
            42
        });
        let mut attempts = 0;
        let (completed, failure) = join_terminal(worker, &AtomicBool::new(false), |_worker| {
            attempts += 1;
            Ok(())
        })
        .await?;
        assert_eq!(completed, 42);
        assert_eq!(attempts, 0);
        assert!(failure.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn idle_pipe_cancels_while_writer_remains_open_and_restores_flags() -> TestResult {
        let (writer, file, witness) = input_pipe()?;
        let original = fcntl_getfl(&witness)?;
        let mut input = ReadyInput::new(file)?;
        assert!(fcntl_getfl(&witness)?.contains(OFlags::NONBLOCK));
        let cancelled = AtomicBool::new(false);
        let pending = tokio::time::timeout(Duration::from_secs(1), input.next(&cancelled));
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancelled.store(true, Ordering::Relaxed);
        };
        let (result, ()) = tokio::join!(pending, cancel);
        assert_eq!(result??, Event::Interrupted);
        input.close()?;
        assert_eq!(fcntl_getfl(&witness)?, original);
        input.close()?;
        assert_eq!(fcntl_getfl(&witness)?, original);
        drop(writer);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_preserves_partial_line_without_a_background_reader() -> TestResult {
        let (mut writer, file, witness) = input_pipe()?;
        let original = fcntl_getfl(&witness)?;
        let mut input = ReadyInput::new(file)?;
        writer.write_all(b"sta").await?;
        let cancelled = AtomicBool::new(false);
        let pending = tokio::time::timeout(Duration::from_secs(1), input.next(&cancelled));
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancelled.store(true, Ordering::Relaxed);
        };
        let (result, ()) = tokio::join!(pending, cancel);
        assert_eq!(result??, Event::Interrupted);
        assert_eq!(input.lines.partial, b"sta");
        cancelled.store(false, Ordering::Relaxed);
        writer.write_all(b"tus\r\nquit\n").await?;
        assert_eq!(
            input.next(&cancelled).await?,
            Event::Line("status".to_owned())
        );
        assert_eq!(
            input.next(&cancelled).await?,
            Event::Line("quit".to_owned())
        );
        input.close()?;
        assert_eq!(fcntl_getfl(&witness)?, original);
        Ok(())
    }

    #[tokio::test]
    async fn pipe_handles_multiple_lines_empty_lines_and_final_eof_line() -> TestResult {
        let (mut writer, file, _witness) = input_pipe()?;
        let mut input = ReadyInput::new(file)?;
        let cancelled = AtomicBool::new(false);
        writer.write_all(b"status\r\n\nidentity").await?;
        drop(writer);
        for expected in [
            Event::Line("status".to_owned()),
            Event::Line(String::new()),
            Event::Line("identity".to_owned()),
            Event::Eof,
            Event::Eof,
        ] {
            assert_eq!(input.next(&cancelled).await?, expected);
        }
        input.close()?;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_utf8_is_an_error_and_does_not_prevent_flag_restoration() -> TestResult {
        let (mut writer, file, witness) = input_pipe()?;
        let original = fcntl_getfl(&witness)?;
        let mut input = ReadyInput::new(file)?;
        writer.write_all(b"\xFF\n").await?;
        let error = input
            .next(&AtomicBool::new(false))
            .await
            .err()
            .ok_or("invalid UTF-8 accepted")?;
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        input.close()?;
        assert_eq!(fcntl_getfl(&witness)?, original);
        Ok(())
    }

    #[tokio::test]
    async fn input_close_prevents_another_read_and_preserves_nonblocking_flags() -> TestResult {
        let (_writer, file, witness) = input_pipe()?;
        let original = fcntl_getfl(&witness)? | OFlags::NONBLOCK;
        fcntl_setfl(&witness, original)?;
        let ready = ReadyInput::new(file)?;
        let mut input = SystemInput {
            source: Some(Source::Ready(Box::new(ready))),
            mode: InputMode::Batch,
        };
        assert_eq!(input.mode(), InputMode::Batch);
        input.close()?;
        assert_eq!(input.mode(), InputMode::Batch);
        assert!(input.next(&AtomicBool::new(false)).await.is_err());
        input.close()?;
        assert_eq!(fcntl_getfl(&witness)?, original);
        Ok(())
    }

    #[test]
    fn line_buffer_rejects_an_oversized_line_before_unbounded_growth() -> TestResult {
        let mut lines = LineBuffer::default();
        for _ in 0..(MAX_LINE_BYTES / READ_BYTES) {
            lines.receive(&[b'x'; READ_BYTES]);
            assert_eq!(lines.next()?, None);
        }
        assert_eq!(lines.partial.len(), MAX_LINE_BYTES);
        lines.receive(b"x");
        assert_eq!(
            lines.next().err().ok_or("oversized line accepted")?.kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(lines.partial.len(), MAX_LINE_BYTES);
        Ok(())
    }

    #[test]
    fn line_buffer_accepts_exact_limit_and_preserves_a_final_bare_cr() -> TestResult {
        let mut lines = LineBuffer::default();
        for _ in 0..(MAX_LINE_BYTES / READ_BYTES) {
            lines.receive(&[b'x'; READ_BYTES]);
            assert_eq!(lines.next()?, None);
        }
        lines.receive(b"\nlast\r");
        assert_eq!(lines.next()?, Some(Event::Line("x".repeat(MAX_LINE_BYTES))));
        assert_eq!(lines.next()?, None);
        lines.receive(&[]);
        assert_eq!(lines.next()?, Some(Event::Line("last\r".to_owned())));
        assert_eq!(lines.next()?, Some(Event::Eof));
        Ok(())
    }

    #[tokio::test]
    async fn dropping_plain_input_restores_flags_as_a_final_fallback() -> TestResult {
        let (_writer, file, witness) = input_pipe()?;
        let original = fcntl_getfl(&witness)?;
        let input = ReadyInput::new(file)?;
        assert!(fcntl_getfl(&witness)?.contains(OFlags::NONBLOCK));
        drop(input);
        assert_eq!(fcntl_getfl(&witness)?, original);
        Ok(())
    }

    #[tokio::test]
    async fn flag_restore_failure_is_returned_and_keeps_restoration_pending() -> TestResult {
        let (_writer, file, witness) = input_pipe()?;
        let original = fcntl_getfl(&witness)?;
        let mut file = FlaggedFile::new(file)?;
        let error = file
            .restore_with(|_, _| Err(io::Error::other("injected restoration failure")))
            .err()
            .ok_or("restoration failure was discarded")?;
        assert!(error.to_string().contains("injected restoration failure"));
        assert!(!file.restored);
        assert!(fcntl_getfl(&witness)?.contains(OFlags::NONBLOCK));
        file.restore()?;
        assert_eq!(fcntl_getfl(&witness)?, original);
        Ok(())
    }

    #[test]
    fn missing_runtime_is_rejected_before_changing_flags() -> TestResult {
        let (stream, _peer) = std::os::unix::net::UnixStream::pair()?;
        let file = File::from(stream.as_fd().try_clone_to_owned()?);
        let original = fcntl_getfl(&stream)?;
        assert!(ReadyInput::new(file).is_err());
        assert_eq!(fcntl_getfl(&stream)?, original);
        Ok(())
    }

    #[test]
    fn scripts_are_loaded_with_a_total_bound_and_share_line_framing() -> TestResult {
        let mut file = tempfile::tempfile()?;
        file.write_all(b"status\r\nidentity")?;
        file.rewind()?;
        let mut input = ScriptInput::load(file)?;
        assert_eq!(input.next()?, Event::Line("status".to_owned()));
        assert_eq!(input.next()?, Event::Line("identity".to_owned()));
        assert_eq!(input.next()?, Event::Eof);
        let file = tempfile::tempfile()?;
        file.set_len(u64::try_from(MAX_SCRIPT_BYTES + 1)?)?;
        assert_eq!(
            ScriptInput::load(file)
                .err()
                .ok_or("oversized script accepted")?
                .kind(),
            io::ErrorKind::InvalidData
        );
        Ok(())
    }

    /// Runs only in the child selected by the real-PTY test below.
    #[cfg(target_os = "macos")]
    #[test]
    fn terminal_sigint_child_fixture() -> TestResult {
        if std::env::var_os("TMD750_REPL_INPUT_PTY_FIXTURE").is_none() {
            return Ok(());
        }
        assert!(io::stdin().is_terminal());
        assert!(io::stdout().is_terminal());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (finished, watchdog) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if watchdog.recv_timeout(Duration::from_secs(3)).is_err() {
                // A regressed joined reader must not hang its fixture process.
                std::process::exit(124);
            }
        });
        let result: TestResult = runtime.block_on(async {
            let mut input = TerminalInput::new()?;
            let cancelled = AtomicBool::new(false);
            assert_eq!(
                input.next(&cancelled).await?,
                Event::Line("status".to_owned())
            );
            println!("terminal fixture accepted the full line");
            assert_eq!(
                input.next(&cancelled).await?,
                Event::Line("again status".to_owned())
            );
            println!("terminal fixture retained editable history");
            let cancel = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                cancelled.store(true, Ordering::Relaxed);
            };
            let (result, ()) = tokio::join!(input.next(&cancelled), cancel);
            assert_eq!(result?, Event::Interrupted);
            let editor = input.editor.take().ok_or("reader lost its editor")?;
            let (install, permitted) = mpsc::channel();
            let worker = std::thread::spawn(move || -> AppResult<_> {
                permitted.recv_timeout(Duration::from_secs(1))?;
                Ok(read_terminal(editor))
            });
            let mut install = Some(install);
            let mut early_wakes = 0;
            let (result, wake_error) = join_terminal(worker, &cancelled, |worker| {
                interrupt_terminal(worker)?;
                if let Some(install) = install.take() {
                    early_wakes += 1;
                    install.send(()).map_err(io::Error::other)?;
                }
                Ok(())
            })
            .await?;
            let (editor, result) = result?;
            input.editor = Some(editor);
            assert!(early_wakes > 0);
            assert!(wake_error.is_none());
            assert_eq!(terminal_event(result)?, Event::Interrupted);
            Ok(())
        });
        finished.send(())?;
        watchdog
            .join()
            .map_err(|_panic| io::Error::other("terminal fixture watchdog failed"))?;
        result?;
        println!("terminal fixture joined after SIGINT");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn actual_rustyline_sigint_wakes_and_joins_an_idle_pty_reader() -> TestResult {
        use std::process::Stdio;

        let output = tempfile::NamedTempFile::new()?;
        let writer_output = output.reopen()?;
        let mut reader_output = output.reopen()?;
        let mut child = tokio::process::Command::new("/usr/bin/script")
            .arg("-q")
            .arg("/dev/null")
            .arg(std::env::current_exe()?)
            .args([
                "--exact",
                "native::input::tests::unix::terminal_sigint_child_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TERM", "xterm")
            .env("TMD750_REPL_INPUT_PTY_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(writer_output.try_clone()?))
            .stderr(Stdio::from(writer_output))
            .kill_on_drop(true)
            .spawn()?;
        let mut writer = child.stdin.take().ok_or("missing fixture stdin owner")?;
        let interaction: TestResult = async {
            fixture_prompt(&mut reader_output, "").await?;
            writer.write_all(b"status\n").await?;
            fixture_prompt(
                &mut reader_output,
                "terminal fixture accepted the full line",
            )
            .await?;
            writer.write_all(b"\x1b[A\x01again \n").await?;
            Ok(())
        }
        .await;
        if let Err(error) = interaction {
            child.kill().await?;
            let _status = child.wait().await?;
            return Err(error);
        }
        let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(result) => result?,
            Err(error) => {
                child.kill().await?;
                let _status = child.wait().await?;
                return Err(error.into());
            }
        };
        drop(writer);
        reader_output.rewind()?;
        let mut transcript = String::new();
        let _read = reader_output
            .take(u64::try_from(MAX_SCRIPT_BYTES)?)
            .read_to_string(&mut transcript)?;
        assert!(status.success(), "PTY fixture failed: {transcript}");
        assert!(
            transcript.contains("terminal fixture joined after SIGINT"),
            "PTY fixture did not join successfully: {transcript}"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn fixture_prompt(output: &mut File, after: &str) -> TestResult {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            output.rewind()?;
            let mut transcript = String::new();
            let _read = output
                .take(u64::try_from(MAX_SCRIPT_BYTES)?)
                .read_to_string(&mut transcript)?;
            if transcript
                .split_once(after)
                .is_some_and(|(_, suffix)| suffix.contains(PROMPT))
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "terminal fixture did not reach its next prompt: {transcript}"
                ))
                .into());
            }
            tokio::time::sleep(CANCELLATION_POLL).await;
        }
    }
}
