//! A one-shot local HTTP listener for the OAuth redirect.
//!
//! The sign-in happens in the user's real browser - not an embedded webview -
//! so they can see the address bar and their password manager works. Microsoft
//! then redirects to `http://127.0.0.1:<port>`, which this catches.
//!
//! Bound to `127.0.0.1` rather than `0.0.0.0`: the redirect never leaves the
//! machine, and binding to all interfaces would expose the authorization code
//! to the local network.
//!
//! Deliberately not a web framework. It answers exactly one request and exits.

use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{AuthError, Result};
use crate::pkce::State;

/// A bound, listening socket waiting for the redirect.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// Bind an ephemeral port on the loopback interface.
    ///
    /// The port is chosen by the OS rather than fixed, so two launcher
    /// instances signing in at once cannot collide.
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|source| AuthError::Loopback { source })?;
        let port = listener
            .local_addr()
            .map_err(|source| AuthError::Loopback { source })?
            .port();
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `redirect_uri` to send to Microsoft. Must match byte for byte on
    /// both the authorize and token calls.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Wait for the browser to arrive, and return the authorization code.
    ///
    /// Ignores anything that is not the callback - browsers speculatively
    /// request `/favicon.ico`, and answering that with "sign-in complete" and
    /// then exiting would strand the real redirect.
    pub async fn wait_for_code(self, expected_state: &State, timeout: Duration) -> Result<String> {
        tokio::time::timeout(timeout, self.accept_until_callback(expected_state))
            .await
            .map_err(|_| AuthError::TimedOut)?
    }

    async fn accept_until_callback(&self, expected_state: &State) -> Result<String> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|source| AuthError::Loopback { source })?;

            let Some(target) = read_request_target(&mut stream).await? else {
                continue;
            };

            let Some(query) = target.split_once('?').map(|(_, q)| q) else {
                respond(&mut stream, 404, "Not found").await;
                continue;
            };

            let params = parse_query(query);

            if let Some(error) = params.iter().find(|(k, _)| k == "error") {
                respond(
                    &mut stream,
                    200,
                    &page("Sign-in cancelled", "You can close this tab."),
                )
                .await;
                return Err(if error.1 == "access_denied" {
                    AuthError::Cancelled
                } else {
                    AuthError::Protocol {
                        stage: crate::error::Stage::Microsoft,
                        detail: format!("authorization failed: {}", error.1),
                    }
                });
            }

            let state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v);
            let code = params.iter().find(|(k, _)| k == "code").map(|(_, v)| v);

            let (Some(state), Some(code)) = (state, code) else {
                respond(&mut stream, 400, "Missing parameters").await;
                continue;
            };

            // A callback whose state we did not issue is either a stale tab or
            // a forgery. Either way the code is not ours to spend.
            if !expected_state.matches(state) {
                respond(&mut stream, 400, "State mismatch").await;
                return Err(AuthError::Protocol {
                    stage: crate::error::Stage::Microsoft,
                    detail: "redirect state did not match the request; ignoring it".to_owned(),
                });
            }

            respond(
                &mut stream,
                200,
                &page(
                    "Signed in",
                    "You can close this tab and return to Deepslate.",
                ),
            )
            .await;
            return Ok(code.clone());
        }
    }
}

/// Read just enough to get the request target out of the request line.
async fn read_request_target(stream: &mut TcpStream) -> Result<Option<String>> {
    // The request line is the first line; an authorization code is short, so a
    // small bounded read is sufficient and caps what an unfriendly client can
    // make us buffer.
    let mut buf = vec![0_u8; 8192];
    let read = stream
        .read(&mut buf)
        .await
        .map_err(|source| AuthError::Loopback { source })?;

    if read == 0 {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&buf[..read]);
    let Some(line) = text.lines().next() else {
        return Ok(None);
    };

    // "GET /callback?code=... HTTP/1.1"
    Ok(line.split_whitespace().nth(1).map(str::to_owned))
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    // Best-effort: the browser having hung up does not change the outcome of
    // the sign-in, and failing here would discard a code we already hold.
    let _unused = stream.write_all(response.as_bytes()).await;
    let _unused = stream.flush().await;
}

fn page(title: &str, message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{background:#14161a;color:#e8eaed;font-family:system-ui,sans-serif;\
         display:grid;place-items:center;height:100vh;margin:0}}\
         div{{text-align:center}}h1{{font-size:21px;font-weight:600;margin:0 0 8px}}\
         p{{color:#a8aeb8;font-size:14px;margin:0}}</style></head>\
         <body><div><h1>{title}</h1><p>{message}</p></div></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn binds_loopback_only_and_reports_its_port() {
        let server = Loopback::bind().await.unwrap();
        assert!(server.port() > 0);
        assert_eq!(
            server.redirect_uri(),
            format!("http://127.0.0.1:{}", server.port())
        );
    }

    #[tokio::test]
    async fn returns_the_code_when_state_matches() {
        let server = Loopback::bind().await.unwrap();
        let port = server.port();
        let state = State::generate();
        let state_value = state.as_str().to_owned();

        let client = tokio::spawn(async move {
            get(
                port,
                &format!("/callback?code=the-code&state={state_value}"),
            )
            .await
        });

        let code = server
            .wait_for_code(&state, Duration::from_secs(5))
            .await
            .expect("should receive the code");

        assert_eq!(code, "the-code");
        assert!(client.await.unwrap().contains("Signed in"));
    }

    /// A callback carrying a state we never issued must not be honoured.
    #[tokio::test]
    async fn rejects_a_forged_state() {
        let server = Loopback::bind().await.unwrap();
        let port = server.port();
        let state = State::generate();

        tokio::spawn(async move { get(port, "/?code=stolen&state=forged").await });

        let err = server
            .wait_for_code(&state, Duration::from_secs(5))
            .await
            .expect_err("forged state must be rejected");

        assert!(matches!(err, AuthError::Protocol { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn user_declining_consent_is_reported_as_cancelled() {
        let server = Loopback::bind().await.unwrap();
        let port = server.port();
        let state = State::generate();

        tokio::spawn(
            async move { get(port, "/?error=access_denied&error_description=nope").await },
        );

        let err = server
            .wait_for_code(&state, Duration::from_secs(5))
            .await
            .expect_err("denial must be an error");

        assert!(matches!(err, AuthError::Cancelled), "got {err:?}");
    }

    /// Browsers ask for /favicon.ico unprompted. Treating that as the redirect
    /// would abandon the sign-in before the real callback arrives.
    #[tokio::test]
    async fn ignores_unrelated_requests_and_keeps_waiting() {
        let server = Loopback::bind().await.unwrap();
        let port = server.port();
        let state = State::generate();
        let state_value = state.as_str().to_owned();

        tokio::spawn(async move {
            get(port, "/favicon.ico").await;
            get(port, "/").await;
            get(
                port,
                &format!("/callback?code=real-code&state={state_value}"),
            )
            .await;
        });

        let code = server
            .wait_for_code(&state, Duration::from_secs(5))
            .await
            .expect("should skip noise and get the real code");

        assert_eq!(code, "real-code");
    }

    #[tokio::test]
    async fn gives_up_rather_than_waiting_forever() {
        let server = Loopback::bind().await.unwrap();
        let err = server
            .wait_for_code(&State::generate(), Duration::from_millis(150))
            .await
            .expect_err("should time out");
        assert!(matches!(err, AuthError::TimedOut), "got {err:?}");
    }
}
