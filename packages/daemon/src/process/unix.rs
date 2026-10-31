use std::{io, process::ExitStatus};

use tokio::{
    process::{Child, Command},
    signal::unix::{Signal, SignalKind, signal},
};

use super::CommandSpec;

pub(super) fn command(spec: &CommandSpec) -> io::Result<Command> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    super::configure(&mut command, spec);
    Ok(command)
}

pub(super) struct Owner {
    pub(super) child: Child,
    pid: libc::pid_t,
    child_signal: Signal,
    unreaped: bool,
}

impl Owner {
    pub(super) fn spawn(mut command: Command) -> io::Result<Self> {
        // Subscribe before spawning so an immediately exiting child cannot race
        // signal registration. Tokio broadcasts SIGCHLD to each owned listener.
        let child_signal = signal(SignalKind::child())?;
        command.process_group(0);
        let child = command.spawn()?;
        let pid = child.id().expect("newly spawned child has a PID") as libc::pid_t;
        Ok(Self {
            child,
            pid,
            child_signal,
            unreaped: true,
        })
    }

    pub(super) fn id(&self) -> u32 {
        self.pid as u32
    }

    pub(super) fn terminate(&mut self, force: bool) -> io::Result<()> {
        self.signal_group(if force { libc::SIGKILL } else { libc::SIGTERM })
    }

    pub(super) fn interrupt(&mut self) -> io::Result<()> {
        self.signal_group(libc::SIGINT)
    }

    fn signal_group(&mut self, signal: libc::c_int) -> io::Result<()> {
        if !self.unreaped {
            return Ok(());
        }
        // The leader remains our unreaped child until all group signals are
        // sent. Its PID therefore cannot be reused for an unrelated group.
        // This owns descendants that inherit the group, not independently
        // daemonized processes that explicitly abandon their parent's group.
        if unsafe { libc::kill(-self.pid, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            let exited = {
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                // WNOWAIT retains PID ownership until descendant cleanup.
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        self.pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if result != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        // An external waiter released our PID authority.
                        self.unreaped = false;
                    }
                    return Err(error);
                }
                (unsafe { info.si_pid() }) != 0
            };
            if exited {
                self.terminate(true)?;
                let result = self.child.wait().await;
                // A Tokio wait error may also mean another waiter reaped the
                // child; never retain numeric PID authority after this call.
                self.unreaped = false;
                return result;
            }
            if self.child_signal.recv().await.is_none() {
                return Err(io::Error::other("SIGCHLD listener closed"));
            }
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Also runs when the supervisor is aborted during runtime shutdown.
        // Child's kill_on_drop/reaper handles direct-child cleanup afterwards.
        let _ = self.terminate(true);
    }
}
