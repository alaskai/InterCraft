// intercraft-geo: geometry backend trait and the Python (build123d) worker client.
// bytemuck 1, serde 1, serde_json 1, anyhow 1

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct FeatureInput {
    pub id: String,
    pub code: String,
    pub params: serde_json::Map<String, serde_json::Value>,
    /// For a part's first step: a STEP file (already checked to be inside the
    /// project's imports folder) that becomes the starting shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<PathBuf>,
}

impl FeatureInput {
    pub fn snippet(id: &str, code: &str, params: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self { id: id.to_owned(), code: code.to_owned(), params: params.clone(), import: None }
    }

    pub fn import(path: PathBuf) -> Self {
        Self { id: IMPORT_STEP_ID.to_owned(), code: String::new(), params: Default::default(), import: Some(path) }
    }
}

/// Feature id of the imported-file step at the start of an imported part.
pub const IMPORT_STEP_ID: &str = "import";

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
    Exported { path: PathBuf },
    Measured { id: u64, data: serde_json::Value },
    Inspected { id: u64, data: serde_json::Value },
    Error {
        #[serde(default)]
        id: Option<u64>,
        error: String,
    },
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

/// A single build step that runs longer than this is treated as stuck (usually an
/// endless loop in a snippet) and the worker is restarted. 60 s, or
/// INTERCRAFT_BUILD_TIMEOUT seconds (the tests use a short one).
pub fn build_timeout() -> Duration {
    static TIMEOUT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        let secs = std::env::var("INTERCRAFT_BUILD_TIMEOUT").ok().and_then(|v| v.parse().ok());
        Duration::from_secs(secs.unwrap_or(60).max(1))
    })
}

/// Per-process memory cap, so a runaway snippet can't swallow the whole machine.
#[cfg(windows)]
const MEMORY_LIMIT: usize = 4 << 30;

/// Loading OpenCascade takes a few seconds, more on a cold disk. Not a build step.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct PythonWorker {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<GeoEvent>,
    cache_dir: PathBuf,
    next_id: u64,
    wake: Wake,
    /// Requests sent but not finished yet.
    in_flight: u32,
    /// When the worker last showed progress (a request sent while idle, or any event).
    last_progress: Instant,
    /// The worker has finished loading and said so.
    ready: bool,
    #[cfg(windows)]
    _job: Option<job::Job>,
}

