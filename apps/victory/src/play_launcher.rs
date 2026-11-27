use anyhow::{Context, bail};
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "fsr_preset.rs"]
mod fsr_preset;
use fsr_preset::FsrPreset;

struct LaunchOptions {
    warp: bool,
    ray_tracing: Option<bool>,
    static_bvh: bool,
    console: &'static str,
    local_gpu_memory: bool,
    native_1440p: bool,
    render_1080p: bool,
    fsr: Option<FsrPreset>,
    sparse_textures: bool,
    upload_ahead: Option<bool>,
    capture_readbacks: bool,
    rt_write_stats: bool,
    rt_store_trace: bool,
    lighting_trace: bool,
}

fn launcher_options(
    args: &mut std::iter::Peekable<impl Iterator<Item = std::ffi::OsString>>,
) -> anyhow::Result<LaunchOptions> {
    let mut options = LaunchOptions {
        warp: false,
        ray_tracing: None,
        static_bvh: false,
        console: "series-x",
        local_gpu_memory: false,
        native_1440p: false,
        render_1080p: false,
        fsr: None,
        sparse_textures: false,
        upload_ahead: None,
        capture_readbacks: false,
        rt_write_stats: false,
        rt_store_trace: false,
        lighting_trace: false,
    };
    while let Some(option) = args.peek() {
        if option == "--warp" {
            options.warp = true;
        } else if option == "--no-rt" {
            options.ray_tracing = Some(false);
        } else if option == "--rt" {
            options.ray_tracing.get_or_insert(true);
        } else if option == "--rt-static-bvh" {
            options.static_bvh = true;
        } else if option == "--local-gpu-memory" {
            options.local_gpu_memory = true;
        } else if option == "--native-1440p" || option == "--1080p-to-1440p" {
            options.native_1440p = true;
            options.render_1080p |= option == "--1080p-to-1440p";
        } else if let Some(preset) = option.to_str().and_then(|option| {
            if option == "--fsr-1440p-to-4k" {
                Some(FsrPreset::Quality)
            } else {
                option.strip_prefix("--fsr-").and_then(FsrPreset::from_name)
            }
        }) {
            if options.fsr.replace(preset).is_some_and(|old| old != preset) {
                bail!("Choose one FSR preset");
            }
        } else if option == "--sparse-textures" {
            options.sparse_textures = true;
        } else if option == "--upload-ahead" || option == "--no-upload-ahead" {
            let enable = option == "--upload-ahead";
            if options.upload_ahead.replace(enable).is_some_and(|old| old != enable) {
                bail!("Choose one texture upload mode");
            }
        } else if option == "--capture-readbacks" {
            options.capture_readbacks = true;
        } else if option == "--rt-write-stats" {
            options.rt_write_stats = true;
        } else if option == "--rt-store-trace" {
            options.rt_store_trace = true;
        } else if option == "--lighting-trace" {
            options.lighting_trace = true;
        } else if option == "--console" {
            args.next();
            options.console = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
                Some("series-x") => "series-x",
                Some("series-s") => "series-s",
                _ => bail!("--console requires series-x or series-s"),
            };
            continue;
        } else {
            break;
        }
        args.next();
    }
    if options.static_bvh && options.ray_tracing.is_some() {
        bail!("Choose one mode from --rt, --no-rt, and --rt-static-bvh");
    }
    if options.fsr.is_some() && options.native_1440p {
        bail!("Choose an FSR preset at 4K or a 1440p output mode");
    }
    if options.fsr.is_some() && options.console != "series-x" {
        bail!("FSR at 4K requires Series X mode");
    }
    Ok(options)
}

