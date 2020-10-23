//! This module contains useful tasks that you can attach to an `Actor`.

use crate::{Action, ActionPerformer, ActionRecipient, LiteTask, ShutdownReceiver};
use anyhow::Error;
use async_trait::async_trait;
use futures::{select, FutureExt, StreamExt};
use std::time::Duration;
use tokio::time::{interval, Instant};

/// The lite task that sends ticks to a `Recipient`.
pub struct HeartBeat {
    duration: Duration,
    recipient: ActionRecipient<Tick>,
}

impl HeartBeat {
    /// Creates a new `HeartBeat` lite task.
    pub fn new<T>(duration: Duration, address: T) -> Self
    where
        ActionRecipient<Tick>: From<T>,
    {
        Self {
            duration,
            recipient: address.into(),
        }
    }
}

/// `Tick` value that sent by `HeartBeat` lite task.
pub struct Tick(pub Instant);

impl Action for Tick {}

#[async_trait]
impl LiteTask for HeartBeat {
    async fn routine(mut self, signal: ShutdownReceiver) -> Result<(), Error> {
        let mut ticks = interval(self.duration).map(Tick).fuse();

        let done = signal.just_done().fuse();
        tokio::pin!(done);

        let recipient = &mut self.recipient;

        loop {
            select! {
                _ = done => {
                    break;
                }
                tick = ticks.select_next_some() => {
                    recipient.act(tick).await?;
                }
            }
        }

        Ok(())
    }
}
