use std::{error::Error, io::Write, path::PathBuf, time};

use base64::{engine::general_purpose, Engine};
use eyre::Context;
use scrypt_ocl::{ProviderId, Scrypter};

use clap::{Args, Parser, Subcommand};

/// Initialize labels on GPU
#[derive(Parser)]
#[command(author, version, about, long_about = None, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[clap(flatten)]
    initialize: Initialize,
}

#[derive(Subcommand)]
enum Commands {
    /// does testing things
    Initialize(Initialize),
    ListProviders,
}

#[derive(Args)]
struct Initialize {
    /// Scrypt N parameter
    #[arg(short, long, default_value_t = 8192)]
    n: usize,

    /// Number of labels to initialize
    #[arg(short, long, default_value_t = 20480 * 30)]
    labels: usize,

    /// Base64-encoded node ID
    #[arg(long, default_value = "hBGTHs44tav7YR87sRVafuzZwObCZnK1Z/exYpxwqSQ=")]
    node_id: String,

    /// Base64-encoded commitment ATX ID
    #[arg(long, default_value = "ZuxocVjIYWfv7A/K1Lmm8+mNsHzAZaWVpbl5+KINx+I=")]
    commitment_atx_id: String,

    /// Path to output file
    #[arg(long, default_value = "labels.bin")]
    output: PathBuf,

    /// Provider ID to use
    /// Use `initializer list-providers` to list available providers.
    /// If not specified, the first available provider will be used.
    #[arg(long)]
    provider: Option<u32>,
}

fn initialize(
    n: usize,
    labels: usize,
    node_id: String,
    commitment_atx_id: String,
    output: PathBuf,
    provider_id: Option<ProviderId>,
) -> eyre::Result<()> {
    println!("Initializing {labels} labels intos {:?}", output.as_path());

    let node_id = general_purpose::STANDARD.decode(node_id)?;
    let commitment_atx_id = general_purpose::STANDARD.decode(commitment_atx_id)?;

    let commitment = post::initialize::calc_commitment(
        node_id
            .as_slice()
            .try_into()
            .wrap_err("nodeID should be 32B")?,
        commitment_atx_id
            .as_slice()
            .try_into()
            .wrap_err("commitment ATX ID should be 32B")?,
    );

    let mut scrypter = Scrypter::new(provider_id, n, &commitment, Some([0xFFu8; 32]))?;
    let mut out_labels = vec![0u8; labels * 16];

    let now = time::Instant::now();
    let vrf_nonce = scrypter.scrypt(0..labels as u64, &mut out_labels)?;
    let elapsed = now.elapsed();
    println!(
            "Initializing {} labels took {} seconds. Speed: {:.0} labels/sec ({:.2} MB/sec, vrf_nonce: {vrf_nonce:?})",
            labels,
            elapsed.as_secs(),
            labels as f64 / elapsed.as_secs_f64(),
            labels as f64 * 16.0 / elapsed.as_secs_f64() / 1024.0 / 1024.0
        );

    let mut file = std::fs::File::create(output)?;
    file.write_all(&out_labels)?;
    Ok(())
}

fn list_providers() -> eyre::Result<()> {
    let providers = scrypt_ocl::get_providers()?;
    println!("Found {} providers", providers.len());
    for (id, provider) in providers.iter().enumerate() {
        println!("{id}: {provider}");
    }
    Ok(())
}

fn main() -> eyre::Result<()> {
    let args = Cli::parse();

    match args
        .command
        .unwrap_or(Commands::Initialize(args.initialize))
    {
        Commands::Initialize(Initialize {
            n,
            labels,
            node_id,
            commitment_atx_id,
            output,
            provider,
        }) => initialize(
            n,
            labels,
            node_id,
            commitment_atx_id,
            output,
            provider.map(ProviderId),
        )?,
        Commands::ListProviders => list_providers()?,
    }

    Ok(())
}
