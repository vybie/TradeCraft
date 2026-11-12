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

#[cfg(not(windows))]
pub fn tradecraft_token() -> Result<String, &'static str> {
    Err("Credential storage is available on Windows only")
}
#[cfg(not(windows))]
pub fn save_tradecraft_token(_: &str) -> Result<(), &'static str> {
    Err("Credential storage is available on Windows only")
}
