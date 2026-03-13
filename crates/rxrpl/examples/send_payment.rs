use rxrpl::{ClientBuilder, KeyType, Wallet};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::generate(KeyType::Ed25519);
    println!("Sender: {}", wallet.address);

    let mut tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": wallet.address,
        "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
        "Amount": "1000000",
    });

    let client = ClientBuilder::new("https://s.altnet.rippletest.net:51234")
        .build_http()?;

    // Autofill fee, sequence, and last ledger sequence
    rxrpl::protocol::tx::autofill::autofill(&mut tx, &client).await?;

    // Sign and serialize
    let (blob, hash) = wallet.sign_and_serialize(&tx)?;
    println!("TX hash: {hash}");

    // Submit and wait for validation
    let result = client.submit_and_wait(&blob, &hash.to_string(), 30).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}
