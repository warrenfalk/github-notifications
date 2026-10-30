use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};
use toml_edit::DocumentMut;

pub fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "github-notifications")
        .context("Cannot determine configuration directory")?;
    Ok(dirs.config_dir().join("github-notifications.toml"))
}

pub fn load_token() -> Result<String> {
    let environment = match std::env::var("GITHUB_NOTIFICATIONS_TOKEN") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        // Do not include the non-Unicode environment value in the error.
        Err(_) => bail!("GITHUB_NOTIFICATIONS_TOKEN must be valid text"),
    };
    let path = config_path()?;
    resolve_token(
        &path,
        environment.as_deref(),
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        || {
            eprintln!(
                "No GitHub token configured. Enter a classic PAT with notifications scope\n(and repo scope for private repository comments). Input is hidden."
            );
            rpassword::prompt_password("GitHub token: ")
                .context("Could not read token from terminal")
        },
    )
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(DocumentMut::new()),
        Err(error) => return Err(error).with_context(|| format!("Cannot read {}", path.display())),
    };
    // TOML diagnostics normally include source text, which may contain a token.
    contents
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid TOML in {} (contents omitted)", path.display()))
}

fn document_token(document: &DocumentMut) -> Result<Option<String>> {
    let Some(item) = document.get("token") else {
        return Ok(None);
    };
    let token = item
        .as_str()
        .context("The config file's token field must be a string")?;
    if token.trim().is_empty() {
        return Ok(None);
    }
    validate_token(token).map(Some)
}

fn validate_token(token: &str) -> Result<String> {
    let token = token.trim();
    ensure!(
        !token.is_empty(),
        "GitHub token is empty; nothing was saved"
    );
    ensure!(
        token.bytes().all(|byte| byte.is_ascii_graphic()),
        "GitHub token must not contain whitespace, control characters, or non-ASCII text"
    );
    Ok(token.to_owned())
}

fn resolve_token(
    path: &Path,
    environment: Option<&str>,
    interactive: bool,
    prompt: impl FnOnce() -> Result<String>,
) -> Result<String> {
    if let Some(token) = environment.filter(|value| !value.trim().is_empty()) {
        return validate_token(token);
    }
    if let Some(token) = document_token(&read_document(path)?)? {
        return Ok(token);
    }
    ensure!(
        interactive,
        "No GitHub token configured. Set GITHUB_NOTIFICATIONS_TOKEN or run `github-notifications sync` in a terminal to save a token in {}",
        path.display()
    );
    let token = validate_token(&prompt()?)?;
    save_token(path, &token)?;
    eprintln!("Saved GitHub token to {}", path.display());
    Ok(token)
}

fn save_token(path: &Path, token: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("Configuration path has no parent directory")?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .with_context(|| format!("Cannot create {}", parent.display()))?;
    let mut document = read_document(path)?;
    ensure!(
        document_token(&document)?.is_none(),
        "A token was configured while prompting; rerun the command to use it"
    );
    document["token"] = toml_edit::value(token);
    // Write beside the destination and rename atomically. NamedTempFile creates
    // owner-only files on Unix, and persist retains that mode on replacement.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(document.to_string().as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Cannot save {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompted_token_is_saved_and_reused_without_prompting() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory
            .path()
            .join("github-notifications/github-notifications.toml");
        assert_eq!(
            resolve_token(&path, None, true, || Ok("  ghp_example  ".into()))?,
            "ghp_example"
        );
        assert_eq!(
            resolve_token(&path, None, false, || panic!("must not prompt"))?,
            "ghp_example"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())?
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        Ok(())
    }

    #[test]
    fn noninteractive_missing_token_fails_without_prompting_or_creating_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("github-notifications.toml");
        let error = resolve_token(&path, None, false, || panic!("must not prompt")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("run `github-notifications sync` in a terminal")
        );
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn saving_preserves_unrelated_configuration_and_comments() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("github-notifications.toml");
        std::fs::write(&path, "# Local settings\npoll_interval = 90\ntoken = ''\n")?;
        resolve_token(&path, None, true, || Ok("ghp_example".into()))?;
        let document = std::fs::read_to_string(&path)?;
        assert!(document.contains("# Local settings"));
        assert!(document.contains("poll_interval = 90"));
        Ok(())
    }

    #[test]
    fn empty_prompt_does_not_write_configuration() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("github-notifications.toml");
        assert!(resolve_token(&path, None, true, || Ok("  ".into())).is_err());
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn malformed_config_error_does_not_disclose_contents() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("github-notifications.toml");
        std::fs::write(&path, "token = 'ghp_sensitive\n")?;
        let error = resolve_token(&path, None, true, || {
            panic!("must not overwrite malformed config")
        })
        .unwrap_err();
        assert!(!format!("{error:#}").contains("ghp_sensitive"));
        Ok(())
    }

    #[test]
    fn environment_overrides_config_without_saving_or_parsing_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("github-notifications.toml");
        let contents = "token = 'malformed configuration\n";
        std::fs::write(&path, contents)?;
        assert_eq!(
            resolve_token(&path, Some("ghp_override"), false, || panic!(
                "must not prompt"
            ))?,
            "ghp_override"
        );
        assert_eq!(std::fs::read_to_string(&path)?, contents);
        Ok(())
    }
}
