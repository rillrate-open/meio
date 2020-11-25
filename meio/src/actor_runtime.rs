//! This module contains `Actor` trait and the runtime to execute it.

use crate::{
    channel,
    lifecycle::{self, Awake, Done, LifecycleNotifier},
    ActionHandler, Address, Controller, Envelope, Id, LiteTask, Operator, TerminationProgress,
    Terminator,
};
use anyhow::Error;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::{select_biased, StreamExt};
use uuid::Uuid;

const MESSAGES_CHANNEL_DEPTH: usize = 32;

// TODO: Move system somewhere
enum System {}

impl Actor for System {}

#[async_trait]
impl ActionHandler<lifecycle::Awake> for System {
    async fn handle(
        &mut self,
        _event: lifecycle::Awake,
        _ctx: &mut Context<Self>,
    ) -> Result<(), Error> {
        unreachable!()
    }
}

#[async_trait]
impl ActionHandler<lifecycle::Done> for System {
    async fn handle(
        &mut self,
        _event: lifecycle::Done,
        _ctx: &mut Context<Self>,
    ) -> Result<(), Error> {
        unreachable!()
    }
}

/// Spawns a standalone `Actor` that has no `Supervisor`.
pub fn standalone<A>(actor: A) -> Address<A>
where
    A: Actor + ActionHandler<Awake>,
{
    spawn(actor, Option::<Address<System>>::None)
}

/// Spawns `Actor` in `ActorRuntime`.
fn spawn<A, S>(actor: A, opt_supervisor: Option<Address<S>>) -> Address<A>
where
    A: Actor + ActionHandler<Awake>,
    S: Actor + ActionHandler<Done>,
{
    let id = Id::of_actor(&actor);
    let supervisor = opt_supervisor.clone().map(Into::into);
    let (controller, operator) = channel::pair(id, supervisor);
    let id = controller.id();
    let (msg_tx, msg_rx) = mpsc::channel(MESSAGES_CHANNEL_DEPTH);
    let (hp_msg_tx, hp_msg_rx) = mpsc::unbounded();
    let address = Address::new(controller, msg_tx, hp_msg_tx);
    let mut awake_address = address.clone();
    let awake_notifier = Box::new(move || awake_address.send_hp_direct(Awake::new()));
    let done_notifier: Option<Box<dyn LifecycleNotifier>> = {
        match opt_supervisor {
            None => None,
            Some(mut addr) => {
                let event = Done::new(id.clone());
                let done_notifier = move || addr.send_hp_direct(event.clone());
                Some(Box::new(done_notifier))
            }
        }
    };
    let context = Context {
        address: address.clone(),
        terminator: Terminator::new(id.clone()),
    };
    let runtime = ActorRuntime {
        id,
        actor,
        context,
        operator,
        awake_notifier,
        done_notifier,
        msg_rx,
        hp_msg_rx,
    };
    tokio::spawn(runtime.entrypoint());
    address
}

/// The main trait. Your structs have to implement it to
/// be compatible with `ActorRuntime` and `Address` system.
///
/// **Recommended** to implement reactive activities.
#[async_trait]
pub trait Actor: Sized + Send + 'static {
    /// Returns unique name of the `Actor`.
    /// Uses `Uuid` by default.
    fn name(&self) -> String {
        let uuid = Uuid::new_v4();
        format!("Actor:{}({})", std::any::type_name::<Self>(), uuid)
    }
}

/// `Context` of a `ActorRuntime` that contains `Address` and `Receiver`.
pub struct Context<A: Actor> {
    address: Address<A>,
    terminator: Terminator,
}

impl<A: Actor> Context<A> {
    /// Returns an instance of the `Address`.
    pub fn address(&mut self) -> &mut Address<A> {
        &mut self.address
    }

    /// Starts and binds an `Actor`.
    pub fn bind_actor<T>(&self, actor: T) -> Address<T>
    where
        T: Actor + ActionHandler<Awake>,
        A: ActionHandler<Done>,
    {
        spawn(actor, Some(self.address.clone()))
    }

