// The system allocator is used so the binary works on every Pi kernel
// regardless of page size (Pi 5 / Bookworm uses 16 KB pages while older
// Pis use 4 KB pages). A page-size-specific allocator like jemalloc
// aborts at startup when its compiled-in page size doesn't match the
// kernel's, which is why we don't use one here.

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

    /// Optional subcommand. Without one, the HTTP server runs.
    ///
    /// Subcommands are invoked by the `/root/bin/{make,release}_snapshot.sh`,
    /// `enable/disable_gadget.sh`, and `manage_free_space.sh` wrappers
    /// installed by the setup wizard. archiveloop calls those wrappers
    /// every cycle, so these subcommands keep the archive flow alive.
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
        /// Reserved for future compat (e.g. `nofsck`); ignored for now.
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
    // Boot-phase timer. Lets us attribute the gap between systemd
    // "Started dashusb.service" and the UDC bind in the journal.
    // Each `phase!` call emits `boot_phase=NAME elapsed_ms=N` so it's
    // greppable: `journalctl -b -u dashusb.service | grep boot_phase`.
    let t0 = std::time::Instant::now();

    // Initialize tracing
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

    // The /root/bin/ wrappers expect subcommands to run to completion
    // synchronously and exit with a status code.
    if let Some(cmd) = args.command {
        std::process::exit(run_subcommand(cmd).await);
    }

    info!("DashUSB server starting on port {}", args.port);

    // Run startup migration in background
    tokio::spawn(async {
        migrate::run_startup_migration().await;
    });

    // Boot-time timezone safety net: if setup left TIME_ZONE=auto unresolved
    // (no network during setup → Pi stuck on UTC → drive telemetry mis-links),
    // re-resolve once now. Non-blocking; no-op once a real zone is set.
    tokio::spawn(async {
        sentryusb_setup::system::ensure_timezone_resolved().await;
    });

    // Periodic malloc_trim releases heap pages glibc would otherwise keep
    // cached in its per-arena free lists. With MALLOC_ARENA_MAX=2 from the
    // systemd unit, this keeps RSS bounded across burst workloads like clip
    // ingest. No-op on non-glibc targets.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    tokio::spawn(async {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        tick.tick().await; // skip the first immediate tick
        loop {
            tick.tick().await;
            // SAFETY: malloc_trim is thread-safe (takes the arena mutex
            // internally per glibc docs) and we call it from a tokio task,
            // never a signal handler. Returns 1 if memory was released, 0
            // if not.
            unsafe { libc::malloc_trim(0); }
        }
    });

    // Publish the active vehicle profile to the bash side (archiveloop
    // sources /root/bin/profile_env.sh). Rewritten only when content
    // differs, so OTA updates propagate profile changes without a
    // setup re-run.
    sentryusb_vehicle_profile::write_profile_env();

    // Initialize auth
    let auth = sentryusb_api::init_auth();
    phase!("auth_initialized");

    // WebSocket hub
    let hub = sentryusb_ws::Hub::new();

    phase!("processor_initialized");

    let app_state = sentryusb_api::router::AppState {
        hub: hub.clone(),
        auth: auth.clone(),
        net_sampler: Arc::new(Mutex::new(HashMap::new())),
    };

    // Resume setup if it was interrupted by a reboot (e.g. dwc2 overlay, root shrink)
    sentryusb_api::setup::auto_resume_setup(hub.clone());

    // Fire the anonymous install beacon once per install (gated by
    // /mutable/.beaconed). No fingerprint and no identifier: just an
    // incrementing counter on the support server. Opted-in update-check
    // telemetry is separate, in check_for_update().
    sentryusb_api::update::spawn_install_beacon();

    // Boot-time storage auto repair (opt-in via the storage_auto_repair
    // preference). Detects a /backingfiles that failed to mount at boot
    // and runs the guarded xfs_repair ladder; see api::storage_repair.
    sentryusb_api::storage_repair::spawn_boot_check(hub.clone());
    phase!("startup_tasks_spawned");

    // Build the API router
    let mut app = sentryusb_api::build_router(app_state.clone());

    // Serve recording video files via the bind mount of
    // /mutable/Recordings at /var/www/html/Recordings.
    app = app.nest_service(
        "/Recordings",
        tower_http::services::ServeDir::new("/var/www/html/Recordings"),
    );

    // Static file serving with SPA fallback (unless dev mode)
    if !args.dev {
        app = app.fallback(embed::spa_handler);
        info!("Serving embedded static files");
    } else {
        info!("Running in development mode (no static file serving)");
    }

    // MUST be applied after every route is registered: axum's
    // `Router::layer` only wraps routes that already exist at call time.
    //
    // The predicate skips bodies that are already compressed (video, image,
    // zip) and octet-stream, which has no Content-Length and would otherwise
    // be gzip-streamed in full on every /api/files/download.
    //
    // Size floor is 1024 rather than tower-http's 32: sub-1 KB JSON costs
    // more CPU to compress than it saves.
    //
    // Brotli quality 6, not the default 11. Quality 11 adds 100-200 ms per
    // large JSON response on a Pi Zero 2W for ~5% more compression. Assets
    // pre-compressed at build time already carry Content-Encoding, so
    // tower-http skips them here.
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

    // Auth middleware
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
    // systemd stops the service with SIGTERM. Without handling it, the
    // graceful drain below only ran on interactive Ctrl+C.
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

/// Dispatch a subcommand. Returns the exit code the wrapper scripts should
/// propagate back to their caller. `0` on success; `1` (or a shell-friendly
/// non-zero) on failure. Errors are printed to stderr so archiveloop's
/// existing `ERROR: make_snapshot.sh failed (exit $?)` log lines stay useful.
async fn run_subcommand(cmd: Command) -> i32 {
    match cmd {
        Command::Gadget { action } => run_gadget(action).await,
        Command::Snapshot { action } => run_snapshot(action).await,
        Command::Space { action } => run_space(action).await,
    }
}

async fn run_gadget(action: GadgetAction) -> i32 {
    // usb_gadget::enable/disable are synchronous and touch configfs; run
    // them on a blocking thread so they don't panic inside a tokio worker
    // on slow udc bind retries.
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
            // archiveloop calls `make_snapshot.sh nofsck` after a reboot
            // to skip the redundant fsck pass; treat anything else
            // (including bare "fsck" or no arg) as fsck-on. The bash
            // wrapper forwards `"$@"` so the first arg is what landed.
            let skip_fsck = args.iter().any(|a| a.eq_ignore_ascii_case("nofsck"));
            match sentryusb_gadget::snapshot::make_snapshot(skip_fsck).await {
                Ok(Some(name)) => {
                    println!("{}", name);
                    0
                }
                Ok(None) => {
                    // Snapshot was identical to the previous one and
                    // discarded. Print nothing: callers capturing stdout
                    // see an empty string and skip.
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
            // A present-but-unparseable reserve must NOT silently fall back to
            // the built-in default: archiveloop and this path would then be
            // enforcing different reserves, with nothing in the log to say so.
            // Fail loudly instead.
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
