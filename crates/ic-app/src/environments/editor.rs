//! The environment editor (ENV-02): name, HTTPS URL, password or client
//! certificate login, author, and the TLS settings (CA file, pinned
//! SHA-256, server-name override, system roots); "Test connection"
//! (ENV-04) with the API user, Icinga's version and the permissions the
//! client would miss, and trust on first use when the certificate isn't
//! trusted (ENV-05).
//!
//! It shows as a dialog (add or edit; delete from there too) and, on the
//! first run without environments, as the onboarding form in the main
//! area (ENV-08). Passwords go from their masked field straight to the
//! keychain when saved (`crate::live::Session::save_environment`); they
//! are never stored in the settings or logged.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use ic_config::Environment;
use ic_core::{ConnectionFailure, ConnectionReport};
use ic_model::Timestamp;
use ic_ui_kit::input::{InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Button, DialogBody, Field, FieldTone, Link, Segmented, Switch, TextField,
    Theme,
};
use secrecy::SecretString;

use super::certificate::{certificate_details, mismatch_warning};
use super::form::{AuthKind, EnvironmentForm, FormField};
use crate::app_state::environments::EnvironmentSaved;
use crate::live;

/// Where the editor is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorMode {
    /// A dialog over the window (add, edit).
    Dialog,
    /// The main area on the first run without environments (ENV-08).
    Onboarding,
}

/// What the editor asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentEditorEvent {
    /// Saved, or cancelled: close it.
    Close,
    /// Delete the edited environment (after asking).
    Delete(String),
}

/// "Test connection" and its answer.
#[derive(Clone, Debug, PartialEq)]
enum TestState {
    Idle,
    Running,
    Done(Result<ConnectionReport, ConnectionFailure>),
}

/// The text fields, by form field.
struct Inputs {
    name: Entity<InputState>,
    url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    cert_path: Entity<InputState>,
    key_path: Entity<InputState>,
    author: Entity<InputState>,
    ca_file: Entity<InputState>,
    pinned: Entity<InputState>,
    server_name: Entity<InputState>,
}

