use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::prelude::*;
use is_terminal::IsTerminal;
use tokio::net::TcpListener;
use tokio_native_tls::{native_tls, TlsAcceptor};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use controller::*;

fn main() {
    use clap::{
        crate_authors, crate_description, crate_name, crate_version, Arg, ArgAction, Command,
        ValueHint,
    };
    use std::process;
    let cmd = Command::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!())
        .about(crate_description!())
        .subcommand_required(true)
        .subcommands([Command::new("run").about("run controllers").args([
            Arg::new("introspection_address")
                .long("introspection-bind-address")
                .help("address to bind for the HTTP introspection server")
                .default_value("[::]:8089"),
            Arg::new("image")
                .long("image-clair")
                .env("RELATED_IMAGE_CLAIR")
                .help("container image for Clair containers if not specifed in a CRD")
                .default_value(DEFAULT_IMAGE.to_string()),
            Arg::new("leader_elect")
                .long("leader-elect")
                .help("Flag for if leader election is needed. Currently does nothing.")
                .hide(true)
                .action(ArgAction::SetTrue),
            Arg::new("webhook_address")
                .long("webhook-bind-address")
                .help("address to bind for the HTTP webhook server")
                .long_help(concat!(
                    "Address to bind for the HTTP webhook server.\n",
                    "If there's a TLS certificate and key at the files specified by ",
                    "`cert-dir`, `cert-name`, and `key-name` then HTTPS will be served."
                ))
                .default_value("[::]:8080"),
            Arg::new("cert_dir")
                .long("cert-dir")
                .help("directory containing TLS cert+key pair")
                .value_hint(ValueHint::DirPath)
                .default_value(
                    std::env::temp_dir()
                        .join("k8s-webhook-server/serving-certs")
                        .into_os_string(),
                ),
            Arg::new("cert_name")
                .long("cert-name")
                .help("file inside `cert-dir` containing the TLS certificate")
                .default_value("tls.crt"),
            Arg::new("key_name")
                .long("key-name")
                .help("file inside `cert-dir` containing the TLS certificate key")
                .default_value("tls.key"),
            Arg::new("controllers")
                .action(ArgAction::Append)
                .default_values(["clair", "indexer", "matcher"]),
        ])]);

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
    _leader_elect: bool,
    cert_dir: PathBuf,
    cert_name: String,
    controllers: Vec<String>,
    image: String,
    introspection_address: std::net::SocketAddr,
    key_name: String,
    webhook_address: std::net::SocketAddr,
}

impl TryFrom<&clap::ArgMatches> for Args {
    type Error = std::net::AddrParseError;

    fn try_from(m: &clap::ArgMatches) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            image: m.get_one::<String>("image").unwrap().clone(),
            webhook_address: m.get_one::<String>("webhook_address").unwrap().parse()?,
            introspection_address: m
                .get_one::<String>("introspection_address")
                .unwrap()
                .parse()?,
            _leader_elect: m.get_flag("leader_elect"),
            controllers: m
                .get_many::<String>("controllers")
                .unwrap()
                .map(Clone::clone)
                .collect(),
            cert_dir: m.get_one::<String>("cert_dir").unwrap().into(),
            cert_name: m.get_one::<String>("cert_name").unwrap().into(),
            key_name: m.get_one::<String>("key_name").unwrap().into(),
        })
    }
}

impl Args {
    fn context(&self, client: kube::Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            image: self.image.clone(),
        })
    }
}

