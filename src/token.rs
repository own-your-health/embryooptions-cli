//! The Embryo Options access token: a JWT the clinic's web app keeps in
//! `localStorage.accessToken` and sends as `Authorization: Bearer` to the
//! member API. embryooptions reads its claims locally, from the
//! base64url payload, without checking the signature (the API does that):
//! the member id the API's paths carry, the clinic's id, and when the token
//! expires (docs/api.md, *bundle*).
//!
//! Nothing here prints or quotes a token: every error names what was wrong
//! with it, never its value.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use pk_cli_core::CliError;
use pk_cli_secrets::Secret;
use serde_json::Value;

/// What a sign-in hands over: the access token, and the refresh token the
/// web app keeps beside it (stored, not yet used: docs/api.md).
pub struct Tokens {
    pub access: Secret,
    pub refresh: Option<Secret>,
}

/// The claims embryooptions reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    /// The `user` claim's `UserId`: the `{memberId}` of the API's paths.
    /// `None` for a sign-in that has not finished the site's registration
    /// step (an `intermediateUser` token).
    pub member_id: Option<String>,
    /// The `user` claim's `IVFCenterId`: the clinic's id, the
    /// `ivfCenterId` query of the member read.
    pub ivf_center_id: Option<String>,
    /// The `user` claim's `MemberType`, as the token gives it.
    pub member_type: Option<String>,
    /// Whether the token carries `intermediateUser` and no `user`.
    pub intermediate: bool,
    /// `exp`, Unix seconds.
    pub expires_at: Option<i64>,
}

impl Claims {
    /// Whether the token has expired at `now` (Unix seconds), with 30 s to
    /// spare for the request still to be made.
    pub fn expired_at(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|exp| exp <= now + 30)
    }
}

fn bad(why: &str) -> CliError {
    CliError::Usage(format!("the access token is not an Embryo Options token: {why}"))
}

/// The claims of `token`, a JWT (`header.payload.signature`, base64url).
pub fn claims(token: &str) -> Result<Claims, CliError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(bad("it is not three base64url parts joined by dots"));
    }
    let payload = URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
        .map_err(|_| bad("its payload is not base64url"))?;
    let payload: Value = serde_json::from_slice(&payload).map_err(|_| bad("its payload is not JSON"))?;
    let object = |name: &str| -> Result<Option<Value>, CliError> {
        match payload.get(name) {
            None | Some(Value::Null) => Ok(None),
            // The web app's `JSON.parse(e.user)`: the claim is a JSON string.
            Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                .map(Some)
                .map_err(|_| bad(&format!("its `{name}` claim is not JSON"))),
            Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
            Some(_) => Err(bad(&format!("its `{name}` claim is not an object"))),
        }
    };
    let user = object("user")?;
    let intermediate = user.is_none() && object("intermediateUser")?.is_some();
    let field = |name: &str| -> Result<Option<String>, CliError> {
        let Some(user) = &user else { return Ok(None) };
        match user.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => id_of(v)
                .map(Some)
                .ok_or_else(|| bad(&format!("its `user.{name}` is not an id"))),
        }
    };
    let member_id = field("UserId")?;
    if user.is_some() && member_id.is_none() {
        return Err(bad("its `user` claim has no `UserId`"));
    }
    let expires_at = match payload.get("exp") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().ok_or_else(|| bad("its `exp` is not a number"))?),
    };
    Ok(Claims {
        member_id,
        ivf_center_id: field("IVFCenterId")?,
        member_type: field("MemberType")?,
        intermediate,
        expires_at,
    })
}

/// An id as it may go into a path or a query: a number, or 1 to 64 ASCII
/// letters, digits and `-`.
fn id_of(v: &Value) -> Option<String> {
    let s = match v {
        Value::Number(n) if n.is_u64() || n.is_i64() => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    is_id(&s).then_some(s)
}

/// 1 to 64 ASCII letters, digits and `-`.
pub fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// What an `auth login --stdin` read, or what the sign-in window's
/// expression answered: `{"accessToken": "...", "refreshToken": "..."}`, or
/// the access token alone. The access token's claims must read. The errors
/// never quote what was read.
pub fn tokens_from(raw: &Secret) -> Result<Tokens, CliError> {
    let text = raw.expose().trim();
    if text.is_empty() {
        return Err(CliError::Usage("the tokens read are empty".into()));
    }
    let (access, refresh) = if text.starts_with('{') {
        let v: Value = serde_json::from_str(text)
            .map_err(|_| CliError::Usage("what was read starts with `{` and is not a JSON object".into()))?;
        let read = |name: &str| -> Result<Option<String>, CliError> {
            match v.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
                Some(_) => Err(CliError::Usage(format!("`{name}` is not a string"))),
            }
        };
        let access =
            read("accessToken")?.ok_or_else(|| CliError::Usage("the JSON read has no `accessToken`".into()))?;
        (access, read("refreshToken")?)
    } else {
        (text.to_string(), None)
    };
    if access.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(CliError::Usage(
            "the access token holds a space or a control character".into(),
        ));
    }
    if refresh
        .as_deref()
        .is_some_and(|r| r.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(CliError::Usage(
            "the refresh token holds a space or a control character".into(),
        ));
    }
    claims(&access)?;
    Ok(Tokens {
        access: Secret::new(access),
        refresh: refresh.map(Secret::new),
    })
}

