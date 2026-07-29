use std::path::PathBuf;

use github_copilot_sdk::{CliProgram, Client, ClientOptions};

#[tokio::main]
async fn main() {
    let use_logged_in_user = std::env::args().nth(1).as_deref() == Some("true");
    let options = ClientOptions::new()
        .with_program(CliProgram::Path(PathBuf::from("copilot")))
        .with_cwd(std::env::temp_dir())
        .with_use_logged_in_user(use_logged_in_user);
    let client = match Client::start(options).await {
        Ok(client) => client,
        Err(error) => {
            eprintln!("Copilot read-only client start failed: {error}");
            std::process::exit(1);
        }
    };
    let models = match client.list_models().await {
        Ok(models) => models,
        Err(error) => {
            let _ = client.stop().await;
            eprintln!("Copilot read-only model discovery failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = client.stop().await {
        eprintln!("Copilot read-only client shutdown failed: {error}");
        std::process::exit(1);
    }
    println!(
        "Copilot read-only model discovery passed ({} models)",
        models.len()
    );
}
