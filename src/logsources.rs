//! Logs of containers and services: Docker (or Podman) containers, Kubernetes pods and systemd
//! journal units, listed and read with the tools' own commands (`docker ps` / `docker logs`,
//! `kubectl get pods` / `kubectl logs`, `systemctl list-units` / `journalctl`), on this
//! computer or on a server over SSH. Nothing here changes anything: only lists and logs are
//! read.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::remote::{hide_console, quote, ssh_base};

/// Where the commands run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Host {
    #[default]
    Local,
    /// `target` is `user@host` or `host`.
    Ssh { target: String, port: Option<u16> },
}

impl Host {
    /// "" (this computer), `user@host`, `host`, `user@host:2222` or `ssh://user@host:2222`.
    pub fn parse(s: &str) -> Option<Host> {
        let s = s.trim();
        if s.is_empty() {
            return Some(Host::Local);
        }
        let s = s.strip_prefix("ssh://").unwrap_or(s).trim_end_matches('/');
        let (target, port) = match s.rsplit_once(':') {
            Some((t, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                (t, Some(p.parse().ok()?))
            }
            _ => (s, None),
        };
        let bad = target.is_empty()
            || target.starts_with(['-', '@'])
            || target.ends_with('@')
            || target.contains([' ', '/', '\\', ':']);
        (!bad).then(|| Host::Ssh {
            target: target.to_string(),
            port,
        })
    }

    /// The host name for titles ("" for this computer).
    pub fn label(&self) -> &str {
        match self {
            Host::Local => "",
            Host::Ssh { target, .. } => target.rsplit('@').next().unwrap_or(target),
        }
    }

    /// `program args…` here, or through `ssh` on the server (the arguments quoted for its shell).
    pub fn command(&self, program: &str, args: &[String]) -> Command {
        let mut c = match self {
            Host::Local => {
                let mut c = Command::new(program);
                c.args(args);
                hide_console(&mut c);
                c
            }
            Host::Ssh { target, port } => {
                let mut c = ssh_base(target, *port);
                let line: Vec<String> = std::iter::once(program)
                    .chain(args.iter().map(String::as_str))
                    .map(quote)
                    .collect();
                c.arg(line.join(" "));
                c
            }
        };
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
}

/// A container, as `docker ps` lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, exited, restarting, paused, created, dead.
    pub state: String,
    /// "Up 2 hours", "Exited (1) 3 days ago".
    pub status: String,
}

/// A pod, as `kubectl get pods -o json` describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pod {
    pub namespace: String,
    pub name: String,
    /// The phase, or why a container waits (CrashLoopBackOff, ImagePullBackOff) or was ended.
    pub status: String,
    pub ready: (u32, u32),
    pub restarts: u32,
    pub containers: Vec<String>,
    pub init_containers: Vec<String>,
}

/// A systemd service, as `systemctl list-units` lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    /// active, inactive, failed.
    pub active: String,
    /// running, exited, dead.
    pub sub: String,
    pub description: String,
}

/// The `docker ps` arguments: every container, also stopped ones, a line of tab-separated
/// fields each (`{{json .}}` would make docker work out each container's size).
pub fn containers_args() -> Vec<String> {
    vec![
        "ps".into(),
        "-a".into(),
        "--format".into(),
        "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Status}}".into(),
    ]
}

pub fn parse_containers(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.trim_end_matches('\r').split('\t').collect();
            (f.len() >= 5 && !f[0].is_empty()).then(|| Container {
                id: f[0].into(),
                name: f[1].into(),
                image: f[2].into(),
                state: f[3].into(),
                status: f[4].into(),
            })
        })
        .collect()
}

/// The kubeconfig's contexts.
pub fn contexts_args() -> Vec<String> {
    vec![
        "config".into(),
        "get-contexts".into(),
        "-o".into(),
        "name".into(),
    ]
}

pub fn current_context_args() -> Vec<String> {
    vec!["config".into(), "current-context".into()]
}

