use super::tail::Tail;

#[test]
fn stderr_tail_retains_bounded_suffix_across_chunks() {
    let mut tail = Tail::new(5);
    tail.append(b"abc");
    assert_eq!(tail.text(), "abc");
    tail.append(b"def");
    assert_eq!(tail.text(), "[stderr truncated; showing tail]\nbcdef");
    tail.append(b"0123456789");
    assert_eq!(tail.text(), "[stderr truncated; showing tail]\n56789");
}

#[test]
fn stderr_tail_decodes_utf8_after_joining_ring_segments() {
    let mut tail = Tail::new(8);
    tail.append(b"discard");
    tail.append("한글".as_bytes());
    assert!(tail.text().ends_with("rd한글"));
    let mut tail = Tail::new(5);
    tail.append("한글".as_bytes());
    assert!(tail.text().ends_with("��글"));
}

#[cfg(unix)]
mod unix {
    use std::{collections::BTreeMap, os::unix::process::ExitStatusExt, time::Duration};

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use tokio_util::sync::CancellationToken;

    use super::super::{CommandSpec, ManagedProcess, WriteError, run};

    fn shell(script: &str) -> CommandSpec {
        CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: BTreeMap::new(),
            clear_env: false,
        }
    }

    async fn await_reaped(pid: u32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned child must be killed and reaped");
    }

    #[tokio::test]
    async fn arguments_are_literal_and_environment_and_cwd_are_applied() {
        let directory = tempfile::tempdir().unwrap();
        let mut spec = shell("printf '%s\\0' \"$PWD\" \"$ONLY_CHILD\" \"$@\"");
        spec.args.extend(
            [
                "argv-fixture",
                "space arg",
                "$(touch should-not-exist)",
                "a;&|<>^!%",
                "\"json\"",
                "",
                "尾",
            ]
            .map(String::from),
        );
        spec.cwd = Some(directory.path().to_path_buf());
        spec.clear_env = true;
        spec.env
            .insert("ONLY_CHILD".into(), "a secret value".into());
        let output = run(spec, 4096, Duration::from_secs(5), CancellationToken::new())
            .await
            .unwrap();
        let pieces = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
        assert_eq!(pieces[0], directory.path().as_os_str().as_encoded_bytes());
        assert_eq!(pieces[1], b"a secret value");
        assert_eq!(pieces[2], b"space arg");
        assert_eq!(pieces[3], b"$(touch should-not-exist)");
        assert_eq!(pieces[4], b"a;&|<>^!%");
        assert_eq!(pieces[5], b"\"json\"");
        assert_eq!(pieces[6], b"");
        assert_eq!(pieces[7], "尾".as_bytes());
        assert!(!directory.path().join("should-not-exist").exists());
    }

    #[tokio::test]
    async fn concurrent_json_writes_are_complete_frames_and_close_delivers_eof() {
        let (process, mut stdout) = ManagedProcess::spawn(shell("cat")).await.unwrap();
        let reader = tokio::spawn(async move {
            let mut output = Vec::new();
            stdout.read_to_end(&mut output).await.unwrap();
            output
        });
        let mut writers = Vec::new();
        for index in 0..32 {
            let process = process.clone();
            writers.push(tokio::spawn(async move {
                process
                    .write_json(
                        &serde_json::json!({"index": index, "text": "한글\nquoted \"text\""}),
                    )
                    .await
                    .unwrap();
            }));
        }
        for writer in writers {
            writer.await.unwrap();
        }
        process.close_stdin().await.unwrap();
        assert_eq!(process.wait().await.unwrap(), 0);
        assert_eq!(process.wait().await.unwrap(), 0);
        let output = reader.await.unwrap();
        let frames = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 32);
        let indices = frames
            .iter()
            .map(|frame| frame["index"].as_u64().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(indices.len(), 32);
    }

    #[tokio::test]
    async fn waiting_does_not_block_tree_termination_or_escalation() {
        let (process, stdout) = ManagedProcess::spawn(shell(
            "trap '' TERM; sleep 30 & printf '%s\\n' \"$!\"; wait",
        ))
        .await
        .unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let descendant: u32 = lines.next_line().await.unwrap().unwrap().parse().unwrap();
        let waiting = {
            let process = process.clone();
            tokio::spawn(async move { process.wait_status().await.unwrap() })
        };
        tokio::time::timeout(
            Duration::from_secs(3),
            process.terminate(Duration::from_millis(20)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(waiting.await.unwrap().signal(), Some(libc::SIGKILL));
        // A killed orphan may briefly remain a zombie until the OS reaps it;
        // either absence or zombie state proves it is no longer executing.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = std::process::Command::new("/bin/ps")
                    .args(["-p", &descendant.to_string(), "-o", "stat="])
                    .output()
                    .unwrap();
                let state = String::from_utf8_lossy(&state.stdout);
                if state.trim().is_empty() || state.trim_start().starts_with('Z') {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant must no longer execute");
    }

    #[tokio::test]
    async fn last_owner_drop_terminates_and_reaps_without_a_waiter() {
        let (process, _stdout) = ManagedProcess::spawn(shell("sleep 30")).await.unwrap();
        let pid = process.id();
        drop(process);
        await_reaped(pid).await;
    }

    #[tokio::test]
    async fn write_failures_distinguish_closed_stdin_from_attempted_io() {
        let (process, _stdout) = ManagedProcess::spawn(shell("cat")).await.unwrap();
        process.close_stdin().await.unwrap();
        let error = process.write_bytes(b"request\n").await.unwrap_err();
        let error = error.downcast_ref::<WriteError>().unwrap();
        assert!(!error.attempted);
        assert_eq!(error.written, 0);
        process.wait().await.unwrap();

        let (process, stdout) =
            ManagedProcess::spawn(shell("exec 0<&-; printf 'ready\\n'; sleep 30"))
                .await
                .unwrap();
        let mut lines = BufReader::new(stdout).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("ready"));
        let error = process.write_bytes(b"request\n").await.unwrap_err();
        let error = error.downcast_ref::<WriteError>().unwrap();
        assert!(error.attempted);
        assert_eq!(error.total, 8);
        tokio::time::timeout(Duration::from_secs(3), process.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_an_inflight_write_closes_the_corrupted_stream_and_stops_child() {
        let (process, _stdout) = ManagedProcess::spawn(shell("sleep 30")).await.unwrap();
        let frame = vec![b'x'; 8 * 1024 * 1024];
        assert!(
            tokio::time::timeout(Duration::from_millis(20), process.write_bytes(&frame))
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(3), process.wait_status())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !process
                .write_bytes(b"next\n")
                .await
                .unwrap_err()
                .downcast_ref::<WriteError>()
                .unwrap()
                .attempted
        );
    }

    #[tokio::test]
    async fn native_exit_status_does_not_confuse_exit_code_with_signal() {
        let (normal, _stdout) = ManagedProcess::spawn(shell("exit 143")).await.unwrap();
        let status = normal.wait_status().await.unwrap();
        assert_eq!(status.code(), Some(143));
        assert_eq!(status.signal(), None);
        let (signaled, stdout) = ManagedProcess::spawn(shell("printf 'ready\\n'; exec sleep 30"))
            .await
            .unwrap();
        let mut lines = BufReader::new(stdout).lines();
        lines.next_line().await.unwrap();
        signaled.terminate(Duration::from_secs(1)).await.unwrap();
        assert_eq!(
            signaled.wait_status().await.unwrap().signal(),
            Some(libc::SIGTERM)
        );
        assert_eq!(signaled.wait().await.unwrap(), 143);
    }

    #[tokio::test]
    async fn caller_owned_stderr_remains_a_real_stream() {
        let (process, mut stdout, mut stderr) =
            ManagedProcess::spawn_with_stderr(shell("printf out; printf err >&2"))
                .await
                .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        tokio::try_join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err)).unwrap();
        assert_eq!(process.wait().await.unwrap(), 0);
        assert_eq!(out, b"out");
        assert_eq!(err, b"err");
        assert_eq!(process.stderr_tail(), "");
    }

    #[tokio::test]
    async fn run_returns_nonzero_codes_and_rejects_output_overflow() {
        let output = run(
            shell("printf 1234; exit 7"),
            4,
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(output.stdout, b"1234");
        assert_eq!(output.status, 7);
        let error = run(
            shell("printf 12345; sleep 30"),
            4,
            Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("output limit"));
    }

    #[tokio::test]
    async fn run_drains_unbounded_stderr_into_a_bounded_tail() {
        let output = run(shell("i=0; while [ \"$i\" -lt 9000 ]; do printf abcdefgh >&2; i=$((i+1)); done; printf END >&2"), 0, Duration::from_secs(5), CancellationToken::new()).await.unwrap();
        assert!(
            output
                .stderr
                .starts_with("[stderr truncated; showing tail]\n")
        );
        assert!(output.stderr.ends_with("END"));
        assert!(output.stderr.len() < 66_000);
    }

    #[tokio::test]
    async fn run_timeout_cancellation_and_dropped_future_cleanup_children() {
        for scenario in ["timeout", "cancel", "drop"] {
            let directory = tempfile::tempdir().unwrap();
            let pid_file = directory.path().join("pid");
            let mut spec = shell("printf '%s' \"$$\" > \"$PID_FILE\"; exec sleep 30");
            spec.env
                .insert("PID_FILE".into(), pid_file.to_string_lossy().into_owned());
            let cancel = CancellationToken::new();
            let task = tokio::spawn(run(
                spec,
                128,
                if scenario == "timeout" {
                    Duration::from_secs(1)
                } else {
                    Duration::from_secs(30)
                },
                cancel.clone(),
            ));
            let pid = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok(value) = tokio::fs::read_to_string(&pid_file).await {
                        if let Ok(pid) = value.parse::<u32>() {
                            break pid;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if scenario == "cancel" {
                cancel.cancel();
            } else if scenario == "drop" {
                task.abort();
            }
            let result = task.await;
            if scenario == "drop" {
                assert!(matches!(result, Err(error) if error.is_cancelled()));
            } else {
                assert!(result.unwrap().is_err());
            }
            await_reaped(pid).await;
        }
    }

    #[tokio::test]
    async fn interrupt_preserves_stdin_and_escalates_int_term_kill() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("signals");
        let mut spec = shell(
            "trap 'printf I >> \"$SIGNAL_LOG\"' INT; trap 'printf T >> \"$SIGNAL_LOG\"' TERM; printf 'ready\\n'; while :; do read line || :; done",
        );
        spec.env
            .insert("SIGNAL_LOG".into(), log.to_string_lossy().into_owned());
        let (process, stdout) = ManagedProcess::spawn(spec).await.unwrap();
        let mut lines = BufReader::new(stdout).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("ready"));
        process.interrupt(Duration::from_millis(100)).await.unwrap();
        assert_eq!(
            process.wait_status().await.unwrap().signal(),
            Some(libc::SIGKILL)
        );
        assert_eq!(tokio::fs::read_to_string(log).await.unwrap(), "IT");
    }

    #[tokio::test]
    async fn cancelled_before_spawn_never_executes_command() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("spawned");
        let mut spec = shell("touch \"$MARKER\"");
        spec.env
            .insert("MARKER".into(), marker.to_string_lossy().into_owned());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            run(spec, 1024, Duration::from_secs(5), cancel)
                .await
                .is_err()
        );
        assert!(!marker.exists());
    }
}

