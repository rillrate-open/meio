//! Contains message of the `Actor`'s lifecycle.

use crate::{Action, ActionHandler, Actor, Address, Id};
use anyhow::{anyhow, Error};
use std::marker::PhantomData;

pub(crate) trait LifecycleNotifier: Send {
    fn notify(&mut self) -> Result<(), Error>;
}

impl<T> LifecycleNotifier for T
where
    T: FnMut() -> Result<(), Error>,
    T: Send,
{
    fn notify(&mut self) -> Result<(), Error> {
        (self)()
    }
}

impl dyn LifecycleNotifier {
    pub fn once<A, M>(address: &Address<A>, msg: M) -> Box<dyn LifecycleNotifier>
    where
        A: Actor + ActionHandler<M>,
        M: Action,
    {
        let mut addr = address.clone();
        let mut msg = Some(msg);
        let notifier = move || {
            if let Some(msg) = msg.take() {
                addr.send_hp_direct(msg)
            } else {
                Err(anyhow!(
                    "Attempt to send the second notification that can be sent once only."
                ))
            }
        };
        Box::new(notifier)
    }
}

/// This message sent by a `Supervisor` to a spawned child actor.
#[derive(Debug)]
pub struct Awake /* TODO: Add `Supervisor` type parameter to support different spawners */ {
    // TODO: Add `Supervisor`
}

impl Awake {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl Action for Awake {
    fn is_high_priority(&self) -> bool {
        true
    }
}

/// The event to ask an `Actor` to interrupt its activity.
#[derive(Debug)]
pub struct Interrupt {}

impl Interrupt {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl Action for Interrupt {
    fn is_high_priority(&self) -> bool {
        true
    }
}

/// Notifies when `Actor`'s activity is completed.
#[derive(Debug)]
pub struct Done<T: Actor> {
    id: Id,
    _origin: PhantomData<T>,
}

impl<T: Actor> Done<T> {
    pub(crate) fn new(id: Id) -> Self {
        Self {
            id,
            _origin: PhantomData,
        }
    }
}

impl<T: Actor> Action for Done<T> {
    fn is_high_priority(&self) -> bool {
        true
    }
}
/*
 * struct Supervisor {
 *   address?
 * }
 *
 * impl Supervisor {
 *   /// The method that allow a child to ask the supervisor to shutdown.
 *   /// It sends `Shutdown` message, the supervisor can ignore it.
 *   fn shutdown();
 * }
*/