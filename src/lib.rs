use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct UptickExtension;

impl zed::Extension for UptickExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let path = worktree.which("uptick-lsp").ok_or_else(|| {
            "uptick-lsp was not found on PATH. \
             Install it with `cargo install --path lsp` from the extension repo, \
             or `cargo install --git https://github.com/stevenbarash/uptick-zed uptick-lsp`."
                .to_string()
        })?;

        Ok(Command {
            command: path,
            args: vec![],
            env: vec![],
        })
    }
}

zed::register_extension!(UptickExtension);
