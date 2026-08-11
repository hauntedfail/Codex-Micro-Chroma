use std::path::Path;

pub const LAUNCH_AGENT_LABEL: &str = "com.local.codex-micro-chroma";

pub fn render_launch_agent(executable: &Path, log_directory: &Path) -> String {
    let executable = xml_escape(&executable.to_string_lossy());
    let stdout = xml_escape(&log_directory.join("worker.log").to_string_lossy());
    let stderr = xml_escape(&log_directory.join("worker-error.log").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCH_AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{executable}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::OsString,
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process::{Command, Output},
    };

    use anyhow::{bail, Context, Result};

    use super::{render_launch_agent, LAUNCH_AGENT_LABEL};

    pub struct ServicePaths {
        pub executable: PathBuf,
        pub plist: PathBuf,
        pub log_directory: PathBuf,
    }

    pub fn install() -> Result<ServicePaths> {
        let paths = service_paths()?;
        let current_executable =
            std::env::current_exe().context("could not locate the current executable")?;
        let executable_directory = paths
            .executable
            .parent()
            .context("installed executable path has no parent")?;
        fs::create_dir_all(executable_directory)
            .context("could not create the application support directory")?;
        fs::create_dir_all(&paths.log_directory).context("could not create the log directory")?;
        fs::create_dir_all(
            paths
                .plist
                .parent()
                .context("LaunchAgent path has no parent")?,
        )
        .context("could not create the LaunchAgents directory")?;

        let temporary_executable = paths.executable.with_extension("tmp");
        fs::copy(&current_executable, &temporary_executable)
            .context("could not prepare the worker executable")?;
        fs::set_permissions(&temporary_executable, fs::Permissions::from_mode(0o755))
            .context("could not make the prepared worker executable")?;
        if let Err(error) = sign_executable(&temporary_executable) {
            let _ = fs::remove_file(&temporary_executable);
            return Err(error);
        }
        fs::rename(&temporary_executable, &paths.executable)
            .context("could not atomically install the worker executable")?;

        let plist = render_launch_agent(&paths.executable, &paths.log_directory);
        let temporary_plist = paths.plist.with_extension("plist.tmp");
        fs::write(&temporary_plist, plist).context("could not write the LaunchAgent plist")?;
        fs::rename(&temporary_plist, &paths.plist)
            .context("could not atomically install the LaunchAgent plist")?;

        let domain = launchd_domain();
        let _ = launchctl([
            OsString::from("bootout"),
            domain.clone(),
            paths.plist.clone().into(),
        ]);
        let output = launchctl([
            OsString::from("bootstrap"),
            domain,
            paths.plist.clone().into(),
        ])?;
        ensure_command_success(output, "bootstrap LaunchAgent")?;
        Ok(paths)
    }

    pub fn uninstall() -> Result<ServicePaths> {
        let paths = service_paths()?;
        let _ = launchctl([
            OsString::from("bootout"),
            launchd_domain(),
            paths.plist.clone().into(),
        ]);
        remove_if_present(&paths.plist).context("could not remove the LaunchAgent plist")?;
        remove_if_present(&paths.executable)
            .context("could not remove the installed executable")?;
        Ok(paths)
    }

    fn service_paths() -> Result<ServicePaths> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let home = PathBuf::from(home);
        Ok(ServicePaths {
            executable: home
                .join("Library/Application Support/CodexMicroChroma")
                .join("codex-micro-chroma"),
            plist: home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCH_AGENT_LABEL}.plist")),
            log_directory: home.join("Library/Logs/CodexMicroChroma"),
        })
    }

    fn launchd_domain() -> OsString {
        // SAFETY: geteuid has no preconditions and only returns the current process identity.
        let user_id = unsafe { libc::geteuid() };
        format!("gui/{user_id}").into()
    }

    fn launchctl<const N: usize>(arguments: [OsString; N]) -> Result<Output> {
        Command::new("/bin/launchctl")
            .args(arguments)
            .output()
            .context("could not execute launchctl")
    }

    fn sign_executable(executable: &std::path::Path) -> Result<()> {
        let output = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--identifier", LAUNCH_AGENT_LABEL])
            .arg(executable)
            .output()
            .context("could not execute codesign")?;
        ensure_command_success(output, "ad-hoc sign the installed worker")
    }

    fn ensure_command_success(output: Output, action: &str) -> Result<()> {
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "could not {action}: {}",
            if detail.is_empty() {
                format!("command exited with {}", output.status)
            } else {
                detail
            }
        )
    }

    fn remove_if_present(path: &std::path::Path) -> std::io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::PathBuf;

    use anyhow::{bail, Result};

    pub struct ServicePaths {
        pub executable: PathBuf,
        pub plist: PathBuf,
        pub log_directory: PathBuf,
    }

    pub fn install() -> Result<ServicePaths> {
        bail!("LaunchAgent installation is only available on macOS")
    }

    pub fn uninstall() -> Result<ServicePaths> {
        bail!("LaunchAgent installation is only available on macOS")
    }
}

pub use platform::*;