pub(super) fn run(warm: bool) -> anyhow::Result<()> {
    let launcher = std::env::current_exe()?;
    let folder = launcher.parent().context("Launcher folder is missing")?;
    let runtime = folder.join("victory.exe");
    let mut args = std::env::args_os().skip(1).peekable();
    let LaunchOptions {
        warp, ray_tracing, static_bvh, console, local_gpu_memory, native_1440p, render_1080p, fsr,
        sparse_textures, upload_ahead, capture_readbacks, rt_write_stats, rt_store_trace, lighting_trace,
    } = launcher_options(&mut args)?;
    let game = match args.next() {
        Some(path) => PathBuf::from(path),
        None => {
            let config = folder.join("game-path.txt");
            let value = fs::read_to_string(&config)
                .with_context(|| format!("Put the game executable path in {}", config.display()))?;
            folder.join(value.trim())
        }
    };
    if args.next().is_some() {
        bail!(
            "Usage\n{} [--rt | --no-rt | --rt-static-bvh] [--warp] [--console series-x|series-s] [--local-gpu-memory] [--native-1440p | --1080p-to-1440p | --fsr-quality | --fsr-balanced | --fsr-performance] [--sparse-textures] [--upload-ahead | --no-upload-ahead] [--capture-readbacks] [--rt-write-stats] [--rt-store-trace] [--lighting-trace] [game executable]",
            launcher.display()
        );
    }
    if !game.is_file() || !runtime.is_file() {
        bail!(
            "Game or VIctory executable is missing.\n\n{}\n{}",
            game.display(),
            runtime.display()
        );
    }
    let game = std::path::absolute(game)?;
    let logs = folder.join("logs");
    let state = folder.join("state");
    let cache = std::env::var_os("VICTORY_CACHE_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| folder.join("cache"), |value| folder.join(value));
    fs::create_dir_all(&logs)?;
    fs::create_dir_all(&state)?;
    fs::create_dir_all(&cache)?;
    let _session = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(state.join("launcher.lock"))
        .context("Could not lock the launcher state. Check for another DexNom RayTime session.")?;
    let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let action = if warm { "warm" } else { "run" };
    let path = logs.join(format!("{action}-{started}-{}.log", std::process::id()));
    let mut log = OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(&path)?;
    writeln!(
        log,
        "Runtime {}\nGame {}",
        runtime.display(),
        game.display()
    )?;
    let latest = if warm {
        "latest-warm.txt"
    } else {
        "latest.txt"
    };
    fs::write(logs.join(latest), path.display().to_string())?;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new(&runtime);
    if let Some(on) = ray_tracing {
        child.env("VICTORY_RAY_TRACING", if on { "1" } else { "0" });
        writeln!(log, "Ray tracing requested: {}", if on { "on" } else { "off" })?;
    } else if static_bvh {
        child.env("VICTORY_RAY_TRACING", "static-bvh");
        writeln!(log, "Ray tracing requested: static BVH traversal")?;
    }
    if warp {
        child.env("VICTORY_D3D12_WARP", "1");
    }
    if local_gpu_memory {
        child.env("VICTORY_LOCAL_GPU_INPUTS", "1");
    }
    if native_1440p {
        child
            .env("VICTORY_NATIVE_1440P", "1")
            .env("VICTORY_RENDER_1080P", if render_1080p { "1" } else { "0" })
            .env("VICTORY_FSR_4K", "0");
        let expected_render = if render_1080p { "1920x1080" } else { "2560x1440" };
        writeln!(log, "Expected scene rendering {expected_render}, output 2560x1440")?;
    }
    if let Some(preset) = fsr {
        child.env("VICTORY_FSR_4K", preset.name());
        let provider = folder.join("fidelityfx");
        if std::env::var_os("VICTORY_FIDELITYFX_DIR").is_none()
            && provider.join("amd_fidelityfx_upscaler_dx12.dll").is_file()
        {
            child.env("VICTORY_FIDELITYFX_DIR", provider);
        }
        let [width, height] = preset.render_size();
        writeln!(log, "FSR {} (SDK mode {}) requested at {width}x{height} scene, 3840x2160 output", preset.name(), preset.mode())?;
    }
    if (fsr.is_some() || upload_ahead == Some(true)) && std::env::var_os("VICTORY_LOG").is_none() {
        let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into());
        child.env("VICTORY_LOG", format!(
            "{filter},victory::run::native_1440p=info,victory_hle::modules::fidelityfx=info,victory_gpu::host=info,victory_gpu::texture::ahead=info"
        ));
    }
    if let Some(enabled) = upload_ahead {
        let value = if enabled { "1" } else { "0" };
        child.env("VICTORY_UPLOAD_AHEAD", value);
        writeln!(log, "Texture copy queue requested: {}", if enabled { "on" } else { "off" })?;
    }
    for (enabled, key, message) in [
        (sparse_textures, "VICTORY_LEVELS_ON_DEMAND", "Sparse texture mips requested"),
        (capture_readbacks, "VICTORY_CAPTURE_READBACKS", "Dirty-page readback capture requested"),
        (rt_write_stats, "VICTORY_RT_WRITE_STATS", "Ray-tracing write statistics requested"),
    ] {
        if enabled {
            child.env(key, "1");
            writeln!(log, "{message}")?;
        }
    }
    if rt_store_trace {
        child.env("VICTORY_CAPTURE_RAW_STORES", victory_shader::emit::RAW_STORE_PROBE_HASH);
        writeln!(log, "InitHeaders store trace requested")?;
    }
    if lighting_trace {
        let trace = folder.join("lighting-trace").join(format!("{started}-{}", std::process::id()));
        fs::create_dir_all(&trace)?;
        let request = trace.join("request.txt");
        // Lighting using the shadow mask, RT resolve, auto-exposure, and the final composite.
        child
            .env("VICTORY_CAPTURE", &trace)
            .env("VICTORY_CAPTURE_REQUEST", &request)
            .env("VICTORY_DUMP_AFTER", "2bf91065944bcff4,7f05ddab771799a9,e622b718b6d9b057,6318f669e4f17d65")
            .env("VICTORY_DUMP_INPUT", "e622b718b6d9b057:s1+0");
        writeln!(log, "Lighting trace request file {}", request.display())?;
    }
    if std::env::var_os("VICTORY_DEBUG_HUD").is_none() {
        child.env("VICTORY_DEBUG_HUD", "1");
    }
    child
        .current_dir(folder)
        .env("VICTORY_DATA_DIR", &state)
        .env("VICTORY_CACHE_DIR", &cache)
        .stdin(Stdio::null())
        .stderr(log.try_clone()?)
        .creation_flags(CREATE_NO_WINDOW);
    let status = if warm {
        preload(&mut child, &game, &cache, &path, &mut log)?
    } else {
        let status = child
            .arg("run")
            .arg(&game)
            .args([
                "--execute",
                "--console",
                console,
                "--unimplemented",
                "trap",
            ])
            .stdout(log.try_clone()?)
            .status()
            .with_context(|| format!("Could not start {}", runtime.display()))?;
        writeln!(log, "VIctory exited with {status}")?;
        status
    };
    if !status.success() {
        bail!(
            "VIctory exited with {status}.\n\nLog file\n{}",
            path.display()
        );
    }
    Ok(())
}

