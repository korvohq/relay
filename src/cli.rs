// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

use clap::{Args, Parser, Subcommand};
use zeroize::Zeroizing;

use crate::{
    config::AppPaths,
    credentials::{validate_provider, validate_secret},
    error::{RelayError, Result},
    ledger::{Period, UsageSummary},
    pricing::format_usd,
    service::RelayService,
};

#[derive(Debug, Parser)]
#[command(
    name = "relay",
    version,
    about = "Visible, capped, and routed AI coding calls"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Store provider API keys in the operating system credential vault.
    Onboard {
        /// Provider to configure; repeat to configure several. Defaults to all configured providers.
        #[arg(long = "provider")]
        providers: Vec<String>,
    },
    /// Inspect or delete credentials without revealing their values.
    Credentials {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    /// Send a metered request after a worst-case cap check.
    Ask(AskArgs),
    /// Show today's and this month's local Relay usage.
    Usage,
    /// Inspect or explicitly update hard spend caps.
    Cap {
        #[command(subcommand)]
        command: CapCommand,
    },
    /// List configured aliases and external price-catalog models.
    Models,
}

#[derive(Debug, Args)]
struct AskArgs {
    /// Prompt to send to the selected provider.
    prompt: String,

    /// Configured alias or canonical provider:model key.
    #[arg(long)]
    model: Option<String>,

    /// Use the configured `think` model alias.
    #[arg(long, conflicts_with = "model")]
    think: bool,

    /// Maximum output tokens included in the pre-flight reservation.
    #[arg(long, default_value_t = 1024)]
    max_output_tokens: u32,

    /// Reserved interface; v0.1 rejects streaming before transport.
    #[arg(long)]
    stream: bool,
}

#[derive(Debug, Subcommand)]
enum CapCommand {
    /// Set one or both hard caps in USD; no environment override exists.
    Set {
        #[arg(long)]
        daily_usd: Option<String>,
        #[arg(long)]
        monthly_usd: Option<String>,
    },
    /// Show the configured hard caps and accounting timezone.
    Show,
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    /// Show whether each provider uses the OS vault, an environment fallback, or neither.
    Status,
    /// Delete one provider credential from the OS vault.
    Delete {
        /// Configured provider name, for example `openai` or `anthropic`.
        provider: String,
    },
}

pub async fn run() -> Result<()> {
    run_with(Cli::parse(), AppPaths::discover()?).await
}

async fn run_with(cli: Cli, paths: AppPaths) -> Result<()> {
    let mut relay = RelayService::load(paths)?;
    match cli.command {
        Command::Onboard { providers } => onboard(&relay, providers)?,
        Command::Credentials { command } => match command {
            CredentialCommand::Status => print_credential_status(&relay)?,
            CredentialCommand::Delete { provider } => {
                ensure_configured_provider(&relay, &provider)?;
                if relay.credentials.delete(&provider)? {
                    println!("Deleted {provider} credential from the OS vault.");
                } else {
                    println!("No OS-vault credential was stored for {provider}.");
                }
                let variable = relay.config.credential_environment_variable(&provider)?;
                if std::env::var_os(variable).is_some() {
                    println!(
                        "Note: {variable} is still set and remains available as a CI/headless fallback."
                    );
                }
            }
        },
        Command::Ask(args) => {
            if args.stream {
                return Err(RelayError::StreamingUnsupported);
            }
            let call = relay
                .ask(
                    args.prompt,
                    args.model.as_deref(),
                    args.think,
                    Some(args.max_output_tokens),
                )
                .await?;
            println!("{}", call.response.text);
            println!(
                "\n[relay] model={} tokens={}+{} latency={}ms cost={}",
                call.canonical_model,
                call.response.usage.input_tokens,
                call.response.usage.output_tokens,
                call.response.latency_ms,
                format_usd(call.cost_microusd),
            );
            if let Some(warning) = call.warning {
                eprintln!("[relay] {warning}");
            }
        }
        Command::Usage => {
            print_usage("Today", relay.usage(Period::Day)?);
            println!();
            print_usage("This month", relay.usage(Period::Month)?);
        }
        Command::Cap { command } => match command {
            CapCommand::Set {
                daily_usd,
                monthly_usd,
            } => {
                relay.update_caps(daily_usd, monthly_usd)?;
                print_caps(&relay);
            }
            CapCommand::Show => print_caps(&relay),
        },
        Command::Models => {
            println!("Aliases:");
            for (alias, model) in &relay.config.models {
                println!("  {alias:<12} {model}");
            }
            println!("\nPrice catalog (paid calls require verified=yes):");
            for (model, price) in relay.prices.entries() {
                println!(
                    "  {model:<32} verified={} input={}/M output={}/M context={} api_model={}",
                    if price.verified { "yes" } else { "no" },
                    format_usd_decimal(price.input_per_mtok),
                    format_usd_decimal(price.output_per_mtok),
                    price.max_context,
                    price.api_model,
                );
            }
        }
    }
    Ok(())
}

