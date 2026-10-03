use std::time::Duration;

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::Mailbox,
    transport::smtp::authentication::Credentials,
};
use serde::{Deserialize, Serialize};
use ternilo_control::AccountEmailDelivery;
use ternilo_protocol::HarnessError;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MailSettings {
    pub host: String,
    pub port: u16,
    pub security: MailSecurity,
    pub from: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MailSecurity {
    Tls,
    Starttls,
    Local,
}

pub(crate) struct AccountMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    sender: Mailbox,
    origin: url::Url,
    pub slots: std::sync::Arc<tokio::sync::Semaphore>,
}

impl AccountMailer {
    pub(crate) fn new(settings: &MailSettings, origin: &str) -> Result<Self, HarnessError> {
        super::super::web::validate_public_url(origin, true)?;
        let origin = url::Url::parse(origin)
            .map_err(|_| HarnessError::invalid("invalid email public URL"))?;
        if settings.host.is_empty()
            || settings.host.len() > 253
            || settings
                .host
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#'))
            || settings.port == 0
        {
            return Err(HarnessError::invalid("SMTP host and port are invalid"));
        }
        let sender = settings
            .from
            .parse::<Mailbox>()
            .map_err(|_| HarnessError::invalid("SMTP sender must be a valid email address"))?;
        let builder = match settings.security {
            MailSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
            MailSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
            }
            MailSecurity::Local => {
                if settings.host != "localhost"
                    && !settings
                        .host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
                {
                    return Err(HarnessError::invalid(
                        "unencrypted SMTP is restricted to a loopback mail relay",
                    ));
                }
                Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                    &settings.host,
                ))
            }
        }
        .map_err(|_| HarnessError::invalid("invalid SMTP TLS configuration"))?;
        let mut builder = builder
            .port(settings.port)
            .timeout(Some(Duration::from_secs(15)));
        if let Some(username) = settings.username.as_ref().filter(|v| !v.is_empty()) {
            let password = settings
                .password
                .as_ref()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    HarnessError::invalid("SMTP password is required when a username is set")
                })?;
            if username.len() > 1024 || password.len() > 4096 {
                return Err(HarnessError::invalid(
                    "SMTP credentials exceed their size limits",
                ));
            }
            builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            sender,
            origin,
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }

    pub(crate) async fn send(
        &self,
        delivery: AccountEmailDelivery,
        reset: bool,
    ) -> Result<(), HarnessError> {
        let mut link = self.origin.clone();
        link.set_path(if reset {
            "/auth/reset-password"
        } else {
            "/auth/verify-email"
        });
        link.set_fragment(Some(&delivery.token));
        let subject = if reset {
            "Ternilo 密码找回 / Password recovery"
        } else {
            "Ternilo 邮箱验证 / Verify your email"
        };
        let instruction = if reset {
            "打开链接设置新密码。 / Open this link to set a new password."
        } else {
            "登录原账号后打开链接，确认此邮箱归你所有。 / Sign in to your account and open this link to verify your email."
        };
        let message = Message::builder().from(self.sender.clone())
            .to(delivery.email.parse().map_err(|_| HarnessError::invalid("account email is invalid"))?)
            .subject(subject)
            .body(format!("{instruction}\n\n{link}\n\n链接 15 分钟内有效，只能使用一次。如果不是你发起的请求，请忽略此邮件。\nThis link expires in 15 minutes and can be used once. Ignore this email if you did not request it.\n"))
            .map_err(|_| HarnessError::execution("could not prepare account email"))?;
        tokio::time::timeout(Duration::from_secs(20), self.transport.send(message))
            .await
            .map_err(|_| HarnessError::unavailable("account email delivery timed out"))?
            .map_err(|_| {
                HarnessError::unavailable("account email delivery failed; check the SMTP settings")
            })?;
        Ok(())
    }
}
