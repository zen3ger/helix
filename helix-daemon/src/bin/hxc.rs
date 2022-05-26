use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use std::num::NonZeroU64;

use helix_daemon::client::Client;
use helix_daemon::proto;

fn main() -> Result<()> {
    let exit_code = main_impl()?;
    std::process::exit(exit_code);
}

#[tokio::main]
async fn main_impl() -> Result<i32> {
    let help = format!(
        "\
{} {}
{}
{}

USAGE:
    hxc [FLAGS] [files]...

ARGS:
    <files>...    Sets the input file to use, position can also be specified via file[:row[:col]]

FLAGS:
    -h, --help                  Prints help information
    -V, --version               Prints version information
    -v FILENAME                 Increase logging verbosity each use for up to 3 times
    -f, --force                 Do not wait for clients to quit gracefully
    -s, --stop                  Stops the daemon, and quits all sessions
    -k, --kill [ID|NAME]        Kills the client session
    -a, --attach [ID|NAME]      Attaches to an existing client session
    -A, --alias ID NAME         Sets the name of the client session
    -l, --list                  List client sessions
",
        env!("CARGO_PKG_NAME"),
        env!("VERSION_AND_GIT_HASH"),
        env!("CARGO_PKG_AUTHORS"),
        env!("CARGO_PKG_DESCRIPTION"),
    );

    let args = Args::parse_args().context("could not parse arguments")?;

    if args.display_help {
        print!("{}", help);
        std::process::exit(0);
    }

    if args.display_version {
        println!("helix-client {}", env!("VERSION_AND_GIT_HASH"));
        std::process::exit(0);
    }

    if args.verbosity.is_some() {
        let (verbosity, filename) = args.verbosity.unwrap();
        setup_logging(&filename, verbosity)?;
    }

    let client = Client::connect(proto::addr()).await?;

    if args.do_list {
        let sessions = client.list_sessions().await?;
        if sessions.len() > 0 {
            println!("{:<4} {:<18} {:<32}", "id", "connected", "alias");
            for (sid, ts, alias) in sessions.iter() {
                let dt: DateTime<Local> = ts.clone().into();
                println!(
                    "{:<4} {:<18} {:<32}",
                    sid.0,
                    dt.format("%Y-%m-%d %H:%M"),
                    alias
                )
            }
        }
        std::process::exit(0);
    }

    if let Some(target) = args.do_kill {
        match target {
            Either::This(id) => {
                let sid = proto::SessionId(id.get());
                client.kill_session(sid, args.use_force).await?;
                println!("session {} killed", sid.0);
            }
            Either::That(name) => {
                client
                    .kill_session_by_alias(name.clone(), args.use_force)
                    .await?;
                println!("session {} killed", name);
            }
        }
        std::process::exit(0);
    }

    if args.do_stop {
        client.stop_server(args.use_force).await?;
        println!("server stopped");
        std::process::exit(0);
    }

    if let Some((sid, name)) = args.do_alias {
        let sid = proto::SessionId(sid.get());
        client.alias_session(sid.clone(), name.clone()).await?;
        println!("session {} aliased to {}", sid.0, name);
        std::process::exit(0);
    }

    let mut session = if let Some(target) = args.do_attach {
        match target {
            Either::This(id) => client.attach_session(proto::SessionId(id.get())).await?,
            Either::That(name) => client.attach_session_by_alias(name).await?,
        }
    } else {
        client.start_session().await?
    };

    let exit_code = session.run().await?;

    Ok(exit_code)
}

#[derive(Default)]
struct Args {
    pub display_help: bool,
    pub display_version: bool,
    pub verbosity: Option<(u64, String)>,
    pub use_force: bool,
    pub do_stop: bool,
    pub do_kill: Option<Either<NonZeroU64, proto::Str>>,
    pub do_attach: Option<Either<NonZeroU64, proto::Str>>,
    pub do_alias: Option<(NonZeroU64, proto::Str)>,
    pub do_list: bool,
}

enum Either<A, B> {
    This(A),
    That(B),
}

impl<A, B> Either<A, B> {
    fn unwrap_this(self) -> A {
        if let Either::This(a) = self {
            a
        } else {
            panic!("unwrap_this called on Either::That")
        }
    }

    fn unwrap_that(self) -> B {
        if let Either::That(b) = self {
            b
        } else {
            panic!("unwrap_that called on Either::This")
        }
    }
}

impl Args {
    fn parse_args() -> Result<Args> {
        let mut args = Args::default();
        let mut argv = std::env::args().peekable();

        let parse_id = |id: Option<&String>| {
            if let Some(id) = id {
                id.parse::<NonZeroU64>()
                    .context("session id must be a number")
                    .map(|id| Either::This(id))
            } else {
                anyhow::bail!("expected session id")
            }
        };
        let parse_name = |name: Option<&String>| {
            if let Some(name) = name {
                Ok(Either::That(proto::Str::from(name.as_str())))
            } else {
                anyhow::bail!("expected session name")
            }
        };
        let parse_id_or_name = |input: Option<&String>| {
            if input.is_some() {
                match parse_id(input) {
                    Ok(id) => Ok(id),
                    Err(e) => parse_name(input).context(e),
                }
            } else {
                anyhow::bail!("expected session id or name")
            }
        };

        // TODO: parse filenames to be openned on client start!
        argv.next(); // skip program name
        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "-h" | "--help" => args.display_help = true,
                "-V" | "--version" => args.display_version = true,
                "-f" | "--force" => args.use_force = true,
                "-s" | "--stop" => args.do_stop = true,
                "-l" | "--list" => args.do_list = true,
                "-k" | "--kill" => {
                    let session = parse_id_or_name(argv.peek())?;
                    args.do_kill = Some(session);
                    let _ = argv.next();
                }
                "-a" | "--attach" => {
                    let session = parse_id_or_name(argv.peek())?;
                    args.do_attach = Some(session);
                    let _ = argv.next();
                }
                "-A" | "--alias" => {
                    let session = parse_id(argv.peek())?.unwrap_this();
                    let _ = argv.next();
                    let name = parse_name(argv.peek())?.unwrap_that();
                    let _ = argv.next();
                    args.do_alias = Some((session, name));
                }
                arg if arg.starts_with("-v") => {
                    let mut verbosity = 1;
                    for ch in arg.chars().skip(2) {
                        if ch != 'v' {
                            anyhow::bail!("unexpected short argument `{}`", ch);
                        }
                        verbosity += 1;
                    }
                    match argv.peek() {
                        Some(filename) if !filename.starts_with("-") => {}
                        Some(arg) => {
                            anyhow::bail!("expected log file name, but got `{}`", arg);
                        }
                        None => {
                            anyhow::bail!("expected log file name");
                        }
                    }
                    let mut filename = argv.next().unwrap();
                    if !filename.ends_with(".log") {
                        filename.push_str(".log");
                    }

                    args.verbosity = Some((verbosity, filename));
                }
                arg => anyhow::bail!("unexpected argument `{}`", arg),
            }
        }

        Ok(args)
    }
}

fn setup_logging(filename: &str, verbosity: u64) -> Result<()> {
    let logpath = helix_loader::cache_dir().join(filename);
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
