//! Stores the API token in the login Keychain. The token never touches disk
//! anywhere else, and nothing in the app logs it.

use std::sync::Once;

use anyhow::{Context, Result};
use keyring_core::{Entry, Error};

use crate::cloudflare::Token;

const SERVICE: &str = "dev.cloudflare-gui.api-token";
const USER: &str = "default";

fn entry() -> Result<Entry> {
    static INIT: Once = Once::new();
    INIT.call_once(
        || match apple_native_keyring_store::keychain::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => log::error!("couldn't open the login keychain: {e}"),
        },
    );
    Entry::new(SERVICE, USER).context("couldn't open the keychain entry")
}

pub fn load_token() -> Result<Option<Token>> {
    match entry()?.get_password() {
        Ok(secret) if !secret.trim().is_empty() => Ok(Some(Token::new(secret))),
        Ok(_) | Err(Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("couldn't read the API token from the keychain"),
    }
}

pub fn save_token(token: &Token) -> Result<()> {
    entry()?
        .set_password(token.expose())
        .context("couldn't save the API token to the keychain")
}

pub fn delete_token() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("couldn't remove the API token from the keychain"),
    }
}
