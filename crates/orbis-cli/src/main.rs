mod cli;
mod commands;
mod render;
mod tui;

use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};

use clap::Parser;
use cli::Cli;
use orbis_core::ProviderRegistry;
use render::Renderer;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orbis: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let color =
        !cli.no_color && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = Renderer::new(color);
    let registry = ProviderRegistry::system();

    if tui::should_launch(cli.command.as_ref(), cli.json, cli.plain) {
        return tui::run(&registry, renderer.theme, cli.json);
    }
    commands::dispatch(cli, &registry, &renderer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;
    use clap::CommandFactory;

    #[test]
    fn plain_mode_is_available_as_a_global_escape_hatch() {
        let cli = Cli::try_parse_from(["orbis", "--plain"]).expect("valid args");
        assert!(cli.plain);
    }

    #[test]
    fn dashboard_has_a_short_ui_alias() {
        let cli = Cli::try_parse_from(["orbis", "ui"]).expect("valid args");
        assert!(matches!(cli.command, Some(Command::Dashboard)));
    }

    #[test]
    fn beginner_vocabulary_and_compatibility_aliases_parse() {
        let cases = [
            ("find", "btop"),
            ("search", "btop"),
            ("show", "btop"),
            ("info", "btop"),
            ("explain", "btop"),
            ("update", ""),
            ("updates", ""),
            ("refresh", ""),
            ("health", ""),
            ("doctor", ""),
        ];
        for (command, argument) in cases {
            let args = if argument.is_empty() {
                vec!["orbis", command]
            } else {
                vec!["orbis", command, argument]
            };
            assert!(Cli::try_parse_from(args).is_ok(), "command did not parse: {command}");
        }
        assert!(Cli::try_parse_from(["orbis", "update", "--plan"]).is_ok());
        assert!(Cli::try_parse_from(["orbis", "update", "--apply"]).is_ok());
        assert!(Cli::try_parse_from(["orbis", "update", "--yes"]).is_ok());
        assert!(Cli::try_parse_from(["orbis", "refresh", "--plan"]).is_ok());
        assert!(Cli::try_parse_from(["orbis", "update", "--plan", "--yes"]).is_err());
    }

    #[test]
    fn root_help_leads_with_beginner_vocabulary() {
        let help = Cli::command().render_help().to_string();
        assert!(help.find("COMMON COMMANDS") < help.find("ADVANCED"));
        assert!(help.contains("orbis find firefox"));
        assert!(help.contains("orbis update"));
        assert!(help.contains("refresh    Refresh software information"));
    }
}