fn onboard(relay: &RelayService, requested: Vec<String>) -> Result<()> {
    let providers: Vec<String> = if requested.is_empty() {
        relay.config.providers().map(str::to_owned).collect()
    } else {
        let mut providers = Vec::new();
        for provider in requested {
            ensure_configured_provider(relay, &provider)?;
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        }
        providers
    };

    println!("Relay secure onboarding");
    println!("Keys are entered with terminal echo disabled and stored in the OS credential vault.");
    println!("Press Enter without a key to skip a provider.\n");

    let mut stored = 0_u32;
    for provider in providers {
        validate_provider(&provider)?;
        let existing = relay.credentials.exists(&provider)?;
        let action = if existing { "replace" } else { "set" };
        let prompt = format!("Paste {provider} API key to {action} it (hidden): ");
        let secret = Zeroizing::new(rpassword::prompt_password(prompt)?);
        if secret.is_empty() {
            println!("Skipped {provider}.");
            continue;
        }
        validate_secret(secret.as_str())?;
        relay.credentials.set(&provider, secret.as_str())?;
        stored += 1;
        println!("Stored {provider} credential in the OS vault.");
    }
    println!("\nOnboarding complete: {stored} credential(s) stored.");
    println!(
        "Run `./target/release/relay credentials status` to check configuration without displaying keys."
    );
    Ok(())
}

fn print_credential_status(relay: &RelayService) -> Result<()> {
    for provider in relay.config.providers() {
        let variable = relay.config.credential_environment_variable(provider)?;
        let environment_present = std::env::var_os(variable).is_some();
        match relay.credentials.exists(provider) {
            Ok(true) => println!("{provider}: stored in OS vault"),
            Ok(false) if environment_present => {
                println!("{provider}: using {variable} environment fallback")
            }
            Ok(false) => println!("{provider}: not configured"),
            Err(error) if environment_present => println!(
                "{provider}: OS vault unavailable ({error}); using {variable} environment fallback"
            ),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn ensure_configured_provider(relay: &RelayService, provider: &str) -> Result<()> {
    validate_provider(provider)?;
    relay.config.credential_environment_variable(provider)?;
    Ok(())
}

fn print_usage(label: &str, usage: UsageSummary) {
    println!(
        "{label}: {} calls, {} input tokens, {} output tokens, {}",
        usage.calls,
        usage.tokens_in,
        usage.tokens_out,
        format_usd(usage.total_microusd),
    );
    for (model, model_usage) in usage.by_model {
        println!(
            "  {model}: {} calls, {}+{} tokens, {}",
            model_usage.calls,
            model_usage.tokens_in,
            model_usage.tokens_out,
            format_usd(model_usage.cost_microusd),
        );
    }
    if usage.pending_microusd > 0 {
        println!(
            "  pending reconciliation: {} reserved",
            format_usd(usage.pending_microusd)
        );
    }
}

fn print_caps(relay: &RelayService) {
    println!(
        "daily={} monthly={} timezone={}",
        relay.config.caps.daily_usd, relay.config.caps.monthly_usd, relay.config.caps.timezone,
    );
}

fn format_usd_decimal(amount: rust_decimal::Decimal) -> String {
    format!("${amount}")
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_documented_ask_command() {
        let cli = Cli::try_parse_from(["relay", "ask", "explain this", "--model", "think"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn model_and_think_are_mutually_exclusive() {
        let cli = Cli::try_parse_from(["relay", "ask", "test", "--model", "default", "--think"]);
        assert!(cli.is_err());
    }

    #[test]
    fn parses_secure_onboarding_without_a_key_argument() {
        let cli = Cli::try_parse_from([
            "relay",
            "onboard",
            "--provider",
            "openai",
            "--provider",
            "anthropic",
        ]);
        assert!(cli.is_ok());
        assert!(
            Cli::try_parse_from([
                "relay",
                "onboard",
                "--provider",
                "openai",
                "secret-must-not-be-an-argument",
            ])
            .is_err()
        );
    }
}