/// `kubectl get pods` as JSON: in all namespaces (`all`), or in the context's own.
pub fn pods_args(context: Option<&str>, all: bool) -> Vec<String> {
    let mut a = Vec::new();
    if let Some(c) = context {
        a.extend(["--context".to_string(), c.to_string()]);
    }
    a.extend(["get", "pods", "-o", "json", "--request-timeout=20s"].map(String::from));
    if all {
        a.push("-A".into());
    }
    a
}

pub fn parse_pods(json: &str) -> Result<Vec<Pod>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let mut out = Vec::new();
    for item in v["items"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let meta = &item["metadata"];
        let names = |key: &str| {
            item["spec"][key]
                .as_array()
                .map(|a| a.iter().map(|c| s(&c["name"])).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let containers = names("containers");
        let statuses = item["status"]["containerStatuses"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let ready = statuses
            .iter()
            .filter(|c| c["ready"].as_bool() == Some(true))
            .count() as u32;
        let restarts = statuses
            .iter()
            .map(|c| c["restartCount"].as_u64().unwrap_or(0) as u32)
            .sum();
        // As kubectl's STATUS column: a waiting or ended container's reason before the phase.
        let reason = statuses.iter().find_map(|c| {
            c["state"]["waiting"]["reason"].as_str().or_else(|| {
                c["state"]["terminated"]["reason"]
                    .as_str()
                    .filter(|r| *r != "Completed")
            })
        });
        let status = if meta["deletionTimestamp"].is_string() {
            "Terminating".to_string()
        } else {
            reason
                .map(String::from)
                .or_else(|| item["status"]["reason"].as_str().map(String::from))
                .unwrap_or_else(|| s(&item["status"]["phase"]))
        };
        out.push(Pod {
            namespace: s(&meta["namespace"]),
            name: s(&meta["name"]),
            status,
            ready: (ready, containers.len() as u32),
            restarts,
            containers,
            init_containers: names("initContainers"),
        });
    }
    Ok(out)
}

/// `systemctl list-units`: the services, also inactive ones.
pub fn units_args() -> Vec<String> {
    [
        "list-units",
        "--type=service",
        "--all",
        "--plain",
        "--no-legend",
        "--no-pager",
    ]
    .map(String::from)
    .to_vec()
}

pub fn parse_units(out: &str) -> Vec<Unit> {
    out.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let name = w.next()?;
            let (_load, active, sub) = (w.next()?, w.next()?, w.next()?);
            name.contains('.').then(|| Unit {
                name: name.into(),
                active: active.into(),
                sub: sub.into(),
                description: w.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// Whose logs to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `tool` is `docker` or `podman`.
    Container {
        tool: String,
        id: String,
        name: String,
    },
    /// `container` None: all of the pod's containers, each line prefixed with its name;
    /// `previous`: the container's run before its last restart.
    Pod {
        context: Option<String>,
        namespace: String,
        pod: String,
        container: Option<String>,
        previous: bool,
    },
    /// `unit` None: the whole journal, or (`kernel`) the kernel's messages.
    Journal { unit: Option<String>, kernel: bool },
}

/// How much of the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    /// The last lines.
    Lines(u32),
    /// The last minutes.
    Minutes(u32),
    All,
}

/// The program and arguments that print `target`'s log (and, `follow`, keep printing what is
/// added); `timestamps` asks Docker and Kubernetes to put the time before each line (the
/// journal has it anyway).
pub fn log_command(
    target: &Target,
    amount: Amount,
    follow: bool,
    timestamps: bool,
) -> (String, Vec<String>) {
    let mut a: Vec<String> = Vec::new();
    match target {
        Target::Container { tool, id, .. } => {
            a.push("logs".into());
            if timestamps {
                a.push("--timestamps".into());
            }
            match amount {
                Amount::Lines(n) => a.extend(["--tail".into(), n.to_string()]),
                Amount::Minutes(m) => a.extend(["--since".into(), format!("{m}m")]),
                Amount::All => {}
            }
            if follow {
                a.push("--follow".into());
            }
            a.push(id.clone());
            (tool.clone(), a)
        }
        Target::Pod {
            context,
            namespace,
            pod,
            container,
            previous,
        } => {
            if let Some(c) = context {
                a.extend(["--context".into(), c.clone()]);
            }
            a.extend(["logs".into(), "-n".into(), namespace.clone(), pod.clone()]);
            match container {
                Some(c) => a.extend(["-c".into(), c.clone()]),
                None => a.extend(["--all-containers=true".into(), "--prefix=true".into()]),
            }
            if *previous {
                a.push("--previous".into());
            }
            if timestamps {
                a.push("--timestamps".into());
            }
            match amount {
                Amount::Lines(n) => a.push(format!("--tail={n}")),
                Amount::Minutes(m) => a.push(format!("--since={m}m")),
                Amount::All => {}
            }
            if follow {
                a.push("--follow".into());
            }
            ("kubectl".into(), a)
        }
        Target::Journal { unit, kernel } => {
            match (unit, kernel) {
                (Some(u), _) => a.extend(["-u".into(), u.clone()]),
                (None, true) => a.push("-k".into()),
                (None, false) => {}
            }
            // ISO times, which BigText's time tools read.
            a.extend(["-o".into(), "short-iso".into(), "--no-pager".into()]);
            match amount {
                Amount::Lines(n) => a.extend(["-n".into(), n.to_string()]),
                Amount::Minutes(m) => a.push(format!("--since=-{m}min")),
                // Following shows only the last ten lines unless told otherwise.
                Amount::All if follow => a.push("--no-tail".into()),
                Amount::All => {}
            }
            if follow {
                a.push("--follow".into());
            }
            ("journalctl".into(), a)
        }
    }
}

impl Target {
    /// The tab's title: `api-1 (docker)`, `payment-7d9f/app (prod)`, `nginx.service (journal)`,
    /// with the server's name after it when there is one.
    pub fn title(&self, host: &Host) -> String {
        let (name, kind) = match self {
            Target::Container { tool, name, .. } => (name.clone(), tool.clone()),
            Target::Pod {
                namespace,
                pod,
                container,
                previous,
                ..
            } => {
                let mut n = match container {
                    Some(c) => format!("{pod}/{c}"),
                    None => pod.clone(),
                };
                if *previous {
                    n.push_str(" (previous)");
                }
                (n, namespace.clone())
            }
            Target::Journal { unit, kernel } => (
                unit.clone().unwrap_or_else(|| {
                    if *kernel {
                        "kernel".into()
                    } else {
                        "journal".into()
                    }
                }),
                "journal".into(),
            ),
        };
        match host.label() {
            "" => format!("{name} ({kind})"),
            h => format!("{name} ({kind}, {h})"),
        }
    }

    /// A file name for the copy: the name with `.log`, so it is highlighted as a log.
    pub fn file_name(&self) -> String {
        let raw = match self {
            Target::Container { name, .. } => name.clone(),
            Target::Pod { pod, container, .. } => container
                .as_ref()
                .map_or(pod.clone(), |c| format!("{pod}-{c}")),
            Target::Journal { unit, kernel } => unit.clone().unwrap_or_else(|| {
                if *kernel {
                    "kernel".into()
                } else {
                    "journal".into()
                }
            }),
        };
        let clean: String = raw
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{}.log", if clean.is_empty() { "log" } else { &clean })
    }
}

/// Runs a listing command to its end (at most `timeout`) and returns what it printed; the error
/// is the program's last line of errors, or that it was not found.
pub fn run_list(mut cmd: Command, program: &str, timeout: Duration) -> Result<String, String> {
    let mut child = cmd.spawn().map_err(|e| not_found(&e, program))?;
    let mut out = child.stdout.take().ok_or("no output")?;
    let mut err = child.stderr.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} did not answer in {} s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let err = err_reader.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(crate::remote::ssh_error(&err, status.code()))
    }
}

/// Bytes of a line without a line break written as they are (a line longer is cut).
const LINE_MAX: usize = 1 << 20;
/// Bytes of errors kept for the message when the command fails.
const ERRORS_KEPT: usize = 4096;

/// What [`copy_merged`] copied.
#[derive(Debug, Default)]
pub struct Copied {
    pub bytes: u64,
    /// The end of the error output, for the message when the command fails.
    pub errors: String,
}

/// Copies a command's output and its errors into `file`, whole lines at a time so neither
/// cuts the other's lines (`docker logs` sends a container's stderr as errors), until both end.
/// ssh's note about adding a host to the known hosts is left out. `progress` counts the bytes.
pub fn copy_merged<O: Read + Send + 'static, E: Read + Send + 'static>(
    out: O,
    err: E,
    file: std::fs::File,
    progress: Option<crate::jobs::Progress>,
) -> io::Result<Copied> {
    let file = std::sync::Arc::new(std::sync::Mutex::new(file));
    let (f2, p2) = (file.clone(), progress.clone());
    let errors = std::thread::spawn(move || copy_lines(err, &f2, true, p2.as_ref()));
    let a = copy_lines(out, &file, false, progress.as_ref());
    let b = errors
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("the error reader stopped")));
    let (out_bytes, _) = a?;
    let (err_bytes, tail) = b?;
    Ok(Copied {
        bytes: out_bytes + err_bytes,
        errors: String::from_utf8_lossy(&tail).into_owned(),
    })
}