/// A JWT with `payload` and a dummy header and signature, for tests.
#[doc(hidden)]
pub fn test_token(payload: &Value) -> String {
    let part = |v: &Value| URL_SAFE_NO_PAD.encode(v.to_string());
    format!(
        "{}.{}.{}",
        part(&serde_json::json!({ "alg": "HS256", "typ": "JWT" })),
        part(payload),
        URL_SAFE_NO_PAD.encode("dummy-signature")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(extra: Value) -> Value {
        let mut user = json!({ "UserId": 4242, "IVFCenterId": 7, "MemberType": 1 });
        if let (Value::Object(u), Value::Object(e)) = (&mut user, extra) {
            u.extend(e);
        }
        user
    }

    #[test]
    fn the_claims_are_read_from_the_payload() {
        let token = test_token(&json!({ "user": user(json!({})).to_string(), "exp": 2_000_000_000, "jti": "x" }));
        let c = claims(&token).unwrap();
        assert_eq!(c.member_id.as_deref(), Some("4242"));
        assert_eq!(c.ivf_center_id.as_deref(), Some("7"));
        assert_eq!(c.member_type.as_deref(), Some("1"));
        assert!(!c.intermediate);
        assert_eq!(c.expires_at, Some(2_000_000_000));
        assert!(!c.expired_at(1_999_999_000));
        assert!(c.expired_at(1_999_999_980), "30 s to spare");

        // The claim as an object, and string ids.
        let token = test_token(&json!({ "user": { "UserId": "ab-12", "IVFCenterId": "9" } }));
        let c = claims(&token).unwrap();
        assert_eq!(c.member_id.as_deref(), Some("ab-12"));
        assert_eq!(c.expires_at, None);
        assert!(!c.expired_at(i64::MAX - 100));
    }

    #[test]
    fn a_registration_step_token_has_no_member() {
        let token = test_token(&json!({ "intermediateUser": json!({ "UserId": 5 }).to_string(), "exp": 1 }));
        let c = claims(&token).unwrap();
        assert!(c.intermediate);
        assert_eq!(c.member_id, None);
    }

    #[test]
    fn a_token_that_does_not_read_is_named_not_quoted() {
        let good = test_token(&json!({ "user": user(json!({})) }));
        let parts: Vec<&str> = good.split('.').collect();
        for bad_token in [
            "dummy_secret".to_string(),
            "a.b".to_string(),
            format!("{}..{}", parts[0], parts[2]),
            format!("{}.!!!.{}", parts[0], parts[2]),
            format!("{}.{}.{}", parts[0], URL_SAFE_NO_PAD.encode("dummy_secret"), parts[2]),
            test_token(&json!({ "user": "dummy_secret{" })),
            test_token(&json!({ "user": { "IVFCenterId": 1 } })),
            test_token(&json!({ "user": { "UserId": "a/b" } })),
            test_token(&json!({ "user": { "UserId": 1.5 } })),
            test_token(&json!({ "user": { "UserId": 1 }, "exp": "soon" })),
        ] {
            let e = claims(&bad_token).err().unwrap();
            assert_eq!(e.exit_code(), 2, "{bad_token}");
            assert!(!e.to_string().contains("dummy_secret"), "{e}");
        }
    }

    #[test]
    fn tokens_are_read_as_json_or_alone() {
        let access = test_token(&json!({ "user": user(json!({})) }));
        let json_in = Secret::new(format!(
            "  {{\"accessToken\":\"{access}\",\"refreshToken\":\"dummy-refresh\"}}\n"
        ));
        let t = tokens_from(&json_in).unwrap();
        assert_eq!(t.access.expose(), access);
        assert_eq!(t.refresh.as_ref().map(|r| r.expose()), Some("dummy-refresh"));

        let t = tokens_from(&Secret::new(format!("{access}\n"))).unwrap();
        assert_eq!(t.access.expose(), access);
        assert!(t.refresh.is_none());

        let t = tokens_from(&Secret::new(format!(
            "{{\"accessToken\":\"{access}\",\"refreshToken\":null}}"
        )))
        .unwrap();
        assert!(t.refresh.is_none());

        for bad_in in [
            "".to_string(),
            "  \n".to_string(),
            "{\"refreshToken\":\"dummy_secret\"}".to_string(),
            "{dummy_secret".to_string(),
            "{\"accessToken\":5}".to_string(),
            "dummy_secret".to_string(),
            format!("{access} {access}"),
            format!("{{\"accessToken\":\"{access}\",\"refreshToken\":\"dummy secret\"}}"),
        ] {
            let e = tokens_from(&Secret::new(bad_in.clone())).err().unwrap();
            assert_eq!(e.exit_code(), 2, "{bad_in}");
            assert!(!e.to_string().contains("dummy"), "{e}");
            assert!(!e.to_string().contains(&access), "{e}");
        }
    }
}