fn preload(
    child: &mut Command,
    game: &Path,
    cache: &Path,
    path: &Path,
    log: &mut File,
) -> anyhow::Result<ExitStatus> {
    let root = game.parent().context("Game install folder is missing")?;
    writeln!(
        log,
        "Scan {}\nCache {}\nThe offline reader recognizes SGD5 records and raw DXBC in files up to 2 GiB. Graphics pipelines are created when the game supplies their draw state.",
        root.display(),
        cache.display()
    )?;
    println!(
        "Preloading shaders and compute pipelines from {}\nLive log: {}",
        root.display(),
        path.display()
    );
    let mut worker = child
        .arg("warm")
        .arg(root)
        .stdout(Stdio::piped())
        .spawn()
        .context("Could not start DexNom RayTime shader preload")?;
    let stdout = worker
        .stdout
        .take()
        .context("Warmup output pipe is missing")?;
    let last =
        BufReader::new(stdout)
            .lines()
            .try_fold(None, |summary, line| -> anyhow::Result<_> {
                let line = line?;
                writeln!(log, "{line}")?;
                let _ = writeln!(std::io::stdout().lock(), "{line}");
                Ok(if line.starts_with("warm ") {
                    Some(line)
                } else {
                    summary
                })
            });
    let status = worker.wait()?;
    writeln!(log, "VIctory exited with {status}")?;
    if status.success()
        && let Err(message) = check_warm_summary(last?.as_deref())
    {
        writeln!(log, "{message}")?;
        bail!("{message}\n\nLog file\n{}", path.display());
    }
    Ok(status)
}

/// The runtime prints this after saving its pipeline library. Preserve its exact
/// counters in the log, including partial refusal; an empty scan is not a preload.
fn check_warm_summary(summary: Option<&str>) -> Result<(), &'static str> {
    let Some(summary) = summary else {
        return Err("VIctory did not report a shader warmup result");
    };
    let mut words = summary.split_whitespace();
    if words.next() != Some("warm")
        || words
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none_or(|n| n == 0)
    {
        return Err("No shaders successfully translated. See the runtime totals in the log");
    }
    Ok(())
}

#[allow(dead_code)] // the console warmer reports errors to stderr
pub(super) fn show_error(text: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(owner: *mut c_void, text: *const u16, title: *const u16, flags: u32) -> i32;
    }
    let text: Vec<_> = text.encode_utf16().chain([0]).collect();
    let title: Vec<_> = "DexNom RayTime".encode_utf16().chain([0]).collect();
    // SAFETY: the two strings are NUL terminated and live for the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10) };
}

#[cfg(test)]
mod tests {
    use super::{check_warm_summary, launcher_options};

    #[test]
    fn explicit_off_wins_in_either_order_and_keeps_the_game_path() {
        for (switches, expected) in [
            (vec![], None), (vec!["--rt"], Some(true)), (vec!["--no-rt"], Some(false)),
            (vec!["--rt", "--no-rt"], Some(false)), (vec!["--no-rt", "--rt"], Some(false)),
        ] {
            let mut args = switches.into_iter().chain(["--warp", "game path.exe"]).map(std::ffi::OsString::from).peekable();
            let options = launcher_options(&mut args).unwrap();
            assert!(options.warp);
            assert_eq!(options.ray_tracing, expected);
            assert_eq!(options.console, "series-x");
            assert!(!options.local_gpu_memory);
            assert!(!options.lighting_trace);
            assert!(!options.rt_store_trace);
            assert_eq!(options.upload_ahead, None);
            assert_eq!(args.next().as_deref(), Some(std::ffi::OsStr::new("game path.exe")));
        }
    }

