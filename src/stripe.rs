//! Payments by Stripe: memberships are Stripe subscriptions, paid on Stripe's
//! own checkout page and managed in its customer portal. Stripe tells the site
//! about changes by webhooks, whose signatures are checked here; the site then
//! asks Stripe for the subscription's current state rather than trusting the
//! order the webhooks arrive in.

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::Sha256;
use std::collections::HashMap;

use crate::Language;

/// The Stripe API version the requests and responses here are written for.
const API_VERSION: &str = "2025-03-31.basil";
/// How old a webhook's signature may be, against replays.
const WEBHOOK_TOLERANCE_SECONDS: i64 = 300;

/// A recurring price, as set up in Stripe.
#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    pub id: String,
    /// In the currency's smallest unit, e.g. cents.
    pub amount: i64,
    pub currency: String,
}

impl Price {
    /// E.g. "25 €" or, in Finnish, "24,90 €".
    #[must_use]
    pub fn text(&self, language: Language) -> String {
        let symbol = match self.currency.as_str() {
            "eur" => "€".to_string(),
            other => other.to_uppercase(),
        };
        let (whole, cents) = (self.amount / 100, self.amount % 100);
        if cents == 0 {
            return format!("{whole} {symbol}");
        }
        let separator = match language {
            Language::En => '.',
            Language::Fi => ',',
        };
        format!("{whole}{separator}{cents:02} {symbol}")
    }
}

#[derive(Deserialize)]
struct PriceObject {
    id: String,
    unit_amount: Option<i64>,
    currency: String,
    recurring: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct UrlObject {
    url: String,
}

#[derive(Deserialize)]
struct CheckoutSession {
    subscription: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub customer: String,
    pub status: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// In API versions before 2025-03-31; later it is on the items.
    current_period_end: Option<i64>,
    items: Items,
}

#[derive(Debug, Deserialize)]
struct Items {
    data: Vec<Item>,
}

#[derive(Debug, Deserialize)]
struct Item {
    current_period_end: Option<i64>,
}

impl Subscription {
    /// Whether the member should have access: paid, or a payment being
    /// retried. Canceling at the end of the period keeps it active till then.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "active" | "trialing" | "past_due")
    }

    /// When the paid period ends, as a Unix time.
    #[must_use]
    pub fn period_end(&self) -> Option<i64> {
        self.items
            .data
            .iter()
            .filter_map(|item| item.current_period_end)
            .max()
            .or(self.current_period_end)
    }

    /// The site's user the subscription was bought for, from its metadata.
    #[must_use]
    pub fn user_id(&self) -> Option<i64> {
        self.metadata.get("user_id")?.parse().ok()
    }
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    data: EventData,
}

#[derive(Deserialize)]
struct EventData {
    object: serde_json::Value,
}

/// What a webhook asks the site to do.
#[derive(Debug, PartialEq)]
pub enum Webhook {
    /// Look up this subscription again and update the member.
    Subscription(String),
    /// Nothing; the site doesn't follow this kind of event.
    Ignored(String),
}

pub struct Stripe {
    http: reqwest::Client,
    /// Without a trailing slash, e.g. `https://api.stripe.com`.
    api_url: String,
    secret_key: String,
    webhook_secret: String,
    pub monthly: Price,
    pub yearly: Price,
    /// Whether Stripe Tax adds VAT by the customer's address.
    automatic_tax: bool,
}

/// What `Stripe::connect` needs to know.
pub struct Settings {
    pub api_url: String,
    pub secret_key: String,
    pub webhook_secret: String,
    pub monthly_price: String,
    pub yearly_price: String,
    pub automatic_tax: bool,
}

