//! Desktop notifications on Linux, over the freedesktop notification
//! interface (`org.freedesktop.Notifications` on the session bus): with
//! the urgency, sound and desktop-entry hints GPUI's own backend doesn't
//! send, and one connection for all of them.
//!
//! Two threads: one posts (`Notify`) and remembers which server id is
//! which notification; one listens for `ActionInvoked` and
//! `NotificationClosed` and hands clicks to the UI thread as
//! [`Response`]s. The connection is opened with the first notification;
//! when there is no session bus or no notification server, notifications
//! are only in the notification centre (logged once, tried again a minute
//! later).
//!
//! Tested end to end by `tests/background.rs` (a fake notification server
//! on a private session bus); unit tests show nothing on the desktop.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gpui::App;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::Value;

use super::desktop::{Desktop, Posted, Response};

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// The body's own action key: a click on the notification itself.
const DEFAULT_ACTION: &str = "default";
/// How many posted notifications are remembered for their clicks.
const MAX_REMEMBERED: usize = 512;
/// How long to wait before trying an unreachable bus again.
const RETRY_AFTER: Duration = Duration::from_mins(1);

/// Server ids of posted notifications and their tags, oldest first.
#[derive(Debug, Default)]
struct Posts {
    tags: HashMap<u32, String>,
    order: VecDeque<u32>,
}

impl Posts {
    fn insert(&mut self, id: u32, tag: String) {
        if self.tags.insert(id, tag).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > MAX_REMEMBERED {
            if let Some(oldest) = self.order.pop_front() {
                self.tags.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, id: u32) {
        self.tags.remove(&id);
        self.order.retain(|known| *known != id);
    }
}

/// Posts notifications over D-Bus; clicks go to `responses`.
#[derive(Debug)]
pub(crate) struct DbusDesktop {
    jobs: mpsc::Sender<Posted>,
}

impl DbusDesktop {
    /// Starts the posting thread (it connects with the first
    /// notification).
    pub(crate) fn start(
        app_name: &str,
        app_id: &str,
        responses: UnboundedSender<Response>,
    ) -> Self {
        let (jobs, receiver) = mpsc::channel::<Posted>();
        let app_name = app_name.to_owned();
        let app_id = app_id.to_owned();
        let spawned = std::thread::Builder::new()
            .name("icygui-notify".to_owned())
            .spawn(move || post_loop(&receiver, &app_name, &app_id, &responses));
        if let Err(error) = spawned {
            tracing::warn!(%error, "no thread for desktop notifications; they stay in the centre");
        }
        Self { jobs }
    }
}

impl Desktop for DbusDesktop {
    fn show(&self, posted: Posted, _cx: &mut App) {
        if self.jobs.send(posted).is_err() {
            tracing::debug!("the notification thread is gone");
        }
    }
}

/// A connection to the notification server, and what it posted.
struct Server {
    proxy: Proxy<'static>,
    posts: Arc<Mutex<Posts>>,
}

/// Posts every notification it is given until the app goes.
fn post_loop(
    jobs: &mpsc::Receiver<Posted>,
    app_name: &str,
    app_id: &str,
    responses: &UnboundedSender<Response>,
) {
    let mut server: Option<Server> = None;
    let mut failed_at: Option<Instant> = None;
    while let Ok(posted) = jobs.recv() {
        if server.is_none() && failed_at.is_none_or(|at| at.elapsed() >= RETRY_AFTER) {
            match connect(responses.clone()) {
                Ok(connected) => {
                    server = Some(connected);
                    failed_at = None;
                }
                Err(error) => {
                    if failed_at.is_none() {
                        tracing::warn!(%error, "no notification server on the session bus; notifications stay in the centre");
                    }
                    failed_at = Some(Instant::now());
                }
            }
        }
        let Some(connected) = &server else {
            continue;
        };
        match notify(&connected.proxy, &posted, app_name, app_id) {
            Ok(id) => {
                tracing::debug!(id, tag = %posted.tag, "notification posted");
                connected
                    .posts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(id, posted.tag);
            }
            Err(error) => {
                tracing::warn!(%error, "the notification server refused a notification");
                // The server may have gone: connect again next time.
                server = None;
                failed_at = None;
            }
        }
    }
}

/// Connects to the session bus and starts listening for clicks.
fn connect(responses: UnboundedSender<Response>) -> zbus::Result<Server> {
    let connection = Connection::session()?;
    let proxy = Proxy::new(&connection, SERVICE, PATH, SERVICE)?;
    let rule = zbus::MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(SERVICE)?
        .build();
    let signals = MessageIterator::for_match_rule(rule, &connection, Some(256))?;
    let posts = Arc::new(Mutex::new(Posts::default()));
    let listening = posts.clone();
    std::thread::Builder::new()
        .name("icygui-notify-clicks".to_owned())
        .spawn(move || listen(signals, &listening, &responses))
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(Server { proxy, posts })
}

/// Hands clicks on our notifications to the UI thread; forgets closed
/// ones. Ends when the connection does.
fn listen(signals: MessageIterator, posts: &Mutex<Posts>, responses: &UnboundedSender<Response>) {
    for message in signals {
        let Ok(message) = message else {
            continue;
        };
        let header = message.header();
        let Some(member) = header.member() else {
            continue;
        };
        match member.as_str() {
            "ActionInvoked" => {
                let Ok((id, key)) = message.body().deserialize::<(u32, String)>() else {
                    continue;
                };
                let tag = posts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .tags
                    .get(&id)
                    .cloned();
                if let Some(tag) = tag {
                    let action = (key != DEFAULT_ACTION).then_some(key);
                    if responses.unbounded_send(Response { tag, action }).is_err() {
                        return;
                    }
                }
            }
            "NotificationClosed" => {
                if let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() {
                    posts
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(id);
                }
            }
            _ => {}
        }
    }
    tracing::debug!("the notification server's signals ended");
}

/// Calls `Notify`; returns the server's id for it.
fn notify(proxy: &Proxy<'_>, posted: &Posted, app_name: &str, app_id: &str) -> zbus::Result<u32> {
    let mut actions: Vec<&str> = vec![DEFAULT_ACTION, "Open"];
    for (id, label) in &posted.actions {
        actions.push(id);
        actions.push(label);
    }
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("urgency", Value::U8(posted.urgency.level()));
    hints.insert("desktop-entry", Value::from(app_id));
    match posted.sound {
        Some(sound) => {
            hints.insert("sound-name", Value::from(sound));
        }
        None => {
            hints.insert("suppress-sound", Value::Bool(true));
        }
    }
    proxy.call(
        "Notify",
        &(
            app_name,
            0_u32,
            app_id,
            posted.title.as_str(),
            posted.body.as_str(),
            actions,
            hints,
            -1_i32,
        ),
    )
}
