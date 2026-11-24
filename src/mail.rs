//! The login emails, and how they are sent: by SMTP, e.g. through Gmail, or,
//! without `SMTP_HOST`, only logged.

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use maud::html;

use crate::Language;
use crate::accounts::{LINK_LIFETIME, LinkKind};

/// Who the emails are from, by default.
pub const DEFAULT_FROM: &str = "Taiji-Timo <taichiwayfarer@gmail.com>";

/// How long sending one email may take before giving up.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How the emails go out.
pub enum Mailer {
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    /// They don't: they are only logged, links and all, for development.
    Log,
    /// For tests: keeps them, or fails as told.
    #[cfg(test)]
    Stub(lettre::transport::stub::AsyncStubTransport),
}

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// From the start, usually port 465, as Gmail's.
    Tls,
    /// Upgraded after connecting, usually port 587.
    StartTls,
    /// None, for a local relay.
    None,
}

impl Encryption {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "tls" => Ok(Self::Tls),
            "starttls" => Ok(Self::StartTls),
            "none" => Ok(Self::None),
            other => Err(format!(
                "SMTP_ENCRYPTION should be tls, starttls or none, got {other}"
            )),
        }
    }
}

impl Mailer {
    pub fn smtp(
        host: &str,
        port: u16,
        encryption: Encryption,
        login: Option<(String, String)>,
    ) -> Result<Self, lettre::transport::smtp::Error> {
        let builder = match encryption {
            Encryption::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)?,
            Encryption::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?,
            Encryption::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        };
        let builder = builder.port(port);
        let builder = match login {
            Some((user, password)) => builder.credentials(Credentials::new(user, password)),
            None => builder,
        };
        Ok(Self::Smtp(builder.build()))
    }

    pub async fn send(&self, message: Message) -> Result<(), String> {
        match self {
            Self::Smtp(smtp) => tokio::time::timeout(SEND_TIMEOUT, smtp.send(message))
                .await
                .map_err(|_| "timed out".to_string())?
                .map(drop)
                .map_err(|error| error.to_string()),
            Self::Log => {
                let header = |name| {
                    message
                        .headers()
                        .get_raw(name)
                        .unwrap_or_default()
                        .to_string()
                };
                tracing::info!(to = %header("To"), subject = %header("Subject"), "Email not sent, as SMTP_HOST isn't set");
                Ok(())
            }
            #[cfg(test)]
            Self::Stub(stub) => stub.send(message).await.map_err(|error| error.to_string()),
        }
    }
}

/// The email with a link to log in, or to finish registering.
pub fn login_link(
    from: &Mailbox,
    to: &str,
    language: Language,
    kind: LinkKind,
    link: &str,
) -> Result<Message, String> {
    let to: Mailbox = to.parse().map_err(|error| format!("{to}: {error}"))?;
    let minutes = LINK_LIFETIME.num_minutes();
    let (subject, greeting, intro, button, expiry, ignore) = match (language, kind) {
        (Language::En, LinkKind::Login) => (
            "Your link to log in to Taiji-Timo",
            "Hi!",
            "Here is your link to log in to the Taiji-Timo lessons:",
            "Log in",
            format!("The link works once, within {minutes} minutes."),
            "If you didn't ask for it, just ignore this email.",
        ),
        (Language::En, LinkKind::Register) => (
            "Finish registering to Taiji-Timo",
            "Hi!",
            "Thank you for registering to the Taiji-Timo lessons! Finish with this link:",
            "Finish registering",
            format!("The link works once, within {minutes} minutes."),
            "If you didn't register, just ignore this email, and no account is made.",
        ),
        (Language::Fi, LinkKind::Login) => (
            "Kirjautumislinkkisi Taiji-Timoon",
            "Hei!",
            "Tällä linkillä kirjaudut Taiji-Timon oppitunneille:",
            "Kirjaudu sisään",
            format!("Linkki toimii kerran {minutes} minuutin ajan."),
            "Jos et pyytänyt linkkiä, voit jättää tämän viestin huomiotta.",
        ),
        (Language::Fi, LinkKind::Register) => (
            "Viimeistele rekisteröitymisesi Taiji-Timoon",
            "Hei!",
            "Kiitos, että rekisteröidyit Taiji-Timon oppitunneille! Viimeistele rekisteröityminen tällä linkillä:",
            "Viimeistele rekisteröityminen",
            format!("Linkki toimii kerran {minutes} minuutin ajan."),
            "Jos et rekisteröitynyt, voit jättää tämän viestin huomiotta, eikä tiliä luoda.",
        ),
    };
    let text = format!("{greeting}\n\n{intro}\n\n{link}\n\n{expiry} {ignore}\n\nTimo\n");
    let html = html! {
        (maud::DOCTYPE)
        html lang=(language.code()) {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (subject) }
            }
            body style="margin:0;padding:24px 16px;background:#f8fafc;color:#182939;font-family:Helvetica,Arial,sans-serif;font-size:16px;line-height:1.5" {
                div style="max-width:560px;margin:0 auto" {
                    p { (greeting) }
                    p { (intro) }
                    p style="margin:24px 0" {
                        a href=(link) style="background:#182939;color:#f8fafc;padding:10px 20px;border-radius:8px;text-decoration:none;display:inline-block" {
                            (button)
                        }
                    }
                    p { (expiry) " " (ignore) }
                    p { "Timo" }
                }
            }
        }
    };
    Message::builder()
        .from(from.clone())
        .to(to)
        .subject(subject)
        .multipart(MultiPart::alternative_plain_html(text, html.into_string()))
        .map_err(|error| error.to_string())
}
