#[cfg(all(windows, feature = "vibestrader-credential"))]
pub fn vibestrader_token() -> Result<String, &'static str> {
    read_token("VibesTrader.Tradier")
}

#[cfg(windows)]
pub fn tradecraft_token() -> Result<String, &'static str> {
    read_token("TradeCraft.Tradier")
}

#[cfg(windows)]
fn read_token(resource: &str) -> Result<String, &'static str> {
    use windows::{Security::Credentials::PasswordVault, core::HSTRING};
    let vault = PasswordVault::new().map_err(|_| "Windows Credential Locker is unavailable")?;
    let credential = vault
        .Retrieve(&HSTRING::from(resource), &HSTRING::from("production"))
        .map_err(|_| "Saved Tradier credential not found")?;
    credential
        .RetrievePassword()
        .map_err(|_| "Could not unlock the saved Tradier credential")?;
    let token = credential
        .Password()
        .map_err(|_| "Could not read the saved Tradier credential")?
        .to_string();
    if token.trim().is_empty() {
        return Err("The saved Tradier credential is empty");
    }
    Ok(token)
}

#[cfg(windows)]
pub fn save_tradecraft_token(token: &str) -> Result<(), &'static str> {
    use windows::{
        Security::Credentials::{PasswordCredential, PasswordVault},
        core::HSTRING,
    };
    let vault = PasswordVault::new().map_err(|_| "Windows Credential Locker is unavailable")?;
    let resource = HSTRING::from("TradeCraft.Tradier");
    let user = HSTRING::from("production");
    let credential =
        PasswordCredential::CreatePasswordCredential(&resource, &user, &HSTRING::from(token))
            .map_err(|_| "Could not prepare credential")?;
    vault
        .Add(&credential)
        .map_err(|_| "Could not save credential in Windows Credential Locker")
}

#[cfg(all(not(windows), feature = "vibestrader-credential"))]
pub fn vibestrader_token() -> Result<String, &'static str> {
    Err("VibesTrader's credential is available on Windows only")
}

#[cfg(target_os = "macos")]
pub fn tradecraft_token() -> Result<String, &'static str> {
    let bytes =
        security_framework::passwords::get_generic_password("TradeCraft.Tradier", "production")
            .map_err(|_| "Could not read the saved Tradier credential from Keychain")?;
    let token = String::from_utf8(bytes).map_err(|_| "Saved Tradier credential is invalid")?;
    if token.trim().is_empty() {
        return Err("The saved Tradier credential is empty");
    }
    Ok(token)
}

#[cfg(target_os = "macos")]
pub fn save_tradecraft_token(token: &str) -> Result<(), &'static str> {
    security_framework::passwords::set_generic_password(
        "TradeCraft.Tradier",
        "production",
        token.as_bytes(),
    )
    .map_err(|_| "Could not save credential in macOS Keychain")
}

/// Description shared by both settings interfaces.
pub fn storage_description() -> &'static str {
    if cfg!(windows) {
        "Stored in Windows Credential Locker."
    } else if cfg!(target_os = "macos") {
        "Stored in macOS Keychain."
    } else if cfg!(target_os = "linux") {
        "Stored in your desktop keyring (Secret Service). Unlock the keyring before saving."
    } else {
        "Credential storage is unavailable on this platform."
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use secret_service::{EncryptionType, blocking::SecretService};
    use std::collections::HashMap;

    pub(super) const SERVICE: &str = "TradeCraft.Tradier";

    fn attributes(service: &str) -> HashMap<&str, &str> {
        HashMap::from([("service", service), ("account", "production")])
    }

    fn connect() -> Result<SecretService<'static>, &'static str> {
        SecretService::connect(EncryptionType::Dh).map_err(|_| {
            "Desktop keyring unavailable. Start a Secret Service keyring (such as GNOME Keyring or KWallet) in your desktop session."
        })
    }

    pub(super) fn read(service: &str) -> Result<String, &'static str> {
        let keyring = connect()?;
        let items = keyring
            .search_items(attributes(service))
            .map_err(|_| "Could not search the desktop keyring")?;
        let item = match items.unlocked.first() {
            Some(item) => item,
            None if !items.locked.is_empty() => {
                return Err("Unlock your desktop keyring and try again");
            }
            None => return Err("Saved Tradier credential not found"),
        };
        let bytes = item
            .get_secret()
            .map_err(|_| "Could not read the saved Tradier credential from the desktop keyring")?;
        let token = String::from_utf8(bytes).map_err(|_| "Saved Tradier credential is invalid")?;
        if token.trim().is_empty() {
            return Err("The saved Tradier credential is empty");
        }
        Ok(token)
    }

    pub(super) fn save(service: &str, token: &str) -> Result<(), &'static str> {
        let token = token.trim();
        if token.is_empty() {
            return Err("Tradier token must not be empty");
        }
        let keyring = connect()?;
        // Never fall back to a session-only collection: tokens must survive logout.
        let collection = keyring
            .get_default_collection()
            .map_err(|_| "No default desktop keyring. Create a default keyring and try again")?;
        collection
            .ensure_unlocked()
            .map_err(|_| "Unlock your default desktop keyring and try again")?;
        collection
            .create_item(
                "TradeCraft Tradier production token",
                attributes(service),
                token.as_bytes(),
                true,
                "text/plain",
            )
            .map_err(|_| "Could not save Tradier credential in the desktop keyring")?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn blank_token_is_rejected_before_contacting_keyring() {
            assert_eq!(
                save("TradeCraft.Test", " \n\t"),
                Err("Tradier token must not be empty")
            );
        }

        #[test]
        #[ignore = "requires an unlocked desktop Secret Service keyring; creates and removes an isolated test item"]
        fn desktop_keyring_round_trip_and_replacement() {
            let service = format!(
                "TradeCraft.Test.{}.{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            assert_eq!(read(&service), Err("Saved Tradier credential not found"));
            save(&service, " dummy-token-one ").unwrap();
            let first = read(&service);
            let replacement = save(&service, "dummy-token-two");
            let second = read(&service);
            let keyring = connect().unwrap();
            let items = keyring.search_items(attributes(&service)).unwrap();
            let count = items.unlocked.len();
            for item in items.unlocked {
                item.delete().unwrap();
            }
            assert_eq!(first.unwrap(), "dummy-token-one");
            replacement.unwrap();
            assert_eq!(second.unwrap(), "dummy-token-two");
            assert_eq!(count, 1, "saving again must replace the existing token");
            assert_eq!(read(&service), Err("Saved Tradier credential not found"));
        }
    }
}

#[cfg(target_os = "linux")]
pub fn tradecraft_token() -> Result<String, &'static str> {
    linux::read(linux::SERVICE)
}

#[cfg(target_os = "linux")]
pub fn save_tradecraft_token(token: &str) -> Result<(), &'static str> {
    linux::save(linux::SERVICE, token)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn tradecraft_token() -> Result<String, &'static str> {
    Err("Credential storage is unavailable on this platform")
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn save_tradecraft_token(_: &str) -> Result<(), &'static str> {
    Err("Credential storage is unavailable on this platform")
}
