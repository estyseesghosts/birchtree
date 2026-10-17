//! Minimal server-rendered login/consent pages for the OAuth authorize flow.
//!
//! This project has no application frontend (see `birchtree.md` §6.3). The
//! browser-based authorization-code flow still needs an authenticated session
//! and a consent decision, so these functions render just enough HTML. No
//! client-side JavaScript is used.

/// Escape text for safe interpolation into HTML text/attribute context.
pub fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn hidden_fields(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                escape(k),
                escape(v)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title></head><body>{}</body></html>\n",
        escape(title),
        body
    )
}

/// Login form. `fields` are the OAuth parameters that must survive the login
/// round-trip.
pub fn login_page(fields: &[(String, String)], error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!("<p class=\"error\">{}</p>", escape(e)))
        .unwrap_or_default();
    let body = format!(
        "<main><h1>Sign in</h1>{error_html}\
         <form method=\"post\" action=\"/oauth/authorize\">\
         <label>Username <input type=\"text\" name=\"username\" autocomplete=\"username\"></label>\
         <label>Password <input type=\"password\" name=\"password\" autocomplete=\"current-password\"></label>\
         {hidden}\
         <button type=\"submit\">Sign in</button>\
         </form></main>",
        error_html = error_html,
        hidden = hidden_fields(fields),
    );
    document("Sign in", &body)
}

/// Consent page: the user is authenticated and approves or denies the app.
pub fn consent_page(
    fields: &[(String, String)],
    username: &str,
    app_name: &str,
    scopes: &[String],
) -> String {
    let scope_list = scopes
        .iter()
        .map(|s| format!("<li>{}</li>", escape(s)))
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        "<main><h1>Authorize {app}</h1>\
         <p>Signed in as <strong>{user}</strong>.</p>\
         <p>{app} is requesting access to:</p><ul>{scopes}</ul>\
         <form method=\"post\" action=\"/oauth/authorize\">\
         {hidden}\
         <input type=\"hidden\" name=\"decision\" value=\"authorize\">\
         <button type=\"submit\">Authorize</button>\
         </form>\
         <form method=\"post\" action=\"/oauth/authorize\">\
         {hidden_deny}\
         <input type=\"hidden\" name=\"decision\" value=\"deny\">\
         <button type=\"submit\">Deny</button>\
         </form></main>",
        app = escape(app_name),
        user = escape(username),
        scopes = scope_list,
        hidden = hidden_fields(fields),
        hidden_deny = hidden_fields(fields),
    );
    document(&format!("Authorize {app_name}"), &body)
}

/// A simple error page for invalid authorize requests.
pub fn error_page(message: &str) -> String {
    document(
        "Authorization error",
        &format!("<main><h1>Authorization error</h1><p>{}</p></main>", escape(message)),
    )
}
