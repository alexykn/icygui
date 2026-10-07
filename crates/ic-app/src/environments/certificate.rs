//! Trust on first use (ENV-05): a server certificate the client doesn't
//! trust, shown with its fingerprint, subject, issuer, names and expiry
//! and a "Trust this certificate" button that pins it. A pin mismatch
//! shows both fingerprints and warns before the new one is trusted.
//!
//! [`certificate_details`] is shared by the environment editor (after a
//! failed "Test connection") and [`CertificateReview`], the dialog the
//! connection banner's "Review certificate" opens for the running
//! engine's failure.

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Window, div, px,
};
use ic_core::{CertificateInfo, ConnectionState};
use ic_model::Timestamp;
use ic_ui_kit::{ActiveTheme as _, Button, ButtonVariant, DialogBody, Theme};

use crate::app_state::AppState;
use crate::format;

/// What the dialog asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CertificateEvent {
    /// Close it.
    Close,
    /// Pin `fingerprint` for the environment's URL `url` and reconnect.
    Trust {
        /// The environment's id.
        environment_id: String,
        /// The URL (as configured) whose server presented it.
        url: String,
        /// The certificate's SHA-256, colon hex.
        fingerprint: String,
    },
    /// Open the environment's settings instead.
    Edit(String),
}

/// The certificate's details as key/value lines: SHA-256 (in two lines),
/// subject, issuer, names and validity (red once expired).
pub(crate) fn certificate_details(
    certificate: &CertificateInfo,
    now: Timestamp,
    theme: &Theme,
) -> AnyElement {
    let colors = theme.colors;
    let fingerprint = certificate.fingerprint();
    // 32 bytes as `AB:` triplets: 16 per line.
    let (first, second) = fingerprint.split_at(fingerprint.len().min(48));
    let (expiry, expired) = format::expiry(certificate.not_after, now);
    let line = |key: &'static str, value: AnyElement| {
        div()
            .flex()
            .gap(px(12.))
            .py(px(4.))
            .border_t_1()
            .border_color(colors.border_row)
            .text_size(theme.text.small)
            .child(
                div()
                    .flex_none()
                    .w(px(86.))
                    .text_color(colors.text_muted)
                    .child(key),
            )
            .child(div().flex_1().min_w_0().child(value))
    };
    let text = |value: String| {
        div()
            .text_color(colors.text)
            .child(value)
            .into_any_element()
    };
    let names = if certificate.names.is_empty() {
        "—".to_owned()
    } else {
        certificate.names.join(", ")
    };
    div()
        .flex()
        .flex_col()
        .child(line(
            "SHA-256",
            div()
                .flex()
                .flex_col()
                .text_color(colors.text_strong)
                .child(first.trim_end_matches(':').to_owned())
                .child(second.to_owned())
                .into_any_element(),
        ))
        .child(line("subject", text(certificate.subject.clone())))
        .child(line("issuer", text(certificate.issuer.clone())))
        .child(line("names", text(names)))
        .child(line(
            "valid until",
            div()
                .text_color(if expired {
                    theme.states.critical
                } else {
                    colors.text
                })
                .child(expiry)
                .into_any_element(),
        ))
        .into_any_element()
}

/// Both fingerprints of a pin mismatch, with the warning.
pub(crate) fn mismatch_warning(pinned: &str, presented: &str, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .rounded(theme.metrics.code_radius)
        .border_1()
        .border_color(theme.states.warning.opacity(0.5))
        .bg(theme.states.warning.opacity(0.08))
        .text_size(theme.text.small)
        .child(
            div()
                .text_color(colors.text_strong)
                .font_weight(FontWeight::MEDIUM)
                .child("The server presented another certificate than the pinned one."),
        )
        .child(div().text_color(colors.text_muted).child(
            "If it was renewed, trust the new one. If not, someone may be intercepting the \
             connection: check with whoever runs Icinga first.",
        ))
        .child(div().text_color(colors.text_muted).child(
            "Several Icinga masters behind this address (a load balancer, round-robin DNS)? \
             Each has its own certificate and a pin trusts only one of them: set Icinga's CA \
             file in the environment's TLS settings instead (the user guide's TLS section \
             explains the names to check).",
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_color(colors.text_faint).child("pinned"))
                .child(div().text_color(colors.text).child(pinned.to_owned()))
                .child(
                    div()
                        .pt(px(4.))
                        .text_color(colors.text_faint)
                        .child("presented"),
                )
                .child(div().text_color(colors.text).child(presented.to_owned())),
        )
        .into_any_element()
}