fn startup(args: Args) -> controller::Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tokio::{runtime, signal};
    use tracing_subscriber::{filter::EnvFilter, prelude::*};

    let env_filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;
    let collector = tracing_subscriber::Registry::default()
        .with(env_filter)
        .with(if std::io::stdout().is_terminal() {
            Some(tracing_subscriber::fmt::layer())
        } else {
            None
        })
        .with(if std::io::stdout().is_terminal() {
            None
        } else {
            Some(tracing_subscriber::fmt::layer().json())
        });
    tracing::subscriber::set_global_default(collector)?;
    let prom = PrometheusBuilder::new().with_http_listener(args.introspection_address);

    let rt = runtime::Builder::new_multi_thread().enable_all().build()?;
    let token = CancellationToken::new();
    rt.handle().spawn(async move {
        if let Err(e) = prom.install() {
            error!("error setting up prometheus endpoint: {e}");
        }
    });
    let ctlstop = token.clone();
    rt.handle().spawn(webhooks(
        args.webhook_address,
        args.cert_dir.join(&args.cert_name),
        args.cert_dir.join(&args.key_name),
        token.clone(),
    ));
    rt.handle().spawn(async move {
        if let Err(err) = signal::ctrl_c().await {
            error!("error reading SIGTERM: {err}");
        }
        token.cancel();
    });
    rt.block_on(run(args, ctlstop))
}

async fn run(args: Args, token: CancellationToken) -> controller::Result<()> {
    use tokio::task;

    let config = kube::Config::infer().await?;
    let client = kube::client::ClientBuilder::try_from(config.clone())?.build();
    // TODO(hank) Will eventually need to use the more manual construction of controllers to make
    // sure the caches are used optimally.

    info!(image = args.image, "default image set");
    info!("setup done, starting controllers");
    let ctx = args.context(client);
    let mut ctrls = task::JoinSet::new();
    for name in &args.controllers {
        let fut = match name.to_lowercase().as_str() {
            "clair" | "clairs" => clairs::controller(token.clone(), ctx.clone())?,
            "indexer" | "indexers" => indexers::controller(token.clone(), ctx.clone())?,
            "matcher" | "matchers" => matchers::controller(token.clone(), ctx.clone())?,
            "notifier" | "notifiers" => todo!(),
            "updater" | "updaters" => todo!(),
            other => {
                warn!(name = other, "unrecognized controller name, skipping");
                continue;
            }
        };
        ctrls.spawn(fut);
    }
    while let Some(res) = ctrls.join_next().await {
        match res {
            Err(e) => error!("error starting controller: {e}"),
            Ok(res) => {
                if let Err(e) = res {
                    error!("error from controller: {e}");
                    token.cancel();
                }
            }
        };
    }
    Ok(())
}

async fn webhooks<A, Pa, Pb>(
    addr: A,
    certfile: Pa,
    keyfile: Pb,
    cancel: CancellationToken,
) -> controller::Result<()>
where
    A: Into<SocketAddr>,
    Pa: AsRef<Path>,
    Pb: AsRef<Path>,
{
    use axum::Server;
    use hyper::server::accept;

    use webhook::State;

    let certfile = certfile.as_ref();
    let keyfile = keyfile.as_ref();
    let addr = addr.into();

    let client = kube::Client::try_default().await?;
    let app = webhook::app(State::new(client));
    let l = TcpListenerStream::new(TcpListener::bind(addr).await?).map_err(Error::from);
    info!(%addr, "started webhook server");
    // I can't figure out how to name the listener type such that it's either
    // TryStream<TcpStream> or TryStream<TlsStream<TcpStream>>.
    if certfile.exists() && keyfile.exists() {
        let (cert, key) = tokio::join!(tokio::fs::read(certfile), tokio::fs::read(keyfile));
        let id = native_tls::Identity::from_pkcs8(&cert?, &key?)?;
        let acceptor = TlsAcceptor::from(native_tls::TlsAcceptor::new(id)?);
        let l = l
            .map_ok(|s| (s, acceptor.clone()))
            .and_then(|(s, a)| async move { a.accept(s).await.map_err(Error::from) });
        Server::builder(accept::from_stream(l))
            .serve(app.into_make_service())
            .with_graceful_shutdown(cancel.cancelled_owned())
            .await
    } else {
        Server::builder(accept::from_stream(l))
            .serve(app.into_make_service())
            .with_graceful_shutdown(cancel.cancelled_owned())
            .await
    }
    .map_err(Error::from)
}
