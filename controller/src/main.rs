use tracing::{error, info};

use controller::*;

fn main() {
    use clap::{
        crate_authors, crate_description, crate_name, crate_version, Arg, ArgAction, Command,
    };
    use std::process;
    let image_default = format!("quay.io/projectquay/clair:{}", env!("DEFAULT_CLAIR_TAG"));
    let cmd = Command::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!())
        .about(crate_description!())
        .subcommand_required(true)
        .args([
            Arg::new("health_address")
                .long("health-probe-bind-address")
                .help("tk")
                .default_value(":8081"),
            Arg::new("image")
                .long("image-clair")
                .env("RELATED_IMAGE_CLAIR")
                .help("tk")
                .default_value(image_default),
            Arg::new("leader_elect")
                .long("leader-elect")
                .help("tk")
                .action(ArgAction::SetTrue),
            Arg::new("metrics_address")
                .long("metrics-bind-address")
                .help("tk")
                .default_value(":8080"),
        ])
        .subcommands([Command::new("run").about("run controller")]);

    if let Err(e) = match cmd.get_matches().subcommand() {
        Some(("run", m)) => match Args::try_from(m) {
            Ok(args) => startup(args),
            Err(e) => Err(Error::from(e)),
        },
        _ => unreachable!(),
    } {
        eprintln!("{e}");
        process::exit(1);
    }
}

struct Args {
    health_address: std::net::SocketAddr,
    image: String,
    leader_elect: bool,
    metrics_address: std::net::SocketAddr,
}

impl TryFrom<&clap::ArgMatches> for Args {
    type Error = std::net::AddrParseError;

    fn try_from(m: &clap::ArgMatches) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            image: m.get_one::<&String>("image").unwrap().to_string(),
            health_address: m.get_one::<&String>("health_address").unwrap().parse()?,
            metrics_address: m.get_one::<&String>("metrics_address").unwrap().parse()?,
            leader_elect: m.get_flag("leader_elect"),
        })
    }
}

fn startup(args: Args) -> controller::Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tokio::runtime;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::prelude::*;

    let logger = tracing_subscriber::fmt::layer().json();
    let env_filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;
    let collector = tracing_subscriber::Registry::default()
        .with(logger)
        .with(env_filter);
    tracing::subscriber::set_global_default(collector)?;
    let prom = PrometheusBuilder::new().with_http_listener(args.metrics_address);

    let rt = runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.handle().spawn(async move {
        if let Err(e) = prom.install() {
            error!("error setting up prometheus endpoint: {e}");
        }
    });
    rt.block_on(run(args.image))
}

async fn run(_img: String) -> controller::Result<()> {
    use kube::Client;
    use tokio::task;

    let client = Client::try_default().await?;

    info!("setup done, starting controllers");
    let mut ctrls = task::JoinSet::new();
    clairs::controller(&mut ctrls, client.clone());
    updaters::controller(&mut ctrls, client.clone());
    while let Some(res) = ctrls.join_next().await {
        match res {
            Err(e) => error!("error starting controller: {e}"),
            Ok(res) => {
                if let Err(e) = res {
                    error!("error from controller: {e}");
                }
            }
        };
    }
    Ok(())
}
