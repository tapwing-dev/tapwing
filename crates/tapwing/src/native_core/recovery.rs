//! Shared restart-and-replay recovery state machine for native lane clients.

use anyhow::Result;

/// Maximum number of automatic driver restarts allowed within one client session.
pub const MAX_RESTARTS: u32 = 3;

/// Whether a failed RPC may be replayed once after restarting the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replay {
    /// Reads and idempotent writes: always safe to repeat once after a restart.
    Always,
    /// Mutating input: repeated only when the failed request provably never ran
    /// on the device/runner (`never_ran == true`).
    IfNotSent,
}

/// Classification of a recoverable driver failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trouble {
    /// Verb phrase for error messages (`"lost its connection"` / `"stopped answering"`).
    pub happened: &'static str,
    /// Gerund/clause for post-restart failures (`"losing its connection"` / `"it stopped answering"`).
    pub after: &'static str,
    /// Phrase for user-visible restart notices (`"connection lost"` / `"stopped answering"`).
    pub notice: &'static str,
    /// Consecutive occurrences before triggering a restart (`1` restarts immediately).
    pub restart_after: u32,
}

/// Outcome of one RPC attempt before recovery.
pub enum FailedAttempt {
    /// Non-recoverable failure (driver answered with an error, or a non-thresholded failure).
    Fatal(anyhow::Error),
    /// Failure eligible for recovery according to [`Trouble`].
    Recoverable {
        /// Underlying failure from the transport/driver.
        error: anyhow::Error,
        /// Classification controlling threshold and diagnostic phrasing.
        trouble: Trouble,
        /// Short cause string included in restart notices.
        cause: String,
        /// Whether the request provably never ran on the target (`ConnectionRefused`, `runnerBusy`, ...).
        never_ran: bool,
    },
}

/// Platform-specific phrasing templates for recovery errors and restart notices.
#[derive(Clone, Copy, Debug)]
pub struct RecoveryMessages {
    /// Subject used in error strings (`"native driver"` or `"iOS driver"`).
    pub driver_definite: &'static str,
    /// Subject used in restart notices (`"native driver"` or `"iOS native driver"`).
    pub notice_subject: &'static str,
    /// Clause describing when replay was withheld (`"while sending this request"` or `"while handling this request"`).
    pub replay_blocked_verb: &'static str,
}

/// Mutable recovery counters and accumulated user-facing restart notices.
#[derive(Debug, Default)]
pub struct RecoveryState {
    restarts: u32,
    stalls: u32,
    notices: Vec<String>,
}

impl RecoveryState {
    /// Creates fresh recovery counters with zero restarts and no pending notices.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns how many times this session has restarted.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// Queues a standalone user-facing notice.
    pub fn push_notice(&mut self, notice: String) {
        self.notices.push(notice);
    }

    /// Drains all accumulated notices since the last call.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Resets consecutive stall counters after any definitive driver answer.
    pub fn on_success(&mut self) {
        self.stalls = 0;
    }
}

/// Client hook allowing [`execute_with_recovery`] to swap `*client = fresh` on restart
/// while carrying forward counters and accumulated notices.
pub trait RecoverableClient {
    /// Mutable access to this client's recovery counters and notices.
    fn recovery_state(&mut self) -> &mut RecoveryState;
    /// Whether a replacement session starter has been configured on this client.
    fn has_recovery(&self) -> bool;
}

