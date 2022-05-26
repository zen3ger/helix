use anyhow::{Context, Result};
use std::path::PathBuf;

use helix_daemon::proto;
use helix_daemon::server::Server;

fn main() -> Result<()> {
    let exit_code = main_impl()?;
    std::process::exit(exit_code);
}

#[tokio::main]
async fn main_impl() -> Result<i32> {
    let logpath = log_file();
    let parent = logpath.parent().unwrap();
    if !parent.exists() {
        std::fs::create_dir_all(parent).ok();
    }

    let help = format!(
        "\
{} {}
{}
{}

USAGE:
    hxd [FLAGS]

FLAGS:
    -h, --help        Prints help information
    -v                Increase logging verbosity each use for up to 3 times
                      (default file: {})
    -V, --version     Prints version information
        
    For more details see `hxc --help`.
",
        env!("CARGO_PKG_NAME"),
        env!("VERSION_AND_GIT_HASH"),
        env!("CARGO_PKG_AUTHORS"),
        env!("CARGO_PKG_DESCRIPTION"),
        logpath.display(),
    );

    let args = Args::parse_args().context("could not parse arguments")?;

    if args.display_help {
        print!("{}", help);
        std::process::exit(0);
    }

    if args.display_version {
        println!("helix-daemon {}", env!("VERSION_AND_GIT_HASH"));
        std::process::exit(0);
    }

    setup_logging(logpath, args.verbosity).context("failed to initialize logging")?;

    let mut server = Server::new(proto::addr()).context("failed to start server")?;

    let exit_code = server.run().await?;

    Ok(exit_code)
}

#[derive(Default)]
struct Args {
    pub display_help: bool,
    pub display_version: bool,
    pub verbosity: u64,
}

impl Args {
    fn parse_args() -> Result<Args> {
        let mut args = Args::default();
        let mut argv = std::env::args();

        argv.next(); // skip program name
        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "-V" | "--version" => args.display_version = true,
                "-h" | "--help" => args.display_help = true,
                arg if arg.starts_with("-v") => {
                    let mut verbosity = 1;
                    for ch in arg.chars().skip(2) {
                        if ch != 'v' {
                            anyhow::bail!("unexpected short argument `{}``", ch);
                        }
                        verbosity += 1;
                    }
                    args.verbosity = verbosity;
                }
                _ => anyhow::bail!("unknown argument `{}``", arg),
            }
        }
        Ok(args)
    }
}

fn log_file() -> std::path::PathBuf {
    helix_loader::cache_dir().join("helix-daemon.log")
}

fn setup_logging(logpath: PathBuf, verbosity: u64) -> Result<()> {
    let mut base_config = fern::Dispatch::new();

    base_config = match verbosity {
        0 => base_config.level(log::LevelFilter::Warn),
        1 => base_config.level(log::LevelFilter::Info),
        2 => base_config.level(log::LevelFilter::Debug),
        _3_or_more => base_config.level(log::LevelFilter::Trace),
    };

    // Separate file config so we can include year, month and day in file logs
    let file_config = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
                record.target(),
                record.level(),
                message
            ))
        })
        .chain(fern::log_file(logpath)?);

    base_config.chain(file_config).apply()?;

    Ok(())
}