fn copy_lines(
    mut from: impl Read,
    to: &std::sync::Mutex<std::fs::File>,
    errors: bool,
    progress: Option<&crate::jobs::Progress>,
) -> io::Result<(u64, Vec<u8>)> {
    use std::io::Write;
    let mut buf = vec![0u8; 64 * 1024];
    let (mut pending, mut tail, mut total) = (Vec::new(), Vec::new(), 0u64);
    let mut put = |b: &[u8], tail: &mut Vec<u8>| -> io::Result<()> {
        let kept: Vec<u8>;
        let b = if errors {
            kept = b
                .split_inclusive(|&c| c == b'\n')
                .filter(|l| !l.starts_with(b"Warning: Permanently added"))
                .flatten()
                .copied()
                .collect();
            &kept[..]
        } else {
            b
        };
        if b.is_empty() {
            return Ok(());
        }
        to.lock()
            .map_err(|_| io::Error::other("the file is not available"))?
            .write_all(b)?;
        total += b.len() as u64;
        if let Some(p) = progress {
            p.add(b.len() as u64);
        }
        if errors {
            tail.extend_from_slice(b);
            if tail.len() > ERRORS_KEPT {
                tail.drain(..tail.len() - ERRORS_KEPT);
            }
        }
        Ok(())
    };
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        pending.extend_from_slice(&buf[..n]);
        if let Some(cut) = memchr::memrchr(b'\n', &pending) {
            put(&pending[..=cut], &mut tail)?;
            pending.drain(..=cut);
        } else if pending.len() > LINE_MAX {
            put(&pending, &mut tail)?;
            pending.clear();
        }
    }
    put(&pending, &mut tail)?;
    Ok((total, tail))
}

