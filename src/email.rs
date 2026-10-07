//! Email Helper - Send emails via SMTP (port of Email.php).
use std::{fs, path::Path};

use lettre::{
    message::{header::ContentType, Attachment, MultiPart, SinglePart},
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

use tracing::{error, info};

use crate::{env, error::ApiError};

pub struct Email;

impl Email {
    /// Send an email using SMTP credentials from environment variables.
    ///
    /// Supported environment variables:
    /// - `MAIL_HOST` (default: "localhost")
    /// - `MAIL_PORT` (default: 587)
    /// - `MAIL_USERNAME`
    /// - `MAIL_PASSWORD`
    /// - `MAIL_ENCRYPTION` ("tls" | "ssl" | "none", default: "tls")
    /// - `MAIL_FROM_ADDRESS` (default: "noreply@example.com")
    /// - `MAIL_FROM_NAME` (default: "Padi Framework")
    pub async fn send(
        to: &str,
        subject: &str,
        body: &str,
        attachments: &[&str],
    ) -> Result<bool, ApiError> {
        let from_addr = env::get_or("MAIL_FROM_ADDRESS", "noreply@example.com");
        let from_name = env::get_or("MAIL_FROM_NAME", "Padi Framework");
        let host = env::get_or("MAIL_HOST", "localhost");
        let port = env::get_int("MAIL_PORT", 587) as u16;
        let username = env::get("MAIL_USERNAME");
        let password = env::get("MAIL_PASSWORD");
        let encryption = env::get_or("MAIL_ENCRYPTION", "tls").to_lowercase();

        let from_header = format!("{} <{}>", from_name, from_addr);
        let from_mailbox = from_header
            .parse()
            .map_err(|e| ApiError::internal(format!("Invalid sender address: {}", e)))?;
        let to_mailbox = to
            .parse()
            .map_err(|e| ApiError::internal(format!("Invalid recipient address: {}", e)))?;

        let email_builder = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject);

        // Build email body (support attachments if provided)
        let message = if attachments.is_empty() {
            email_builder
                .header(ContentType::TEXT_HTML)
                .body(body.to_string())
                .map_err(|e| ApiError::internal(format!("Failed to build email: {}", e)))?
        } else {
            let mut multipart = MultiPart::mixed().singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(body.to_string()),
            );

            for file_path in attachments {
                let path = Path::new(file_path);
                if path.is_file() {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "attachment".to_string());

                    if let Ok(file_content) = fs::read(path) {
                        let attachment = Attachment::new(file_name)
                            .body(file_content, ContentType::parse("application/octet-stream").unwrap());
                        multipart = multipart.singlepart(attachment);
                    }
                }
            }

            email_builder
                .multipart(multipart)
                .map_err(|e| ApiError::internal(format!("Failed to build multipart email: {}", e)))?
        };

        // Configure SMTP transport
        let mut transport_builder = match encryption.as_str() {
            "ssl" => {
                let tls_params = TlsParameters::builder(host.clone())
                    .build()
                    .map_err(|e| ApiError::internal(format!("TLS parameter error: {}", e)))?;
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
                    .port(port)
                    .tls(Tls::Wrapper(tls_params))
            }
            "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
                .port(port)
                .tls(Tls::None),
            _ => {
                // Default: STARTTLS
                let tls_params = TlsParameters::builder(host.clone())
                    .build()
                    .map_err(|e| ApiError::internal(format!("TLS parameter error: {}", e)))?;
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
                    .port(port)
                    .tls(Tls::Required(tls_params))
            }
        };

        if let (Some(u), Some(p)) = (username, password) {
            transport_builder = transport_builder.credentials(Credentials::new(u, p));
        }

        let mailer = transport_builder.build();

        match mailer.send(message).await {
            Ok(_) => {
                info!("Email successfully sent to {}", to);
                Ok(true)
            }
            Err(e) => {
                error!("Email delivery failed to {}: {}", to, e);
                Err(ApiError::internal(format!("Email delivery failed: {}", e)))
            }
        }
    }
}
