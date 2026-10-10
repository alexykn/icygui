//! Desktop notifications on Linux, over the freedesktop notification
//! interface (`org.freedesktop.Notifications` on the session bus): with
//! the urgency, sound and desktop-entry hints GPUI's own backend doesn't
//! send, and one connection for all of them.
//!
//! Two threads: one posts (`Notify`) and remembers which server id is
//! which notification; one listens for `ActionInvoked` and
//! `NotificationClosed` and hands clicks to the UI thread as
//! [`Response`]s. The connection, and with it the listening thread, is
//! opened with the first notification and kept for good: only when the
//! bus itself goes (the listener ends) is a new one opened. When there is
//! no session bus or no notification server (`GetCapabilities` fails),
//! notifications are only in the notification centre: logged once, tried
//! again a minute later, without new connections or threads.
//!
//! Servers that parse markup in the body (`body-markup`) get it escaped:
//! plugin output often has `&` and `<`, and must never become a link or an
//! image fetched from the desktop.
//!
//! Tested end to end by `tests/background.rs` (a fake notification server
//! on a private session bus); unit tests show nothing on the desktop.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
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
/// How long to wait before trying an unreachable bus or server again.
const RETRY_AFTER: Duration = Duration::from_mins(1);
/// The longest body sent (the specification's guidance for servers).
const MAX_BODY_CHARS: usize = 1000;

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

/// The session bus connection, its listener, and what was posted.
struct Server {
    proxy: Proxy<'static>,
    posts: Arc<Mutex<Posts>>,
    /// Cleared by the listener when the connection ends.
    alive: Arc<AtomicBool>,
    /// Whether the server parses markup in the body (`None`: not asked
    /// yet, or asked again after a failure).
    markup: Option<bool>,
}

/// When posting stopped working, and whether that was logged.
#[derive(Debug, Default)]
struct Backoff {
    failed_at: Option<Instant>,
    warned: bool,
}

impl Backoff {
    /// Whether to try now.
    fn ready(&self) -> bool {
        self.failed_at.is_none_or(|at| at.elapsed() >= RETRY_AFTER)
    }

    /// Records a failure; logs only the first of a series.
    fn failed(&mut self, what: &str, error: &zbus::Error) {
        if !self.warned {
            tracing::warn!(%error, "{what}; notifications stay in the centre and are tried again every minute");
            self.warned = true;
        }
        self.failed_at = Some(Instant::now());
    }

    fn succeeded(&mut self) {
        if self.warned {
            tracing::info!("desktop notifications work again");
        }
        *self = Self::default();
    }
}

