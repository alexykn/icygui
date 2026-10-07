//! `cargo xtask screenshots`: the README's screenshots and clips, taken from
//! `icygui --demo` (ENV-11), so they stay current with the app.
//!
//! Linux only. Starts its own Xvfb on a free display, runs the debug app in
//! demo mode once per scene (fixed seed, a private home and runtime
//! directory, no session bus: nothing reaches the desktop the command runs
//! on), drives it with `xdotool` and records it with `ffmpeg`'s `x11grab`:
//!
//! - stills: one frame of the window, reduced to a 256-colour palette
//!   (`palettegen` / `paletteuse`, no dithering), as PNG;
//! - clips: a lossless recording of the window, scaled to
//!   [`GIF_WIDTH`] and converted to a GIF with a generated palette.
//!
//! GPUI renders at twice the size (`GPUI_X11_SCALE_FACTOR=2`), so the stills
//! stay sharp on high-density screens. The window keeps the app's default
//! size ([`WIDTH`] × [`HEIGHT`] logical pixels), and the pointer rests where
//! nothing reacts to it except while a scene clicks.
//!
//! Needs `Xvfb`, `xdotool`, `ffmpeg` and a Vulkan driver (Mesa's software
//! one works): `sudo apt-get install xvfb xdotool ffmpeg mesa-vulkan-drivers`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::{APP_ID, BINARY, Flags, Result, cargo, create_dir, remove_dir, root, run, target_dir};

/// The app's default window size, in logical pixels.
const WIDTH: u32 = 1440;
/// See [`WIDTH`].
const HEIGHT: u32 = 900;
/// GPUI's scale factor under Xvfb.
const SCALE: u32 = 2;
/// The width of the clips, in pixels.
const GIF_WIDTH: u32 = 1200;
/// Frames per second of the clips.
const GIF_FPS: u32 = 10;
/// A clip larger than this gets a warning.
const GIF_BUDGET: u64 = 3 * 1024 * 1024;
/// The demo simulator's seed: the same seed tells the same story.
const SEED: &str = "7";
/// Where the images go, relative to the repository.
pub(crate) const OUT_DIR: &str = "docs/screenshots";
/// How long the app may take to show its window (a debug build starting
/// on a software renderer).
const WINDOW_TIMEOUT: Duration = Duration::from_secs(90);
/// Delay between typed characters, in milliseconds.
const TYPE_DELAY_MS: &str = "70";

/// What a scene does once the window is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Waits this many milliseconds.
    Wait(u64),
    /// Presses a key combination (`xdotool key` syntax: `ctrl+k`, `Return`).
    Key(&'static str),
    /// Types text, a character at a time.
    Type(&'static str),
    /// Clicks at a point of the window, in logical pixels, then moves the
    /// pointer out of the window again.
    Click(u32, u32),
    /// Starts recording (clips only; the clip ends with the last step).
    Record,
}

/// What a scene produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    /// `<name>.png`, taken after the last step.
    Still,
    /// `<name>.gif`, recorded from [`Step::Record`] to the last step.
    Clip,
}

/// One screenshot or clip.
#[derive(Debug)]
struct Scene {
    /// The file name without extension.
    name: &'static str,
    output: Output,
    /// Extra environment for the app (the `ICYGUI_DEMO_*` switches).
    env: &'static [(&'static str, &'static str)],
    steps: &'static [Step],
}

impl Scene {
    fn file_name(&self) -> String {
        match self.output {
            Output::Still => format!("{}.png", self.name),
            Output::Clip => format!("{}.gif", self.name),
        }
    }
}

/// Time for the demo to load and settle after the window shows.
const LOADED: Step = Step::Wait(5_000);

/// The filter typed into the dashboard editor.
const POSTGRES_FILTER: &str = r#"host.vars.role == "postgres" && service.state != 0"#;