impl Stripe {
    /// Connects to Stripe and fetches the prices, so a wrong key or price
    /// stops the server at start-up.
    pub async fn connect(settings: Settings) -> Result<Self, String> {
        let mut stripe = Self {
            http: http_client()?,
            api_url: settings.api_url.trim_end_matches('/').to_string(),
            secret_key: settings.secret_key,
            webhook_secret: settings.webhook_secret,
            monthly: Price {
                id: settings.monthly_price,
                amount: 0,
                currency: String::new(),
            },
            yearly: Price {
                id: settings.yearly_price,
                amount: 0,
                currency: String::new(),
            },
            automatic_tax: settings.automatic_tax,
        };
        stripe.monthly = stripe.price(&stripe.monthly.id).await?;
        stripe.yearly = stripe.price(&stripe.yearly.id).await?;
        Ok(stripe)
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        form: &[(&str, String)],
    ) -> Result<T, String> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.api_url))
            .bearer_auth(&self.secret_key)
            .header("Stripe-Version", API_VERSION);
        if !form.is_empty() {
            request = request.form(form);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("Stripe {path}: {error}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| format!("Stripe {path}: {error}"))?;
        if !status.is_success() {
            // Stripe's errors say what is wrong, without secrets.
            return Err(format!("Stripe {path}: {status}: {body}"));
        }
        serde_json::from_str(&body).map_err(|error| format!("Stripe {path}: {error}"))
    }

    async fn price(&self, id: &str) -> Result<Price, String> {
        let price: PriceObject = self
            .request(reqwest::Method::GET, &format!("/v1/prices/{id}"), &[])
            .await?;
        match (price.unit_amount, price.recurring) {
            (Some(amount), Some(_)) => Ok(Price {
                id: price.id,
                amount,
                currency: price.currency,
            }),
            _ => Err(format!(
                "Stripe price {id} should be a recurring price with a fixed amount"
            )),
        }
    }

    /// A checkout page for a membership at `price`, returning its address.
    /// `customer` is the user's Stripe customer from an earlier membership.
    pub async fn checkout(
        &self,
        price: &Price,
        user_id: i64,
        email: &str,
        customer: Option<&str>,
        language: Language,
        site_url: &str,
    ) -> Result<String, String> {
        let lang = language.code();
        let mut form = vec![
            ("mode", "subscription".to_string()),
            ("line_items[0][price]", price.id.clone()),
            ("line_items[0][quantity]", "1".to_string()),
            (
                "success_url",
                format!(
                    "{site_url}/membership/welcome?session={{CHECKOUT_SESSION_ID}}&lang={lang}"
                ),
            ),
            ("cancel_url", format!("{site_url}/account?lang={lang}")),
            ("client_reference_id", user_id.to_string()),
            ("subscription_data[metadata][user_id]", user_id.to_string()),
            ("locale", lang.to_string()),
            ("allow_promotion_codes", "true".to_string()),
        ];
        match customer {
            Some(customer) => {
                form.push(("customer", customer.to_string()));
                if self.automatic_tax {
                    form.push(("customer_update[address]", "auto".to_string()));
                }
            }
            None => form.push(("customer_email", email.to_string())),
        }
        if self.automatic_tax {
            form.push(("automatic_tax[enabled]", "true".to_string()));
        }
        let session: UrlObject = self
            .request(reqwest::Method::POST, "/v1/checkout/sessions", &form)
            .await?;
        Ok(session.url)
    }

    /// The subscription bought in the checkout `session`, if any yet.
    pub async fn checkout_subscription(&self, session: &str) -> Result<Option<String>, String> {
        if !session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Ok(None);
        }
        let session: CheckoutSession = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/checkout/sessions/{session}"),
                &[],
            )
            .await?;
        Ok(session.subscription)
    }

    pub async fn subscription(&self, id: &str) -> Result<Subscription, String> {
        self.request(
            reqwest::Method::GET,
            &format!("/v1/subscriptions/{id}"),
            &[],
        )
        .await
    }

    /// The customer portal, where members change or cancel their membership
    /// and see their receipts, returning its address.
    pub async fn portal(&self, customer: &str, return_url: &str) -> Result<String, String> {
        let session: UrlObject = self
            .request(
                reqwest::Method::POST,
                "/v1/billing_portal/sessions",
                &[
                    ("customer", customer.to_string()),
                    ("return_url", return_url.to_string()),
                ],
            )
            .await?;
        Ok(session.url)
    }

    /// Deletes the customer, which also ends their subscriptions at once.
    pub async fn delete_customer(&self, customer: &str) -> Result<(), String> {
        let _: serde_json::Value = self
            .request(
                reqwest::Method::DELETE,
                &format!("/v1/customers/{customer}"),
                &[],
            )
            .await?;
        Ok(())
    }

    /// Checks a webhook's `Stripe-Signature` header against its body, and
    /// says what it asks for. `now` is the current Unix time.
    pub fn webhook(&self, payload: &[u8], signature: &str, now: i64) -> Result<Webhook, String> {
        let mut timestamp = None;
        let mut signatures = Vec::new();
        for part in signature.split(',') {
            match part.split_once('=') {
                Some(("t", value)) => timestamp = value.parse::<i64>().ok(),
                Some(("v1", value)) => signatures.push(value),
                _ => {}
            }
        }
        let timestamp = timestamp.ok_or("no timestamp in the signature")?;
        if (now - timestamp).abs() > WEBHOOK_TOLERANCE_SECONDS {
            return Err("the signature is too old".to_string());
        }
        let valid = signatures.iter().any(|signature| {
            let Some(bytes) = decode_hex(signature) else {
                return false;
            };
            let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(self.webhook_secret.as_bytes()) else {
                return false;
            };
            mac.update(timestamp.to_string().as_bytes());
            mac.update(b".");
            mac.update(payload);
            mac.verify_slice(&bytes).is_ok()
        });
        if !valid {
            return Err("the signature doesn't match".to_string());
        }
        let event: Event =
            serde_json::from_slice(payload).map_err(|error| format!("event: {error}"))?;
        let object = &event.data.object;
        let id = match event.kind.as_str() {
            "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted"
            | "customer.subscription.paused"
            | "customer.subscription.resumed" => object.get("id"),
            "checkout.session.completed" => object.get("subscription"),
            _ => None,
        };
        Ok(match id.and_then(serde_json::Value::as_str) {
            Some(id) => Webhook::Subscription(id.to_string()),
            None => Webhook::Ignored(event.kind),
        })
    }
}

