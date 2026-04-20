use anyhow::Context;
use config::AppConfig;
use ingest::IngestPipeline;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_env().context("failed to load application config")?;
    let runtime = IngestPipeline::from_config(&config.ingest)
        .context("failed to build ingest pipeline")?
        .spawn();
    let (mut event_receiver, mut runtime_event_receiver, stream_tasks) = runtime.into_parts();

    loop {
        tokio::select! {
            event = event_receiver.recv() => {
                match event {
                    Some(event) => println!("{}", serde_json::to_string(&event)?),
                    None => break,
                }
            }
            runtime_event = runtime_event_receiver.recv() => {
                match runtime_event {
                    Some(event) => eprintln!("{}", serde_json::to_string(&event)?),
                    None => break,
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to listen for ctrl-c")?;
                for task in &stream_tasks {
                    task.abort();
                }
                break;
            }
        }
    }

    Ok(())
}