/// The filter field of the dashboard editor, in logical pixels.
const EDITOR_FILTER_FIELD: (u32, u32) = (1253, 374);
/// Where the pointer rests: the empty middle of the list's header, which
/// has no hover state. (GPUI assumes a pointer in the middle of a new window
/// until it sees one move, so parking it outside isn't enough.)
const POINTER_REST: (u32, u32) = (900, 20);
/// The *Review certificate* button of the "not trusted" notice.
const REVIEW_CERTIFICATE_BUTTON: (u32, u32) = (821, 528);

/// Every scene, in the order they're taken.
const SCENES: &[Scene] = &[
    Scene {
        name: "dashboard",
        output: Output::Still,
        env: &[],
        steps: &[LOADED],
    },
    Scene {
        name: "service-pane",
        output: Output::Still,
        env: &[("ICYGUI_DEMO_OPEN", "service")],
        steps: &[LOADED],
    },
    Scene {
        name: "host-pane",
        output: Output::Still,
        env: &[("ICYGUI_DEMO_OPEN", "host")],
        steps: &[LOADED],
    },
    Scene {
        name: "command-palette",
        output: Output::Still,
        env: &[("ICYGUI_DEMO_OPEN", "service")],
        steps: &[
            LOADED,
            Step::Key("ctrl+k"),
            Step::Wait(500),
            Step::Type("ack postgres"),
            Step::Wait(800),
        ],
    },
    Scene {
        name: "acknowledge",
        output: Output::Still,
        env: &[("ICYGUI_DEMO_OPEN", "service")],
        steps: &[
            LOADED,
            Step::Key("a"),
            Step::Wait(600),
            Step::Type("Failover drill on db-prod-01, lag expected until 15:30"),
            Step::Wait(600),
        ],
    },
    Scene {
        name: "notification-centre",
        output: Output::Still,
        env: &[("ICYGUI_DEMO_STORM", "10")],
        steps: &[
            LOADED,
            Step::Wait(9_000),
            Step::Key("ctrl+k"),
            Step::Wait(500),
            Step::Type("notifications"),
            Step::Wait(500),
            Step::Key("Return"),
            Step::Wait(1_000),
        ],
    },
    Scene {
        name: "notification-settings",
        output: Output::Still,
        env: &[],
        steps: &[
            LOADED,
            Step::Key("ctrl+k"),
            Step::Wait(500),
            Step::Type("notification settings"),
            Step::Wait(500),
            Step::Key("Return"),
            Step::Wait(1_000),
        ],
    },
    Scene {
        name: "dashboard-editor",
        output: Output::Still,
        env: &[],
        steps: &[
            LOADED,
            Step::Key("ctrl+n"),
            Step::Wait(1_000),
            Step::Key("ctrl+a"),
            Step::Type("postgres"),
            Step::Click(EDITOR_FILTER_FIELD.0, EDITOR_FILTER_FIELD.1),
            Step::Key("ctrl+a"),
            Step::Type(POSTGRES_FILTER),
            Step::Wait(1_000),
        ],
    },
    Scene {
        name: "certificate",
        output: Output::Still,
        env: &[("ICYGUI_DEMO_FAULT", "tls")],
        steps: &[
            LOADED,
            Step::Click(REVIEW_CERTIFICATE_BUTTON.0, REVIEW_CERTIFICATE_BUTTON.1),
            Step::Wait(1_000),
        ],
    },
    Scene {
        name: "live-updates",
        output: Output::Clip,
        env: &[("ICYGUI_DEMO_STORM", "10")],
        steps: &[Step::Wait(4_000), Step::Record, Step::Wait(12_000)],
    },
    Scene {
        name: "keyboard",
        output: Output::Clip,
        env: &[],
        steps: &[
            LOADED,
            Step::Record,
            Step::Wait(1_000),
            Step::Key("j"),
            Step::Wait(500),
            Step::Key("j"),
            Step::Wait(500),
            Step::Key("j"),
            Step::Wait(500),
            Step::Key("k"),
            Step::Wait(700),
            Step::Key("Return"),
            Step::Wait(1_500),
            Step::Key("a"),
            Step::Wait(800),
            Step::Type("on it, failover drill"),
            Step::Wait(600),
            Step::Key("Return"),
            Step::Wait(2_500),
            Step::Key("Escape"),
            Step::Wait(1_500),
        ],
    },
    Scene {
        name: "palette",
        output: Output::Clip,
        env: &[],
        steps: &[
            LOADED,
            Step::Record,
            Step::Wait(800),
            Step::Key("ctrl+k"),
            Step::Wait(600),
            Step::Type("db-prod-03"),
            Step::Wait(1_000),
            Step::Key("Return"),
            Step::Wait(2_000),
            Step::Key("ctrl+k"),
            Step::Wait(500),
            Step::Type("ack rabbit"),
            Step::Wait(1_000),
            Step::Key("Return"),
            Step::Wait(2_000),
        ],
    },
    Scene {
        name: "editor",
        output: Output::Clip,
        env: &[],
        steps: &[
            LOADED,
            Step::Record,
            Step::Wait(600),
            Step::Key("ctrl+n"),
            Step::Wait(1_000),
            Step::Key("ctrl+a"),
            Step::Type("postgres"),
            Step::Wait(400),
            Step::Click(EDITOR_FILTER_FIELD.0, EDITOR_FILTER_FIELD.1),
            Step::Key("ctrl+a"),
            Step::Type(POSTGRES_FILTER),
            Step::Wait(1_500),
            Step::Key("ctrl+s"),
            Step::Wait(2_000),
        ],
    },
];