/// An HTTPS client with Mozilla's root certificates built in, so that neither
/// the hardened service nor the build sandbox needs the system's, and with
/// ring for cryptography, as lettre uses, rather than building aws-lc.
fn http_client() -> Result<reqwest::Client, String> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| error.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|start| u8::from_str_radix(text.get(start..start + 2)?, 16).ok())
        .collect()
}

/// A webhook signature, as Stripe makes them, for tests.
#[cfg(test)]
pub fn sign(secret: &str, payload: &[u8], timestamp: i64) -> String {
    use std::fmt::Write;
    // Any key length works for HMAC, so this doesn't fail.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload);
    let signature = mac
        .finalize()
        .into_bytes()
        .iter()
        .fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        });
    format!("t={timestamp},v1={signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripe() -> Result<Stripe, String> {
        Ok(Stripe {
            http: http_client()?,
            api_url: "http://127.0.0.1:9".to_string(),
            secret_key: "sk_test".to_string(),
            webhook_secret: "whsec_test".to_string(),
            monthly: Price {
                id: "price_month".to_string(),
                amount: 2500,
                currency: "eur".to_string(),
            },
            yearly: Price {
                id: "price_year".to_string(),
                amount: 25000,
                currency: "eur".to_string(),
            },
            automatic_tax: false,
        })
    }

    #[test]
    fn prices_are_written_as_usual() -> Result<(), String> {
        let mut price = stripe()?.monthly;
        assert_eq!(price.text(Language::En), "25 €");
        price.amount = 2490;
        assert_eq!(price.text(Language::En), "24.90 €");
        assert_eq!(price.text(Language::Fi), "24,90 €");
        price.amount = 2405;
        price.currency = "usd".to_string();
        assert_eq!(price.text(Language::Fi), "24,05 USD");
        Ok(())
    }

    #[test]
    fn webhooks_need_a_fresh_valid_signature() -> Result<(), String> {
        let stripe = stripe()?;
        let payload =
            br#"{"type":"customer.subscription.updated","data":{"object":{"id":"sub_1"}}}"#;
        let now = 1_800_000_000;
        let signature = sign("whsec_test", payload, now);
        assert_eq!(
            stripe.webhook(payload, &signature, now + 10),
            Ok(Webhook::Subscription("sub_1".to_string()))
        );
        // One of several signatures may match, e.g. while the secret is changed.
        let several = format!(
            "t={now},v1=00ff,{}",
            signature.replace(&format!("t={now},"), "")
        );
        assert!(stripe.webhook(payload, &several, now).is_ok());

        assert!(stripe.webhook(payload, &signature, now + 301).is_err());
        let wrong = sign("whsec_other", payload, now);
        assert!(stripe.webhook(payload, &wrong, now).is_err());
        let changed =
            br#"{"type":"customer.subscription.updated","data":{"object":{"id":"sub_2"}}}"#;
        assert!(stripe.webhook(changed, &signature, now).is_err());
        assert!(stripe.webhook(payload, "v1=abc", now).is_err());
        Ok(())
    }

    #[test]
    fn webhooks_name_the_subscription_to_look_up() -> Result<(), String> {
        let stripe = stripe()?;
        let now = 1_800_000_000;
        let event = |kind: &str, object: &str| {
            let payload = format!(r#"{{"type":"{kind}","data":{{"object":{object}}}}}"#);
            stripe.webhook(
                payload.as_bytes(),
                &sign("whsec_test", payload.as_bytes(), now),
                now,
            )
        };
        assert_eq!(
            event(
                "checkout.session.completed",
                r#"{"id":"cs_1","subscription":"sub_1"}"#
            ),
            Ok(Webhook::Subscription("sub_1".to_string()))
        );
        assert_eq!(
            event("customer.subscription.deleted", r#"{"id":"sub_1"}"#),
            Ok(Webhook::Subscription("sub_1".to_string()))
        );
        assert_eq!(
            event("invoice.paid", r#"{"id":"in_1"}"#),
            Ok(Webhook::Ignored("invoice.paid".to_string()))
        );
        Ok(())
    }

    #[test]
    fn subscriptions_end_with_their_last_item() -> Result<(), serde_json::Error> {
        let subscription: Subscription = serde_json::from_str(
            r#"{"id":"sub_1","customer":"cus_1","status":"past_due","metadata":{"user_id":"7"},
                "items":{"data":[{"current_period_end":100},{"current_period_end":200}]}}"#,
        )?;
        assert!(subscription.is_active());
        assert_eq!(subscription.period_end(), Some(200));
        assert_eq!(subscription.user_id(), Some(7));
        let old: Subscription = serde_json::from_str(
            r#"{"id":"sub_1","customer":"cus_1","status":"canceled","current_period_end":50,
                "items":{"data":[{}]}}"#,
        )?;
        assert!(!old.is_active());
        assert_eq!(old.period_end(), Some(50));
        assert_eq!(old.user_id(), None);
        Ok(())
    }
}
