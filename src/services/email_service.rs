use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use std::env;
use tracing::{info, error};

#[derive(Clone)]
pub struct EmailService {
    sender_email: String,
    admin_email: String,
    mailer_config: (String, String, String, u16),
}

impl EmailService {
    pub fn new() -> Self {
        let smtp_server = env::var("SMTP_SERVER").unwrap_or_else(|_| "smtp.gmail.com".to_string()).trim().to_string();
        let smtp_user = env::var("SMTP_USER").unwrap_or_default().trim().to_string();
        let smtp_pass = env::var("SMTP_PASS").unwrap_or_default().trim().to_string();
        let smtp_port = env::var("SMTP_PORT")
            .unwrap_or_else(|_| "587".to_string())
            .trim()
            .parse()
            .unwrap_or(587);
        let admin_email = env::var("NOTIFICATION_EMAIL").unwrap_or_default().trim().to_string();

        Self {
            sender_email: smtp_user.clone(),
            admin_email,
            mailer_config: (smtp_server, smtp_user, smtp_pass, smtp_port),
        }
    }

    fn is_office365(&self) -> bool {
        let server = &self.mailer_config.0;
        server.contains("office365.com") || server.contains("outlook.com") || server.contains("microsoft.com")
    }

    // SMTP_IP_RELAY=true       — unauthenticated relay, server whitelists by IP
    // SMTP_IP_RELAY_TLS=true   — same but server requires STARTTLS (still no credentials)
    fn ip_relay_mode(&self) -> Option<bool> {
        if env::var("SMTP_IP_RELAY").unwrap_or_default().trim().to_lowercase() == "true" {
            let wants_tls = env::var("SMTP_IP_RELAY_TLS")
                .unwrap_or_default().trim().to_lowercase() == "true";
            Some(wants_tls)
        } else {
            None
        }
    }

    fn get_transport(&self) -> SmtpTransport {
        let (server, user, pass, port) = &self.mailer_config;

        if let Some(use_tls) = self.ip_relay_mode() {
            if use_tls {
                // Relay needs STARTTLS but no auth — builder_dangerous avoids auth negotiation entirely
                info!("Email: IP relay with STARTTLS (no auth) via {}:{}", server, port);
                let tls_params = TlsParameters::new(server.clone())
                    .expect("Failed to create TLS parameters");
                return SmtpTransport::builder_dangerous(server)
                    .port(*port)
                    .tls(Tls::Opportunistic(tls_params))
                    .build();
            } else {
                // Fully open relay, plain SMTP, no TLS, no auth
                info!("Email: IP relay plain SMTP (no auth, no TLS) via {}:{}", server, port);
                return SmtpTransport::builder_dangerous(server)
                    .port(*port)
                    .build();
            }
        }

        // Normal authenticated modes
        let creds = Credentials::new(user.clone(), pass.clone());
        let tls_params = TlsParameters::new(server.clone())
            .expect("Failed to create TLS parameters");

        if self.is_office365() {
            // APRIL 5th, Office 365 only accepts LOGIN or PLAIN auth mechanisms.
            SmtpTransport::starttls_relay(server)
                .expect("Failed to create Office 365 SMTP transport")
                .port(*port)
                .credentials(creds)
                .authentication(vec![Mechanism::Login, Mechanism::Plain])
                .tls(Tls::Required(tls_params))
                .hello_name(lettre::transport::smtp::extension::ClientId::Domain(
                    env::var("SMTP_EHLO_HOSTNAME")
                        .unwrap_or_else(|_| "localhost".to_string())
                ))
                .build()
        } else {
            SmtpTransport::starttls_relay(server)
                .expect("Failed to create SMTP transport")
                .port(*port)
                .credentials(creds)
                .tls(Tls::Required(tls_params))
                .build()
        }
    }

    pub fn send_error_alert(&self, error_message: &str) {
        self.execute_send("Shopify Sync ERROR Alert", error_message);
    }

    pub fn send_report(&self, subject: &str, body: &str) {
        self.execute_send(subject, body);
    }

    fn execute_send(&self, subject: &str, body: &str) {
        if self.sender_email.is_empty() || self.admin_email.is_empty() {
            error!("Email service not fully configured. Skipping send.");
            return;
        }

        let from_addr = match self.sender_email.parse() {
            Ok(a) => a,
            Err(e) => { error!("Invalid SMTP_USER email format: '{}' - Error: {}", self.sender_email, e); return; }
        };
        let to_addr = match self.admin_email.parse() {
            Ok(a) => a,
            Err(e) => { error!("Invalid NOTIFICATION_EMAIL format: '{}' - Error: {}", self.admin_email, e); return; }
        };

        let email_res = Message::builder()
            .from(from_addr)
            .to(to_addr)
            .subject(subject)
            .body(body.to_string());

        match email_res {
            Ok(email) => {
                match self.get_transport().send(&email) {
                    Ok(_) => info!("Email notification sent successfully to {}", self.admin_email),
                    Err(e) => error!("SMTP Relay failed: {:?}", e),
                }
            }
            Err(e) => error!("Failed to build email message: {}", e),
        }
    }
}