/// "docker was not found" for a program that is not installed (or not on the PATH).
pub fn not_found(e: &io::Error, program: &str) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        format!("{program} was not found")
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(Host::parse(" "), Some(Host::Local));
        assert_eq!(
            Host::parse("deploy@web1"),
            Some(Host::Ssh {
                target: "deploy@web1".into(),
                port: None
            })
        );
        assert_eq!(
            Host::parse("ssh://root@10.0.0.5:2222/"),
            Some(Host::Ssh {
                target: "root@10.0.0.5".into(),
                port: Some(2222)
            })
        );
        assert_eq!(
            Host::parse("web1:22").map(|h| h.label().to_string()),
            Some("web1".into())
        );
        assert_eq!(Host::parse("-oProxyCommand=x"), None);
        assert_eq!(Host::parse("a b"), None);
        assert_eq!(Host::parse("deploy@web1").unwrap().label(), "web1");
    }

    #[test]
    fn listings() {
        let ps = "c5bed015af40\tartex-artex-1\tghcr.io/x/artex:latest\texited\tExited (137) 2 days ago\r\n4a1\tapi\tnginx:1.27\trunning\tUp 3 hours\n\nbroken line\n";
        assert_eq!(
            parse_containers(ps),
            [
                Container {
                    id: "c5bed015af40".into(),
                    name: "artex-artex-1".into(),
                    image: "ghcr.io/x/artex:latest".into(),
                    state: "exited".into(),
                    status: "Exited (137) 2 days ago".into()
                },
                Container {
                    id: "4a1".into(),
                    name: "api".into(),
                    image: "nginx:1.27".into(),
                    state: "running".into(),
                    status: "Up 3 hours".into()
                },
            ]
        );
        let units = "nginx.service     loaded active   running A high performance web server\ncron.service loaded inactive dead Regular background program processing daemon\n\n0 loaded units listed.\n";
        assert_eq!(
            parse_units(units),
            [
                Unit {
                    name: "nginx.service".into(),
                    active: "active".into(),
                    sub: "running".into(),
                    description: "A high performance web server".into()
                },
                Unit {
                    name: "cron.service".into(),
                    active: "inactive".into(),
                    sub: "dead".into(),
                    description: "Regular background program processing daemon".into()
                },
            ]
        );
        let pods = r#"{"items": [
            {"metadata": {"name": "payment-7d9f", "namespace": "prod"},
             "spec": {"initContainers": [{"name": "migrate"}], "containers": [{"name": "app"}, {"name": "proxy"}]},
             "status": {"phase": "Running", "containerStatuses": [
                {"name": "app", "ready": false, "restartCount": 5, "state": {"waiting": {"reason": "CrashLoopBackOff"}}},
                {"name": "proxy", "ready": true, "restartCount": 0, "state": {"running": {}}}]}},
            {"metadata": {"name": "web-1", "namespace": "default", "deletionTimestamp": "2024-05-01T10:00:00Z"},
             "spec": {"containers": [{"name": "web"}]}, "status": {"phase": "Running"}},
            {"metadata": {"name": "job-x", "namespace": "default"}, "spec": {"containers": [{"name": "job"}]},
             "status": {"phase": "Succeeded", "containerStatuses": [{"name": "job", "ready": false, "restartCount": 0, "state": {"terminated": {"reason": "Completed"}}}]}}
        ]}"#;
        let p = parse_pods(pods).unwrap();
        assert_eq!(
            p[0],
            Pod {
                namespace: "prod".into(),
                name: "payment-7d9f".into(),
                status: "CrashLoopBackOff".into(),
                ready: (1, 2),
                restarts: 5,
                containers: vec!["app".into(), "proxy".into()],
                init_containers: vec!["migrate".into()]
            }
        );
        assert_eq!(
            (p[1].status.as_str(), p[2].status.as_str()),
            ("Terminating", "Succeeded")
        );
        assert!(parse_pods("not json").is_err());
        assert_eq!(parse_pods(r#"{"items": []}"#).unwrap(), []);
    }

    #[test]
    fn log_commands() {
        let c = Target::Container {
            tool: "docker".into(),
            id: "4a1".into(),
            name: "api".into(),
        };
        assert_eq!(
            log_command(&c, Amount::Lines(1000), true, false),
            (
                "docker".into(),
                ["logs", "--tail", "1000", "--follow", "4a1"]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert_eq!(
            log_command(&c, Amount::Minutes(60), false, true).1,
            ["logs", "--timestamps", "--since", "60m", "4a1"]
        );
        let p = Target::Pod {
            context: Some("prod-eu".into()),
            namespace: "prod".into(),
            pod: "payment-7d9f".into(),
            container: Some("app".into()),
            previous: true,
        };
        assert_eq!(
            log_command(&p, Amount::All, false, false),
            (
                "kubectl".into(),
                [
                    "--context",
                    "prod-eu",
                    "logs",
                    "-n",
                    "prod",
                    "payment-7d9f",
                    "-c",
                    "app",
                    "--previous"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        let all = Target::Pod {
            context: None,
            namespace: "prod".into(),
            pod: "p".into(),
            container: None,
            previous: false,
        };
        assert_eq!(
            log_command(&all, Amount::Lines(10), true, true).1,
            [
                "logs",
                "-n",
                "prod",
                "p",
                "--all-containers=true",
                "--prefix=true",
                "--timestamps",
                "--tail=10",
                "--follow"
            ]
        );
        let j = Target::Journal {
            unit: Some("nginx.service".into()),
            kernel: false,
        };
        assert_eq!(
            log_command(&j, Amount::Minutes(15), true, true),
            (
                "journalctl".into(),
                [
                    "-u",
                    "nginx.service",
                    "-o",
                    "short-iso",
                    "--no-pager",
                    "--since=-15min",
                    "--follow"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        assert_eq!(
            log_command(
                &Target::Journal {
                    unit: None,
                    kernel: false
                },
                Amount::All,
                true,
                false
            )
            .1,
            ["-o", "short-iso", "--no-pager", "--no-tail", "--follow"]
        );
        assert_eq!(
            log_command(
                &Target::Journal {
                    unit: None,
                    kernel: true
                },
                Amount::All,
                false,
                false
            )
            .1,
            ["-k", "-o", "short-iso", "--no-pager"]
        );
        let host = Host::parse("deploy@web1").unwrap();
        assert_eq!(
            (c.title(&Host::Local), c.title(&host)),
            ("api (docker)".to_string(), "api (docker, web1)".to_string())
        );
        assert_eq!(p.title(&Host::Local), "payment-7d9f/app (previous) (prod)");
        assert_eq!(
            (j.file_name(), p.file_name()),
            (
                "nginx.service.log".to_string(),
                "payment-7d9f-app.log".to_string()
            )
        );
    }

    #[test]
    fn commands_here_and_over_ssh() {
        let args: Vec<String> = ["logs", "--tail", "10", "it's"].map(String::from).to_vec();
        let c = Host::Local.command("docker", &args);
        assert_eq!(c.get_program(), "docker");
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            ["logs", "--tail", "10", "it's"]
        );
        let c = Host::Ssh {
            target: "deploy@web1".into(),
            port: Some(2222),
        }
        .command("docker", &args);
        assert_eq!(c.get_program(), "ssh");
        let a: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            a.last().map(String::as_str),
            Some(r#"'docker' 'logs' '--tail' '10' 'it'\''s'"#)
        );
        assert!(
            a.windows(2).any(|w| w == ["-p", "2222"]) && a.contains(&"deploy@web1".to_string())
        );
    }

    /// Writes `parts` with a pause after each, as a program printing as it goes.
    struct Slow(std::collections::VecDeque<&'static [u8]>);
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(5));
            let Some(p) = self.0.pop_front() else {
                return Ok(0);
            };
            buf[..p.len()].copy_from_slice(p);
            Ok(p.len())
        }
    }

    #[test]
    fn output_and_errors_merge_by_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.log");
        let out = Slow(
            [
                &b"out one, in "[..],
                b"two parts\nout 2\n",
                b"no line break at the end",
            ]
            .into(),
        );
        let err = Slow([&b"Warning: Permanently added 'h' (ED25519) to the list of known hosts.\r\nerr "[..], b"line\n"].into());
        let progress = crate::jobs::Progress::new(0);
        let c = copy_merged(
            out,
            err,
            std::fs::File::create(&path).unwrap(),
            Some(progress.clone()),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "err line",
                "no line break at the end",
                "out 2",
                "out one, in two parts"
            ]
        );
        assert_eq!(
            (c.bytes, progress.done()),
            (text.len() as u64, text.len() as u64)
        );
        assert_eq!(c.errors, "err line\n");
    }

    #[cfg(unix)]
    #[test]
    fn listing_runs_and_fails_with_the_error() {
        let ok = run_list(
            Host::Local.command("sh", &["-c".into(), "printf 'a\\tb\\tc\\td\\te\\n'".into()]),
            "sh",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(parse_containers(&ok).len(), 1);
        let err = run_list(
            Host::Local.command(
                "sh",
                &[
                    "-c".into(),
                    "echo 'Cannot connect to the Docker daemon' >&2; exit 1".into(),
                ],
            ),
            "sh",
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(err, "Cannot connect to the Docker daemon");
        assert_eq!(
            run_list(
                Host::Local.command("no-such-program-x", &[]),
                "no-such-program-x",
                Duration::from_secs(5)
            )
            .unwrap_err(),
            "no-such-program-x was not found"
        );
    }
}
