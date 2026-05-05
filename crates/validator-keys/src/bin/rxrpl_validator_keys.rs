//! `rxrpl-validator-keys`: rippled-compatible validator keychain CLI.
//!
//! Subcommands:
//!   - `generate`: create a fresh master keypair, write `validator_keys.json`.
//!   - `show`:     display the persisted public key, fingerprint, key type.
//!   - `create-token` / `rotate`: emit a signed manifest binding the master
//!     key to a freshly generated ephemeral key, bumping `token_sequence`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use rxrpl_validator_keys::{
    encode_node_public_key, generate_manifest, generate_master, KeyType, ValidatorKeysFile,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliKeyType {
    Ed25519,
    Secp256k1,
}

impl From<CliKeyType> for KeyType {
    fn from(value: CliKeyType) -> Self {
        match value {
            CliKeyType::Ed25519 => KeyType::Ed25519,
            CliKeyType::Secp256k1 => KeyType::Secp256k1,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "rxrpl-validator-keys",
    about = "rxrpl validator keychain (rippled-compatible)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Generate a fresh master keypair and write validator_keys.json.
    Generate {
        #[arg(long, value_enum, default_value_t = CliKeyType::Ed25519)]
        key_type: CliKeyType,
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
        /// Overwrite an existing keys file at the destination.
        #[arg(long)]
        force: bool,
    },
    /// Display the public key, fingerprint, and key type from a keys file.
    Show {
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
    },
    /// Generate a new ephemeral key, sign a manifest, and bump token_sequence.
    CreateToken {
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
        /// Optional domain claim included in the manifest (sfDomain).
        #[arg(long)]
        domain: Option<String>,
        /// Key type for the freshly minted ephemeral key.
        #[arg(long, value_enum, default_value_t = CliKeyType::Ed25519)]
        ephemeral_key_type: CliKeyType,
    },
}

fn fingerprint(public_key_hex: &str) -> String {
    public_key_hex
        .as_bytes()
        .chunks(2)
        .take(20)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(":")
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Generate {
            key_type,
            output_dir,
            force,
        } => {
            let path = ValidatorKeysFile::default_path(&output_dir);
            if path.exists() && !force {
                return Err(format!(
                    "{} already exists; pass --force to overwrite",
                    path.display()
                )
                .into());
            }
            let (_seed, _kp, file) = generate_master(key_type.into())?;
            file.save(&path)?;
            println!("wrote {}", path.display());
            println!("key_type:    {}", file.key_type);
            println!("public_key:  {}", file.public_key);
            Ok(())
        }
        Cmd::Show { output_dir } => {
            let path = ValidatorKeysFile::default_path(&output_dir);
            let file = ValidatorKeysFile::load(&path)?;
            let kp = file.master_keypair()?;
            let hex_pk = hex::encode_upper(kp.public_key.as_bytes());
            println!("path:           {}", path.display());
            println!("key_type:       {}", file.key_type);
            println!("public_key:     {}", file.public_key);
            println!("public_key_hex: {hex_pk}");
            println!("fingerprint:    {}", fingerprint(&hex_pk));
            println!("revoked:        {}", file.revoked);
            println!("token_sequence: {}", file.token_sequence);
            Ok(())
        }
        Cmd::CreateToken {
            output_dir,
            domain,
            ephemeral_key_type,
        } => {
            let path = ValidatorKeysFile::default_path(&output_dir);
            let mut file = ValidatorKeysFile::load(&path)?;
            if file.revoked {
                return Err("master key has been revoked; cannot mint new tokens".into());
            }
            let next_seq = file
                .token_sequence
                .checked_add(1)
                .ok_or("token_sequence overflow")?;
            let master_kp = file.master_keypair()?;
            let manifest = generate_manifest(
                &master_kp,
                next_seq,
                ephemeral_key_type.into(),
                domain.as_deref(),
            )?;
            file.token_sequence = next_seq;
            file.save(&path)?;

            let _ = encode_node_public_key; // re-export anchor

            println!("sequence:              {}", manifest.sequence);
            println!("master_public_key:     {}", manifest.master_public_key);
            println!("ephemeral_public_key:  {}", manifest.ephemeral_public_key);
            println!("ephemeral_secret_key:  {}", manifest.ephemeral_secret_key);
            if let Some(d) = &manifest.domain {
                println!("domain:                {d}");
            }
            println!();
            println!("[validator_token]");
            println!("{}", manifest.manifest_hex);
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
