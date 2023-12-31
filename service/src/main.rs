use std::{fs::read_to_string, path::PathBuf, time::Duration};

use clap::{Args, Parser, ValueEnum};
use eyre::Context;
use sysinfo::{Pid, ProcessExt, ProcessStatus, System, SystemExt};
use tokio::sync::oneshot::{self, error::TryRecvError, Receiver};
use tonic::transport::{Certificate, Identity};

use post::pow::randomx::RandomXFlag;
use post_service::client;

/// Post Service
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// directory of POST data
    #[arg(short, long)]
    dir: PathBuf,
    /// address to connect to
    #[arg(short, long)]
    address: String,
    /// time to wait before reconnecting to the node
    #[arg(long, default_value = "5", value_parser = |secs: &str| secs.parse().map(Duration::from_secs))]
    reconnect_interval_s: Duration,
    /// Maximum number of retries to connect to the node
    /// The default is infinite.
    #[arg(long)]
    max_retries: Option<usize>,

    #[command(flatten, next_help_heading = "POST configuration")]
    post_config: PostConfig,

    #[command(flatten, next_help_heading = "POST settings")]
    post_settings: PostSettings,

    #[command(flatten, next_help_heading = "TLS configuration")]
    tls: Option<Tls>,

    /// watch PID and exit if it dies
    #[arg(long)]
    watch_pid: Option<sysinfo::Pid>,
}

#[derive(Args, Debug)]
/// POST configuration - network parameters
struct PostConfig {
    /// The minimal number of units that must be initialized.
    #[arg(long, default_value_t = 4)]
    pub min_num_units: u32,
    /// The maximal number of units that can be initialized.
    #[arg(long, default_value_t = u32::MAX)]
    pub max_num_units: u32,
    ///  The number of labels per unit.
    #[arg(long, default_value_t = 4294967296)]
    pub labels_per_unit: u64,
    /// K1 specifies the difficulty for a label to be a candidate for a proof
    #[arg(long, default_value_t = 26)]
    k1: u32,
    /// K2 is the number of labels below the required difficulty required for a proof
    #[arg(long, default_value_t = 37)]
    k2: u32,
    /// K3 is the size of the subset of proof indices that is validated
    #[arg(long, default_value_t = 37)]
    k3: u32,
    /// difficulty for the nonce proof of work (aka "k2pow")
    #[arg(
        long,
        default_value = "000dfb23b0979b4b000000000000000000000000000000000000000000000000",
        value_parser(parse_difficulty)
    )]
    pow_difficulty: [u8; 32],
    /// scrypt parameters for initialization
    #[command(flatten)]
    scrypt: ScryptParams,
}

/// Scrypt parameters for initialization
#[derive(Args, Debug)]
struct ScryptParams {
    /// scrypt N parameter
    #[arg(short, default_value_t = 8192)]
    n: usize,
    /// scrypt R parameter
    #[arg(short, default_value_t = 1)]
    r: usize,
    /// scrypt P parameter
    #[arg(short, default_value_t = 1)]
    p: usize,
}

#[derive(Args, Debug)]
/// POST proof generation settings
struct PostSettings {
    /// number of threads to use
    /// '0' means use all available threads
    #[arg(long, default_value_t = 1)]
    threads: usize,
    /// number of nonces to attempt in single pass over POS data
    ///
    /// Each group of 16 nonces requires a separate PoW. Must be a multiple of 16.
    ///
    /// Higher value gives a better chance to find a proof within less passes over the POS data,
    /// but also slows down the process.
    #[arg(long, default_value_t = 128, value_parser(parse_nonces))]
    nonces: usize,
    /// modes of operation for RandomX
    #[arg(long, default_value_t = RandomXMode::Fast)]
    randomx_mode: RandomXMode,
}

/// RandomX modes of operation
///
/// They are interchangeable as they give the same results but have different
/// purpose and memory requirements.
#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
enum RandomXMode {
    /// Fast mode for proving. Requires 2080 MiB of memory.
    Fast,
    /// Light mode for verification. Requires only 256 MiB of memory, but runs significantly slower
    Light,
}

/// TLS configuration
///
/// Either all fields must be specified or none
#[derive(Args, Debug, Clone)]
#[group(required = false)]
pub struct Tls {
    /// CA certificate
    #[arg(long, required = false)]
    pub ca_cert: PathBuf,
    #[arg(long, required = false)]
    pub cert: PathBuf,
    #[arg(long, required = false)]
    pub key: PathBuf,
    /// domain name to verify the certificate of server against
    /// defaults to server hostname
    #[arg(long)]
    pub domain: Option<String>,
}

impl std::fmt::Display for RandomXMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

impl From<RandomXMode> for RandomXFlag {
    fn from(val: RandomXMode) -> Self {
        match val {
            RandomXMode::Fast => RandomXFlag::get_recommended_flags() | RandomXFlag::FLAG_FULL_MEM,
            RandomXMode::Light => RandomXFlag::get_recommended_flags(),
        }
    }
}

fn parse_nonces(arg: &str) -> eyre::Result<usize> {
    let nonces = arg.parse()?;
    eyre::ensure!(nonces % 16 == 0, "nonces must be multiple of 16");
    eyre::ensure!(nonces / 16 <= 256, format!("max nonces is {}", 256 * 16));
    Ok(nonces)
}