/// `cargo xtask screenshots [--only NAME,…] [--no-gifs]`.
pub(crate) fn screenshots(flags: &Flags) -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err(
            "screenshots need Linux (Xvfb); run them in a Linux VM or container".to_owned(),
        );
    }
    let scenes = select(flags.only.as_deref(), flags.no_gifs)?;
    for tool in ["Xvfb", "xdotool", "ffmpeg"] {
        if !on_path(tool) {
            return Err(format!(
                "{tool} is missing; install with `sudo apt-get install xvfb xdotool ffmpeg \
                 mesa-vulkan-drivers`"
            ));
        }
    }
    run(cargo()
        .current_dir(root())
        .args(["build", "--locked", "-p", "ic-app"]))?;
    let binary = target_dir().join("debug").join(BINARY);
    let out = root().join(OUT_DIR);
    create_dir(&out)?;
    let work = target_dir().join("screenshots");
    remove_dir(&work)?;
    create_dir(&work)?;

    let result = (|| -> Result<()> {
        let display = Display::start(&work)?;
        for scene in &scenes {
            eprintln!("» scene {}", scene.name);
            take(scene, &binary, &display, &work, &out)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            remove_dir(&work)?;
            report(&out, &scenes)
        }
        // Keep the scenes' app logs for a look.
        Err(error) => Err(format!("{error}\n(app logs in {})", work.display())),
    }
}

/// The scenes to take: all, or the named ones; `--no-gifs` drops clips.
fn select(only: Option<&str>, no_gifs: bool) -> Result<Vec<&'static Scene>> {
    let mut scenes: Vec<&Scene> = SCENES
        .iter()
        .filter(|scene| !(no_gifs && scene.output == Output::Clip))
        .collect();
    if let Some(only) = only {
        let names: Vec<&str> = only
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        if let Some(unknown) = names
            .iter()
            .find(|name| !SCENES.iter().any(|scene| scene.name == **name))
        {
            let known: Vec<&str> = SCENES.iter().map(|scene| scene.name).collect();
            return Err(format!("no scene {unknown}; scenes: {}", known.join(", ")));
        }
        scenes.retain(|scene| names.contains(&scene.name));
    }
    Ok(scenes)
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

