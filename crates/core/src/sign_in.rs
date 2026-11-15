//! Signing in to a provider instead of pasting a key from its site.
//!
//! OpenRouter is the one that offers it to an app; the others hand out keys
//! only on their own sites. It's OAuth with PKCE: the person signs in there,
//! the app is handed back a code, and trades it for a key of their own,
//! labelled with the app's name in their account. The code is worth nothing
//! without the verifier only this sign-in holds, so it can safely pass
//! through a page on the way back.
//!
//! **The way back is the site's relay page ([`RELAY`]), not the app.** A
//! sign-in that comes straight back to `localhost` is named on the consent
//! page as `localhost:<port>`, which is no name to trust with an account.
//! The relay forwards the code to the port the app listens on for it, which
//! it learns from `state`: OpenRouter hands that back unchanged.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SecureRandom as _;

/// OpenRouter's API, which a sign-in makes a key for: its preset's URL.
pub const ENDPOINT: &str = "https://openrouter.ai/api/v1";

/// Where OpenRouter sends the person once they've signed in: the site's page
/// that forwards the code to the app.
pub const RELAY: &str = "https://vestigecounty.github.io/thinkboard/signin/";

/// The path the relay sends the code to, on this machine.
pub const CALLBACK: &str = "/callback";

/// The key's label in the person's account, where they can find it to revoke.
const KEY_LABEL: &str = "ThinkBoard";

/// One sign-in: the verifier the code is traded with, and the `state` that
/// tells the code coming back from any other — the port it comes back to,
/// then a random part.
pub struct SignIn {
    verifier: String,
    pub state: String,
}

impl SignIn {
    /// A new sign-in, coming back to `port` on this machine. `None` where the
    /// system has no randomness to give, which a sign-in can't go without.
    pub fn new(port: u16) -> Option<Self> {
        Some(SignIn { verifier: random(32)?, state: format!("{port}.{}", random(16)?) })
    }

    /// Where to send the person: OpenRouter's sign-in, asking for a key for
    /// this app, to be sent back through the relay.
    pub fn url(&self) -> String {
        let challenge = challenge(&self.verifier);
        let params = [
            ("callback_url", RELAY),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("key_label", KEY_LABEL),
            ("state", self.state.as_str()),
        ];
        reqwest::Url::parse_with_params("https://openrouter.ai/auth", &params).expect("a valid URL").into()
    }

    /// The code in what came back — a request for [`CALLBACK`] — if it's this
    /// sign-in's: anything else that reaches the port is turned away.
    pub fn code_in(&self, target: &str) -> Option<String> {
        let url = reqwest::Url::parse("http://localhost").ok()?.join(target).ok()?;
        if url.path() != CALLBACK {
            return None;
        }
        let param = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
        (param("state")? == self.state).then(|| param("code")).flatten().filter(|code| !code.is_empty())
    }

    /// The key, for the code the sign-in came back with: one go, since a code
    /// is good once, and for ten minutes.
    pub async fn key(&self, code: &str) -> Result<String, String> {
        let body = serde_json::json!({ "code": code, "code_verifier": self.verifier, "code_challenge_method": "S256" });
        let response = reqwest::Client::new()
            .post("https://openrouter.ai/api/v1/auth/keys")
            .header("X-Title", "ThinkBoard")
            .json(&body)
            .send()
            .await
            .map_err(|_| "Couldn't reach OpenRouter to finish signing in — check the connection, then sign in again.".to_owned())?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        match status {
            200..=299 => key_in(&text),
            // The code had expired, been used, or wasn't this sign-in's.
            403 => Err("The sign-in had run out of time — sign in again.".to_owned()),
            _ => Err(format!("OpenRouter couldn't finish the sign-in ({status}) — sign in again.")),
        }
    }
}

/// The key in the exchange's answer, `{"key": "sk-or-…"}`.
fn key_in(body: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|answer| answer.get("key")?.as_str().map(|key| key.trim().to_owned()))
        .filter(|key| !key.is_empty())
        .ok_or_else(|| "OpenRouter didn't send a key back — sign in again.".to_owned())
}

/// `bytes` of the system's randomness, base64url: the verifier's characters,
/// and `state`'s, which the relay checks for.
fn random(bytes: usize) -> Option<String> {
    let mut buffer = vec![0; bytes];
    ring::rand::SystemRandom::new().fill(&mut buffer).ok()?;
    Some(URL_SAFE_NO_PAD.encode(buffer))
}

/// RFC 7636's S256 challenge: the verifier's SHA-256, base64url unpadded.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign_in() -> SignIn {
        SignIn { verifier: "v".to_owned(), state: "51423.abc_-9".to_owned() }
    }

    #[test]
    fn the_challenge_is_rfc_7636s() {
        // Appendix B's example verifier and its S256 challenge.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn a_sign_in_carries_its_port_and_asks_for_a_key_through_the_relay() {
        let new = SignIn::new(51423).expect("randomness");
        assert!(new.state.starts_with("51423."), "{}", new.state);
        assert_eq!(new.verifier.len(), 43, "32 bytes, base64url: RFC 7636's 43 to 128 characters");
        assert!(new.state.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)), "{}", new.state);
        assert_ne!(new.state, SignIn::new(51423).expect("randomness").state);

        let url = reqwest::Url::parse(&new.url()).expect("a URL");
        let param = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
        assert_eq!(url.host_str(), Some("openrouter.ai"));
        assert_eq!(param("callback_url").as_deref(), Some(RELAY));
        assert_eq!(param("code_challenge"), Some(challenge(&new.verifier)));
        assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(param("key_label").as_deref(), Some("ThinkBoard"));
        assert_eq!(param("state"), Some(new.state));
    }

    #[test]
    fn only_this_sign_ins_code_is_taken() {
        let sign_in = sign_in();
        assert_eq!(sign_in.code_in("/callback?code=c0de&state=51423.abc_-9").as_deref(), Some("c0de"));
        assert_eq!(sign_in.code_in("/callback?state=51423.abc_-9&code=a%2Fb").as_deref(), Some("a/b"));
        assert_eq!(sign_in.code_in("/callback?code=c0de&state=51423.other"), None, "another sign-in's");
        assert_eq!(sign_in.code_in("/callback?code=c0de"), None, "no state");
        assert_eq!(sign_in.code_in("/callback?code=&state=51423.abc_-9"), None, "no code");
        assert_eq!(sign_in.code_in("/favicon.ico"), None);
        assert_eq!(sign_in.code_in("/elsewhere?code=c0de&state=51423.abc_-9"), None);
    }

    #[test]
    fn the_key_is_read_from_the_exchange_or_its_absence_said() {
        assert_eq!(key_in(r#"{"key": "sk-or-v1-abc"}"#).as_deref(), Ok("sk-or-v1-abc"));
        assert!(key_in(r#"{"key": ""}"#).is_err());
        assert!(key_in("not json").is_err());
    }
}
