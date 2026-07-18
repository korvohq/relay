use clap::{Args, Parser, Subcommand};

use crate::{
    config::AppPaths,
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

pub async fn run() -> Result<()> {
    run_with(Cli::parse(), AppPaths::discover()?).await
}

async fn run_with(cli: Cli, paths: AppPaths) -> Result<()> {
    let mut relay = RelayService::load(paths)?;
    match cli.command {
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
}
