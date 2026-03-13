use rxrpl::ClientBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new("wss://s1.ripple.com:443")
        .build_ws()
        .await?;

    let result = client
        .subscribe(vec!["ledger".to_string()])
        .await?;
    println!("Subscribed: {}", serde_json::to_string_pretty(&result)?);

    if let Some(mut stream) = client.subscription_stream() {
        loop {
            match stream.next().await {
                Ok(event) => {
                    println!("{}", serde_json::to_string_pretty(&event)?);
                }
                Err(e) => {
                    eprintln!("Stream error: {e}");
                    break;
                }
            }
        }
    }

    Ok(())
}
