//! Administrative access to volatile reviews. No secret command-line values,
//! terminal echo, ordinary messages, automatic resubmission or printed answers.
use crate::client::Client;
use agentdocker_core::{
    Request, Response,
    secret::{PROVIDER_NOTICE, SecretAnswers},
};
use anyhow::{Result, anyhow, bail, ensure};
use std::io::{IsTerminal, Read};

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(clap::Subcommand)]
enum Command {
    /// List only metadata for the person's pending temporary reviews.
    List,
    /// Cancel one temporary review without submitting an answer.
    Cancel { review: String },
    /// Submit a complete JSON answer object from a pipe; values are never arguments.
    Answer {
        review: String,
        /// Codex/model may retain or repeat the answers, including in ordinary output.
        #[arg(long)]
        acknowledge_provider_retention: bool,
    },
}

pub async fn run(client: &Client, args: Args) -> Result<()> {
    let Response::Agent { agent } = client.call(&Request::Me { workdir: None }).await? else {
        bail!("the human recipient is unavailable");
    };
    match args.command {
        Command::List => {
            let Response::SecretReviews { reviews } = client
                .call(&Request::SecretReviews {
                    recipient: agent.id.to_string(),
                })
                .await?
            else {
                bail!("this daemon does not support temporary reviews");
            };
            println!("{}", serde_json::to_string_pretty(&reviews)?);
        }
        Command::Cancel { review } => {
            ensure!(
                matches!(
                    client
                        .call(&Request::CancelSecretReview {
                            from: agent.id.to_string(),
                            review
                        })
                        .await?,
                    Response::Ok
                ),
                "review cancellation was not confirmed"
            );
            println!("Temporary review closed; an answer already taken cannot be recalled.");
        }
        Command::Answer {
            review,
            acknowledge_provider_retention,
        } => {
            ensure!(
                acknowledge_provider_retention,
                "{PROVIDER_NOTICE} Pass --acknowledge-provider-retention to submit."
            );
            ensure!(
                !std::io::stdin().is_terminal(),
                "Use masked entry in the app. This command accepts a JSON answer object through a pipe only."
            );
            let mut bytes = Vec::new();
            std::io::stdin().take(524_289).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 524_288,
                "secret answer input exceeds its wire bound"
            );
            let answers: SecretAnswers = serde_json::from_slice(&bytes).map_err(|_| {
                anyhow!("secret answers require a bounded JSON object of text values")
            })?;
            drop(bytes);
            ensure!(
                matches!(
                    client
                        .call(&Request::AnswerSecretReview {
                            from: agent.id.to_string(),
                            review,
                            answers,
                            retention_acknowledged: true
                        })
                        .await?,
                    Response::Ok
                ),
                "secret submission was not confirmed; do not resend automatically"
            );
            println!("Temporary answer submitted; this is not a provider receipt.");
        }
    }
    Ok(())
}