impl PythonWorker {
    /// `wake` is called from the reader thread whenever an event arrives.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        Self::start(Arc::new(wake))
    }

    /// Kill the worker and start a fresh one. Used when a build is stuck.
    pub fn restart(&mut self) -> Result<()> {
        let fresh = Self::start(self.wake.clone())?;
        *self = fresh; // dropping the old one kills it
        Ok(())
    }

    /// True when a build has gone longer than `build_timeout()` without any progress.
    pub fn stuck(&self) -> bool {
        self.in_flight > 0 && self.last_progress.elapsed() > self.current_timeout()
    }

    /// Whether the OS memory cap is in force (Windows job object). False elsewhere.
    pub fn memory_capped(&self) -> bool {
        #[cfg(windows)]
        return self._job.is_some();
        #[cfg(not(windows))]
        false
    }

    fn current_timeout(&self) -> Duration {
        if self.ready { build_timeout() } else { STARTUP_TIMEOUT }
    }

    fn start(wake: Wake) -> Result<Self> {
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
        #[cfg(windows)]
        let job = job::Job::limit(&child, MEMORY_LIMIT);
        let stdin = child.stdin.take().context("worker stdin")?;
        let stdout = child.stdout.take().context("worker stdout")?;

        let (tx, events) = channel();
        let reader_wake = wake.clone();
        std::thread::spawn(move || {
            let wake = reader_wake;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let event = serde_json::from_str::<GeoEvent>(&line).unwrap_or_else(|e| {
                    GeoEvent::Error { id: None, error: format!("bad worker message: {e}: {line}") }
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
        Ok(Self {
            child,
            stdin,
            events,
            cache_dir,
            next_id: 1,
            wake,
            in_flight: 0,
            last_progress: Instant::now(),
            ready: false,
            #[cfg(windows)]
            _job: job,
        })
    }

    /// Bookkeeping for the stuck check, applied to every event.
    fn track(&mut self, event: &GeoEvent) {
        self.last_progress = Instant::now();
        if matches!(event, GeoEvent::Ready { .. }) {
            self.ready = true;
        }
        if matches!(
            event,
            GeoEvent::Done { .. }
                | GeoEvent::Exported { .. }
                | GeoEvent::Measured { .. }
                | GeoEvent::Inspected { .. }
                | GeoEvent::Error { .. }
        ) {
            self.in_flight = self.in_flight.saturating_sub(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Step,
    Stl,
}

impl PythonWorker {
    fn send(&mut self, mut msg: serde_json::Value) -> Result<()> {
        msg["id"] = self.next_id.into();
        msg["cache_dir"] = serde_json::json!(self.cache_dir);
        self.next_id += 1;
        if self.in_flight == 0 {
            self.last_progress = Instant::now();
        }
        self.in_flight += 1;
        writeln!(self.stdin, "{msg}").context("writing to worker")?;
        self.stdin.flush().context("flushing worker stdin")
    }

    fn recv_blocking(&mut self) -> Result<GeoEvent> {
        match self.events.recv_timeout(self.current_timeout()) {
            Ok(GeoEvent::Exited) => bail!("the geometry worker stopped (it may have run out of memory)"),
            Ok(event) => {
                self.track(&event);
                Ok(event)
            }
            Err(_) => bail!(
                "a build step ran for over {} s and was stopped. An endless loop in a snippet is the usual cause",
                build_timeout().as_secs()
            ),
        }
    }

    /// Rebuild and wait for every feature result. For callers without a UI loop.
    pub fn build_blocking(&mut self, req: RebuildRequest) -> Result<Vec<FeatureResult>> {
        self.rebuild(req)?;
        let mut results = Vec::new();
        loop {
            match self.recv_blocking()? {
                GeoEvent::Feature(result) => results.push(result),
                GeoEvent::Done { .. } => return Ok(results),
                GeoEvent::Error { error, .. } => bail!(error),
                _ => {}
            }
        }
    }

    /// Ask for a measurement without waiting. `a` and `b` select an edge, vertex, face or
    /// point of a built shape, e.g. `{"mesh": key, "kind": "edge", "index": 3}`. With
    /// only `a`, the result describes that one item (length, radius...). Returns the
    /// request id, which comes back on `GeoEvent::Measured` or `Error`.
    pub fn request_measure(&mut self, a: serde_json::Value, b: Option<serde_json::Value>) -> Result<u64> {
        let id = self.next_id;
        self.send(serde_json::json!({ "cmd": "measure", "a": a, "b": b }))?;
        Ok(id)
    }

    pub fn measure_blocking(&mut self, a: serde_json::Value, b: Option<serde_json::Value>) -> Result<serde_json::Value> {
        let id = self.request_measure(a, b)?;
        self.wait_for(id)
    }

    /// Faces, holes and circular edges of a built shape, by its mesh key.
    pub fn inspect_blocking(&mut self, mesh_key: &str) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.send(serde_json::json!({ "cmd": "inspect", "mesh": mesh_key }))?;
        self.wait_for(id)
    }

    fn wait_for(&mut self, id: u64) -> Result<serde_json::Value> {
        loop {
            match self.recv_blocking()? {
                GeoEvent::Measured { id: got, data } | GeoEvent::Inspected { id: got, data } if got == id => return Ok(data),
                GeoEvent::Error { id: Some(got), error } if got == id => bail!(error),
                _ => {}
            }
        }
    }

    /// Start an export without waiting. The result arrives as `GeoEvent::Exported` or `Error`.
    pub fn request_export(&mut self, req: RebuildRequest, format: ExportFormat, path: &Path) -> Result<()> {
        self.send(export_message(req, format, path))
    }

    /// Build the part and write it to `path`. STEP stays exact, STL is tessellated.
    pub fn export_blocking(
        &mut self,
        req: RebuildRequest,
        format: ExportFormat,
        path: &Path,
    ) -> Result<PathBuf> {
        self.send(export_message(req, format, path))?;
        loop {
            match self.recv_blocking()? {
                GeoEvent::Exported { path } => return Ok(path),
                GeoEvent::Error { error, .. } => bail!(error),
                _ => {}
            }
        }
    }
}

fn export_message(req: RebuildRequest, format: ExportFormat, path: &Path) -> serde_json::Value {
    serde_json::json!({
        "cmd": "export",
        "part": req.part,
        "features": req.features,
        "path": path,
        "format": match format {
            ExportFormat::Step => "step",
            ExportFormat::Stl => "stl",
        },
    })
}

impl GeometryBackend for PythonWorker {
    fn rebuild(&mut self, req: RebuildRequest) -> Result<()> {
        self.send(serde_json::json!({
            "cmd": "rebuild",
            "part": req.part,
            "features": req.features,
        }))
    }

    fn try_recv(&mut self) -> Option<GeoEvent> {
        let event = self.events.try_recv().ok()?;
        self.track(&event);
        Some(event)
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
    /// One polyline per BRep edge, in the shape's edge order.
    pub edges: Vec<Vec<[f32; 3]>>,
    /// BRep vertices (corners), in the shape's vertex order.
    pub corners: Vec<[f32; 3]>,
}

impl Mesh {
    /// The key the worker caches this shape under, for measure and inspect requests.
    pub fn key_of(path: &Path) -> Option<String> {
        Some(path.file_stem()?.to_string_lossy().into_owned())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("reading mesh {}", path.display()))
    }

    /// ICM2: header, triangles, then edge polylines and corner points.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader { bytes, at: 0 };
        if r.take(4)? != b"ICM2" {
            bail!("not an ICM2 mesh");
        }
        let nv = r.u32()? as usize;
        let nt = r.u32()? as usize;
        let positions = r.points(nv)?;
        let normals = r.points(nv)?;
        let indices: Vec<u32> = bytemuck::pod_collect_to_vec(r.take(nt * 12)?);
        if indices.iter().any(|&i| i as usize >= nv) {
            bail!("a triangle points past the vertex list");
        }
        let edge_count = r.u32()? as usize;
        let mut edges = Vec::with_capacity(edge_count.min(100_000));
        for _ in 0..edge_count {
            let n = r.u32()? as usize;
            edges.push(r.points(n)?);
        }
        let corner_count = r.u32()? as usize;
        let corners = r.points(corner_count)?;
        if r.at != bytes.len() {
            bail!("trailing bytes after the mesh");
        }
        Ok(Self { positions, normals, indices, edges, corners })
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

/// Bounds-checked little-endian reader, so a damaged mesh file is an error, not a panic.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let Some(end) = end else { bail!("mesh file is truncated") };
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn points(&mut self, n: usize) -> Result<Vec<[f32; 3]>> {
        let len = n.checked_mul(12).ok_or_else(|| anyhow::anyhow!("mesh count overflows"))?;
        Ok(bytemuck::pod_collect_to_vec(self.take(len)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut b = b"ICM2".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            b.extend(bytemuck::cast_slice(&v));
        }
        for _ in 0..3 {
            b.extend(bytemuck::cast_slice(&[0f32, 0., 1.]));
        }
        b.extend(bytemuck::cast_slice(&[0u32, 1, 2]));
        b.extend(1u32.to_le_bytes()); // one edge
        b.extend(2u32.to_le_bytes());
        b.extend(bytemuck::cast_slice(&[0f32, 0., 0., 1., 0., 0.]));
        b.extend(1u32.to_le_bytes()); // one corner
        b.extend(bytemuck::cast_slice(&[0f32, 0., 0.]));
        b
    }

    #[test]
    fn parses_a_mesh_with_edges() {
        let mesh = Mesh::parse(&sample()).unwrap();
        assert_eq!((mesh.positions.len(), mesh.indices.len(), mesh.edges.len(), mesh.corners.len()), (3, 3, 1, 1));
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        let good = sample();
        for cut in 0..good.len() {
            assert!(Mesh::parse(&good[..cut]).is_err(), "accepted a file cut at {cut}");
        }
        let mut bad_index = good.clone();
        bad_index[12 + 72..12 + 76].copy_from_slice(&99u32.to_le_bytes());
        assert!(Mesh::parse(&bad_index).is_err());
        let mut huge = good.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Mesh::parse(&huge).is_err());
        let mut extra = good;
        extra.push(0);
        assert!(Mesh::parse(&extra).is_err());
    }
}

#[cfg(windows)]
mod job {
    // Windows job object: caps the worker's memory and kills it when the app exits.
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };

    pub struct Job(HANDLE);

    // The handle is only used to close the job, from whichever thread drops it.
    unsafe impl Send for Job {}

    impl Job {
        /// None if the job can't be set up; the worker still runs, just without the cap.
        pub fn limit(child: &Child, memory_limit: usize) -> Option<Self> {
            // SAFETY: plain Win32 calls on a handle we own; the struct is zeroed then filled.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let job = Job(handle);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                info.ProcessMemoryLimit = memory_limit;
                let ok = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 || AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0 {
                    return None;
                }
                Some(job)
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closing the handle we created. With KILL_ON_JOB_CLOSE this ends the worker.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
