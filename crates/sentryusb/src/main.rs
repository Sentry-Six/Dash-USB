// The system allocator supports both the 4 KiB and 16 KiB page sizes used by
// supported Pi kernels; page-size-specific allocators can abort at startup.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use tower_http::CompressionLevel;
use tower_http::compression::{
    CompressionLayer,
    predicate::{NotForContentType, Predicate, SizeAbove},
};
use tracing::info;

mod embed;
mod migrate;

#[derive(Parser)]
#[command(name = "dashusb", about = "Dash USB server")]
struct Args {
    /// HTTP server port (only used when no subcommand is given)
    #[arg(short, long, default_value_t = 8788)]
    port: u16,

    /// Development mode (don't serve embedded static files)
    #[arg(long)]
    dev: bool,

    /// Optional command used by the installed gadget and archive wrappers.
    /// Without one, the HTTP server runs.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// USB gadget control (configfs + UDC bind/unbind).
    Gadget {
        #[command(subcommand)]
        action: GadgetAction,
    },
    /// Cam-disk snapshot management (reflink-backed).
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,
    },
    /// Free-space management on `/backingfiles`.
    Space {
        #[command(subcommand)]
        action: SpaceAction,
    },
}

#[derive(Subcommand)]
enum GadgetAction {
    /// Attach the USB mass-storage gadget + bind the UDC.
    Enable {
        /// Ignored. The `/root/bin/enable_gadget.sh` shim splats `"$@"`,
        /// so callers can pass through unused args.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Unbind the UDC + tear down the configfs hierarchy.
    Disable {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SnapshotAction {
    /// Create a new reflink snapshot of `/backingfiles/cam_disk.bin`.
    Make {
        /// Accepted but ignored for wrapper CLI compatibility.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Release (delete) an existing snapshot by name (`snap-NNNNNN`).
    Release {
        /// Snapshot name passed through by the `release_snapshot.sh` wrapper.
        name: String,
    },
}

#[derive(Subcommand)]
enum SpaceAction {
    /// Delete old snapshots until `/backingfiles` has enough free space.
    Manage {
        /// Reserve in bytes (archiveloop passes its 10GB+3% figure);
        /// omitted = same formula computed here.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    // Record startup phases so journal logs expose delays before the UDC bind.
    let t0 = std::time::Instant::now();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dashusb=info,sentryusb_api=info,tower_http=info".into()),
        )
        .init();

    macro_rules! phase {
        ($name:expr) => {
            info!(boot_phase = $name, elapsed_ms = t0.elapsed().as_millis() as u64);
        };
    }
    phase!("tracing_initialized");

    let args = Args::parse();
    phase!("args_parsed");

    // Installed wrappers require a synchronous exit status.
    if let Some(cmd) = args.command {
        std::process::exit(run_subcommand(cmd).await);
    }

    info!("DashUSB server starting on port {}", args.port);

    tokio::spawn(async {
        migrate::run_startup_migration().await;
    });

    // Retry unresolved automatic timezones without blocking startup.
    tokio::spawn(async {
        sentryusb_setup::system::ensure_timezone_resolved().await;
    });

    // Return cached glibc heap pages after burst workloads.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    tokio::spawn(async {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        tick.tick().await; // skip the first immediate tick
        loop {
            tick.tick().await;
            // SAFETY: glibc documents malloc_trim as thread-safe; this runs in
            // a task, not a signal handler.
            unsafe { libc::malloc_trim(0); }
        }
    });

    // Keep archiveloop's generated profile environment current across updates.
    sentryusb_vehicle_profile::write_profile_env();

    let auth = sentryusb_api::init_auth();
    phase!("auth_initialized");

    let hub = sentryusb_ws::Hub::new();

    phase!("processor_initialized");

    let app_state = sentryusb_api::router::AppState {
        hub: hub.clone(),
        auth: auth.clone(),
        net_sampler: Arc::new(Mutex::new(HashMap::new())),
    };

    // Resume setup if it was interrupted by a reboot (e.g. dwc2 overlay, root shrink)
    sentryusb_api::setup::auto_resume_setup(hub.clone());

    // The once-per-install beacon has no fingerprint or identifier; update
    // telemetry is separately opt-in.
    sentryusb_api::update::spawn_install_beacon();

    // Run the opt-in boot repair check for an unmounted /backingfiles.
    sentryusb_api::storage_repair::spawn_boot_check(hub.clone());
    phase!("startup_tasks_spawned");

    let mut app = sentryusb_api::build_router(app_state.clone());

    // Serve recording video files via the bind mount of
    // /mutable/Recordings at /var/www/html/Recordings.
    app = app.nest_service(
        "/Recordings",
        tower_http::services::ServeDir::new("/var/www/html/Recordings"),
    );

    if !args.dev {
        app = app.fallback(embed::spa_handler);
        info!("Serving embedded static files");
    } else {
        info!("Running in development mode (no static file serving)");
    }

    // Apply after registering routes because Router::layer wraps only existing
    // routes. Skip compressed/streaming media, avoid sub-1 KiB CPU overhead,
    // and use Brotli quality 6 to keep Pi response latency bounded.
    let compression = CompressionLayer::new()
        .br(true)
        .gzip(true)
        .deflate(true)
        .quality(CompressionLevel::Precise(6))
        .compress_when(
        SizeAbove::new(1024)
            .and(NotForContentType::new("video/"))
            .and(NotForContentType::new("audio/"))
            .and(NotForContentType::new("image/"))
            .and(NotForContentType::new("application/octet-stream"))
            .and(NotForContentType::new("application/zip"))
            .and(NotForContentType::new("application/grpc"))
            .and(NotForContentType::new("text/event-stream")),
    );
    app = app.layer(compression);

    app = app.layer(axum::middleware::from_fn_with_state(
        auth,
        sentryusb_api::auth::auth_middleware,
    ));
    phase!("router_built");

    let addr = std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, args.port));
    info!("DashUSB server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind address");
    phase!("listener_bound");