    #[test]
    fn console_profile_is_explicit_and_preserves_the_game_path() {
        let mut args = ["--no-rt", "--console", "series-s", "--local-gpu-memory",
            "--sparse-textures", "--capture-readbacks", "--rt-write-stats", "--rt-store-trace", "--lighting-trace", "game path.exe"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .peekable();
        let options = launcher_options(&mut args).unwrap();
        assert!(!options.warp);
        assert_eq!(options.ray_tracing, Some(false));
        assert_eq!(options.console, "series-s");
        assert!(options.local_gpu_memory);
        assert!(options.sparse_textures && options.capture_readbacks && options.rt_write_stats);
        assert!(options.lighting_trace);
        assert!(options.rt_store_trace);
        assert_eq!(args.next().as_deref(), Some(std::ffi::OsStr::new("game path.exe")));

        let mut invalid = ["--console", "one-x"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .peekable();
        assert!(launcher_options(&mut invalid).is_err());
    }

    #[test]
    fn scene_upscale_uses_1440p_output_and_preserves_the_game_path() {
        for (switches, expected) in [
            (vec!["--native-1440p"], false),
            (vec!["--1080p-to-1440p"], true),
            (vec!["--native-1440p", "--1080p-to-1440p"], true),
            (vec!["--1080p-to-1440p", "--native-1440p"], true),
        ] {
            let mut args = switches
                .into_iter()
                .chain(["game path.exe"])
                .map(std::ffi::OsString::from)
                .peekable();
            let options = launcher_options(&mut args).unwrap();
            assert!(options.native_1440p);
            assert_eq!(options.render_1080p, expected);
            assert_eq!(args.next().as_deref(), Some(std::ffi::OsStr::new("game path.exe")));
        }
    }

    #[test]
    fn fsr_presets_use_the_series_x_4k_output() {
        use super::FsrPreset;
        for (switch, preset, queue, enabled) in [
            ("--fsr-1440p-to-4k", FsrPreset::Quality, "--upload-ahead", true),
            ("--fsr-quality", FsrPreset::Quality, "--upload-ahead", true),
            ("--fsr-balanced", FsrPreset::Balanced, "--upload-ahead", true),
            ("--fsr-performance", FsrPreset::Performance, "--upload-ahead", true),
            ("--fsr-performance", FsrPreset::Performance, "--no-upload-ahead", false),
        ] {
            let mut args = [switch, "--rt", queue, "--rt-store-trace", "game path.exe"]
                .into_iter()
                .map(std::ffi::OsString::from)
                .peekable();
            let options = launcher_options(&mut args).unwrap();
            assert_eq!(options.fsr, Some(preset));
            assert_eq!(options.upload_ahead, Some(enabled));
            assert!(options.rt_store_trace);
            assert!(!options.native_1440p && !options.render_1080p);
            assert_eq!(options.console, "series-x");
            assert_eq!(options.ray_tracing, Some(true));
            assert_eq!(args.next().as_deref(), Some(std::ffi::OsStr::new("game path.exe")));
        }

        for switches in [
            &["--fsr-1440p-to-4k", "--native-1440p"][..],
            &["--1080p-to-1440p", "--fsr-1440p-to-4k"],
            &["--console", "series-s", "--fsr-1440p-to-4k"],
            &["--fsr-performance", "--fsr-balanced"],
            &["--upload-ahead", "--no-upload-ahead"],
        ] {
            let mut invalid = switches.iter().copied().map(std::ffi::OsString::from).peekable();
            assert!(launcher_options(&mut invalid).is_err());
        }
    }

    #[test]
    fn empty_and_incomplete_scans_are_reported() {
        assert!(check_warm_summary(None).is_err());
        assert!(
            check_warm_summary(Some(
                "warm  0 translated, 0 refused; 0 compute pipelines, 0 refused; 0 s"
            ))
            .is_err()
        );
        assert!(
            check_warm_summary(Some(
                "warm  0 translated, 2 refused; 0 compute pipelines, 0 refused; 1 s"
            ))
            .is_err()
        );
        assert!(
            check_warm_summary(Some(
                "warm  1 translated, 2 refused; 3 compute pipelines, 4 refused; 5 s"
            ))
            .is_ok()
        );
    }
}
