use rxrpl::{KeyType, Wallet};

fn main() {
    let wallet = Wallet::generate(KeyType::Ed25519);
    let seed = wallet.seed_encoded().expect("seed encoding failed");

    println!("Address:    {}", wallet.address);
    println!(
        "Public key: {}",
        hex::encode_upper(wallet.public_key.as_bytes())
    );
    println!("Seed:       {seed}");
    println!("Key type:   {:?}", wallet.key_type);
}
