//! Phase 0 qualification: probe specifications, sandbox observations and
//! their evaluation against the sandbox invariants in the foundation plan.
//!
//! The harness (`bokkie-qualify`) builds a [`ProbeSpec`] from the outside,
//! runs `bokkie-sandbox-probe` inside the Codex sandbox, and evaluates the
//! returned [`ProbeReport`]. Every check is a real read, write, process
//! inspection or connection attempt. The probe never prints file contents.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod layout;

/// Whether an operation is expected to succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    Allow,
    Deny,
}

/// One planned check. `label` names the invariant first, as in `inv1:...`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathSpec {
    pub label: String,
    pub path: String,
    pub expect: Expect,
}

/// A process outside the sandbox that the sandbox must not reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSpec {
    pub pid: u32,
    /// The process name as seen from outside the sandbox.
    pub comm: String,
    /// The start time in clock ticks since boot. PID numbers differ between
    /// namespaces but this does not, so (comm, start time) identifies the
    /// same process on both sides.
    pub start_time: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSpec {
    pub reads: Vec<PathSpec>,
    /// Writes create a uniquely named file in a directory, or open an existing
    /// file for appending without writing any bytes.
    pub writes: Vec<PathSpec>,
    /// Files created by allowed writes are removed unless listed here.
    #[serde(default)]
    pub keep_written: Vec<String>,
    pub peers: Vec<PeerSpec>,
    /// The `/proc/1/cmdline` seen outside the sandbox.
    pub outer_pid1_cmdline: String,
    /// A `host:port` to connect to, with the expected result.
    pub connect: Option<PathSpec>,
    /// Users whose names in a directory listing reveal their data.
    #[serde(default)]
    pub other_users: Vec<String>,
}

/// The result of one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Outcome {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    pub fn from_io<T>(result: std::io::Result<T>) -> Self {
        match result {
            Ok(_) => Self::ok(),
            Err(error) => Self {
                ok: false,
                error: Some(describe(&error)),
            },
        }
    }
}