/// Whether `pinned` (as configured) names the same certificate as
/// `fingerprint`.
pub(crate) fn same_fingerprint(pinned: &str, fingerprint: &[u8; 32]) -> bool {
    ic_config::parse_fingerprint(pinned).is_ok_and(|pinned| &pinned == fingerprint)
}

/// What trusting a presented certificate means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrustOffer {
    /// The presented certificate's SHA-256 (colon hex), which trusting
    /// pins.
    pub(crate) fingerprint: String,
    /// The configured pin it would replace: set only when a pin exists and
    /// names another certificate (renewed, or someone intercepting), so the
    /// dialog shows both fingerprints and a danger-styled button.
    pub(crate) replaces: Option<String>,
}

/// What the review offers for `certificate` when the environment pins
/// `pinned`.
pub(crate) fn trust_offer(pinned: Option<&str>, certificate: &CertificateInfo) -> TrustOffer {
    TrustOffer {
        fingerprint: certificate.fingerprint(),
        replaces: pinned
            .filter(|pinned| !same_fingerprint(pinned, &certificate.sha256))
            .map(str::to_owned),
    }
}

/// The dialog for the running engine's certificate failure.
pub(crate) struct CertificateReview {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
}

impl EventEmitter<CertificateEvent> for CertificateReview {}

impl Focusable for CertificateReview {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CertificateReview {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            focus_handle: cx.focus_handle(),
        }
    }

    /// The environment, the URL whose server presented the certificate,
    /// and what trusting it means, while the connection fails on a
    /// certificate that could be read. Pins are per URL (ENV-12).
    pub(crate) fn offer(&self, cx: &App) -> Option<(String, String, TrustOffer)> {
        let state = self.state.read(cx);
        let environment = state.environment()?;
        // The URL that failed on its certificate, or a standby whose
        // certificate isn't trusted while another URL is retried.
        let (url, _, Some(certificate)) = state.connection().state.as_ref()?.untrusted()? else {
            return None;
        };
        let pinned = environment
            .urls
            .iter()
            .find(|entry| entry.url.trim() == url.trim())?
            .pinned_sha256
            .as_deref();
        Some((
            environment.id.clone(),
            url.to_owned(),
            trust_offer(pinned, certificate),
        ))
    }

    /// Trusts the presented certificate ("trust this certificate", or
    /// "trust the new certificate" after a mismatch).
    pub(crate) fn trust(&mut self, cx: &mut Context<Self>) {
        if let Some((environment_id, url, offer)) = self.offer(cx) {
            cx.emit(CertificateEvent::Trust {
                environment_id,
                url,
                fingerprint: offer.fingerprint,
            });
        }
    }
}

impl CertificateReview {
    /// The dialog's buttons: "edit environment", cancel, and trusting the
    /// certificate (`trust`: whether one can be trusted, and whether that
    /// replaces a pin).
    fn with_buttons(
        mut dialog: DialogBody,
        environment_id: Option<String>,
        trust: Option<bool>,
        cx: &Context<Self>,
    ) -> DialogBody {
        if let Some(id) = environment_id.clone() {
            dialog = dialog.footer_start(
                Button::new("certificate-edit", "edit environment").on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| {
                        cx.emit(CertificateEvent::Edit(id.clone()));
                    },
                )),
            );
        }
        dialog = dialog.action(
            Button::new("certificate-cancel", "cancel")
                .key_hint("esc")
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(CertificateEvent::Close))),
        );
        if let (Some(mismatch), Some(_)) = (trust, environment_id) {
            dialog = dialog.action(
                Button::new(
                    "certificate-trust",
                    if mismatch {
                        "trust the new certificate"
                    } else {
                        "trust this certificate"
                    },
                )
                .variant(if mismatch {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.trust(cx))),
            );
        }
        dialog
    }
}

