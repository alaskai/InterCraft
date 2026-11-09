// intercraft-geo: geometry backend trait and the Python (build123d) worker client.
// bytemuck 1, serde 1, serde_json 1, anyhow 1

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct FeatureInput {
    pub id: String,
    pub code: String,
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct RebuildRequest {
    pub part: String,
    pub features: Vec<FeatureInput>,
}

/// Numbers reported after every feature. All lengths in mm.
#[derive(Debug, Clone, Deserialize)]
pub struct Report {
    pub volume: f64,
    pub volume_before: Option<f64>,
    pub bbox_min: [f64; 3],
    pub bbox_max: [f64; 3],
    pub faces: u32,
    pub edges: u32,
    pub valid: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureResult {
    pub part: String,
    pub feature_id: String,
    pub index: usize,
    pub ok: bool,
    #[serde(default)]
    pub cached: bool,
    pub mesh: Option<PathBuf>,
    pub report: Option<Report>,
    pub error: Option<String>,
    pub ms: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum GeoEvent {
    Ready { build123d: String },
    Feature(FeatureResult),
    Done { part: String },
    Error { error: String },
    /// The worker process ended. Not sent by the worker itself.
    #[serde(skip)]
    Exited,
}

/// Anything that can turn a feature list into geometry.
/// Results arrive as events so the UI can show each step as it lands.
pub trait GeometryBackend {
    fn rebuild(&mut self, req: RebuildRequest) -> Result<()>;
    fn try_recv(&mut self) -> Option<GeoEvent>;
}

pub struct PythonWorker {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<GeoEvent>,
    cache_dir: PathBuf,
    next_id: u64,
}

impl PythonWorker {
    /// `wake` is called from the reader thread whenever an event arrives.
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Result<Self> {
        let python = find_python();
        let script = find_worker_script();
        if !script.exists() {
            bail!("worker script not found at {}", script.display());
        }

        let mut cmd = Command::new(&python);
        cmd.arg(&script)
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting worker with {}", python.display()))?;
        let stdin = child.stdin.take().context("worker stdin")?;
        let stdout = child.stdout.take().context("worker stdout")?;

        let (tx, events) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let event = serde_json::from_str::<GeoEvent>(&line).unwrap_or_else(|e| {
                    GeoEvent::Error { error: format!("bad worker message: {e}: {line}") }
                });
                if tx.send(event).is_err() {
                    return;
                }
                wake();
            }
            let _ = tx.send(GeoEvent::Exited);
            wake();
        });

        let cache_dir = std::env::temp_dir().join("intercraft-cache");
        Ok(Self { child, stdin, events, cache_dir, next_id: 1 })
    }
}

impl GeometryBackend for PythonWorker {
    fn rebuild(&mut self, req: RebuildRequest) -> Result<()> {
        let msg = serde_json::json!({
            "id": self.next_id,
            "cmd": "rebuild",
            "part": req.part,
            "features": req.features,
            "cache_dir": self.cache_dir,
        });
        self.next_id += 1;
        writeln!(self.stdin, "{msg}").context("writing to worker")?;
        self.stdin.flush().context("flushing worker stdin")?;
        Ok(())
    }

    fn try_recv(&mut self) -> Option<GeoEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for PythonWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_python() -> PathBuf {
    if let Some(p) = std::env::var_os("INTERCRAFT_PYTHON") {
        return p.into();
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let venv = Path::new(&local).join("InterCraft/venv/Scripts/python.exe");
        if venv.exists() {
            return venv;
        }
    }
    "python".into()
}

fn find_worker_script() -> PathBuf {
    if let Some(p) = std::env::var_os("INTERCRAFT_WORKER") {
        return p.into();
    }
    // Dev layout: the worker sits next to the crates folder.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../worker/worker.py")
}

/// Triangle mesh as written by the worker (.icm file).
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.len() < 12 || &bytes[0..4] != b"ICM1" {
            bail!("{} is not an ICM1 mesh", path.display());
        }
        let nv = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
        let nt = u32::from_le_bytes(bytes[8..12].try_into()?) as usize;
        let pos_end = 12 + nv * 12;
        let nrm_end = pos_end + nv * 12;
        let idx_end = nrm_end + nt * 12;
        if bytes.len() != idx_end {
            bail!("{} has the wrong length", path.display());
        }
        Ok(Self {
            positions: bytemuck::pod_collect_to_vec(&bytes[12..pos_end]),
            normals: bytemuck::pod_collect_to_vec(&bytes[pos_end..nrm_end]),
            indices: bytemuck::pod_collect_to_vec(&bytes[nrm_end..idx_end]),
        })
    }

    /// Min and max corners, or None for an empty mesh.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = *self.positions.first()?;
        Some(self.positions.iter().fold((first, first), |(mut lo, mut hi), p| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        }))
    }
}