/// Stops a child process when dropped: `SIGTERM` first, so it cleans up
/// after itself (Xvfb removes its lock file), then `SIGKILL`.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let terminated = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .is_ok_and(|status| status.success());
        if terminated
            && wait_until(Duration::from_secs(5), || {
                self.0.try_wait().is_ok_and(|status| status.is_some())
            })
        {
            return;
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A private X server.
struct Display {
    name: String,
    _server: Guard,
}

impl Display {
    fn start(work: &Path) -> Result<Self> {
        let number = (90..200)
            .find(|n| {
                !Path::new(&format!("/tmp/.X{n}-lock")).exists()
                    && !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            })
            .ok_or("no free X display between :90 and :199")?;
        let name = format!(":{number}");
        let (width, height) = Self::screen();
        let log = fs::File::create(work.join("xvfb.log")).map_err(|error| error.to_string())?;
        let server = Command::new("Xvfb")
            .args([
                name.as_str(),
                "-screen",
                "0",
                &format!("{width}x{height}x24"),
                "-nolisten",
                "tcp",
            ])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|error| format!("starting Xvfb: {error}"))?;
        let display = Self {
            name,
            _server: Guard(server),
        };
        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
        if !wait_until(Duration::from_secs(10), || socket.exists()) {
            return Err("Xvfb didn't start (see xvfb.log)".to_owned());
        }
        // Park the pointer outside where the window will be, so no row
        // starts out hovered.
        display.park_pointer()?;
        Ok(display)
    }

    /// The screen is a little larger than the window, so the pointer has
    /// somewhere to go.
    fn screen() -> (u32, u32) {
        (WIDTH * SCALE + 120, HEIGHT * SCALE + 100)
    }

    fn xdotool(&self) -> Command {
        let mut command = Command::new("xdotool");
        command.env("DISPLAY", &self.name);
        command
    }

    fn park_pointer(&self) -> Result<()> {
        let (width, height) = Self::screen();
        quiet(self.xdotool().args([
            "mousemove",
            &(width - 1).to_string(),
            &(height - 1).to_string(),
        ]))
    }
}

/// The app's window on the screen.
#[derive(Debug)]
struct Window {
    id: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Window {
    /// The `x11grab` input for this window.
    fn grab_input(&self, display: &Display) -> Vec<String> {
        [
            "-f",
            "x11grab",
            "-draw_mouse",
            "0",
            "-video_size",
            &format!("{}x{}", self.width, self.height),
        ]
        .iter()
        .map(|arg| (*arg).to_owned())
        .chain([
            "-i".to_owned(),
            format!("{}+{},{}", display.name, self.x, self.y),
        ])
        .collect()
    }
}

fn take(scene: &Scene, binary: &Path, display: &Display, work: &Path, out: &Path) -> Result<()> {
    let dir = work.join(scene.name);
    remove_dir(&dir)?;
    let runtime = dir.join("runtime");
    create_dir(&runtime)?;
    set_private(&runtime)?;
    let log = fs::File::create(dir.join("app.log")).map_err(|error| error.to_string())?;
    let mut command = Command::new(binary);
    command
        .arg("--demo")
        .env("DISPLAY", &display.name)
        .env("GPUI_X11_SCALE_FACTOR", SCALE.to_string())
        .env("ICYGUI_WINDOW_CONTROLS", "always")
        .env("ICYGUI_DEMO_SEED", SEED)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("XDG_RUNTIME_DIR", &runtime)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .envs(scene.env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    let app = Guard(
        command
            .spawn()
            .map_err(|error| format!("starting {}: {error}", binary.display()))?,
    );
    let window = find_window(display)?;
    if (window.width, window.height) != (WIDTH * SCALE, HEIGHT * SCALE) {
        return Err(format!(
            "the window is {}x{}, expected {}x{}",
            window.width,
            window.height,
            WIDTH * SCALE,
            HEIGHT * SCALE
        ));
    }
    quiet(
        display
            .xdotool()
            .args(["windowfocus", "--sync", &window.id]),
    )?;

    let recording = dir.join("clip.mkv");
    let mut recorder: Option<Child> = None;
    for step in scene.steps {
        if let Some(started) = perform(*step, display, &window, &recording)? {
            recorder = Some(started);
        }
    }

    let target = out.join(scene.file_name());
    match scene.output {
        Output::Still => {
            // One frame first: `palettegen` reads its whole input before it
            // answers, and the screen never ends.
            let frame = dir.join("frame.png");
            run(Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y"])
                .args(window.grab_input(display))
                .args(["-frames:v", "1"])
                .arg(&frame))?;
            drop(app);
            run(Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-i"])
                .arg(&frame)
                .arg("-vf")
                .arg(
                    "split[a][b];[a]palettegen=max_colors=256:stats_mode=full[p];\
                     [b][p]paletteuse=dither=none",
                )
                .arg(&target))?;
        }
        Output::Clip => {
            let recorder = recorder.ok_or_else(|| format!("{} never records", scene.name))?;
            stop_recording(recorder)?;
            drop(app);
            gif(&recording, &target)?;
        }
    }
    remove_dir(&dir)
}

/// Moves the pointer to [`POINTER_REST`].
fn rest_pointer(display: &Display, window: &Window) -> Result<()> {
    quiet(display.xdotool().args([
        "mousemove",
        "--window",
        &window.id,
        &(POINTER_REST.0 * SCALE).to_string(),
        &(POINTER_REST.1 * SCALE).to_string(),
    ]))
}

/// Runs one step; returns the recorder when the step starts recording.
fn perform(
    step: Step,
    display: &Display,
    window: &Window,
    recording: &Path,
) -> Result<Option<Child>> {
    match step {
        Step::Wait(ms) => thread::sleep(Duration::from_millis(ms)),
        Step::Key(keys) => quiet(display.xdotool().args(["key", keys]))?,
        Step::Type(text) => {
            quiet(
                display
                    .xdotool()
                    .args(["type", "--delay", TYPE_DELAY_MS, "--", text]),
            )?;
        }
        Step::Click(x, y) => {
            quiet(display.xdotool().args([
                "mousemove",
                "--window",
                &window.id,
                &(x * SCALE).to_string(),
                &(y * SCALE).to_string(),
                "click",
                "1",
            ]))?;
            thread::sleep(Duration::from_millis(100));
            rest_pointer(display, window)?;
        }
        Step::Record => {
            let recorder = Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y"])
                .args(["-framerate", &GIF_FPS.to_string()])
                .args(window.grab_input(display))
                .args(["-c:v", "utvideo"])
                .arg(recording)
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|error| format!("starting ffmpeg: {error}"))?;
            // x11grab needs a moment before the first frame.
            thread::sleep(Duration::from_millis(300));
            return Ok(Some(recorder));
        }
    }
    Ok(None)
}

fn stop_recording(mut recorder: Child) -> Result<()> {
    if let Some(stdin) = recorder.stdin.as_mut() {
        // `q` ends ffmpeg's recording cleanly.
        let _ = stdin.write_all(b"q");
    }
    let status = recorder.wait().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("recording failed: {status}"))
    }
}