/// The environment editor view.
pub(crate) struct EnvironmentEditor {
    mode: EditorMode,
    form: EnvironmentForm,
    inputs: Inputs,
    /// Problems are shown once saving was tried.
    show_issues: bool,
    /// The TLS settings are shown (they start folded away unless one is
    /// set: most installations need none, or trust on first use).
    show_tls: bool,
    test: TestState,
    test_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<EnvironmentEditorEvent> for EnvironmentEditor {}

impl Focusable for EnvironmentEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EnvironmentEditor {
    /// An editor for `environment` (`None`: a new one).
    pub(crate) fn new(
        environment: Option<&Environment>,
        mode: EditorMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let form = environment.map_or_else(EnvironmentForm::new_environment, EnvironmentForm::edit);
        let password_placeholder = if form.is_new() {
            "the API user's password"
        } else {
            "unchanged (kept in the keychain)"
        };
        let input = |value: &str,
                     placeholder: &'static str,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let inputs = Inputs {
            name: input(&form.name, "prod-cluster", window, cx),
            url: input(
                &form.url,
                "https://icinga-master.example.com:5665",
                window,
                cx,
            ),
            username: input(&form.username, "icygui", window, cx),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(password_placeholder)
            }),
            cert_path: input(&form.cert_path, "/path/to/client.crt", window, cx),
            key_path: input(&form.key_path, "/path/to/client.key", window, cx),
            author: input(&form.author, "defaults to the API user", window, cx),
            ca_file: input(&form.ca_file, "/var/lib/icinga2/certs/ca.crt", window, cx),
            pinned: input(&form.pinned, "AB:CD:… (SHA-256)", window, cx),
            server_name: input(&form.server_name, "the name in the certificate", window, cx),
        };
        let mut subscriptions = Vec::new();
        for (field, entity) in [
            (FormField::Name, &inputs.name),
            (FormField::Url, &inputs.url),
            (FormField::Username, &inputs.username),
            (FormField::Password, &inputs.password),
            (FormField::CertPath, &inputs.cert_path),
            (FormField::KeyPath, &inputs.key_path),
            (FormField::Author, &inputs.author),
            (FormField::CaFile, &inputs.ca_file),
            (FormField::Pin, &inputs.pinned),
            (FormField::ServerName, &inputs.server_name),
        ] {
            subscriptions.push(cx.subscribe_in(
                entity,
                window,
                move |this: &mut Self, input, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        this.set_field(field, value);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.save(cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ));
        }
        inputs.name.update(cx, |name, cx| name.focus(window, cx));
        let show_tls = !form.ca_file.trim().is_empty()
            || !form.pinned.trim().is_empty()
            || !form.server_name.trim().is_empty()
            || !form.use_system_roots;
        Self {
            mode,
            form,
            inputs,
            show_issues: false,
            show_tls,
            test: TestState::Idle,
            test_task: None,
            save_task: None,
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Where the keyboard goes when the editor opens: the name.
    pub(crate) fn default_focus(&self, cx: &App) -> FocusHandle {
        self.inputs.name.focus_handle(cx)
    }

    /// The form as edited so far.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn form(&self) -> &EnvironmentForm {
        &self.form
    }

    /// The problems shown next to the fields.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn shown_issues(&self) -> BTreeMap<FormField, String> {
        self.issues()
    }

    /// A field's text input (tests type into it).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn input(&self, field: FormField) -> &Entity<InputState> {
        self.input_for(field)
    }

    /// The last test's answer.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn test_result(&self) -> Option<&Result<ConnectionReport, ConnectionFailure>> {
        match &self.test {
            TestState::Done(result) => Some(result),
            TestState::Idle | TestState::Running => None,
        }
    }

    /// Why the last save failed.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn input_for(&self, field: FormField) -> &Entity<InputState> {
        match field {
            FormField::Name => &self.inputs.name,
            FormField::Url => &self.inputs.url,
            FormField::Username => &self.inputs.username,
            FormField::Password => &self.inputs.password,
            FormField::CertPath => &self.inputs.cert_path,
            FormField::KeyPath => &self.inputs.key_path,
            FormField::Author => &self.inputs.author,
            FormField::CaFile => &self.inputs.ca_file,
            FormField::Pin => &self.inputs.pinned,
            FormField::ServerName => &self.inputs.server_name,
        }
    }

    fn set_field(&mut self, field: FormField, value: String) {
        let form = &mut self.form;
        match field {
            FormField::Name => form.name = value,
            FormField::Url => form.url = value,
            FormField::Username => form.username = value,
            FormField::Password => form.password_typed = !value.is_empty(),
            FormField::CertPath => form.cert_path = value,
            FormField::KeyPath => form.key_path = value,
            FormField::Author => form.author = value,
            FormField::CaFile => form.ca_file = value,
            FormField::Pin => form.pinned = value,
            FormField::ServerName => form.server_name = value,
        }
        self.error = None;
    }

    /// Sets a field's text, as if typed (trusting a certificate fills the
    /// pin, a file prompt a path).
    fn fill(
        &mut self,
        field: FormField,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.input_for(field).clone();
        input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
        self.set_field(field, value);
        cx.notify();
    }

    /// "Trust this certificate" after a failed test (ENV-05): pins
    /// `fingerprint` and shows the TLS fields, so the pin saving keeps is
    /// in view; the next test uses it.
    pub(crate) fn trust_presented(
        &mut self,
        fingerprint: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill(FormField::Pin, fingerprint, window, cx);
        self.show_tls = true;
        self.test = TestState::Idle;
    }

    /// The typed password, if any (only for password login).
    fn typed_password(&self, cx: &App) -> Option<SecretString> {
        if self.form.auth != AuthKind::Password {
            return None;
        }
        let value = self.inputs.password.read(cx).value();
        (!value.is_empty()).then(|| SecretString::from(value.to_string()))
    }

    /// "Test connection" (ENV-04).
    pub(crate) fn test(&mut self, cx: &mut Context<Self>) {
        let Some(session) = live::session(cx) else {
            self.test = TestState::Done(Err(ConnectionFailure::Other(
                "testing needs the running app".to_owned(),
            )));
            cx.notify();
            return;
        };
        let environment = self.form.to_environment();
        let password = self.typed_password(cx);
        let task = session.update(cx, |session, cx| {
            session.test_environment(environment, password, cx)
        });
        self.test = TestState::Running;
        self.test_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.test = TestState::Done(result);
                this.test_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Saves (after checking the form): the password into the keychain,
    /// then the settings; the engine (re)starts as needed.
    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        if self.save_task.is_some() {
            return;
        }
        self.show_issues = true;
        if !self.form.issues().is_empty() {
            cx.notify();
            return;
        }
        let Some(session) = live::session(cx) else {
            self.error = Some("Saving needs the running app.".to_owned());
            cx.notify();
            return;
        };
        let environment = self.form.to_environment();
        let password = self.typed_password(cx);
        let task = session.update(cx, |session, cx| {
            session.save_environment(environment, password, cx)
        });
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let saved = task.await;
            let _ = this.update(cx, |this, cx| {
                this.save_task = None;
                match saved {
                    Ok(saved) => {
                        tracing::debug!(?saved, "environment saved");
                        if saved != EnvironmentSaved::Unchanged || this.mode == EditorMode::Dialog {
                            cx.emit(EnvironmentEditorEvent::Close);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Asks the system for a file for `field` (CA, certificate, key); the
    /// text field takes typed paths too, so a desktop without a file
    /// chooser only loses the button's convenience.
    fn browse(field: FormField, window: &Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    tracing::info!(%error, "no file chooser; type the path instead");
                    None
                }
                Ok(Ok(None)) | Err(_) => None,
            };
            if let Some(path) = chosen {
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.fill(field, path.display().to_string(), window, cx);
                    });
                });
            }
        })
        .detach();
    }

    fn issues(&self) -> BTreeMap<FormField, String> {
        if self.show_issues {
            self.form.issues()
        } else {
            BTreeMap::new()
        }
    }

    /// A labelled text field, with its problem.
    fn text_field(
        &self,
        label: &'static str,
        field: FormField,
        hint: Option<&'static str>,
        issues: &BTreeMap<FormField, String>,
    ) -> Field {
        let error = issues.get(&field).cloned();
        let control = TextField::new(self.input_for(field))
            .bordered(true)
            .invalid(error.is_some());
        let field = Field::new(label).control(control).error(error);
        match hint {
            Some(hint) => field.hint(hint),
            None => field,
        }
    }

    /// A path field with a "browse…" link.
    fn path_field(
        &self,
        label: &'static str,
        field: FormField,
        hint: Option<&'static str>,
        issues: &BTreeMap<FormField, String>,
        cx: &Context<Self>,
    ) -> Field {
        let error = issues.get(&field).cloned();
        let id = SharedString::from(format!("browse-{field:?}"));
        let control = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div().flex_1().min_w_0().child(
                    TextField::new(self.input_for(field))
                        .bordered(true)
                        .invalid(error.is_some()),
                ),
            )
            .child(Button::new(id, "browse…").on_click(
                cx.listener(move |_, _: &ClickEvent, window, cx| Self::browse(field, window, cx)),
            ));
        let field_element = Field::new(label).control(control).error(error);
        match hint {
            Some(hint) => field_element.hint(hint),
            None => field_element,
        }
    }

    /// The login fields: user and password, or certificate and key.
    fn render_login(&self, issues: &BTreeMap<FormField, String>, cx: &Context<Self>) -> AnyElement {
        match self.form.auth {
            AuthKind::Password => div()
                .flex()
                .gap(px(10.))
                .child(div().flex_1().min_w_0().child(self.text_field(
                    "API user",
                    FormField::Username,
                    None,
                    issues,
                )))
                .child(div().flex_1().min_w_0().child(self.text_field(
                    "password",
                    FormField::Password,
                    None,
                    issues,
                )))
                .into_any_element(),
            AuthKind::Certificate => div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(self.path_field(
                    "certificate file",
                    FormField::CertPath,
                    Some("PEM; the ApiUser's client_cn names its CN"),
                    issues,
                    cx,
                ))
                .child(self.path_field(
                    "key file",
                    FormField::KeyPath,
                    Some("PEM, unencrypted"),
                    issues,
                    cx,
                ))
                .into_any_element(),
        }
    }

    /// The TLS fields: CA file, pin, server name and system roots.
    fn render_tls(
        &self,
        issues: &BTreeMap<FormField, String>,
        cx: &Context<Self>,
    ) -> [AnyElement; 4] {
        [
            self.path_field(
                "CA file",
                FormField::CaFile,
                Some("Icinga's CA certificate, to trust the API's certificate."),
                issues,
                cx,
            )
            .into_any_element(),
            self.text_field(
                "pinned SHA-256",
                FormField::Pin,
                Some("Trust exactly this certificate (no CA or name check)."),
                issues,
            )
            .into_any_element(),
            self.text_field(
                "server name",
                FormField::ServerName,
                Some("Check the certificate against this name instead of the URL's host."),
                issues,
            )
            .into_any_element(),
            Switch::new("environment-system-roots", self.form.use_system_roots)
                .label("also trust the system's root certificates")
                .on_change(cx.listener(|this, on: &bool, _, cx| {
                    this.form.use_system_roots = *on;
                    cx.notify();
                }))
                .into_any_element(),
        ]
    }

    /// The form's fields.
    fn render_fields(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let issues = self.issues();
        // A problem in a TLS field shows the TLS fields.
        let show_tls = self.show_tls
            || [FormField::CaFile, FormField::Pin, FormField::ServerName]
                .iter()
                .any(|field| issues.contains_key(field));
        let auth = Segmented::new("environment-auth")
            .option("password")
            .option("client certificate")
            .selected(usize::from(self.form.auth == AuthKind::Certificate))
            .on_select(cx.listener(|this, index: &usize, _, cx| {
                this.form.auth = if *index == 1 {
                    AuthKind::Certificate
                } else {
                    AuthKind::Password
                };
                cx.notify();
            }));
        let login = self.render_login(&issues, cx);
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(self.text_field("name", FormField::Name, None, &issues))
            .child(self.text_field(
                "URL",
                FormField::Url,
                Some("The Icinga 2 API, https only, usually port 5665."),
                &issues,
            ))
            .child(Field::new("login").control(auth))
            .child(login)
            .child(self.text_field(
                "author",
                FormField::Author,
                Some("Recorded on acknowledgements, downtimes and comments."),
                &issues,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pt(px(6.))
                    .pb(px(2.))
                    .border_b_1()
                    .border_color(theme.colors.border_header)
                    .text_size(theme.text.label)
                    .text_color(theme.colors.text_faint)
                    .child("TLS")
                    .child(div().flex_1())
                    .child(
                        Link::new(
                            "environment-tls-toggle",
                            if show_tls {
                                "hide"
                            } else {
                                "CA, pinned certificate, server name…"
                            },
                        )
                        .quiet()
                        .text_size(theme.text.label)
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.show_tls = !this.show_tls;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .when(show_tls, |fields| {
                fields.children(self.render_tls(&issues, cx))
            })
            .into_any_element()
    }

    /// "Test connection" and what it found.
    fn render_test(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let running = self.test == TestState::Running;
        let button = Button::new(
            "environment-test",
            if running {
                "testing…"
            } else {
                "test connection"
            },
        )
        .disabled(running)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.test(cx)));
        let result: Option<AnyElement> = match &self.test {
            TestState::Idle | TestState::Running => None,
            TestState::Done(Ok(report)) => Some(report_view(report, &theme)),
            TestState::Done(Err(failure)) => Some(Self::failure_view(failure, &theme, cx)),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(button)
                    .child(
                        div()
                            .text_size(theme.text.label)
                            .text_color(colors.text_faint)
                            .child("Logs in and reads the permissions; changes nothing."),
                    ),
            )
            .children(result)
            .into_any_element()
    }

    /// A failed test: why, and for TLS failures the certificate with
    /// "Trust this certificate" (it fills the pin; saving keeps it).
    fn failure_view(failure: &ConnectionFailure, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let headline = |text: String| {
            div()
                .text_color(theme.states.critical)
                .font_weight(FontWeight::MEDIUM)
                .child(text)
        };
        let column = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .text_size(theme.text.small);
        match failure {
            ConnectionFailure::Unauthorized => column
                .child(headline("Icinga refused the login.".to_owned()))
                .child(
                    div()
                        .text_color(colors.text_muted)
                        .child("Check the API user and its password (or client certificate)."),
                )
                .into_any_element(),
            ConnectionFailure::Unreachable(message) => column
                .child(headline("The server can't be reached.".to_owned()))
                .child(div().text_color(colors.text_muted).child(message.clone()))
                .into_any_element(),
            ConnectionFailure::Other(message) => {
                column.child(headline(message.clone())).into_any_element()
            }
            ConnectionFailure::Tls {
                message,
                certificate,
            } => {
                let mut column = column
                    .child(headline(
                        "The server's certificate isn't trusted.".to_owned(),
                    ))
                    .child(div().text_color(colors.text_muted).child(message.clone()));
                if let Some(certificate) = certificate {
                    let fingerprint = certificate.fingerprint();
                    column = column
                        .child(certificate_details(certificate, Timestamp::now(), theme))
                        .child(
                            div().flex().child(
                                Button::new("environment-trust", "trust this certificate")
                                    .primary()
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.trust_presented(fingerprint.clone(), window, cx);
                                        },
                                    )),
                            ),
                        );
                }
                column.into_any_element()
            }
            ConnectionFailure::CertificateMismatch { expected, actual } => {
                let actual_pin = actual.clone();
                column
                    .child(mismatch_warning(expected, actual, theme))
                    .child(
                        div().flex().child(
                            Button::new("environment-trust-new", "trust the new certificate")
                                .variant(ic_ui_kit::ButtonVariant::Danger)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.trust_presented(actual_pin.clone(), window, cx);
                                })),
                        ),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_dialog(&self, cx: &Context<Self>) -> AnyElement {
        let title = match &self.form.base {
            Some(base) => format!("Edit environment · {}", base.name),
            None => "Add environment".to_owned(),
        };
        let mut dialog = DialogBody::new(title)
            .child(self.render_fields(cx))
            .child(self.render_test(cx));
        if let Some(error) = self.error.clone() {
            dialog = dialog.child(error_line(error, cx.theme()));
        }
        if let Some(base) = &self.form.base {
            let id = base.id.clone();
            dialog = dialog.footer_start(
                Link::new("environment-delete", "delete environment…")
                    .quiet()
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(EnvironmentEditorEvent::Delete(id.clone()));
                    })),
            );
        }
        let saving = self.save_task.is_some();
        dialog
            .action(
                Button::new("environment-cancel", "cancel")
                    .key_hint("esc")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        cx.emit(EnvironmentEditorEvent::Close);
                    })),
            )
            .action(
                Button::new("environment-save", if saving { "saving…" } else { "save" })
                    .primary()
                    .disabled(saving)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
            )
            .into_any_element()
    }

    fn render_onboarding(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let saving = self.save_task.is_some();
        div()
            .id("onboarding")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(22.))
                    .w(px(560.))
                    .mx_auto()
                    .py(px(48.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(theme.text.title)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(colors.text_strong)
                                    .child("Connect to Icinga"),
                            )
                            .child(div().text_size(theme.text.body).text_color(colors.text_muted).child(
                                "icygui talks to the Icinga 2 REST API directly. Add your first \
                                 environment here; more can be added later and switched between \
                                 from the status in the sidebar's footer.",
                            )),
                    )
                    .child(self.render_fields(cx))
                    .child(self.render_test(cx))
                    .when_some(self.error.clone(), |column, error| {
                        column.child(error_line(error, theme))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(
                                Button::new(
                                    "onboarding-connect",
                                    if saving { "connecting…" } else { "connect" },
                                )
                                .primary()
                                .disabled(saving)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(theme.text.label)
                                    .text_color(colors.text_faint)
                                    .child("Just looking around? Start icygui --demo."),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for EnvironmentEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.mode {
            EditorMode::Dialog => self.render_dialog(cx),
            EditorMode::Onboarding => self.render_onboarding(cx),
        };
        div()
            .id("environment-editor")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .min_h_0()
            .when(self.mode == EditorMode::Onboarding, gpui::Styled::size_full)
            .child(content)
    }
}

/// A successful test (ENV-04): the API user, Icinga's version, the
/// user's permissions and those the client would miss.
fn report_view(report: &ConnectionReport, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let missing = &report.missing_permissions;
    let required = ic_core::REQUIRED_PERMISSIONS.len();
    let (tone, summary) = if missing.is_empty() {
        (
            FieldTone::Good,
            format!("all {required} permissions the client uses"),
        )
    } else {
        (
            FieldTone::Bad,
            format!("{} of {required} permissions missing", missing.len()),
        )
    };
    let line = |key: &'static str, value: String| {
        div()
            .flex()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .w(px(96.))
                    .text_color(colors.text_muted)
                    .child(key),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(colors.text)
                    .child(value),
            )
    };
    let version = if report.info.version.is_empty() {
        report.status.version.clone()
    } else {
        report.info.version.clone()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .text_size(theme.text.small)
        .child(
            div()
                .pb(px(4.))
                .text_color(theme.states.ok)
                .font_weight(FontWeight::MEDIUM)
                .child(format!("Connected as {}", report.info.user)),
        )
        .child(line("Icinga", version))
        .child(line(
            "permissions",
            if report.info.permissions.is_empty() {
                "none".to_owned()
            } else {
                report.info.permissions.join(", ")
            },
        ))
        .child(
            div()
                .flex()
                .gap(px(12.))
                .child(
                    div()
                        .flex_none()
                        .w(px(96.))
                        .text_color(colors.text_muted)
                        .child("client"),
                )
                .child(div().text_color(tone.color(theme)).child(summary)),
        )
        .when(!missing.is_empty(), |column| {
            column.child(
                div()
                    .pl(px(108.))
                    .flex()
                    .flex_col()
                    .text_color(colors.text_muted)
                    .children(
                        missing
                            .iter()
                            .map(|permission| div().child(permission.clone())),
                    ),
            )
        })
        .into_any_element()
}

/// A failed save, under the form.
fn error_line(error: String, theme: &Theme) -> AnyElement {
    div()
        .text_size(theme.text.small)
        .text_color(theme.states.critical)
        .child(error)
        .into_any_element()
}