    info!(
        boot_phase = "ready",
        elapsed_total_ms = t0.elapsed().as_millis() as u64,
        "DashUSB ready to serve requests",
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("server error");
}

async fn shutdown_signal() {
    // systemd uses SIGTERM; interactive sessions use Ctrl+C.
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
    info!("Shutdown signal received, draining connections...");
}

/// Dispatch a wrapper command, returning its shell exit status and writing
/// failures to stderr for archiveloop diagnostics.
async fn run_subcommand(cmd: Command) -> i32 {
    match cmd {
        Command::Gadget { action } => run_gadget(action).await,
        Command::Snapshot { action } => run_snapshot(action).await,
        Command::Space { action } => run_space(action).await,
    }
}

async fn run_gadget(action: GadgetAction) -> i32 {
    // configfs operations are synchronous and may block during UDC retries.
    let result = match action {
        GadgetAction::Enable { .. } => {
            tokio::task::spawn_blocking(sentryusb_gadget::enable).await
        }
        GadgetAction::Disable { .. } => {
            tokio::task::spawn_blocking(sentryusb_gadget::disable).await
        }
    };
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            eprintln!("gadget: {}", e);
            1
        }
        Err(e) => {
            eprintln!("gadget task panicked: {}", e);
            1
        }
    }
}

async fn run_snapshot(action: SnapshotAction) -> i32 {
    match action {
        SnapshotAction::Make { args } => {
            // Only an explicit `nofsck` from archiveloop skips the check.
            let skip_fsck = args.iter().any(|a| a.eq_ignore_ascii_case("nofsck"));
            match sentryusb_gadget::snapshot::make_snapshot(skip_fsck).await {
                Ok(Some(name)) => {
                    println!("{}", name);
                    0
                }
                Ok(None) => {
                    // Empty stdout tells the wrapper no snapshot was retained.
                    0
                }
                Err(e) => {
                    eprintln!("snapshot make: {}", e);
                    1
                }
            }
        }
        SnapshotAction::Release { name } => {
            match sentryusb_gadget::snapshot::release_snapshot(&name).await {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("snapshot release {}: {}", name, e);
                    1
                }
            }
        }
    }
}

async fn run_space(action: SpaceAction) -> i32 {
    match action {
        SpaceAction::Manage { args } => {
            // Reject invalid explicit reserves instead of diverging from
            // archiveloop's space policy.
            let reserve = match args.first().map(|a| (a, a.parse::<u64>())) {
                None => Ok(None),
                Some((_, Ok(v))) => Ok(Some(v)),
                Some((a, Err(e))) => Err(format!("invalid reserve {a:?} (expected bytes): {e}")),
            };
            match reserve {
                Err(msg) => {
                    eprintln!("space manage: {msg}");
                    2
                }
                Ok(reserve) => match sentryusb_gadget::space::manage_free_space(reserve).await {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("space manage: {}", e);
                        1
                    }
                },
            }
        }
    }
}