fn describe(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("{:?} (os error {code})", error.kind()),
        None => error.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathResult {
    pub label: String,
    pub path: String,
    pub expect: Expect,
    pub outcome: Outcome,
    /// Entry names of a directory that could be listed. Names only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerResult {
    pub pid: u32,
    pub outer_comm: String,
    pub outer_start_time: Option<u64>,
    /// What the sandbox sees at `/proc/<pid>/comm`, if anything.
    pub seen_comm: Option<String>,
    pub seen_start_time: Option<u64>,
    pub root: Outcome,
    pub cwd: Outcome,
    pub environ: Outcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub uid: Option<String>,
    pub gid: Option<String>,
    pub no_new_privs: Option<String>,
    pub seccomp: Option<String>,
    pub seccomp_filters: Option<String>,
    pub cap_eff: Option<String>,
    pub apparmor: Option<String>,
    pub namespaces: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub identity: Identity,
    pub pid1_cmdline: Option<String>,
    pub visible_pids: Vec<(u32, String)>,
    /// The `/proc/self/mountinfo` lines for mounts at `/proc`.
    pub proc_mounts: Vec<String>,
    pub reads: Vec<PathResult>,
    pub writes: Vec<PathResult>,
    pub peers: Vec<PeerResult>,
    pub connect: Option<PathResult>,
}

// ---------------------------------------------------------------------------
// Running the probe (inside the sandbox)

pub fn run_probe(spec: &ProbeSpec, nonce: &str) -> ProbeReport {
    ProbeReport {
        identity: identity(),
        pid1_cmdline: read_cmdline(Path::new("/proc/1/cmdline")),
        visible_pids: visible_pids(),
        proc_mounts: std::fs::read_to_string("/proc/self/mountinfo")
            .map(|mounts| {
                mounts
                    .lines()
                    .filter(|line| line.split(' ').nth(4) == Some("/proc"))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        reads: spec
            .reads
            .iter()
            .map(|check| {
                let mut read = result(check, try_read(Path::new(&check.path)));
                read.entries = list(Path::new(&check.path));
                read
            })
            .collect(),
        writes: spec
            .writes
            .iter()
            .map(|check| {
                let keep = spec.keep_written.contains(&check.label);
                result(check, try_write(Path::new(&check.path), nonce, keep))
            })
            .collect(),
        peers: spec.peers.iter().map(probe_peer).collect(),
        connect: spec
            .connect
            .as_ref()
            .map(|check| result(check, try_connect(&check.path))),
    }
}

fn result(check: &PathSpec, outcome: Outcome) -> PathResult {
    PathResult {
        label: check.label.clone(),
        path: check.path.clone(),
        expect: check.expect,
        outcome,
        entries: Vec::new(),
    }
}

fn list(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Reads one byte of a file, or lists a directory. Contents are discarded.
pub fn try_read(path: &Path) -> Outcome {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Outcome::from_io(std::fs::read_dir(path)),
        Ok(_) => Outcome::from_io(std::fs::File::open(path).and_then(|mut file| {
            let mut byte = [0u8; 1];
            file.read(&mut byte)
        })),
        Err(error) => Outcome::from_io::<()>(Err(error)),
    }
}

/// Creates a file inside a directory, or opens an existing file for
/// appending without writing. A created file is removed unless `keep`.
pub fn try_write(path: &Path, nonce: &str, keep: bool) -> Outcome {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let target = path.join(format!(".bokkie-probe-{nonce}"));
            let outcome = Outcome::from_io(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target),
            );
            if outcome.ok && !keep {
                let _ = std::fs::remove_file(&target);
            }
            outcome
        }
        Ok(_) => Outcome::from_io(std::fs::OpenOptions::new().append(true).open(path)),
        // A missing path is tested by creating it, as a sandbox might.
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let outcome = Outcome::from_io(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path),
            );
            if outcome.ok && !keep {
                let _ = std::fs::remove_file(path);
            }
            outcome
        }
        Err(error) => Outcome::from_io::<()>(Err(error)),
    }
}

fn try_connect(target: &str) -> Outcome {
    let addresses: Vec<SocketAddr> = match target.to_socket_addrs() {
        Ok(addresses) => addresses.collect(),
        Err(error) => return Outcome::from_io::<()>(Err(error)),
    };
    let mut last = Outcome {
        ok: false,
        error: Some("no address".into()),
    };
    for address in addresses {
        last = Outcome::from_io(TcpStream::connect_timeout(&address, Duration::from_secs(5)));
        if last.ok {
            break;
        }
    }
    last
}

fn probe_peer(peer: &PeerSpec) -> PeerResult {
    let base = format!("/proc/{}", peer.pid);
    PeerResult {
        pid: peer.pid,
        outer_comm: peer.comm.clone(),
        outer_start_time: peer.start_time,
        seen_comm: std::fs::read_to_string(format!("{base}/comm"))
            .ok()
            .map(|comm| comm.trim().to_owned()),
        seen_start_time: start_time(peer.pid),
        root: Outcome::from_io(std::fs::read_dir(format!("{base}/root"))),
        cwd: Outcome::from_io(std::fs::read_dir(format!("{base}/cwd"))),
        environ: try_read(Path::new(&format!("{base}/environ"))),
    }
}

/// Field 22 of `/proc/<pid>/stat`: start time in clock ticks since boot.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces; fields resume after its ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split(' ').nth(19)?.parse().ok()
}

