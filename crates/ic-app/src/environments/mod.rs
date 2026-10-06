//! Environments (ENV-01 … ENV-05, ENV-08): the editor (add, edit, test,
//! delete; also the first run's onboarding form) and trust on first use for
//! certificates the client doesn't trust. The switcher is in the sidebar's
//! footer (`crate::sidebar`); starting and stopping engines, the keychain
//! and event logs are `crate::live::Session`'s.

pub(crate) mod certificate;
pub(crate) mod editor;
pub(crate) mod form;

pub(crate) use self::certificate::{CertificateEvent, CertificateReview};
pub(crate) use self::editor::{EditorMode, EnvironmentEditor, EnvironmentEditorEvent};
