//! Shared classification of GitHub users, bots, and unavailable identities.
use super::graphql_fetch::string;
use anyhow::{Result, ensure};
use serde_json::Value;

pub(super) enum Actor {
    Human(String),
    Bot,
    Unknown,
}
pub(super) fn actor(value: &Value) -> Result<Actor> {
    if value.is_null() {
        return Ok(Actor::Unknown);
    }
    let login = string(value, "login")?.to_lowercase();
    ensure!(!login.trim().is_empty(), "Empty actor login");
    Ok(match string(value, "__typename")? {
        "Bot" => Actor::Bot,
        "User"
            if login.ends_with("[bot]")
                || [
                    "dependabot",
                    "dependabot-preview",
                    "renovate",
                    "github-actions",
                ]
                .contains(&login.as_str()) =>
        {
            Actor::Bot
        }
        "User" => Actor::Human(login),
        _ => Actor::Unknown,
    })
}