/// Converts a recording into a GIF: scaled, with a palette made for it.
fn gif(recording: &Path, target: &Path) -> Result<()> {
    run(Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(recording)
        .arg("-vf")
        .arg(format!(
            "fps={GIF_FPS},scale={GIF_WIDTH}:-1:flags=lanczos,split[a][b];\
             [a]palettegen=max_colors=128:stats_mode=diff[p];\
             [b][p]paletteuse=dither=none:diff_mode=rectangle"
        ))
        .args(["-loop", "0"])
        .arg(target))
}

/// Waits for the app's window and reads its geometry.
fn find_window(display: &Display) -> Result<Window> {
    let mut id = String::new();
    let found = wait_until(WINDOW_TIMEOUT, || {
        let output = display
            .xdotool()
            .args(["search", "--classname", APP_ID])
            .stderr(Stdio::null())
            .output();
        if let Ok(output) = output
            && let Some(first) = String::from_utf8_lossy(&output.stdout).lines().next()
        {
            first.trim().clone_into(&mut id);
            return !id.is_empty();
        }
        false
    });
    if !found {
        return Err("the app's window didn't show".to_owned());
    }
    // Let the first frame land before reading the geometry.
    thread::sleep(Duration::from_millis(500));
    let output = display
        .xdotool()
        .args(["getwindowgeometry", "--shell", &id])
        .output()
        .map_err(|error| format!("xdotool: {error}"))?;
    parse_geometry(&id, &String::from_utf8_lossy(&output.stdout))
}

