//! Open an SFTP connection for a profile, prompting for the password here in the
//! CLI (the core never prompts).

use mcmove_core::config::Profile;
use mcmove_core::sftp::{Auth, Sftp};

pub async fn connect(profile: &Profile) -> anyhow::Result<Sftp> {
    let auth = if profile.key_path.is_empty() {
        let pw = rpassword::prompt_password(format!("Panel password for {}: ", profile.username))?;
        Auth::Password(pw)
    } else {
        Auth::KeyFile(profile.key_path.clone())
    };
    Ok(Sftp::connect(profile, auth).await?)
}