pub fn read_cmdline(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| {
        bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Every numeric `/proc` entry with its `comm`.
pub fn visible_pids() -> Vec<(u32, String)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids: Vec<(u32, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .map(|pid| {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|comm| comm.trim().to_owned())
                .unwrap_or_default();
            (pid, comm)
        })
        .collect();
    pids.sort();
    pids
}

pub fn identity() -> Identity {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_owned())
    };
    let namespaces = ["pid", "mnt", "user", "net", "ipc", "uts", "cgroup"]
        .into_iter()
        .filter_map(|ns| {
            std::fs::read_link(format!("/proc/self/ns/{ns}"))
                .ok()
                .map(|link| (ns.to_owned(), link.display().to_string()))
        })
        .collect();
    Identity {
        uid: field("Uid:"),
        gid: field("Gid:"),
        no_new_privs: field("NoNewPrivs:"),
        seccomp: field("Seccomp:"),
        seccomp_filters: field("Seccomp_filters:"),
        cap_eff: field("CapEff:"),
        apparmor: std::fs::read_to_string("/proc/self/attr/current")
            .ok()
            .map(|label| label.trim_end_matches(['\0', '\n']).to_owned()),
        namespaces,
    }
}

// ---------------------------------------------------------------------------
// Evaluation (outside the sandbox)

/// One evaluated check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub invariant: String,
    pub label: String,
    pub pass: bool,
    pub detail: String,
}