/// Posts every notification it is given until the app goes.
fn post_loop(
    jobs: &mpsc::Receiver<Posted>,
    app_name: &str,
    app_id: &str,
    responses: &UnboundedSender<Response>,
) {
    let mut server: Option<Server> = None;
    let mut backoff = Backoff::default();
    while let Ok(posted) = jobs.recv() {
        if !backoff.ready() {
            continue;
        }
        // A connection whose listener ended (the bus went) is replaced;
        // a live one is kept, so failures never add threads.
        if server
            .as_ref()
            .is_none_or(|server| !server.alive.load(Ordering::Acquire))
        {
            server = None;
            match connect(responses.clone()) {
                Ok(connected) => server = Some(connected),
                Err(error) => {
                    backoff.failed("no session bus", &error);
                    continue;
                }
            }
        }
        let Some(connected) = server.as_mut() else {
            continue;
        };
        // Is a notification server there, and does it parse markup?
        let markup = match connected.markup {
            Some(markup) => markup,
            None => match capabilities(&connected.proxy) {
                Ok(capabilities) => {
                    let markup = capabilities.iter().any(|name| name == "body-markup");
                    connected.markup = Some(markup);
                    markup
                }
                Err(error) => {
                    backoff.failed("no notification server on the session bus", &error);
                    continue;
                }
            },
        };
        if let Some(tag) = &posted.closes {
            close(connected, tag);
        }
        match notify(&connected.proxy, &posted, app_name, app_id, markup) {
            Ok(id) => {
                backoff.succeeded();
                tracing::debug!(id, tag = %posted.tag, "notification posted");
                connected
                    .posts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(id, posted.tag);
            }
            Err(error) => {
                // The server may have gone or been replaced: ask again
                // what it can, a minute from now, on the same connection.
                connected.markup = None;
                backoff.failed("the notification server refused a notification", &error);
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
    let alive = Arc::new(AtomicBool::new(true));
    let listening = posts.clone();
    let ended = alive.clone();
    std::thread::Builder::new()
        .name("icygui-notify-clicks".to_owned())
        .spawn(move || {
            listen(signals, &listening, &responses);
            ended.store(false, Ordering::Release);
        })
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(Server {
        proxy,
        posts,
        alive,
        markup: None,
    })
}

/// The server's capabilities (`GetCapabilities`); fails when no
/// notification server is on the bus.
fn capabilities(proxy: &Proxy<'_>) -> zbus::Result<Vec<String>> {
    proxy.call("GetCapabilities", &())
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

/// The body as sent: at most [`MAX_BODY_CHARS`] characters, with `&`, `<`
/// and `>` escaped for a server that parses markup.
fn body_text(body: &str, markup: bool) -> String {
    let mut text: String = body.chars().take(MAX_BODY_CHARS).collect();
    if text.len() < body.len() {
        text.push('…');
    }
    if !markup {
        return text;
    }
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// The actions list: the body's own (`default`), then the buttons. The
/// body's action has no label of its own when there is an *Open* button,
/// so servers that show it as a button don't show *Open* twice.
fn action_list(posted: &Posted) -> Vec<&str> {
    let has_open = posted
        .actions
        .iter()
        .any(|(id, _)| *id == super::desktop::OPEN_ACTION);
    let mut actions: Vec<&str> = vec![DEFAULT_ACTION, if has_open { "" } else { "Open" }];
    for (id, label) in &posted.actions {
        actions.push(id);
        actions.push(label);
    }
    actions
}

/// Takes the notification posted with `tag` away (`CloseNotification`),
/// if the server still shows it.
fn close(server: &Server, tag: &str) {
    let id = server
        .posts
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .tags
        .iter()
        .find(|(_, posted)| posted.as_str() == tag)
        .map(|(id, _)| *id);
    if let Some(id) = id {
        let closed: zbus::Result<()> = server.proxy.call("CloseNotification", &(id,));
        if let Err(error) = closed {
            tracing::debug!(%error, id, "a notification couldn't be closed");
        }
    }
}

/// Calls `Notify`; returns the server's id for it.
fn notify(
    proxy: &Proxy<'_>,
    posted: &Posted,
    app_name: &str,
    app_id: &str,
    markup: bool,
) -> zbus::Result<u32> {
    let actions = action_list(posted);
    let body = body_text(&posted.body, markup);
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
            body.as_str(),
            actions,
            hints,
            // Persistent: until dismissed (or closed by its recovery).
            if posted.persistent { 0_i32 } else { -1_i32 },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::super::desktop::{OPEN_ACTION, Urgency};
    use super::*;

    fn posted(body: &str, actions: Vec<(&'static str, &'static str)>) -> Posted {
        Posted {
            tag: "t".to_owned(),
            title: "CRITICAL · procs".to_owned(),
            body: body.to_owned(),
            urgency: Urgency::Critical,
            sound: None,
            actions,
            persistent: false,
            closes: None,
        }
    }

    #[test]
    fn bodies_are_escaped_for_servers_that_parse_markup() {
        let output = "procs & threads < 5 <a href=\"https://evil\">x</a>";
        assert_eq!(body_text(output, false), output);
        assert_eq!(
            body_text(output, true),
            "procs &amp; threads &lt; 5 &lt;a href=\"https://evil\"&gt;x&lt;/a&gt;"
        );
        let long = "x".repeat(5000);
        assert_eq!(body_text(&long, false).chars().count(), MAX_BODY_CHARS + 1);
    }

    #[test]
    fn open_shows_once() {
        let with_open = posted(
            "",
            vec![("acknowledge", "Acknowledge"), (OPEN_ACTION, "Open")],
        );
        assert_eq!(
            action_list(&with_open),
            ["default", "", "acknowledge", "Acknowledge", "open", "Open"]
        );
        let summary = posted("", Vec::new());
        assert_eq!(action_list(&summary), ["default", "Open"]);
    }

    #[test]
    fn failures_back_off_for_a_minute() {
        let mut backoff = Backoff::default();
        assert!(backoff.ready());
        backoff.failed("test", &zbus::Error::Failure("no server".to_owned()));
        assert!(!backoff.ready(), "no new attempt right away");
        assert!(backoff.warned);
        backoff.succeeded();
        assert!(backoff.ready());
        assert!(!backoff.warned);
    }
}