fn parse_difficulty(arg: &str) -> eyre::Result<[u8; 32]> {
    hex::decode(arg)?
        .as_slice()
        .try_into()
        .wrap_err("invalid difficulty length")
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let args = Cli::parse();

    let env = env_logger::Env::default().filter_or("RUST_LOG", "info");
    env_logger::init_from_env(env);

    log::info!("POST network parameters: {:?}", args.post_config);
    log::info!("POST proving settings: {:?}", args.post_settings);

    let scrypt = post::config::ScryptParams::new(
        args.post_config.scrypt.n,
        args.post_config.scrypt.r,
        args.post_config.scrypt.p,
    );
    let service = post_service::service::PostService::new(
        args.dir,
        post::config::ProofConfig {
            k1: args.post_config.k1,
            k2: args.post_config.k2,
            k3: args.post_config.k3,
            pow_difficulty: args.post_config.pow_difficulty,
        },
        post::config::InitConfig {
            min_num_units: args.post_config.min_num_units,
            max_num_units: args.post_config.max_num_units,
            labels_per_unit: args.post_config.labels_per_unit,
            scrypt,
        },
        args.post_settings.nonces,
        args.post_settings.threads,
        args.post_settings.randomx_mode.into(),
    )
    .wrap_err("creating Post Service")?;

    let tls = if let Some(tls) = args.tls {
        log::info!(
            "configuring TLS: server: (CA cert: {}, domain: {:?}), client: (cert: {}, key: {})",
            tls.ca_cert.display(),
            tls.domain,
            tls.cert.display(),
            tls.key.display(),
        );
        let server_ca_cert = read_to_string(tls.ca_cert)?;
        let cert = read_to_string(tls.cert)?;
        let key = read_to_string(tls.key)?;
        Some((
            tls.domain,
            Certificate::from_pem(server_ca_cert),
            Identity::from_pem(cert, key),
        ))
    } else {
        log::info!("not configuring TLS");
        None
    };

    let client = client::ServiceClient::new(args.address, tls, service)?;
    let client_handle = tokio::spawn(client.run(args.max_retries, args.reconnect_interval_s));

    // A channel to communicate when the blocking task should quit.
    let (term_tx, term_rx) = oneshot::channel();

    tokio::select! {
        Some(err) = watch_pid_if_needed(args.watch_pid.map(|p| (p, term_rx))) => {
            log::info!("PID watcher exited: {err:?}");
            return Ok(())
        }
        err = client_handle => {
            drop(term_tx);
            return err.unwrap();
        }
    }
}

async fn watch_pid_if_needed(
    watch: Option<(Pid, Receiver<()>)>,
) -> Option<std::result::Result<(), tokio::task::JoinError>> {
    match watch {
        Some((pid, term)) => Some(
            tokio::task::spawn_blocking(move || watch_pid(pid, Duration::from_secs(1), term)).await,
        ),
        None => None,
    }
}

// watch given PID and return when it dies
fn watch_pid(pid: Pid, interval: Duration, mut term: Receiver<()>) {
    log::info!("watching PID {pid}");

    let mut sys = System::new();
    while sys.refresh_process(pid) {
        if let Some(p) = sys.process(pid) {
            match p.status() {
                ProcessStatus::Zombie | ProcessStatus::Dead => {
                    log::info!("PID {pid} died");
                    return;
                }
                _ => {}
            }
        }
        match term.try_recv() {
            Ok(_) | Err(TryRecvError::Closed) => {
                log::debug!("PID watcher received termination signal");
                return;
            }
            _ => std::thread::sleep(interval),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use sysinfo::{Pid, PidExt};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn watch_pid_if_needed() {
        // Don't watch
        assert!(super::watch_pid_if_needed(None).await.is_none());
        // Watch
        let (_term_tx, term_rx) = oneshot::channel();
        super::watch_pid_if_needed(Some((Pid::from(0), term_rx)))
            .await
            .expect("should be some")
            .expect("should be OK");
    }

    #[tokio::test]
    async fn watching_pid_zombie() {
        let mut proc = Command::new("sleep").arg("99999").spawn().unwrap();
        let pid = proc.id();
        let (_term_tx, term_rx) = oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            super::watch_pid(
                sysinfo::Pid::from_u32(pid),
                std::time::Duration::from_millis(10),
                term_rx,
            )
        });
        // just kill leaves a zombie process
        proc.kill().unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn watching_pid_reaped() {
        let mut proc = Command::new("sleep").arg("99999").spawn().unwrap();
        let pid = proc.id();
        let (_term_tx, term_rx) = oneshot::channel();

        let handle = tokio::task::spawn_blocking(move || {
            super::watch_pid(
                sysinfo::Pid::from_u32(pid),
                std::time::Duration::from_millis(10),
                term_rx,
            )
        });

        // kill and wait
        proc.kill().unwrap();
        proc.wait().unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn terminate_watching_pid() {
        let mut proc = Command::new("sleep").arg("99999").spawn().unwrap();
        let pid = proc.id();
        let (term_tx, term_rx) = oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            super::watch_pid(
                sysinfo::Pid::from_u32(pid),
                std::time::Duration::from_millis(10),
                term_rx,
            )
        });
        // Terminate by closing the channel
        drop(term_tx);
        handle.await.unwrap();

        let (term_tx, term_rx) = oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            super::watch_pid(
                sysinfo::Pid::from_u32(pid),
                std::time::Duration::from_millis(10),
                term_rx,
            )
        });
        // Terminate by sending a signal
        term_tx.send(()).unwrap();
        handle.await.unwrap();

        proc.kill().unwrap();
        proc.wait().unwrap();
    }
}