/// Runs `attempt` on `client`, restarting via `restart` and optionally replaying once
/// according to `replay` and `trouble`.
pub fn execute_with_recovery<C, T, Attempt, Restart, BeforeReplay>(
    client: &mut C,
    msgs: RecoveryMessages,
    replay: Replay,
    mut attempt: Attempt,
    mut restart: Restart,
    before_replay: BeforeReplay,
) -> Result<T>
where
    C: RecoverableClient,
    Attempt: FnMut(&mut C) -> std::result::Result<T, FailedAttempt>,
    Restart: FnMut(&mut C, &Trouble) -> Result<()>,
    BeforeReplay: FnOnce(&mut C) -> Result<()>,
{
    let (error, trouble, cause, never_ran) = match attempt(client) {
        Ok(value) => {
            client.recovery_state().on_success();
            return Ok(value);
        }
        Err(FailedAttempt::Fatal(err)) => {
            client.recovery_state().on_success();
            return Err(err);
        }
        Err(FailedAttempt::Recoverable { error, trouble, cause, never_ran }) => (error, trouble, cause, never_ran),
    };

    crate::interrupt::check()?;

    if trouble.restart_after > 1 {
        let state = client.recovery_state();
        state.stalls = state.stalls.saturating_add(1);
        if state.stalls < trouble.restart_after {
            return Err(error);
        }
    }

    if !client.has_recovery() {
        return Err(error);
    }

    let prev_restarts = client.recovery_state().restarts;
    if prev_restarts >= MAX_RESTARTS {
        return Err(error.context(format!(
            "the {} {} again after {MAX_RESTARTS} restarts; giving up",
            msgs.driver_definite, trouble.happened
        )));
    }

    // Preserve prior notices across `*client = fresh` and restore them if restarting fails.
    let prev_notices = std::mem::take(&mut client.recovery_state().notices);
    if let Err(restart_err) = restart(client, &trouble) {
        client.recovery_state().notices = prev_notices;
        return Err(error.context(format!(
            "the {} {} and could not be restarted: {restart_err:#}",
            msgs.driver_definite, trouble.happened
        )));
    }

    let next_restarts = prev_restarts + 1;
    let fresh_state = client.recovery_state();
    let fresh_notices = std::mem::take(&mut fresh_state.notices);
    fresh_state.restarts = next_restarts;
    fresh_state.stalls = 0;
    fresh_state.notices = prev_notices;
    fresh_state.notices.extend(fresh_notices);
    fresh_state.notices.push(format!(
        "{} {} ({cause}); restarted it (restart {next_restarts}/{MAX_RESTARTS})",
        msgs.notice_subject, trouble.notice
    ));

    if replay == Replay::IfNotSent && !never_ran {
        return Err(error.context(format!(
            "the {} {} {} and was restarted; the request was not repeated because it may already have been applied",
            msgs.driver_definite, trouble.happened, msgs.replay_blocked_verb
        )));
    }

    before_replay(client)?;
    match attempt(client) {
        Ok(value) => {
            client.recovery_state().on_success();
            Ok(value)
        }
        Err(FailedAttempt::Fatal(retry_err)) | Err(FailedAttempt::Recoverable { error: retry_err, .. }) => {
            Err(retry_err.context(format!(
                "the {} was restarted after {}, but the request failed again",
                msgs.driver_definite, trouble.after
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    struct FakeClient {
        state: RecoveryState,
        has_recovery: bool,
        generation: u32,
    }

    impl RecoverableClient for FakeClient {
        fn recovery_state(&mut self) -> &mut RecoveryState {
            &mut self.state
        }

        fn has_recovery(&self) -> bool {
            self.has_recovery
        }
    }

    const TEST_MSGS: RecoveryMessages = RecoveryMessages {
        driver_definite: "test driver",
        notice_subject: "test native driver",
        replay_blocked_verb: "while handling this request",
    };

    const CONN_LOST: Trouble = Trouble {
        happened: "lost its connection",
        after: "losing its connection",
        notice: "connection lost",
        restart_after: 1,
    };

    const STALLED: Trouble = Trouble {
        happened: "stopped answering",
        after: "it stopped answering",
        notice: "stopped answering",
        restart_after: 2,
    };

    #[test]
    fn replays_always_and_carries_prior_and_fresh_notices() {
        let mut c = FakeClient { state: RecoveryState::new(), has_recovery: true, generation: 0 };
        c.state.push_notice("earlier".into());

        let out = execute_with_recovery(
            &mut c,
            TEST_MSGS,
            Replay::Always,
            |cli| {
                if cli.generation == 0 {
                    Err(FailedAttempt::Recoverable {
                        error: anyhow!("eof"),
                        trouble: CONN_LOST,
                        cause: "eof".into(),
                        never_ran: false,
                    })
                } else {
                    Ok(cli.generation)
                }
            },
            |cli, _| {
                let mut fresh =
                    FakeClient { state: RecoveryState::new(), has_recovery: true, generation: cli.generation + 1 };
                fresh.state.push_notice("from-fresh".into());
                *cli = fresh;
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("replay should succeed");

        assert_eq!(out, 1);
        assert_eq!(c.state.restarts(), 1);
        assert_eq!(
            c.state.take_notices(),
            vec![
                "earlier".to_string(),
                "from-fresh".to_string(),
                "test native driver connection lost (eof); restarted it (restart 1/3)".to_string(),
            ]
        );
    }

    #[test]
    fn if_not_sent_replays_when_never_ran_and_stops_when_ambiguous() {
        let mut c = FakeClient { state: RecoveryState::new(), has_recovery: true, generation: 0 };
        let val = execute_with_recovery(
            &mut c,
            TEST_MSGS,
            Replay::IfNotSent,
            |cli| {
                if cli.generation == 0 {
                    Err(FailedAttempt::Recoverable {
                        error: anyhow!("refused"),
                        trouble: CONN_LOST,
                        cause: "refused".into(),
                        never_ran: true,
                    })
                } else {
                    Ok(99)
                }
            },
            |cli, _| {
                cli.generation += 1;
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("never_ran input should replay");
        assert_eq!(val, 99);

        let err = execute_with_recovery(
            &mut c,
            TEST_MSGS,
            Replay::IfNotSent,
            |_| {
                Err::<(), _>(FailedAttempt::Recoverable {
                    error: anyhow!("broken pipe"),
                    trouble: CONN_LOST,
                    cause: "broken pipe".into(),
                    never_ran: false,
                })
            },
            |cli, _| {
                cli.generation += 1;
                Ok(())
            },
            |_| Ok(()),
        )
        .expect_err("ambiguous input must not replay");
        assert!(err.to_string().contains("not repeated"));
        assert_eq!(c.state.restarts(), 2);
    }

    #[test]
    fn failed_restart_preserves_existing_notices() {
        let mut c = FakeClient { state: RecoveryState::new(), has_recovery: true, generation: 0 };
        c.state.push_notice("first restart".into());

        let err = execute_with_recovery(
            &mut c,
            TEST_MSGS,
            Replay::Always,
            |_| {
                Err::<(), _>(FailedAttempt::Recoverable {
                    error: anyhow!("timed out"),
                    trouble: STALLED,
                    cause: "stall 1".into(),
                    never_ran: false,
                })
            },
            |_, _| unreachable!("first stall must not restart"),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"));

        let err = execute_with_recovery(
            &mut c,
            TEST_MSGS,
            Replay::Always,
            |_| {
                Err::<(), _>(FailedAttempt::Recoverable {
                    error: anyhow!("timed out 2"),
                    trouble: STALLED,
                    cause: "stall 2".into(),
                    never_ran: false,
                })
            },
            |_, _| Err(anyhow!("boom")),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("could not be restarted: boom"));
        assert_eq!(c.state.take_notices(), vec!["first restart".to_string()]);
    }
}
