//! Own Claude's shared-home child outside the SDK's timed child destructor.
use agent_client_protocol::{AcpAgent, Agent, ByteStreams, Client, ConnectTo, Error};
use futures::AsyncReadExt;
use std::path::PathBuf;
use std::time::Duration;

pub(super) struct RefreshSafeTransport {
    pub agent: AcpAgent,
    pub refresh_lock: Option<PathBuf>,
    child_return: tokio::sync::oneshot::Sender<SharedChild>,
}

pub(super) struct SharedChild {
    child: Option<async_process::Child>,
    refresh_lock: PathBuf,
}

impl SharedChild {
    pub(super) async fn finish(&mut self) -> Result<(), Error> {
        let child = self.child.as_mut().expect("live child");
        super::super::refresh_lock::wait_for_release(Some(&self.refresh_lock)).await;
        let terminated = tokio::time::timeout(Duration::from_secs(2), child.status())
            .await
            .is_err();
        if terminated {
            // A refresh may start during the ordinary exit grace period.
            super::super::refresh_lock::wait_for_release(Some(&self.refresh_lock)).await;
            terminate_group(child);
        }
        let status = child.status().await.map_err(Error::into_internal_error)?;
        self.child = None;
        if status.success() || terminated {
            Ok(())
        } else {
            Err(Error::into_internal_error(std::io::Error::other(format!(
                "agent process exited with {status}"
            ))))
        }
    }
}

impl RefreshSafeTransport {
    pub(super) fn new(
        agent: AcpAgent,
        refresh_lock: Option<PathBuf>,
    ) -> (Self, tokio::sync::oneshot::Receiver<SharedChild>) {
        let (child_return, completion) = tokio::sync::oneshot::channel();
        (
            Self {
                agent,
                refresh_lock,
                child_return,
            },
            completion,
        )
    }
}

// The SDK can drop the transport future when its foreground finishes. Return
// ownership to the backend in that case; its supervisor must await child exit.
struct ChildLease {
    child: Option<SharedChild>,
    returned: Option<tokio::sync::oneshot::Sender<SharedChild>>,
    stderr_task: tokio::task::JoinHandle<()>,
}

impl Drop for ChildLease {
    fn drop(&mut self) {
        self.stderr_task.abort();
        if let (Some(child), Some(returned)) = (self.child.take(), self.returned.take()) {
            let _ = returned.send(child);
        }
    }
}

fn terminate_group(child: &mut async_process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
    // Shared homes have no timed force-kill fallback. Reaping below waits for
    // the process to complete its own graceful termination.
}

impl Drop for SharedChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let path = self.refresh_lock.clone();
        // Supervisor cancellation must not invoke the SDK's immediate kill.
        // During runtime teardown, inability to spawn leaves the process alive.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                super::super::refresh_lock::wait_for_release(Some(&path)).await;
                terminate_group(&mut child);
                let _ = child.status().await;
            });
        }
    }
}

impl ConnectTo<Client> for RefreshSafeTransport {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), Error> {
        let Some(path) = self.refresh_lock else {
            return ConnectTo::<Client>::connect_to(self.agent, client).await;
        };
        let (stdin, stdout, mut stderr, child) = self.agent.spawn_process()?;
        let child = SharedChild {
            child: Some(child),
            refresh_lock: path,
        };
        // Drain without retaining credential-bearing stderr or blocking pipes.
        let stderr_task = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            while matches!(stderr.read(&mut buffer).await, Ok(count) if count > 0) {}
        });
        let _lease = ChildLease {
            child: Some(child),
            returned: Some(self.child_return),
            stderr_task,
        };
        ConnectTo::<Client>::connect_to(ByteStreams::new(stdin, stdout), client).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[tokio::test]
    async fn sdk_foreground_completion_returns_child_for_refresh_safe_graceful_reaping() {
        let home = tempfile::tempdir().unwrap();
        let pid_file = home.path().join("pid");
        let signalled = home.path().join("signalled");
        let lock = home
            .path()
            .join(super::super::super::refresh_lock::CLAUDE_REFRESH_LOCK);
        let agent = AcpAgent::new(agent_client_protocol::AcpAgentConfig::new("/bin/sh").args([
            "-c".to_string(),
            "trap 'echo term > \"$2\"; exit 0' TERM; echo $$ > \"$1\"; cat >/dev/null; sleep 0.3".to_string(),
            "test".to_string(), pid_file.to_string_lossy().into_owned(), signalled.to_string_lossy().into_owned(),
        ]));
        let (transport, returned) = RefreshSafeTransport::new(agent, Some(lock.clone()));
        agent_client_protocol::Client
            .builder()
            .connect_with(transport, async move |_| {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !pid_file.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                Ok(())
            })
            .await
            .unwrap();
        let mut child = returned.await.unwrap();
        let pid = child.child.as_ref().unwrap().id();
        std::fs::create_dir(&lock).unwrap();
        let finish = tokio::spawn(async move { child.finish().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(alive(pid));
        assert!(!finish.is_finished());
        std::fs::remove_dir(lock).unwrap();
        tokio::time::timeout(Duration::from_secs(3), finish)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!alive(pid));
        assert!(
            !signalled.exists(),
            "normal SDK shutdown must allow EOF exit before SIGTERM"
        );
    }

    #[tokio::test]
    async fn shared_child_destructor_waits_for_refresh_after_supervisor_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(super::super::super::refresh_lock::CLAUDE_REFRESH_LOCK);
        std::fs::create_dir(&path).unwrap();
        let agent =
            AcpAgent::new(agent_client_protocol::AcpAgentConfig::new("/bin/sleep").args(["60"]));
        let (_stdin, _stdout, _stderr, child) = agent.spawn_process().unwrap();
        let pid = child.id();
        drop(SharedChild {
            child: Some(child),
            refresh_lock: path.clone(),
        });
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(alive(pid), "cancellation must retain the live child");
        assert!(path.is_dir(), "Ottam must not clean the shared lock");
        std::fs::remove_dir(&path).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while alive(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn shared_child_rechecks_refresh_started_during_exit_grace() {
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(super::super::super::refresh_lock::CLAUDE_REFRESH_LOCK);
        let agent =
            AcpAgent::new(agent_client_protocol::AcpAgentConfig::new("/bin/sleep").args(["60"]));
        let (_stdin, _stdout, _stderr, child) = agent.spawn_process().unwrap();
        let pid = child.id();
        let lock = path.clone();
        let finish = tokio::spawn(async move {
            SharedChild {
                child: Some(child),
                refresh_lock: lock,
            }
            .finish()
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::create_dir(&path).unwrap();
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(
            alive(pid),
            "exit grace must not force termination during refresh"
        );
        assert!(!finish.is_finished());
        std::fs::remove_dir(&path).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(3), finish)
            .await
            .unwrap();
        assert!(!alive(pid));
    }
}