    /// Starts and binds an `Actor`.
    pub fn bind_task<T: LiteTask>(&self, task: T) -> Controller {
        T::start(task, Some(self.address.controller()))
    }

    /// Returns a reference to an `Address`.
    pub fn terminator(&mut self) -> &mut Terminator {
        &mut self.terminator
    }
}

// TODO: Maybe add `S: Supervisor` parameter to
// avoid using blind notifiers, etc?
/// `ActorRuntime` for `Actor`.
pub struct ActorRuntime<A: Actor> {
    id: Id,
    actor: A,
    context: Context<A>,
    operator: Operator,
    awake_notifier: Box<dyn LifecycleNotifier>,
    // TODO: Try to skip `Option` wrapper here.
    // It can be possible when the pair of `Controller` and `Operator`
    // will be removed from the `Address` type.
    done_notifier: Option<Box<dyn LifecycleNotifier>>,
    /// `Receiver` that have to be used to receive incoming messages.
    msg_rx: mpsc::Receiver<Envelope<A>>,
    /// High-priority receiver
    hp_msg_rx: mpsc::UnboundedReceiver<Envelope<A>>,
}

impl<A: Actor> ActorRuntime<A> {
    /// The `entrypoint` of the `ActorRuntime` that calls `routine` method.
    async fn entrypoint(mut self) {
        self.operator.initialize();
        if let Err(err) = self.awake_notifier.notify() {
            log::error!(
                "Can't send awake notification to the actor {:?}: {}",
                self.id,
                err
            );
        }
        self.routine().await;
        log::info!("Actor finished: {:?}", self.id);
        // It's important to finalize `Operator` after `terminate` call,
        // because that can contain some activities for parent `Actor`.
        // Unregistering ids for example.
        if let Some(done_notifier) = self.done_notifier.as_mut() {
            if let Err(err) = done_notifier.notify() {
                log::error!(
                    "Can't send done notification from the actor {:?}: {}",
                    self.id,
                    err
                );
            }
        }
        self.operator.finalize();
    }

    async fn routine(&mut self) {
        loop {
            select_biased! {
                event = self.operator.next() => {
                    log::trace!("Stop signal received: {:?} for {:?}", event, self.id);
                    // Because `Operator` contained an instance of the `Controller`.
                    let signal = event.expect("actor controller couldn't be closed");
                    let child = signal.into();
                    let progress = self.context.terminator().track_child_or_stop_signal(child);
                    if progress == TerminationProgress::SafeToStop {
                        log::info!("Actor {:?} is completed.", self.id);
                        self.msg_rx.close();
                        break;
                    }
                }
                hp_envelope = self.hp_msg_rx.next() => {
                    if let Some(mut envelope) = hp_envelope {
                        let handle_res = envelope.handle(&mut self.actor, &mut self.context).await;
                        if let Err(err) = handle_res {
                            log::error!("Handler for {:?} (high-priority) failed: {}", self.id, err);
                        }
                    } else {
                        // Even if all `Address` dropped `Actor` can do something useful on
                        // background. Than don't terminate actors without `Addresses`, because
                        // it still has controllers.
                        // Background tasks = something spawned that `Actors` waits for finishing.
                        log::trace!("Messages stream of {:?} (high-priority) drained.", self.id);
                    }
                }
                lp_envelope = self.msg_rx.next() => {
                    if let Some(mut envelope) = lp_envelope {
                        let handle_res = envelope.handle(&mut self.actor, &mut self.context).await;
                        if let Err(err) = handle_res {
                            log::error!("Handler for {:?} failed: {}", self.id, err);
                        }
                    } else {
                        // Even if all `Address` dropped `Actor` can do something useful on
                        // background. Than don't terminate actors without `Addresses`, because
                        // it still has controllers.
                        // Background tasks = something spawned that `Actors` waits for finishing.
                        log::trace!("Messages stream of {:?} drained.", self.id);
                    }
                }
            }
        }
    }
}