/// Reads `xdotool getwindowgeometry --shell` output.
fn parse_geometry(id: &str, text: &str) -> Result<Window> {
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .ok_or_else(|| format!("no {key} in xdotool's answer: {text}"))
    };
    let number = |key: &str| -> Result<i64> {
        value(key)?
            .trim()
            .parse::<i64>()
            .map_err(|error| format!("{key}: {error}"))
    };
    let int = |key: &str| -> Result<i32> {
        i32::try_from(number(key)?).map_err(|error| format!("{key}: {error}"))
    };
    let size = |key: &str| -> Result<u32> {
        u32::try_from(number(key)?).map_err(|error| format!("{key}: {error}"))
    };
    Ok(Window {
        id: id.to_owned(),
        x: int("X")?,
        y: int("Y")?,
        width: size("WIDTH")?,
        height: size("HEIGHT")?,
    })
}

/// Polls `ready` until it holds (`true`) or `timeout` passes (`false`).
fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if ready() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Runs a helper without echoing it; its errors still say what failed.
fn quiet(command: &mut Command) -> Result<()> {
    let output = command
        .output()
        .map_err(|error| format!("failed to start {command:?}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{command:?} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(unix)]
fn set_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private(_path: &Path) -> Result<()> {
    Ok(())
}

/// Prints what was written and its size; warns about clips over budget.
fn report(out: &Path, scenes: &[&Scene]) -> Result<()> {
    let mut total = 0;
    for scene in scenes {
        let path = out.join(scene.file_name());
        let size = fs::metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .len();
        total += size;
        println!("wrote {} ({} KiB)", path.display(), size / 1024);
        if scene.output == Output::Clip && size > GIF_BUDGET {
            eprintln!(
                "warning: {} is over {} MiB; shorten the scene",
                path.display(),
                GIF_BUDGET / 1024 / 1024
            );
        }
    }
    println!("{} files, {} KiB", scenes.len(), total / 1024);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_names_are_unique_and_clips_record() {
        for (index, scene) in SCENES.iter().enumerate() {
            assert!(
                SCENES[..index].iter().all(|other| other.name != scene.name),
                "{} twice",
                scene.name
            );
            let records = scene.steps.contains(&Step::Record);
            assert_eq!(
                records,
                scene.output == Output::Clip,
                "{}: only clips record",
                scene.name
            );
        }
    }

    #[test]
    fn selects_scenes() {
        assert_eq!(select(None, false).unwrap().len(), SCENES.len());
        assert!(
            select(None, true)
                .unwrap()
                .iter()
                .all(|scene| scene.output == Output::Still)
        );
        let some = select(Some("dashboard, keyboard"), false).unwrap();
        assert_eq!(
            some.iter().map(|scene| scene.name).collect::<Vec<_>>(),
            ["dashboard", "keyboard"]
        );
        assert!(select(Some("nope"), false).is_err());
    }

    #[test]
    fn reads_window_geometry() {
        let window = parse_geometry(
            "2097153",
            "WINDOW=2097153\nX=62\nY=50\nWIDTH=2880\nHEIGHT=1800\nSCREEN=0\n",
        )
        .unwrap();
        assert_eq!(
            (window.x, window.y, window.width, window.height),
            (62, 50, 2880, 1800)
        );
        assert!(parse_geometry("1", "X=1\n").is_err());
    }

    /// Every image the README and the user guide show is one a scene
    /// makes, and every scene's image is in the repository.
    #[test]
    fn documents_show_generated_images() {
        let root = root();
        for document in ["README.md", "docs/user-guide.md"] {
            let text = fs::read_to_string(root.join(document)).unwrap();
            for (index, _) in text.match_indices(OUT_DIR) {
                let rest = &text[index + OUT_DIR.len() + 1..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
                    .unwrap_or(rest.len());
                let file = &rest[..end];
                if file.is_empty() {
                    continue;
                }
                assert!(
                    SCENES.iter().any(|scene| scene.file_name() == file),
                    "{document} shows {file}, which no scene makes"
                );
            }
        }
        for scene in SCENES {
            let path = root.join(OUT_DIR).join(scene.file_name());
            assert!(path.is_file(), "{} is missing", path.display());
        }
    }
}