#[cfg(windows)]
mod windows {
    use std::{
        collections::BTreeMap,
        io::Write,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        time::Duration,
    };

    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio_util::sync::CancellationToken;
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };

    use super::super::{CommandSpec, ManagedProcess, run};

    const FIXTURE: &str = "process::tests::windows::child_fixture";

    fn fixture(mode: &str) -> CommandSpec {
        CommandSpec {
            program: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec!["--exact".into(), FIXTURE.into(), "--nocapture".into()],
            cwd: None,
            env: BTreeMap::from([("PASEO_PROCESS_TEST_MODE".into(), mode.into())]),
            clear_env: false,
        }
    }

    // An actual native child executable: no Node, PowerShell or provider engine.
    // Without the private role environment this is an ordinary no-op fixture.
    #[test]
    fn child_fixture() {
        let Ok(mode) = std::env::var("PASEO_PROCESS_TEST_MODE") else {
            return;
        };
        if mode == "args" {
            let expected: Vec<String> =
                serde_json::from_str(&std::env::var("PASEO_PROCESS_TEST_ARGV").unwrap()).unwrap();
            assert_eq!(std::env::args().skip(1).collect::<Vec<_>>(), expected);
            println!("ARGUMENTS_OK");
            std::process::exit(0);
        }
        if mode == "exit" {
            std::process::exit(7);
        }
        let mut child = None;
        if mode == "tree" {
            child = Some(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", FIXTURE, "--nocapture"])
                    .env("PASEO_PROCESS_TEST_MODE", "hang")
                    .env_remove("PASEO_PROCESS_TEST_PID_FILE")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        let pid = std::process::id();
        if let Ok(path) = std::env::var("PASEO_PROCESS_TEST_PID_FILE") {
            std::fs::write(path, pid.to_string()).unwrap();
        }
        println!(
            "READY {pid} {}",
            child.as_ref().map_or(0, std::process::Child::id)
        );
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(30));
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        std::process::exit(0);
    }

    fn open_process(pid: u32) -> OwnedHandle {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { OwnedHandle::from_raw_handle(handle) }
    }

    async fn await_exited(handle: &OwnedHandle) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT => tokio::time::sleep(Duration::from_millis(10)).await,
                    status => panic!("unexpected process wait status {status}"),
                }
            }
        })
        .await
        .expect("owned Windows process must terminate");
    }

    #[tokio::test]
    async fn executable_and_batch_script_arguments_preserve_literal_metacharacters() {
        for batch in [false, true] {
            let directory = tempfile::Builder::new()
                .prefix("paseo %literal% & ! space ")
                .tempdir()
                .unwrap();
            let mut spec = fixture("args");
            let native = spec.program.clone();
            spec.args.extend(
                [
                    "--",
                    "",
                    "space arg",
                    r#"{"key":"value with spaces","quote":"\"yes\""}"#,
                    "%EXPAND_ME%",
                    "!EXPAND_ME!",
                    "%(refname)%09%(committerdate:unix)",
                    r#"x" & exit /b 99 & "y"#,
                    "a&b|c<d>e^f(g)",
                    "한글",
                    r"trailing\",
                ]
                .map(String::from),
            );
            spec.env.insert(
                "PASEO_PROCESS_TEST_ARGV".into(),
                serde_json::to_string(&spec.args).unwrap(),
            );
            spec.env
                .insert("EXPAND_ME".into(), "MUST_NOT_EXPAND".into());
            spec.env
                .insert("literal".into(), "MUST_NOT_REWRITE_PATH".into());
            spec.cwd = Some(directory.path().to_path_buf());
            if batch {
                let script = directory.path().join("provider %literal% & !.cmd");
                std::fs::write(&script, "@echo off\r\n\"%PASEO_PROCESS_TEST_EXE%\" %*\r\n")
                    .unwrap();
                spec.env.insert("PASEO_PROCESS_TEST_EXE".into(), native);
                spec.program = script.to_string_lossy().into_owned();
            }
            let result = run(
                spec,
                16 * 1024,
                Duration::from_secs(10),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result.status, 0, "{}", result.stderr);
            assert!(String::from_utf8_lossy(&result.stdout).contains("ARGUMENTS_OK"));
        }
    }

    #[tokio::test]
    async fn windows_job_contains_and_terminates_descendants() {
        let (process, stdout) = ManagedProcess::spawn(fixture("tree")).await.unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let pids = loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("fixture ready output");
            if let Some(pids) = line.strip_prefix("READY ") {
                break pids
                    .split_whitespace()
                    .map(|value| value.parse::<u32>().unwrap())
                    .collect::<Vec<_>>();
            }
        };
        let handles = pids.into_iter().map(open_process).collect::<Vec<_>>();
        process.terminate(Duration::ZERO).await.unwrap();
        for handle in handles {
            await_exited(&handle).await;
        }
    }

    #[tokio::test]
    async fn windows_exit_timeout_cancellation_and_dropped_future_preserve_ownership() {
        let result = run(
            fixture("exit"),
            4096,
            Duration::from_secs(10),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, 7);
        for scenario in ["timeout", "cancel", "drop"] {
            let directory = tempfile::tempdir().unwrap();
            let pid_file = directory.path().join("pid");
            let mut spec = fixture("hang");
            spec.env.insert(
                "PASEO_PROCESS_TEST_PID_FILE".into(),
                pid_file.to_string_lossy().into_owned(),
            );
            let cancel = CancellationToken::new();
            let timeout = if scenario == "timeout" {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(30)
            };
            let task = tokio::spawn(run(spec, 4096, timeout, cancel.clone()));
            let pid = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(value) = tokio::fs::read_to_string(&pid_file).await {
                        if let Ok(pid) = value.parse::<u32>() {
                            break pid;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let handle = open_process(pid);
            if scenario == "cancel" {
                cancel.cancel();
            } else if scenario == "drop" {
                task.abort();
            }
            let result = task.await;
            if scenario == "drop" {
                assert!(matches!(result, Err(error) if error.is_cancelled()));
            } else {
                assert!(result.unwrap().is_err());
            }
            await_exited(&handle).await;
        }
    }
}