impl Render for CertificateReview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let now = Timestamp::now();
        let state = self.state.read(cx);
        let environment_id = state
            .environment()
            .map(|environment| environment.id.clone());
        let offer = self.offer(cx).map(|(_, _, offer)| offer);
        let mut endpoint = state.connection().endpoint.clone();
        let failure = match state
            .connection()
            .state
            .as_ref()
            .and_then(ConnectionState::untrusted)
        {
            Some((url, message, certificate)) => {
                endpoint = ic_config::ApiUrl::new(url).label();
                Some((message.to_owned(), certificate.cloned()))
            }
            None => None,
        };
        let title = format!("Certificate of {endpoint}");
        let mut dialog = DialogBody::new(title);
        match &failure {
            None => {
                dialog = dialog.child(
                    div()
                        .text_color(colors.text_muted)
                        .child("The connection isn't failing on its certificate (any more)."),
                );
            }
            Some((message, certificate)) => {
                dialog = dialog.child(
                    div()
                        .text_color(colors.text_muted)
                        .child(format!("icygui doesn't trust it: {message}")),
                );
                match certificate {
                    Some(certificate) => {
                        let replaces = offer.as_ref().and_then(|offer| offer.replaces.as_deref());
                        if let Some(pinned) = replaces {
                            dialog = dialog.child(mismatch_warning(
                                pinned,
                                &certificate.fingerprint(),
                                &theme,
                            ));
                        } else {
                            dialog = dialog.child(div().text_color(colors.text_muted).child(
                                "Compare the fingerprint with the one on the Icinga master \
                                 (openssl x509 -noout -fingerprint -sha256 -in \
                                 /var/lib/icinga2/certs/<node>.crt) before trusting it.",
                            ));
                        }
                        dialog = dialog.child(certificate_details(certificate, now, &theme));
                    }
                    None => {
                        dialog = dialog.child(div().text_color(colors.text_muted).child(
                            "The server's certificate couldn't be read. Check the URL and \
                             the TLS settings.",
                        ));
                    }
                }
            }
        }
        let trust = offer.map(|offer| offer.replaces.is_some());
        let dialog = Self::with_buttons(dialog, environment_id, trust, cx);
        div()
            .id("certificate-review")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .min_h_0()
            .child(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate(sha256: [u8; 32]) -> CertificateInfo {
        CertificateInfo {
            sha256,
            subject: "CN=icinga-master".to_owned(),
            issuer: "CN=Icinga CA".to_owned(),
            names: vec!["icinga-master".to_owned()],
            not_before: Timestamp::EPOCH,
            not_after: Timestamp::from_unix_seconds(2_000_000_000.0),
        }
    }

    #[test]
    fn a_pin_mismatch_offers_the_new_certificate_with_both_fingerprints() {
        let presented = certificate([0xab; 32]);
        let other = ic_config::format_fingerprint(&[0xcd; 32]);
        // Nothing pinned yet: plain trust on first use.
        assert_eq!(
            trust_offer(None, &presented),
            TrustOffer {
                fingerprint: presented.fingerprint(),
                replaces: None,
            }
        );
        // The pin names this certificate (in another notation): no warning.
        assert_eq!(
            trust_offer(Some(&"ab".repeat(32)), &presented).replaces,
            None
        );
        // Another certificate is pinned: both fingerprints.
        let offer = trust_offer(Some(&other), &presented);
        assert_eq!(offer.fingerprint, presented.fingerprint());
        assert_eq!(offer.replaces, Some(other));
    }

    #[test]
    fn fingerprints_compare_in_any_notation() {
        let bytes = [0xab; 32];
        assert!(same_fingerprint(
            &ic_config::format_fingerprint(&bytes),
            &bytes
        ));
        assert!(same_fingerprint(&"ab".repeat(32), &bytes));
        assert!(!same_fingerprint(&"cd".repeat(32), &bytes));
        assert!(!same_fingerprint("garbage", &bytes));
    }
}