/// Evaluates a report. `outer_visible` reports whether a file the sandbox
/// wrote under a shared path such as `/tmp` is visible outside it.
pub fn evaluate(report: &ProbeReport, spec: &ProbeSpec) -> Vec<Finding> {
    let mut findings = Vec::new();
    for check in report
        .reads
        .iter()
        .chain(&report.writes)
        .chain(&report.connect)
    {
        if check.label.starts_with(layout::SHARED_WRITE_PREFIX) {
            continue; // Judged by visibility outside the sandbox instead.
        }
        let expected_ok = check.expect == Expect::Allow;
        let mut pass = check.outcome.ok == expected_ok;
        let mut detail = format!(
            "{} expected {:?}, observed {}",
            check.path,
            check.expect,
            match &check.outcome.error {
                None => "success".to_owned(),
                Some(error) => error.clone(),
            }
        );
        // Bubblewrap creates the parents of a mounted workspace, so the
        // shared users directory may list; it must not name anyone else.
        if check.label == USERS_DIR_LABEL && check.outcome.ok {
            let leaked: Vec<&String> = check
                .entries
                .iter()
                .filter(|e| spec.other_users.contains(e))
                .collect();
            pass = leaked.is_empty();
            detail = format!(
                "{} lists {:?}; other users named: {leaked:?}",
                check.path, check.entries
            );
        }
        findings.push(Finding {
            invariant: invariant_of(&check.label),
            label: check.label.clone(),
            pass,
            detail,
        });
    }

    // Invariant 4: a fresh procfs, not the container's.
    let pid1 = report.pid1_cmdline.clone().unwrap_or_default();
    let fresh = !pid1.is_empty() && pid1 != spec.outer_pid1_cmdline;
    findings.push(Finding {
        invariant: "inv4".into(),
        label: "inv4:fresh-procfs".into(),
        pass: fresh,
        detail: format!(
            "sandbox /proc/1/cmdline is {pid1:?}; outside it is {:?}; {} pids visible",
            spec.outer_pid1_cmdline,
            report.visible_pids.len()
        ),
    });
    let mut same_processes = Vec::new();
    for peer in &report.peers {
        let same = peer.seen_comm.as_deref() == Some(peer.outer_comm.as_str())
            && peer.seen_start_time.is_some()
            && peer.seen_start_time == peer.outer_start_time;
        if same {
            same_processes.push(peer);
        }
    }
    findings.push(Finding {
        invariant: "inv4".into(),
        label: "inv4:outer-processes".into(),
        pass: same_processes.is_empty(),
        detail: if same_processes.is_empty() {
            format!(
                "none of {} outer processes is visible inside the sandbox",
                report.peers.len()
            )
        } else {
            same_processes
                .iter()
                .map(|p| {
                    format!(
                        "pid {} {} visible (root {}, cwd {}, environ {})",
                        p.pid, p.outer_comm, p.root.ok, p.cwd.ok, p.environ.ok
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        },
    });
    findings
}

/// The check that lists the parent of every user's directory.
pub const USERS_DIR_LABEL: &str = "inv3:users-dir";

fn invariant_of(label: &str) -> String {
    label.split(':').next().unwrap_or(label).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bokkie-qualify-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_and_writes_report_real_outcomes() {
        let dir = scratch();
        std::fs::write(dir.join("file"), "secret").unwrap();
        assert!(try_read(&dir.join("file")).ok);
        assert!(try_read(&dir).ok);
        let missing = try_read(&dir.join("missing"));
        assert!(!missing.ok);
        assert!(missing.error.unwrap().contains("NotFound"));

        assert!(try_write(&dir, "n1", false).ok);
        assert!(!dir.join(".bokkie-probe-n1").exists(), "removed after");
        assert!(try_write(&dir, "n2", true).ok);
        assert!(dir.join(".bokkie-probe-n2").exists(), "kept");
        // Appending to an existing file changes nothing.
        assert!(try_write(&dir.join("file"), "n3", false).ok);
        assert_eq!(std::fs::read_to_string(dir.join("file")).unwrap(), "secret");
        assert!(!try_write(&dir.join("no/such/dir"), "n4", false).ok);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn report(pid1: &str, peer_seen: Option<&str>, read_ok: bool) -> ProbeReport {
        ProbeReport {
            identity: Identity::default(),
            pid1_cmdline: Some(pid1.into()),
            visible_pids: vec![(1, "bwrap".into())],
            proc_mounts: Vec::new(),
            reads: vec![PathResult {
                label: "inv1:alice-codex-home".into(),
                path: "/data/users/alice/codex".into(),
                expect: Expect::Deny,
                outcome: Outcome {
                    ok: read_ok,
                    error: (!read_ok).then(|| "PermissionDenied (os error 13)".into()),
                },
                entries: Vec::new(),
            }],
            writes: Vec::new(),
            peers: vec![PeerResult {
                pid: 7,
                outer_comm: "codex".into(),
                outer_start_time: Some(42),
                seen_comm: peer_seen.map(str::to_owned),
                seen_start_time: peer_seen.map(|_| 42),
                root: Outcome::from_io::<()>(Err(std::io::Error::from_raw_os_error(2))),
                cwd: Outcome::from_io::<()>(Err(std::io::Error::from_raw_os_error(2))),
                environ: Outcome::from_io::<()>(Err(std::io::Error::from_raw_os_error(2))),
            }],
            connect: None,
        }
    }

    fn spec() -> ProbeSpec {
        ProbeSpec {
            outer_pid1_cmdline: "/sbin/docker-init -- bokkie-qualify idle".into(),
            ..ProbeSpec::default()
        }
    }

    #[test]
    fn a_fresh_procfs_and_denied_reads_pass() {
        let findings = evaluate(&report("bwrap --args", None, false), &spec());
        assert!(findings.iter().all(|f| f.pass), "{findings:#?}");
    }

    #[test]
    fn the_silent_procfs_fallback_fails_invariant_4() {
        // Without a fresh procfs the sandbox sees the container's PID 1 and
        // the peer's name, even if its links happen to be unreadable.
        let findings = evaluate(
            &report(
                "/sbin/docker-init -- bokkie-qualify idle",
                Some("codex"),
                false,
            ),
            &spec(),
        );
        let failed: Vec<_> = findings.iter().filter(|f| !f.pass).collect();
        assert_eq!(failed.len(), 2, "{findings:#?}");
        assert!(failed.iter().all(|f| f.invariant == "inv4"));
    }

    #[test]
    fn a_readable_codex_home_fails_invariant_1() {
        let findings = evaluate(&report("bwrap", None, true), &spec());
        let failed: Vec<_> = findings.iter().filter(|f| !f.pass).collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].invariant, "inv1");
    }
}
