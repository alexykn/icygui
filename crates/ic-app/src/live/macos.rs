//! Desktop notifications on macOS, through `UNUserNotificationCenter`
//! (NOTE-01, NOTE-03): with the rule's sound, which GPUI's own backend
//! never sets (its content has no sound, and its delegate presents
//! notifications without one while the app is in front).
//!
//! The center needs the app bundle: outside one (`cargo run`) asking for
//! it aborts the process, so without a bundle identifier notifications
//! are only in the notification centre. Permission is asked with the
//! first notification. The delegate hands clicks to the UI thread as
//! [`Response`]s and lets notifications show (with their sound) while the
//! app is in front too. GPUI's response callback is never registered on
//! macOS: it would install GPUI's delegate in place of this one.

#![expect(
    unsafe_code,
    reason = "the UserNotifications framework is Objective-C: a delegate class and its \
              well-known identifiers"
)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use gpui::App;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use super::desktop::{Desktop, Posted, Response};

/// A notification's button: action id and label.
type Action = (&'static str, &'static str);
/// Registered sets of buttons and their category (id and object).
type Categories = HashMap<Vec<Action>, (String, Retained<UNNotificationCategory>)>;

/// Posts to the notification center (none outside an app bundle).
pub(crate) struct MacDesktop {
    center: Option<Notifications>,
}

impl std::fmt::Debug for MacDesktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacDesktop")
            .field("available", &self.center.is_some())
            .finish()
    }
}

impl MacDesktop {
    /// Connects to the notification center; clicks go to `responses`.
    pub(crate) fn start(responses: UnboundedSender<Response>) -> Self {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            tracing::info!(
                "desktop notifications are off: not running from the app bundle \
                 (they stay in the notification centre)"
            );
            return Self { center: None };
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = ResponseDelegate::new(responses);
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        Self {
            center: Some(Notifications {
                center,
                _delegate: delegate,
                categories: RefCell::new(HashMap::new()),
                authorization_requested: Cell::new(false),
            }),
        }
    }
}

impl Desktop for MacDesktop {
    fn show(&self, posted: Posted, _cx: &mut App) {
        if let Some(center) = &self.center {
            center.show(&posted);
        }
    }
}

/// The notification center, its delegate and the registered button sets.
struct Notifications {
    center: Retained<UNUserNotificationCenter>,
    /// The center keeps its delegate weakly: this keeps it alive.
    _delegate: Retained<ResponseDelegate>,
    /// Every set of buttons registered so far, by its category id: macOS
    /// replaces the whole registered set whenever one is added.
    categories: RefCell<Categories>,
    authorization_requested: Cell<bool>,
}

impl Notifications {
    /// Asks for permission to alert and play sounds, once.
    fn request_authorization(&self) {
        if self.authorization_requested.replace(true) {
            return;
        }
        let completion = RcBlock::new(|granted: Bool, error: *mut NSError| {
            // SAFETY: when non-null, `error` is a valid `NSError` for the
            // duration of the callback.
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::warn!(
                    error = %error.localizedDescription(),
                    "permission for desktop notifications couldn't be asked"
                );
            } else if !granted.as_bool() {
                tracing::info!("desktop notifications are not allowed in the system settings");
            }
        });
        self.center
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &completion,
            );
    }

    fn show(&self, posted: &Posted) {
        self.request_authorization();
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&posted.title));
        content.setBody(&NSString::from_str(&posted.body));
        if posted.sound.is_some() {
            // The rule wants a sound: the system's alert sound (named
            // freedesktop sounds don't exist here).
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        if !posted.actions.is_empty() {
            let category = self.register_category(&posted.actions);
            content.setCategoryIdentifier(&NSString::from_str(&category));
        }
        // Delivered at once (no trigger); the tag as the request id lets a
        // newer notification for the same intent replace the older one.
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&posted.tag),
            &content,
            None,
        );
        let completion = RcBlock::new(|error: *mut NSError| {
            // SAFETY: when non-null, `error` is a valid `NSError` for the
            // duration of the callback.
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::warn!(
                    error = %error.localizedDescription(),
                    "a desktop notification couldn't be shown"
                );
            }
        });
        self.center
            .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
    }

    /// The category with these buttons, registered on first use.
    fn register_category(&self, actions: &[Action]) -> String {
        let mut categories = self.categories.borrow_mut();
        if let Some((identifier, _)) = categories.get(actions) {
            return identifier.clone();
        }
        let identifier = format!("icygui-notification-{}", categories.len());
        let buttons: Vec<Retained<UNNotificationAction>> = actions
            .iter()
            .map(|(id, label)| {
                UNNotificationAction::actionWithIdentifier_title_options(
                    &NSString::from_str(id),
                    &NSString::from_str(label),
                    UNNotificationActionOptions::empty(),
                )
            })
            .collect();
        let category =
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(&identifier),
                &NSArray::from_retained_slice(&buttons),
                &NSArray::new(),
                UNNotificationCategoryOptions::empty(),
            );
        categories.insert(actions.to_vec(), (identifier.clone(), category));
        let all: Vec<Retained<UNNotificationCategory>> = categories
            .values()
            .map(|(_, category)| category.clone())
            .collect();
        self.center
            .setNotificationCategories(&NSSet::from_retained_slice(&all));
        identifier
    }
}

/// The delegate's state: where clicks go.
struct DelegateIvars {
    responses: UnboundedSender<Response>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and
    // `ResponseDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "IcyguiNotificationDelegate"]
    #[ivars = DelegateIvars]
    struct ResponseDelegate;

    // SAFETY: `NSObjectProtocol` has no requirements beyond `NSObject`'s.
    unsafe impl NSObjectProtocol for ResponseDelegate {}

    // SAFETY: the methods below have the protocol's signatures.
    unsafe impl UNUserNotificationCenterDelegate for ResponseDelegate {
        // A click on a notification or one of its buttons, possibly on
        // another thread: the channel takes it to the UI thread.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_notification_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let tag = response.notification().request().identifier().to_string();
            let action = response.actionIdentifier();
            // SAFETY: the framework's constant, an immutable `NSString`.
            let body = unsafe { UNNotificationDefaultActionIdentifier };
            let action = (&*action != body).then(|| action.to_string());
            if self
                .ivars()
                .responses
                .unbounded_send(Response { tag, action })
                .is_err()
            {
                tracing::debug!("a notification was clicked after the UI went");
            }
            completion_handler.call(());
        }

        // Shows notifications while the app is in front too, with their
        // sound (macOS keeps them silent and out of sight otherwise).
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present_notification(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);

impl ResponseDelegate {
    fn new(responses: UnboundedSender<Response>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { responses });
        // SAFETY: `NSObject`'s `init` is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}
