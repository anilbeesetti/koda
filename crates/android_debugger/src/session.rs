use crate::{handles::Handles, wire::Wire};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_ITEMS: usize = 100_000;
const MAX_CHILDREN: usize = 1_000;
const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;
const MAX_OPERATION_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OPERATION_WORK: usize = 2_000_000;
const CLASS_SLICE: usize = 16;
const CLASS_SLICE_TIME: Duration = Duration::from_millis(100);
// These notifications have fixed schemas and no source/target text. A 4 KiB
// reservation includes their small nested JSON maps, keys and queue slot.
const EVENT_BYTES: usize = 4096;
const MAX_PENDING_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_EVENTS: usize = MAX_PENDING_EVENT_BYTES / EVENT_BYTES;
const EVENT_QUEUE_BYTES: usize = MAX_PENDING_EVENTS * std::mem::size_of::<PendingEvent>();

struct PendingEvent {
    value: Json,
}

/// Charges allocations before ownership is taken, independently of the retained
/// metadata cache. Limits are errors, never a successful truncated collection.
struct Operation {
    end: Instant,
    owned: usize,
    response: usize,
    work: usize,
}
impl Operation {
    fn new() -> Self {
        Self {
            end: Instant::now() + OPERATION_TIMEOUT,
            owned: 0,
            response: 0,
            work: 0,
        }
    }
    fn check(&mut self, work: usize) -> Result<()> {
        ensure!(
            Instant::now() < self.end,
            "debugger operation deadline exceeded"
        );
        self.work = self
            .work
            .checked_add(work)
            .context("debugger work counter overflow")?;
        ensure!(
            self.work <= MAX_OPERATION_WORK,
            "debugger operation work budget exceeded"
        );
        Ok(())
    }
    fn reserve(&mut self, bytes: usize) -> Result<()> {
        let total = self
            .owned
            .checked_add(bytes)
            .context("debugger allocation budget overflow")?;
        ensure!(
            total <= MAX_OPERATION_BYTES,
            "debugger operation allocation budget exceeded"
        );
        self.owned = total;
        Ok(())
    }
    fn items<T>(&mut self, count: usize) -> Result<()> {
        self.reserve(
            count
                .checked_mul(std::mem::size_of::<T>() + 32)
                .context("debugger collection size overflow")?,
        )
    }
    fn string(&mut self, text: &str) -> Result<String> {
        self.reserve(text.len() + 32)?;
        let mut result = String::new();
        result.try_reserve_exact(text.len())?;
        result.push_str(text);
        Ok(result)
    }
    fn response(&mut self, bytes: usize) -> Result<()> {
        let total = self
            .response
            .checked_add(bytes)
            .context("debugger response budget overflow")?;
        ensure!(
            total <= MAX_RESPONSE_BYTES,
            "debugger response exceeds 8 MiB budget"
        );
        self.reserve(bytes)?;
        self.response = total;
        Ok(())
    }
}

/// Measures escaped JSON string size without creating a serialized copy.
fn json_string_bytes(text: &str) -> Result<usize> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("JSON size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, text)?;
    Ok(count.0)
}

fn assign_variable_type(
    operation: &mut Operation,
    variable: &mut Json,
    signature: String,
) -> Result<()> {
    // The field metadata already owns this string. Moving it into one JSON
    // property charges exactly one serialized string, not three copies.
    operation.response(json_string_bytes(&signature)?)?;
    variable["type"] = Json::String(signature);
    Ok(())
}

fn local_page<'a>(
    slots: &'a [Slot],
    receiver: bool,
    start: usize,
    count: usize,
) -> Vec<Option<&'a Slot>> {
    let receiver = receiver.then_some(None).into_iter();
    receiver
        .chain(slots.iter().filter(|slot| slot.name != "this").map(Some))
        .skip(start)
        .take(count)
        .collect()
}

#[derive(Clone)]
struct SourceMetadata {
    smap: Option<Arc<crate::source::Smap>>,
    file: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Location {
    tag: u8,
    class: u64,
    method: u64,
    index: u64,
}
#[derive(Clone)]
struct Class {
    tag: u8,
    id: u64,
    signature: String,
    status: u32,
}

enum CompositeEvent {
    Location {
        kind: u8,
        request: u32,
        thread: u64,
        location: Location,
    },
    Exception {
        thread: u64,
        exception: JavaValue,
    },
    Thread {
        kind: u8,
        thread: u64,
    },
    Prepare {
        thread: u64,
        class: Class,
    },
    Death,
}
#[derive(Clone)]
struct Method {
    id: u64,
    name: String,
}
#[derive(Clone)]
struct Field {
    id: u64,
    name: String,
    signature: String,
    modifiers: u32,
    owner: u64,
}
#[derive(Clone)]
struct Frame {
    thread: u64,
    id: u64,
    location: Location,
}
#[derive(Clone)]
struct Slot {
    index: u32,
    name: String,
    signature: String,
}
#[derive(Clone, Debug)]
struct JavaValue {
    tag: u8,
    bits: u64,
}
#[derive(Clone)]
enum Children {
    Locals(Frame),
    Object(JavaValue),
    Coroutine(JavaValue),
}
#[derive(Clone)]
struct Breakpoint {
    dap_id: u64,
    path: PathBuf,
    line: u32,
    requests: Vec<u32>,
    installed: BTreeSet<LocationKey>,
}
fn breakpoint_bytes(breakpoint: &Breakpoint) -> Result<usize> {
    breakpoint
        .installed
        .len()
        .checked_mul(128)
        .and_then(|bytes| bytes.checked_add(breakpoint.path.as_os_str().len() + 128))
        .context("breakpoint allocation size overflow")
}
type LocationKey = (u64, u64, u64);
#[derive(Clone)]
struct Step {
    thread: u64,
    depth: u32,
    start: Position,
    inline_out: bool,
    request: u32,
    remaining: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Position {
    path: PathBuf,
    line: u32,
}

/// One attached VM. Handles are invalidated before any command can resume it.
pub struct Session {
    wire: Wire,
    roots: Vec<PathBuf>,
    classes: BTreeMap<u64, Class>,
    source_metadata: BTreeMap<u64, SourceMetadata>,
    source_indexed: BTreeSet<u64>,
    retired_classes: BTreeSet<u64>,
    indexed_sources: BTreeMap<PathBuf, bool>,
    class_work: VecDeque<u64>,
    class_work_bytes: usize,
    prepare_request: Option<u32>,
    prepare_snapshot_pending: bool,
    prepare_suspensions: BTreeSet<u64>,
    prepare_vm_suspensions: u32,
    prepare_vm_was_suspended: bool,
    prepare_vm_stop_notified: bool,
    pending_events: VecDeque<PendingEvent>,
    pending_event_bytes: usize,
    pending_queue_bytes: usize,
    method_lines: BTreeMap<(u64, u64), Arc<Vec<(u64, u32)>>>,
    metadata_bytes: usize,
    methods: BTreeMap<u64, Arc<Vec<Method>>>,
    operation: Operation,
    checkpoint: Option<Box<dyn FnMut() -> Result<()>>>,
    frames: Handles<Frame>,
    children: Handles<Children>,
    threads: Handles<u64>,
    thread_ids: BTreeMap<u64, u64>,
    breakpoints: Vec<Breakpoint>,
    next_breakpoint: u64,
    exception_request: Option<u32>,
    exceptions: BTreeMap<u64, JavaValue>,
    step: Option<Step>,
    pending_step_clear: bool,
    suspended: bool,
    configured: bool,
    terminated: bool,
    terminal_notice_pending: bool,
}

impl Session {
    pub fn attach(arguments: &Json) -> Result<Self> {
        Self::attach_with_checkpoint(arguments, None)
    }

    pub fn attach_with_checkpoint(
        arguments: &Json,
        mut checkpoint: Option<Box<dyn FnMut() -> Result<()>>>,
    ) -> Result<Self> {
        if let Some(checkpoint) = &mut checkpoint {
            checkpoint()?;
        }
        let host = arguments["hostName"].as_str().unwrap_or("127.0.0.1");
        ensure!(
            matches!(host, "127.0.0.1" | "localhost" | "::1"),
            "Android debugger attaches only to a local owned JDWP forward"
        );
        let port = arguments["port"]
            .as_u64()
            .filter(|p| (1..=65535).contains(p))
            .context("invalid JDWP port")? as u16;
        let timeout = arguments["timeout"]
            .as_u64()
            .unwrap_or(5000)
            .clamp(1, 30_000);
        let root = arguments["projectRoot"]
            .as_str()
            .context("projectRoot is required")?;
        ensure!(
            Path::new(root).is_absolute(),
            "projectRoot must be absolute"
        );
        let roots = match arguments["sourceRoots"].as_array() {
            Some(values) => {
                ensure!(
                    values.len() <= crate::source::MAX_SOURCE_ROOTS,
                    "too many source roots"
                );
                values
                    .iter()
                    .map(|v| {
                        let p = PathBuf::from(v.as_str().context("source root must be a path")?);
                        ensure!(p.is_absolute(), "source roots must be absolute");
                        Ok(p)
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            None => vec![PathBuf::from(root)],
        };
        let wire = Wire::attach(
            host,
            port,
            Duration::from_millis(timeout).min(OPERATION_TIMEOUT),
        )?;
        if let Some(checkpoint) = &mut checkpoint {
            checkpoint()?;
        }
        let mut session = Self {
            wire,
            roots,
            classes: BTreeMap::new(),
            source_metadata: BTreeMap::new(),
            source_indexed: BTreeSet::new(),
            retired_classes: BTreeSet::new(),
            indexed_sources: BTreeMap::new(),
            class_work: VecDeque::new(),
            class_work_bytes: 0,
            prepare_request: None,
            prepare_snapshot_pending: false,
            prepare_suspensions: BTreeSet::new(),
            prepare_vm_suspensions: 0,
            prepare_vm_was_suspended: false,
            prepare_vm_stop_notified: false,
            pending_events: VecDeque::new(),
            pending_event_bytes: 0,
            pending_queue_bytes: 0,
            method_lines: BTreeMap::new(),
            metadata_bytes: 0,
            methods: BTreeMap::new(),
            operation: Operation::new(),
            checkpoint,
            frames: Handles::default(),
            children: Handles::default(),
            threads: Handles::default(),
            thread_ids: BTreeMap::new(),
            breakpoints: Vec::new(),
            next_breakpoint: 1,
            exception_request: None,
            exceptions: BTreeMap::new(),
            step: None,
            pending_step_clear: false,
            suspended: false,
            configured: false,
            terminated: false,
            terminal_notice_pending: false,
        };
        session.command(1, 8, Bytes::new())?; // Suspend before querying/preparing breakpoints.
        session.suspended = true;
        session.refresh_classes()?;
        session.event_request(6, 0, vec![])?;
        session.event_request(7, 0, vec![])?;
        Ok(session)
    }

    /// The DAP input pump checks queued cancellation and EOF at every network
    /// boundary, without a worker thread or an unbounded thread cleanup path.
    pub fn set_checkpoint(&mut self, checkpoint: Option<Box<dyn FnMut() -> Result<()>>>) {
        self.checkpoint = checkpoint;
    }
    fn begin_operation(&mut self) {
        self.operation = Operation::new();
    }

    fn reserve_event(&mut self, value: Json) -> Result<PendingEvent> {
        ensure!(
            self.pending_event_bytes
                .checked_add(EVENT_BYTES)
                .is_some_and(|bytes| bytes <= MAX_PENDING_EVENT_BYTES),
            "debugger pending notification budget exceeded"
        );
        self.operation.reserve(EVENT_BYTES)?;
        let queue_bytes = if self.pending_queue_bytes == 0 {
            EVENT_QUEUE_BYTES
        } else {
            0
        };
        self.operation.reserve(queue_bytes)?;
        self.reserve_metadata(EVENT_BYTES + queue_bytes)?;
        if let Err(error) = self.pending_events.try_reserve(1) {
            self.metadata_bytes -= EVENT_BYTES + queue_bytes;
            return Err(error.into());
        }
        if self.pending_events.capacity() > MAX_PENDING_EVENTS {
            if self.pending_events.is_empty() {
                self.pending_events = VecDeque::new();
            }
            self.metadata_bytes -= EVENT_BYTES + queue_bytes;
            bail!("notification queue allocator exceeded admitted capacity");
        }
        if queue_bytes != 0 {
            self.pending_queue_bytes = EVENT_QUEUE_BYTES;
        }
        self.pending_event_bytes += EVENT_BYTES;
        Ok(PendingEvent { value })
    }

    fn abandon_event(&mut self, _: PendingEvent) {
        self.pending_event_bytes -= EVENT_BYTES;
        self.metadata_bytes -= EVENT_BYTES;
        self.release_empty_event_queue();
    }

    fn release_empty_event_queue(&mut self) {
        if self.pending_event_bytes == 0 {
            self.pending_events = VecDeque::new();
            self.metadata_bytes -= self.pending_queue_bytes;
            self.pending_queue_bytes = 0;
        }
    }

    fn queue_event(&mut self, value: Json) -> Result<()> {
        let event = self.reserve_event(value)?;
        self.pending_events.push_back(event);
        Ok(())
    }

    /// A committed notification survives later indexing/cleanup errors. The DAP
    /// owner writes each event before reporting such an error or retrying it.
    pub(crate) fn take_pending_event(&mut self) -> Option<Json> {
        let Some(event) = self.pending_events.pop_front() else {
            if self.terminal_notice_pending {
                self.terminal_notice_pending = false;
                // One fixed terminal notice is retained as a scalar, not as an
                // unbounded JSON allocation competing with a full event outbox.
                return Some(json!({"event":"terminated","body":{}}));
            }
            return None;
        };
        self.pending_event_bytes -= EVENT_BYTES;
        self.metadata_bytes -= EVENT_BYTES;
        self.release_empty_event_queue();
        Some(event.value)
    }

    fn drain_events(&mut self) -> Result<Vec<Json>> {
        // A prior operation can have left an outbox. Admit the delivered JSON
        // ownership again before transferring it out of the retained cache.
        self.operation.reserve(
            self.pending_event_bytes
                + self.pending_queue_bytes
                + if self.terminal_notice_pending {
                    EVENT_BYTES
                } else {
                    0
                },
        )?;
        let mut events = Vec::new();
        events.try_reserve_exact(
            self.pending_events.len() + usize::from(self.terminal_notice_pending),
        )?;
        while let Some(event) = self.take_pending_event() {
            events.push(event);
        }
        Ok(events)
    }

    pub(crate) fn poll_error_is_recoverable(&self, error: &anyhow::Error) -> bool {
        !self.terminated
            && self.wire.is_usable()
            && crate::wire::error_code(error).is_some_and(|code| code != 112)
    }

    pub(crate) fn is_terminated(&self) -> bool {
        self.terminated
    }

    fn mark_terminated(&mut self) {
        if !self.terminated {
            self.terminal_notice_pending = true;
        }
        self.terminated = true;
        self.invalidate();
        self.pending_step_clear = false;
        self.step = None;
        // VM_DEAD or a decoded death event proves these counts have no live VM
        // to resume. This is terminal evidence, not a fabricated successful Clear.
        self.metadata_bytes -= self.prepare_suspensions.len() * 48;
        self.prepare_suspensions.clear();
        self.prepare_vm_suspensions = 0;
        self.prepare_vm_stop_notified = false;
        self.suspended = false;
    }

    fn install_background_breakpoint(
        &mut self,
        breakpoint: &mut Breakpoint,
        class: &Class,
        metadata: &SourceMetadata,
    ) -> Result<()> {
        self.install_breakpoint_locations(breakpoint, class, metadata, true)
    }
    fn checkpoint(&mut self, work: usize) -> Result<()> {
        self.operation.check(work)?;
        if let Some(checkpoint) = &mut self.checkpoint {
            checkpoint()?;
        }
        Ok(())
    }
    fn command(&mut self, set: u8, command: u8, body: Bytes) -> Result<Vec<u8>> {
        ensure!(!self.terminated, "JDWP target has terminated");
        self.checkpoint(1)?;
        let result = self
            .wire
            .command_until(set, command, body.0, self.operation.end);
        if result
            .as_ref()
            .err()
            .is_some_and(|error| crate::wire::error_code(error) == Some(112))
        {
            self.mark_terminated();
        }
        result
    }

    fn object_size(&self) -> usize {
        self.wire.ids().object
    }
    fn class_size(&self) -> usize {
        self.wire.ids().reference_type
    }
    fn method_size(&self) -> usize {
        self.wire.ids().method
    }
    fn frame_size(&self) -> usize {
        self.wire.ids().frame
    }
    fn field_size(&self) -> usize {
        self.wire.ids().field
    }

    fn refresh_classes(&mut self) -> Result<()> {
        let body = self.command(1, 3, Bytes::new())?;
        let mut r = Read::new(&body);
        let count = r.count(MAX_ITEMS)?;
        for _ in 0..count {
            self.checkpoint(1)?;
            let tag = r.byte()?;
            let id = r.id(self.class_size())?;
            let signature = r.string_ref()?;
            let status = r.u32()?;
            if let Some(class) = self.classes.get_mut(&id) {
                ensure!(
                    class.signature == signature && class.tag == tag,
                    "loaded class identity changed"
                );
                class.status = status;
                continue;
            }
            ensure!(
                self.classes.len() < MAX_ITEMS,
                "loaded class count exceeds budget"
            );
            self.reserve_metadata(signature.len() + 128)?;
            let class = Class {
                tag,
                id,
                signature: signature.to_owned(),
                status,
            };
            self.classes.insert(class.id, class);
        }
        r.finish()
    }

    fn methods(&mut self, class: u64) -> Result<Arc<Vec<Method>>> {
        if let Some(methods) = self.methods.get(&class) {
            return Ok(methods.clone());
        }
        let body = self.command(2, 5, Bytes::new().id(class, self.class_size()))?;
        let mut r = Read::new(&body);
        let count = r.count(MAX_ITEMS)?;
        self.reserve_metadata(
            count
                .checked_mul(std::mem::size_of::<Method>() + 32)
                .context("method cache size overflow")?,
        )?;
        let mut methods = Vec::new();
        methods.try_reserve_exact(count)?;
        for _ in 0..count {
            self.checkpoint(1)?;
            let id = r.id(self.method_size())?;
            let name = r.string_ref()?;
            self.reserve_metadata(name.len())?;
            let method = Method {
                id,
                name: name.to_owned(),
            };
            r.string_ref()?; // Signature is not retained.
            r.u32()?;
            methods.push(method);
        }
        r.finish()?;
        let methods = Arc::new(methods);
        self.methods.insert(class, methods.clone());
        Ok(methods)
    }

    fn fields(&mut self, class: u64) -> Result<Vec<Field>> {
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = class;
        while current != 0 {
            ensure!(
                seen.insert(current) && seen.len() <= 256,
                "cyclic or excessive class hierarchy"
            );
            let body = self.command(2, 4, Bytes::new().id(current, self.class_size()))?;
            let mut r = Read::new(&body);
            let count = r.count(MAX_ITEMS)?;
            ensure!(
                result
                    .len()
                    .checked_add(count)
                    .is_some_and(|n| n <= MAX_ITEMS),
                "field count exceeds inspection budget"
            );
            self.operation.items::<Field>(count)?;
            result.try_reserve_exact(count)?;
            for _ in 0..count {
                self.checkpoint(1)?;
                result.push(Field {
                    id: r.id(self.field_size())?,
                    name: self.operation.string(r.string_ref()?)?,
                    signature: self.operation.string(r.string_ref()?)?,
                    modifiers: r.u32()?,
                    owner: current,
                });
            }
            r.finish()?;
            let body = self.command(3, 1, Bytes::new().id(current, self.class_size()))?;
            let mut r = Read::new(&body);
            current = r.id(self.class_size())?;
            r.finish()?;
        }
        Ok(result)
    }

    fn reserve_metadata(&mut self, bytes: usize) -> Result<()> {
        let total = self
            .metadata_bytes
            .checked_add(bytes)
            .context("debug metadata budget overflow")?;
        ensure!(
            total <= MAX_METADATA_BYTES,
            "debug metadata exceeds 32 MiB session budget"
        );
        self.metadata_bytes = total;
        Ok(())
    }

    fn lines(&mut self, location: Location) -> Result<Arc<Vec<(u64, u32)>>> {
        if let Some(lines) = self.method_lines.get(&(location.class, location.method)) {
            return Ok(lines.clone());
        }
        let body = self.command(
            6,
            1,
            Bytes::new()
                .id(location.class, self.class_size())
                .id(location.method, self.method_size()),
        )?;
        let mut r = Read::new(&body);
        r.u64()?;
        r.u64()?;
        let count = r.count(MAX_ITEMS)?;
        self.reserve_metadata(
            count
                .checked_mul(std::mem::size_of::<(u64, u32)>())
                .and_then(|n| n.checked_add(64))
                .context("line cache size overflow")?,
        )?;
        let mut lines = Vec::new();
        lines.try_reserve_exact(count)?;
        for _ in 0..count {
            self.checkpoint(1)?;
            lines.push((r.u64()?, r.u32()?));
        }
        r.finish()?;
        let lines = Arc::new(lines);
        self.method_lines
            .insert((location.class, location.method), lines.clone());
        Ok(lines)
    }

    fn source_position(&mut self, location: Location, caller: bool) -> Result<Position> {
        let output = self
            .lines(location)?
            .iter()
            .filter(|(index, _)| *index <= location.index)
            .max_by_key(|(index, _)| *index)
            .context("no executable source line")?
            .1;
        let signature = self
            .classes
            .get(&location.class)
            .context("unknown frame class")?
            .signature
            .clone();
        self.position_for_line(location.class, &signature, output, caller)
    }

    fn position_for_line(
        &mut self,
        class: u64,
        signature: &str,
        output: u32,
        caller: bool,
    ) -> Result<Position> {
        let metadata = self.source_metadata(class)?;
        if let Some(map) = metadata.smap {
            return self.smap_position(&map, output, caller);
        }
        let file = metadata.file.context(crate::source::UnresolvedSource::new(
            "class has no source metadata",
        ))?;
        let path = crate::source::resolve_java_source(&self.roots, signature, &file)?;
        Ok(Position { path, line: output })
    }

    fn metadata_has_owned_source(&self, class: u64, metadata: &SourceMetadata) -> Result<bool> {
        if let Some(map) = &metadata.smap {
            for stratum in map.stratum_names() {
                for file in map.files(stratum)? {
                    match crate::source::resolve_source_path(&self.roots, &file.path) {
                        Ok(Some(_)) => return Ok(true),
                        Ok(None) => {}
                        Err(error)
                            if error
                                .downcast_ref::<crate::source::UnresolvedSource>()
                                .is_some() =>
                        {
                            return Ok(true);
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            return Ok(false);
        }
        let Some(file) = &metadata.file else {
            return Ok(false);
        };
        let signature = &self
            .classes
            .get(&class)
            .context("unknown source class")?
            .signature;
        match crate::source::resolve_java_source(&self.roots, signature, file) {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .downcast_ref::<crate::source::UnresolvedSource>()
                    .is_some() =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn cache_source_metadata(
        &mut self,
        class: u64,
        metadata: SourceMetadata,
        charged: usize,
    ) -> Result<SourceMetadata> {
        // A compact negative index retains the fact that a class was inspected,
        // without owning its metadata or consuming the 8192 project entries.
        let owned = self.metadata_has_owned_source(class, &metadata)?;
        let indexed = self.source_indexed.contains(&class);
        if !indexed {
            ensure!(
                self.source_indexed.len() < MAX_ITEMS,
                "source index exceeds class budget"
            );
        }
        if owned {
            ensure!(
                self.source_metadata.len() < 8192,
                "source metadata class count exceeds budget"
            );
        }
        self.reserve_metadata((if indexed { 0 } else { 48 }) + if owned { charged } else { 0 })?;
        self.source_indexed.insert(class);
        if owned {
            self.source_metadata.insert(class, metadata.clone());
        }
        Ok(metadata)
    }

    fn source_metadata(&mut self, class: u64) -> Result<SourceMetadata> {
        if let Some(metadata) = self.source_metadata.get(&class) {
            self.operation
                .reserve(metadata.file.as_ref().map_or(0, |file| file.len()) + 64)?;
            return Ok(metadata.clone());
        }
        if self.source_indexed.contains(&class) {
            return Ok(SourceMetadata {
                smap: None,
                file: None,
            });
        }
        match self.command(2, 12, Bytes::new().id(class, self.class_size())) {
            Ok(bytes) => {
                let mut r = Read::new(&bytes);
                let smap = r.string_ref()?;
                r.finish()?;
                if !smap.is_empty() {
                    let reservation = smap
                        .len()
                        .checked_mul(32)
                        .and_then(|n| n.checked_add(128))
                        .context("SMAP allocation size overflow")?;
                    self.operation.reserve(reservation)?;
                    let parsed = crate::source::Smap::parse(smap)?;
                    let charged = parsed
                        .estimated_heap_bytes()
                        .checked_add(128)
                        .context("SMAP cache size overflow")?;
                    ensure!(
                        charged <= reservation,
                        "SMAP heap exceeded its pre-allocation reservation"
                    );
                    return self.cache_source_metadata(
                        class,
                        SourceMetadata {
                            smap: Some(Arc::new(parsed)),
                            file: None,
                        },
                        charged,
                    );
                }
            }
            Err(error) if matches!(crate::wire::error_code(&error), Some(99 | 101)) => {}
            Err(error) => return Err(error),
        }
        let file = match self.command(2, 7, Bytes::new().id(class, self.class_size())) {
            Ok(bytes) => {
                let mut r = Read::new(&bytes);
                let file = self.operation.string(r.string_ref()?)?;
                r.finish()?;
                Some(file)
            }
            Err(error) if crate::wire::is_absent_information(&error) => None,
            Err(error) => return Err(error),
        };
        let charged = file.as_ref().map_or(128, |file| file.len() + 128);
        self.cache_source_metadata(class, SourceMetadata { smap: None, file }, charged)
    }

    fn smap_position(
        &self,
        map: &crate::source::Smap,
        output: u32,
        caller: bool,
    ) -> Result<Position> {
        let stratum = if caller && map.has_stratum("KotlinDebug") {
            "KotlinDebug"
        } else {
            "Kotlin"
        };
        let resolved = map.resolve_output(stratum, output, &self.roots);
        let (path, line) = match resolved {
            Err(error)
                if caller
                    && error
                        .downcast_ref::<crate::source::UnresolvedSource>()
                        .is_some() =>
            {
                map.resolve_output("Kotlin", output, &self.roots)?
            }
            value => value?,
        };
        Ok(Position { path, line })
    }

    fn event_request(&mut self, kind: u8, suspend: u8, modifiers: Vec<Vec<u8>>) -> Result<u32> {
        let mut b = Bytes::new()
            .byte(kind)
            .byte(suspend)
            .u32(modifiers.len() as u32);
        for modifier in modifiers {
            b.0.extend(modifier);
        }
        let bytes = self.command(15, 1, b)?;
        let mut r = Read::new(&bytes);
        let id = r.u32()?;
        r.finish()?;
        Ok(id)
    }

    fn clear_request(&mut self, kind: u8, id: u32) -> Result<()> {
        self.command(15, 2, Bytes::new().byte(kind).u32(id))?;
        Ok(())
    }

    fn update_prepare_request(&mut self, needed: bool) -> Result<()> {
        if needed && self.prepare_request.is_none() {
            // An unfiltered fallback is intentional: a future Kotlin caller can
            // inline a requested file from another package. SourceName/ClassMatch
            // guesses would miss that legitimate owner. Only its event thread
            // stops, metadata is looked up once for all breakpoints, and no request
            // exists when the user has no breakpoints.
            let request = self.event_request(8, 1, vec![])?;
            self.prepare_request = Some(request);
            self.prepare_snapshot_pending = true;
        } else if !needed {
            if let Some(request) = self.prepare_request {
                self.clear_request(8, request)?;
                self.prepare_request = None;
                self.prepare_snapshot_pending = false;
            }
        }
        if needed && self.prepare_snapshot_pending {
            // Capture classes prepared while no watcher was needed. Install the
            // watcher first so its future events close the snapshot race.
            self.refresh_classes()?;
            self.prepare_snapshot_pending = false;
        }
        Ok(())
    }

    fn retire_unused_prepare_request(&mut self) -> Result<()> {
        if !self.breakpoints.is_empty() || self.prepare_request.is_none() {
            return Ok(());
        }
        // User cancellation cannot veto cleanup of an already acknowledged
        // watcher. Keep its exact ID on a rejected Clear and retry from poll.
        let checkpoint = self.checkpoint.take();
        let operation = std::mem::replace(&mut self.operation, Operation::new());
        self.operation.end = Instant::now() + Duration::from_secs(2);
        let result = self.update_prepare_request(false);
        self.operation = operation;
        self.checkpoint = checkpoint;
        result
    }

    fn class_priority(&self, class: &Class, path: &Path) -> u8 {
        let signature = class
            .signature
            .trim_start_matches('L')
            .trim_end_matches(';');
        let name = signature.rsplit('/').next().unwrap_or(signature);
        if path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| {
                name == stem
                    || name.strip_prefix(stem).is_some_and(|tail| {
                        tail == "Kt" || tail.starts_with('$') || tail.starts_with("Kt$")
                    })
            })
        {
            return 0;
        }
        if self.roots.iter().any(|root| {
            path.strip_prefix(root)
                .ok()
                .and_then(Path::parent)
                .and_then(Path::to_str)
                .is_some_and(|package| {
                    signature
                        .rsplit_once('/')
                        .map_or(package.is_empty(), |(owner, _)| owner == package)
                })
        }) {
            return 1;
        }
        if self.source_metadata.contains_key(&class.id) {
            2
        } else {
            3
        }
    }

    fn schedule_classes(&mut self, path: &Path) -> Result<()> {
        let exists = path
            .try_exists()
            .context("inspect breakpoint source identity")?;
        let previous = self.indexed_sources.get(path).copied();
        if previous.is_none() || previous == Some(false) && exists {
            // A newly provided source can make previously foreign SMAP entries
            // resolvable. Retain positive metadata, and reconsider negative IDs
            // incrementally instead of permanently hiding newly available source.
            let before = self.source_indexed.len();
            self.source_indexed
                .retain(|id| self.source_metadata.contains_key(id));
            let released = (before - self.source_indexed.len())
                .checked_mul(48)
                .context("source index accounting overflow")?;
            self.metadata_bytes = self
                .metadata_bytes
                .checked_sub(released)
                .context("source index accounting underflow")?;
            if previous.is_none() {
                ensure!(
                    self.indexed_sources.len() < 10_000,
                    "source identity budget exceeded"
                );
                self.reserve_metadata(path.as_os_str().len() + 128)?;
                self.indexed_sources.insert(path.to_path_buf(), exists);
            } else {
                *self
                    .indexed_sources
                    .get_mut(path)
                    .context("missing indexed source")? = exists;
            }
        }
        self.operation.items::<(u8, u64)>(self.classes.len())?;
        let mut classes = Vec::new();
        classes.try_reserve_exact(self.classes.len())?;
        for class in self.classes.values() {
            self.operation.check(1)?;
            if class.status & 2 != 0
                && !class.signature.starts_with('[')
                && !self.retired_classes.contains(&class.id)
                && (!self.source_indexed.contains(&class.id)
                    || self.source_metadata.contains_key(&class.id))
            {
                classes.push((self.class_priority(class, path), class.id));
            }
        }
        classes.sort_unstable();
        let charged = classes
            .len()
            .checked_mul(16)
            .and_then(|n| n.checked_add(64))
            .context("class queue size overflow")?;
        let remaining = self
            .metadata_bytes
            .checked_sub(self.class_work_bytes)
            .context("class queue accounting underflow")?;
        ensure!(
            remaining
                .checked_add(charged)
                .is_some_and(|n| n <= MAX_METADATA_BYTES),
            "debugger metadata exceeds 32 MiB budget"
        );
        let mut work = VecDeque::new();
        work.try_reserve_exact(classes.len())?;
        work.extend(classes.into_iter().map(|(_, id)| id));
        self.class_work = work;
        self.class_work_bytes = charged;
        self.metadata_bytes = remaining + charged;
        Ok(())
    }

    fn resolve_class_slice(
        &mut self,
        breakpoints: &mut [Breakpoint],
        initial_path: Option<&Path>,
    ) -> Result<Vec<Json>> {
        let slice_end = Instant::now() + CLASS_SLICE_TIME;
        let mut output = Vec::new();
        for _ in 0..CLASS_SLICE {
            self.checkpoint(1)?;
            if initial_path.is_none() && Instant::now() >= slice_end {
                break;
            }
            let Some(&id) = self.class_work.front() else {
                break;
            };
            if self.retired_classes.contains(&id) {
                self.class_work.pop_front();
                continue;
            }
            let Some(class) = self.classes.get(&id) else {
                self.class_work.pop_front();
                continue;
            };
            if initial_path.is_some_and(|path| self.class_priority(class, path) > 1) {
                break;
            }
            self.operation.reserve(class.signature.len() + 128)?;
            let class = class.clone();
            let installed = (|| -> Result<()> {
                let metadata = self.source_metadata(id)?;
                for breakpoint in &mut *breakpoints {
                    self.checkpoint(1)?;
                    let verified = !breakpoint.requests.is_empty();
                    if initial_path.is_none() {
                        self.install_background_breakpoint(breakpoint, &class, &metadata)?;
                    } else {
                        self.install_breakpoint_with_metadata(breakpoint, &class, &metadata)?;
                    }
                    if initial_path.is_some() && !verified && !breakpoint.requests.is_empty() {
                        self.operation.response(256)?;
                        self.operation.reserve(256)?;
                        output.push(json!({"event":"breakpoint","body":{"reason":"changed","breakpoint":{"id":breakpoint.dap_id,"verified":true,"line":breakpoint.line}}}));
                    }
                }
                Ok(())
            })();
            if let Err(error) = installed {
                if crate::wire::error_code(&error) == Some(21) {
                    // INVALID_CLASS is terminal for this loaded-class identity.
                    // Continuing this slice lets a later valid inline owner win.
                    self.retire_class(id)?;
                    continue;
                }
                if self.poll_error_is_recoverable(&error) {
                    self.class_work.pop_front();
                    self.class_work.push_back(id);
                }
                return Err(error);
            }
            // Requests/outbox survive errors. Transient class failures rotate;
            // permanent unloaded identities are never probed again.
            self.class_work.pop_front();
        }
        Ok(output)
    }

    fn resolve_pending_classes(&mut self) -> Result<Vec<Json>> {
        if self.breakpoints.is_empty() || self.class_work.is_empty() {
            return self.drain_events();
        }
        let mut breakpoints = std::mem::take(&mut self.breakpoints);
        let result = self.resolve_class_slice(&mut breakpoints, None);
        self.breakpoints = breakpoints;
        result?;
        self.drain_events()
    }

    fn queue_class_retry(&mut self, class: u64) -> Result<()> {
        if self.terminated || self.retired_classes.contains(&class) {
            return Ok(());
        }
        self.operation.check(self.class_work.len() + 1)?;
        if !self.class_work.contains(&class) {
            ensure!(
                self.class_work.len() < MAX_ITEMS,
                "class retry queue exceeds budget"
            );
            self.class_work.try_reserve(1)?;
            self.reserve_metadata(16)?;
            self.class_work_bytes += 16;
            self.class_work.push_back(class);
        }
        Ok(())
    }

    fn retire_class(&mut self, class: u64) -> Result<()> {
        if !self.retired_classes.contains(&class) {
            ensure!(
                self.retired_classes.len() < MAX_ITEMS,
                "retired class index exceeds budget"
            );
            self.reserve_metadata(48)?;
            self.retired_classes.insert(class);
            // Keep acknowledged breakpoint request IDs owned for later Clear or
            // Dispose. Only this proven-invalid class's source work is retired.
            self.queue_event(json!({"event":"output","body":{"category":"console","output":format!("Loaded class {class} is no longer available; its source-index work was retired.\n")}}))?;
        }
        self.operation.check(self.class_work.len() + 1)?;
        self.class_work.retain(|id| *id != class);
        Ok(())
    }

    fn release_prepare_suspensions(&mut self) -> Result<()> {
        self.release_prepare_suspensions_inner(true)
    }

    fn release_prepare_suspensions_inner(&mut self, notify_recovery: bool) -> Result<()> {
        if self.prepare_suspensions.is_empty() && self.prepare_vm_suspensions == 0 {
            return Ok(());
        }
        // Thread counts and promoted system-thread ALL counts belong to prepare
        // composites, independently of user/attach suspensions. Every thread is
        // attempted once per cleanup turn, so a rejected count cannot starve the
        // others. Failed counts remain owned under one two-second deadline.
        let checkpoint = self.checkpoint.take();
        let operation = std::mem::replace(&mut self.operation, Operation::new());
        self.operation.end = Instant::now() + Duration::from_secs(2);
        let result = (|| {
            let mut failure = None;
            self.operation
                .items::<u64>(self.prepare_suspensions.len())?;
            let threads = self.prepare_suspensions.iter().copied().collect::<Vec<_>>();
            for thread in threads {
                match self.command(11, 3, Bytes::new().id(thread, self.object_size())) {
                    Ok(_) => {
                        self.prepare_suspensions.remove(&thread);
                        self.metadata_bytes = self
                            .metadata_bytes
                            .checked_sub(48)
                            .context("prepare suspension accounting underflow")?;
                    }
                    Err(error) => {
                        if self.terminated || !self.wire.is_usable() {
                            return Err(error);
                        }
                        if failure.is_none() {
                            failure = Some(error);
                        }
                    }
                }
            }
            let had_vm_count = self.prepare_vm_suspensions != 0;
            while self.prepare_vm_suspensions != 0 {
                match self.command(1, 9, Bytes::new()) {
                    Ok(_) => self.prepare_vm_suspensions -= 1,
                    Err(error) => {
                        if self.terminated || !self.wire.is_usable() {
                            return Err(error);
                        }
                        if !self.prepare_vm_stop_notified {
                            self.queue_event(json!({"event":"stopped","body":{"reason":"pause","description":"Waiting for debugger class-preparation cleanup","allThreadsStopped":true}}))?;
                            self.prepare_vm_stop_notified = true;
                        }
                        if failure.is_none() {
                            failure = Some(error);
                        }
                        break;
                    }
                }
            }
            if had_vm_count && self.prepare_vm_suspensions == 0 {
                self.suspended = self.prepare_vm_was_suspended;
                if self.prepare_vm_stop_notified && !self.suspended && notify_recovery {
                    // No Java event thread exists for a debugger system thread.
                    // The all-threads flag makes this a global continuation.
                    self.queue_event(json!({"event":"continued","body":{"threadId":0,"allThreadsContinued":true}}))?;
                }
                self.prepare_vm_stop_notified = false;
            }
            match failure {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })();
        self.operation = operation;
        self.checkpoint = checkpoint;
        result
    }

    fn adopt_prepare_stop(&mut self) -> Result<()> {
        if self.prepare_vm_suspensions != 0 && !self.prepare_vm_was_suspended {
            // A user pause/step must gain its own count. Prepare cleanup may then
            // release exactly its counts without undoing that explicit user stop.
            self.command(1, 8, Bytes::new())?;
            self.prepare_vm_was_suspended = true;
            self.suspended = true;
        }
        Ok(())
    }

    pub fn set_breakpoints(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let declared = arguments["source"]["path"]
            .as_str()
            .context("breakpoint source path is required")?;
        self.operation.reserve(declared.len())?;
        let path = PathBuf::from(declared);
        ensure!(path.is_absolute(), "breakpoint paths must be absolute");
        let requested = arguments["breakpoints"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        ensure!(
            requested.len() <= 10_000 && self.breakpoints.len() + requested.len() <= 10_000,
            "breakpoint budget exceeded"
        );
        self.operation.items::<Breakpoint>(requested.len())?;
        self.operation.response(
            requested
                .len()
                .checked_mul(256)
                .context("breakpoint response size overflow")?,
        )?;
        self.operation.items::<u32>(
            self.breakpoints
                .iter()
                .filter(|b| b.path == path)
                .map(|b| b.requests.len())
                .sum(),
        )?;
        let old = self
            .breakpoints
            .iter()
            .filter(|b| b.path == path)
            .flat_map(|b| b.requests.iter().copied())
            .collect::<Vec<_>>();
        let released = self
            .breakpoints
            .iter()
            .filter(|b| b.path == path)
            .try_fold(0_usize, |total, breakpoint| {
                total
                    .checked_add(breakpoint_bytes(breakpoint)?)
                    .context("breakpoint accounting overflow")
            })?;
        for id in old {
            self.clear_request(2, id)?;
            // An acknowledged Clear has already relinquished target ownership.
            // Retire it immediately, even if a later Clear is cancelled or
            // rejected. A retry must reach the surviving requests directly.
            for breakpoint in self.breakpoints.iter_mut().filter(|b| b.path == path) {
                breakpoint.requests.retain(|request| *request != id);
            }
        }
        self.breakpoints.retain(|b| b.path != path);
        self.metadata_bytes = self
            .metadata_bytes
            .checked_sub(released)
            .context("breakpoint accounting underflow")?;
        let mut pending = Vec::new();
        let mut replies = Vec::new();
        self.operation.items::<(u64, usize)>(requested.len())?;
        let mut reply_indices = BTreeMap::new();
        let installed = (|| -> Result<()> {
            for request in requested {
                self.checkpoint(1)?;
                let line = request["line"]
                    .as_u64()
                    .filter(|n| (1..=u32::MAX as u64).contains(n))
                    .context("invalid breakpoint line")? as u32;
                let id = self.next_breakpoint;
                self.next_breakpoint = id.checked_add(1).context("breakpoint id overflow")?;
                if ["condition", "hitCondition", "logMessage"]
                    .iter()
                    .any(|key| request.get(*key).is_some_and(|v| !v.is_null()))
                {
                    replies.push(json!({"id":id,"verified":false,"line":line,"message":"Conditional breakpoints and logpoints require target evaluation and are unsupported"}));
                } else {
                    self.operation.reserve(path.as_os_str().len() + 128)?;
                    self.reserve_metadata(path.as_os_str().len() + 128)?;
                    reply_indices.insert(id, replies.len());
                    pending.push(Breakpoint {
                        dap_id: id,
                        path: path.clone(),
                        line,
                        requests: Vec::new(),
                        installed: BTreeSet::new(),
                    });
                    replies.push(json!({"id":id,"verified":false,"line":line}));
                }
            }
            if pending.is_empty() {
                return Ok(());
            }
            self.update_prepare_request(true)?;
            self.schedule_classes(&path)?;
            // Likely direct owners are handled immediately. Other packages are
            // still inspected, including cross-package SMAP inline owners, but
            // in cancellable bounded slices rather than blocking this response.
            self.resolve_class_slice(&mut pending, Some(&path))?;
            Ok(())
        })();
        if let Err(error) = installed {
            let cleanup = self.rollback_breakpoints(pending);
            let prepare_cleanup = self.retire_unused_prepare_request();
            if let Err(prepare_cleanup) = prepare_cleanup {
                return Err(error.context(format!("ClassPrepare request cleanup incomplete: {prepare_cleanup:#}; breakpoint cleanup: {cleanup:?}")));
            }
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.context(format!(
                    "partial breakpoint cleanup incomplete: {cleanup:#}"
                ))),
            };
        }
        for breakpoint in pending {
            let index = *reply_indices
                .get(&breakpoint.dap_id)
                .context("breakpoint reply ownership missing")?;
            replies[index]["verified"] = json!(!breakpoint.requests.is_empty());
            if breakpoint.requests.is_empty() && !self.class_work.is_empty() {
                replies[index]["message"] = json!("Waiting for loaded-class source indexing");
            }
            self.breakpoints.push(breakpoint);
        }
        if self.breakpoints.is_empty() {
            self.retire_unused_prepare_request()?;
        } else {
            self.update_prepare_request(true)?;
        }
        Ok(json!({"breakpoints":replies}))
    }

    fn rollback_breakpoints(&mut self, pending: Vec<Breakpoint>) -> Result<()> {
        // Cleanup gets a separate finite two-second budget. Cancel/EOF must not
        // abandon a successfully installed request. On transport failure the
        // remaining exact requests stay owned by this Session until VM.Dispose.
        let checkpoint = self.checkpoint.take();
        self.begin_operation();
        self.operation.end = Instant::now() + Duration::from_secs(2);
        let mut failure = None;
        for mut breakpoint in pending {
            let retained_bytes = breakpoint_bytes(&breakpoint)?;
            let mut remaining = Vec::new();
            for id in breakpoint.requests.drain(..) {
                match self.clear_request(2, id) {
                    Ok(()) => {}
                    Err(error) => {
                        remaining.push(id);
                        if failure.is_none() {
                            failure = Some(error);
                        }
                    }
                }
            }
            if !remaining.is_empty() {
                breakpoint.requests = remaining;
                self.breakpoints.push(breakpoint);
            } else {
                self.metadata_bytes = self
                    .metadata_bytes
                    .checked_sub(retained_bytes)
                    .context("breakpoint cleanup accounting underflow")?;
            }
        }
        self.checkpoint = checkpoint;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn install_breakpoint_with_metadata(
        &mut self,
        breakpoint: &mut Breakpoint,
        class: &Class,
        metadata: &SourceMetadata,
    ) -> Result<()> {
        self.install_breakpoint_locations(breakpoint, class, metadata, false)
    }

    fn install_breakpoint_locations(
        &mut self,
        breakpoint: &mut Breakpoint,
        class: &Class,
        metadata: &SourceMetadata,
        background: bool,
    ) -> Result<()> {
        let output_lines = if let Some(map) = &metadata.smap {
            let outputs = map.output_lines_for_resolved_source(
                "Kotlin",
                &breakpoint.path,
                breakpoint.line,
                &self.roots,
            )?;
            if outputs.is_empty() {
                return Ok(());
            }
            Some(outputs.into_iter().collect::<BTreeSet<_>>())
        } else {
            let Some(file) = &metadata.file else {
                return Ok(());
            };
            match crate::source::resolve_java_source(&self.roots, &class.signature, &file) {
                Ok(path) if path == breakpoint.path => {}
                Ok(_) => return Ok(()),
                Err(error)
                    if error
                        .downcast_ref::<crate::source::UnresolvedSource>()
                        .is_some() =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
            None
        };
        for method in self.methods(class.id)?.iter() {
            self.checkpoint(1)?;
            let location = Location {
                tag: class.tag,
                class: class.id,
                method: method.id,
                index: 0,
            };
            let lines = match self.lines(location) {
                Ok(lines) => lines,
                Err(error) if matches!(crate::wire::error_code(&error), Some(101 | 511)) => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            for &(index, output) in lines.iter() {
                self.checkpoint(1)?;
                let key = (class.id, method.id, index);
                let matches = output_lines
                    .as_ref()
                    .map_or(output == breakpoint.line, |lines| lines.contains(&output));
                if matches && !breakpoint.installed.contains(&key) {
                    // Reserve local ownership before sending, but commit the
                    // installed key only after an acknowledged EventRequest.Set.
                    breakpoint.requests.try_reserve(1)?;
                    self.reserve_metadata(128)?;
                    let modifier = Bytes::new()
                        .byte(7)
                        .location(
                            Location { index, ..location },
                            self.class_size(),
                            self.method_size(),
                        )
                        .0;
                    // Reserve only once there is a matching executable location,
                    // before the target can acknowledge its first request. Foreign
                    // classes do not allocate a notification for every breakpoint.
                    let event = if background && breakpoint.requests.is_empty() {
                        match self.reserve_event(json!({"event":"breakpoint","body":{"reason":"changed","breakpoint":{"id":breakpoint.dap_id,"verified":true,"line":breakpoint.line}}})) {
                            Ok(event) => Some(event),
                            Err(error) => {
                                self.metadata_bytes -= 128;
                                return Err(error);
                            }
                        }
                    } else {
                        None
                    };
                    let request = match self.event_request(2, 2, vec![modifier]) {
                        Ok(request) => request,
                        Err(error) => {
                            self.metadata_bytes -= 128;
                            if let Some(event) = event {
                                self.abandon_event(event);
                            }
                            return Err(error);
                        }
                    };
                    breakpoint.installed.insert(key);
                    breakpoint.requests.push(request);
                    if let Some(event) = event {
                        self.pending_events.push_back(event);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn set_exception_breakpoints(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        if let Some(id) = self.exception_request {
            self.clear_request(4, id)?;
            self.exception_request = None;
        }
        let filters = arguments["filters"].as_array().cloned().unwrap_or_default();
        let caught = filters
            .iter()
            .any(|f| matches!(f.as_str(), Some("C" | "caught")));
        let uncaught = filters
            .iter()
            .any(|f| matches!(f.as_str(), Some("U" | "uncaught")));
        ensure!(
            filters
                .iter()
                .all(|f| matches!(f.as_str(), Some("C" | "U" | "caught" | "uncaught"))),
            "unsupported exception filter"
        );
        if caught || uncaught {
            let modifier = Bytes::new()
                .byte(8)
                .id(0, self.class_size())
                .byte(u8::from(caught))
                .byte(u8::from(uncaught))
                .0;
            self.exception_request = Some(self.event_request(4, 2, vec![modifier])?);
        }
        Ok(json!({}))
    }

    fn invalidate(&mut self) {
        self.frames.clear();
        self.children.clear();
        self.exceptions.clear();
        self.suspended = false;
    }

    pub fn configure(&mut self) -> Result<Json> {
        self.begin_operation();
        ensure!(!self.configured, "configurationDone was already received");
        // The VMStart event may hold another suspend count. Dispose that count
        // only when actually observed, rather than guessing target launch state.
        self.resume()?;
        self.configured = true;
        Ok(json!({}))
    }

    fn resume(&mut self) -> Result<()> {
        let suspended = self.suspended;
        self.invalidate();
        self.suspended = suspended;
        self.command(1, 9, Bytes::new())?;
        // Only an acknowledged Resume relinquishes the user/attach or real
        // location-event count. A pending internal ALL event still stops the
        // VM, but its later cleanup must not restore the count released here.
        self.prepare_vm_was_suspended = false;
        self.suspended = self.prepare_vm_suspensions != 0;
        Ok(())
    }

    fn thread_handle(&mut self, thread: u64) -> Result<u64> {
        if let Some(id) = self.thread_ids.get(&thread) {
            return Ok(*id);
        }
        let id = self.threads.insert(0, thread)?;
        self.thread_ids.insert(thread, id);
        Ok(id)
    }

    fn thread(&self, id: &Json) -> Result<u64> {
        self.threads
            .get(id.as_u64().context("threadId is required")?)
            .copied()
    }

    pub fn threads(&mut self) -> Result<Json> {
        self.begin_operation();
        let bytes = self.command(1, 4, Bytes::new())?;
        let mut r = Read::new(&bytes);
        let count = r.count(MAX_ITEMS)?;
        self.operation.items::<Json>(count)?;
        let mut threads = Vec::new();
        threads.try_reserve_exact(count)?;
        for _ in 0..count {
            let thread = r.id(self.object_size())?;
            let bytes = self.command(11, 1, Bytes::new().id(thread, self.object_size()))?;
            let mut name = Read::new(&bytes);
            let name = name.string_ref()?;
            self.operation.response(json_string_bytes(name)? + 128)?;
            threads.push(json!({"id":self.thread_handle(thread)?,"name":name}));
        }
        r.finish()?;
        Ok(json!({"threads":threads}))
    }

    fn raw_frames(&mut self, thread: u64, start: u32, count: u32) -> Result<Vec<Frame>> {
        ensure!(self.suspended, "thread is running");
        let bytes = self.command(
            11,
            6,
            Bytes::new()
                .id(thread, self.object_size())
                .u32(start)
                .u32(count),
        )?;
        let mut r = Read::new(&bytes);
        let count = r.count(MAX_ITEMS)?;
        self.operation.items::<Frame>(count)?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(count)?;
        for _ in 0..count {
            frames.push(Frame {
                thread,
                id: r.id(self.frame_size())?,
                location: r.location(self.class_size(), self.method_size())?,
            });
        }
        r.finish()?;
        Ok(frames)
    }

    pub fn stack_trace(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let thread = self.thread(&arguments["threadId"])?;
        let start = arguments["startFrame"]
            .as_u64()
            .unwrap_or(0)
            .min(MAX_ITEMS as u64) as u32;
        let count = arguments["levels"]
            .as_u64()
            .filter(|n| *n != 0)
            .unwrap_or(1000)
            .min(1000) as u32;
        let raw = self.raw_frames(thread, start, count)?;
        self.operation.items::<Json>(raw.len())?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(raw.len())?;
        for frame in raw {
            let method = self
                .methods(frame.location.class)?
                .iter()
                .find(|m| m.id == frame.location.method)
                .cloned()
                .context("unknown frame method")?;
            let position = match self.source_position(frame.location, false) {
                Ok(position) => Some(position),
                Err(error)
                    if matches!(crate::wire::error_code(&error), Some(101 | 511))
                        || error
                            .downcast_ref::<crate::source::UnresolvedSource>()
                            .is_some() =>
                {
                    None
                }
                Err(error) => return Err(error),
            };
            let source_bytes = position.as_ref().map_or(0, |position| {
                position.path.as_os_str().len().saturating_mul(12) + 128
            });
            self.operation
                .response(json_string_bytes(&method.name)? + source_bytes + 256)?;
            let source = position.as_ref().map(
                |p| json!({"path":p.path,"name":p.path.file_name().map(|s|s.to_string_lossy())}),
            );
            let line = position.as_ref().map_or(0, |p| p.line);
            let id = self.frames.insert(thread, frame)?;
            frames.push(json!({"id":id,"name":method.name,"source":source,"line":line,"column":1}));
        }
        Ok(json!({"stackFrames":frames}))
    }

    fn slots(&mut self, frame: &Frame) -> Result<Vec<Slot>> {
        let bytes = self.command(
            6,
            2,
            Bytes::new()
                .id(frame.location.class, self.class_size())
                .id(frame.location.method, self.method_size()),
        )?;
        let mut r = Read::new(&bytes);
        r.u32()?;
        let count = r.count(MAX_ITEMS)?;
        self.operation.items::<Slot>(count)?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(count)?;
        for _ in 0..count {
            self.checkpoint(1)?;
            let start = r.u64()?;
            let name = self.operation.string(r.string_ref()?)?;
            let signature = self.operation.string(r.string_ref()?)?;
            let length = r.u32()? as u64;
            let index = r.u32()?;
            if frame.location.index >= start
                && frame.location.index
                    < start
                        .checked_add(length)
                        .context("local variable range overflow")?
            {
                slots.push(Slot {
                    index,
                    name,
                    signature,
                });
            }
        }
        r.finish()?;
        Ok(slots)
    }

    fn local_value(&mut self, frame: &Frame, slot: &Slot) -> Result<JavaValue> {
        let tag = slot
            .signature
            .bytes()
            .next()
            .context("empty local signature")?;
        let bytes = self.command(
            16,
            1,
            Bytes::new()
                .id(frame.thread, self.object_size())
                .id(frame.id, self.frame_size())
                .u32(1)
                .u32(slot.index)
                .byte(if tag == b'L' { b'L' } else { tag }),
        )?;
        let mut r = Read::new(&bytes);
        ensure!(r.u32()? == 1, "unexpected local value count");
        let value = r.value(self.object_size())?;
        r.finish()?;
        Ok(value)
    }

    fn receiver(&mut self, frame: &Frame) -> Result<JavaValue> {
        let bytes = self.command(
            16,
            3,
            Bytes::new()
                .id(frame.thread, self.object_size())
                .id(frame.id, self.frame_size()),
        )?;
        let mut r = Read::new(&bytes);
        let value = r.value(self.object_size())?;
        r.finish()?;
        Ok(value)
    }

    fn object_class(&mut self, value: &JavaValue) -> Result<u64> {
        ensure!(
            value.bits != 0 && value.is_object(),
            "null or primitive has no fields"
        );
        let bytes = self.command(9, 1, Bytes::new().id(value.bits, self.object_size()))?;
        let mut r = Read::new(&bytes);
        r.byte()?;
        let id = r.id(self.class_size())?;
        r.finish()?;
        Ok(id)
    }

    fn field_value(&mut self, receiver: &JavaValue, field: &Field) -> Result<JavaValue> {
        let bytes = if field.modifiers & 8 != 0 {
            self.command(
                2,
                6,
                Bytes::new()
                    .id(field.owner, self.class_size())
                    .u32(1)
                    .id(field.id, self.field_size()),
            )?
        } else {
            self.command(
                9,
                2,
                Bytes::new()
                    .id(receiver.bits, self.object_size())
                    .u32(1)
                    .id(field.id, self.field_size()),
            )?
        };
        let mut r = Read::new(&bytes);
        ensure!(r.u32()? == 1, "unexpected field value count");
        let value = r.value(self.object_size())?;
        r.finish()?;
        Ok(value)
    }

    fn array_length(&mut self, value: &JavaValue) -> Result<u32> {
        ensure!(
            value.tag == b'[' && value.bits != 0,
            "value is not an array"
        );
        let bytes = self.command(13, 1, Bytes::new().id(value.bits, self.object_size()))?;
        let mut r = Read::new(&bytes);
        let length = r.u32()?;
        r.finish()?;
        Ok(length)
    }

    fn array_values(
        &mut self,
        value: &JavaValue,
        start: u32,
        count: u32,
    ) -> Result<Vec<JavaValue>> {
        let length = self.array_length(value)?;
        ensure!(
            start <= length && count <= length - start && count as usize <= MAX_CHILDREN,
            "array index or inspection range is out of bounds"
        );
        let bytes = self.command(
            13,
            2,
            Bytes::new()
                .id(value.bits, self.object_size())
                .u32(start)
                .u32(count),
        )?;
        let mut r = Read::new(&bytes);
        let tag = r.byte()?;
        ensure!(r.u32()? == count, "unexpected array region length");
        let mut values = Vec::with_capacity(count as usize);
        for _ in 0..count {
            values.push(if (JavaValue { tag, bits: 0 }).is_object() {
                r.value(self.object_size())?
            } else {
                r.untagged_value(tag, self.object_size())?
            });
        }
        r.finish()?;
        Ok(values)
    }

    fn display(&mut self, value: &JavaValue) -> Result<String> {
        Ok(match value.tag {
            b'Z' => (value.bits != 0).to_string(),
            b'B' => (value.bits as i8).to_string(),
            b'S' => (value.bits as i16).to_string(),
            b'I' => (value.bits as i32).to_string(),
            b'J' => (value.bits as i64).to_string(),
            b'C' => char::from_u32(value.bits as u32)
                .map_or_else(|| format!("\\u{:04x}", value.bits), |c| format!("'{c}'")),
            b'F' => f32::from_bits(value.bits as u32).to_string(),
            b'D' => f64::from_bits(value.bits).to_string(),
            b'V' => "void".into(),
            _ if value.bits == 0 && value.is_object() => "null".into(),
            b's' => {
                let bytes = self.command(10, 1, Bytes::new().id(value.bits, self.object_size()))?;
                let mut r = Read::new(&bytes);
                let text = r.string_ref()?;
                r.finish()?;
                self.operation.response(json_string_bytes(text)?)?;
                serde_json::to_string(text)?
            }
            b'[' => format!("array[{}]", self.array_length(value)?),
            _ if value.is_object() => {
                let class = self.object_class(value)?;
                let signature = self
                    .classes
                    .get(&class)
                    .map_or("object", |class| class.signature.as_str());
                self.operation.response(signature.len() + 32)?;
                format!("{signature}@{:x}", value.bits)
            }
            _ => bail!("unsupported JDWP value tag {}", value.tag),
        })
    }

    fn variable(&mut self, owner: u64, name: &str, value: JavaValue) -> Result<Json> {
        self.operation.response(json_string_bytes(name)? + 128)?;
        let display = self.display(&value)?;
        self.operation.response(json_string_bytes(&display)?)?;
        let reference = if value.bits != 0 && value.is_object() && value.tag != b's' {
            self.children
                .insert(owner, Children::Object(value.clone()))?
        } else {
            0
        };
        Ok(json!({"name":name,"value":display,"variablesReference":reference}))
    }

    pub fn scopes(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let frame = self
            .frames
            .get(
                arguments["frameId"]
                    .as_u64()
                    .context("frameId is required")?,
            )?
            .clone();
        let locals = self
            .children
            .insert(frame.thread, Children::Locals(frame.clone()))?;
        let mut scopes =
            vec![json!({"name":"Locals","variablesReference":locals,"expensive":false})];
        let receiver = self.receiver(&frame)?;
        if receiver.bits != 0 {
            let class = self.object_class(&receiver)?;
            let fields = self.fields(class)?;
            if fields.iter().any(|field| field.name == "label")
                && fields
                    .iter()
                    .any(|field| field.name == "completion" || field.name.starts_with("L$"))
            {
                let state = self
                    .children
                    .insert(frame.thread, Children::Coroutine(receiver))?;
                scopes.push(json!({"name":"Coroutine state (raw)","variablesReference":state,"expensive":false}));
            }
        }
        Ok(json!({"scopes":scopes}))
    }

    pub fn variables(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let reference = arguments["variablesReference"]
            .as_u64()
            .context("variablesReference is required")?;
        let children = self.children.get(reference)?.clone();
        let start = arguments["start"].as_u64().unwrap_or(0);
        let count = arguments["count"]
            .as_u64()
            .filter(|n| *n != 0)
            .unwrap_or(MAX_CHILDREN as u64)
            .min(MAX_CHILDREN as u64);
        ensure!(start <= u32::MAX as u64, "invalid variable start");
        let mut variables = Vec::new();
        match children {
            Children::Locals(frame) => {
                let slots = match self.slots(&frame) {
                    Ok(slots) => slots,
                    Err(e) if crate::wire::is_absent_information(&e) => Vec::new(),
                    Err(e) => return Err(e),
                };
                let receiver = self.receiver(&frame)?;
                self.operation.items::<Option<&Slot>>(count as usize)?;
                for slot in local_page(&slots, receiver.bits != 0, start as usize, count as usize) {
                    self.checkpoint(1)?;
                    let (name, value) = match slot {
                        Some(slot) => (slot.name.as_str(), self.local_value(&frame, slot)?),
                        None => ("this", receiver.clone()),
                    };
                    variables.push(self.variable(frame.thread, name, value)?);
                }
            }
            Children::Object(value) | Children::Coroutine(value) => {
                if value.tag == b'[' {
                    let length = self.array_length(&value)?;
                    ensure!(start <= length as u64, "array start is out of bounds");
                    let values = self.array_values(
                        &value,
                        start as u32,
                        count.min(length as u64 - start) as u32,
                    )?;
                    for (offset, value) in values.into_iter().enumerate() {
                        variables.push(self.variable(
                            0,
                            &format!("[{}]", start + offset as u64),
                            value,
                        )?);
                    }
                } else {
                    let class = self.object_class(&value)?;
                    for field in self
                        .fields(class)?
                        .into_iter()
                        .skip(start as usize)
                        .take(count as usize)
                    {
                        let child = self.field_value(&value, &field)?;
                        let mut variable = self.variable(0, &field.name, child)?;
                        assign_variable_type(&mut self.operation, &mut variable, field.signature)?;
                        variables.push(variable);
                    }
                }
            }
        }
        Ok(json!({"variables":variables}))
    }

    pub fn evaluate(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let expression = crate::expression::Expression::parse(
            arguments["expression"]
                .as_str()
                .context("expression is required")?,
        )?;
        let frame = self
            .frames
            .get(
                arguments["frameId"]
                    .as_u64()
                    .context("frameId is required for read-only evaluation")?,
            )?
            .clone();
        let value = self.evaluate_expression(&frame, &expression)?;
        let variable = self.variable(frame.thread, "", value)?;
        Ok(json!({"result":variable["value"],"variablesReference":variable["variablesReference"]}))
    }

    fn evaluate_expression(
        &mut self,
        frame: &Frame,
        expression: &crate::expression::Expression,
    ) -> Result<JavaValue> {
        use crate::expression::Access;
        let mut value = if expression.root == "this" {
            self.receiver(frame)?
        } else {
            let slots = self.slots(frame)?;
            if let Some(slot) = slots.iter().find(|slot| slot.name == expression.root) {
                self.local_value(frame, slot)?
            } else {
                let receiver = self.receiver(frame)?;
                self.named_field(&receiver, &expression.root)?
            }
        };
        for access in &expression.accesses {
            value = match access {
                Access::Field(name) => self.named_field(&value, name)?,
                Access::Index(index) => self
                    .array_values(&value, *index, 1)?
                    .pop()
                    .context("array index has no value")?,
            };
        }
        Ok(value)
    }

    fn named_field(&mut self, receiver: &JavaValue, name: &str) -> Result<JavaValue> {
        let class = self.object_class(receiver)?;
        let field = self
            .fields(class)?
            .into_iter()
            .find(|f| f.name == name)
            .context("unknown field (getters and method calls are not evaluated)")?;
        self.field_value(receiver, &field)
    }

    pub fn completions(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let frame = self
            .frames
            .get(
                arguments["frameId"]
                    .as_u64()
                    .context("frameId is required")?,
            )?
            .clone();
        let text = arguments["text"]
            .as_str()
            .context("completion text is required")?;
        let column = arguments["column"]
            .as_u64()
            .context("completion column is required")?;
        let prefix = crate::expression::Completion::parse(text, column as usize)?;
        let mut names = Vec::new();
        match prefix.receiver {
            Some(expression) => {
                let value = self.evaluate_expression(&frame, &expression)?;
                let class = self.object_class(&value)?;
                names.extend(self.fields(class)?.into_iter().map(|f| f.name));
            }
            None => {
                names.extend(self.slots(&frame)?.into_iter().map(|s| s.name));
                names.push("this".into());
            }
        }
        names.sort();
        names.dedup();
        let mut targets = Vec::new();
        for label in names
            .into_iter()
            .filter(|n| n.starts_with(&prefix.prefix))
            .take(MAX_CHILDREN)
        {
            self.checkpoint(1)?;
            self.operation.response(json_string_bytes(&label)? + 96)?;
            targets.push(json!({"label":label,"type":"property"}));
        }
        Ok(json!({"targets":targets}))
    }

    pub fn exception_info(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let thread = self.thread(&arguments["threadId"])?;
        let exception = self
            .exceptions
            .get(&thread)
            .cloned()
            .context("thread did not stop on an exception")?;
        let mut seen = BTreeSet::new();
        let details = self.exception_details(&exception, &mut seen)?;
        self.operation.response(
            json_string_bytes(details["typeName"].as_str().unwrap_or(""))?
                + json_string_bytes(details["message"].as_str().unwrap_or(""))?
                + 128,
        )?;
        Ok(
            json!({"exceptionId":details["typeName"],"description":details["message"],"breakMode":"always","details":details}),
        )
    }

    fn exception_details(
        &mut self,
        exception: &JavaValue,
        seen: &mut BTreeSet<u64>,
    ) -> Result<Json> {
        ensure!(
            seen.len() < 64 && seen.insert(exception.bits),
            "cyclic or excessive exception cause chain"
        );
        let class = self.object_class(exception)?;
        let type_name = self
            .classes
            .get(&class)
            .map(|c| {
                c.signature
                    .trim_start_matches('L')
                    .trim_end_matches(';')
                    .replace('/', ".")
            })
            .unwrap_or_else(|| "java.lang.Throwable".into());
        let fields = self.fields(class)?;
        let message = if let Some(field) = fields.iter().find(|f| f.name == "detailMessage") {
            let value = self.field_value(exception, field)?;
            self.display(&value)?
        } else {
            String::new()
        };
        let mut inner = Vec::new();
        if let Some(field) = fields.iter().find(|f| f.name == "cause") {
            let cause = self.field_value(exception, field)?;
            if cause.bits != 0 && !seen.contains(&cause.bits) {
                inner.push(self.exception_details(&cause, seen)?);
            }
        }
        self.operation
            .response(json_string_bytes(&type_name)? * 2 + json_string_bytes(&message)? + 256)?;
        Ok(
            json!({"typeName":type_name,"fullTypeName":type_name,"message":message,"innerException":inner}),
        )
    }

    pub fn continue_request(&mut self, _arguments: &Json) -> Result<Json> {
        self.begin_operation();
        self.clear_step()?;
        let internal_vm_stop = self.prepare_vm_suspensions != 0;
        self.release_prepare_suspensions_inner(false)?;
        if !internal_vm_stop || self.suspended {
            self.resume()?;
        } else {
            self.invalidate();
        }
        Ok(json!({"allThreadsContinued":true}))
    }

    pub fn pause(&mut self, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let thread = self.thread(&arguments["threadId"])?;
        self.adopt_prepare_stop()?;
        if !self.suspended {
            self.command(1, 8, Bytes::new())?;
            self.suspended = true;
        }
        Ok(json!({"threadId":self.thread_handle(thread)?}))
    }

    pub fn step(&mut self, command: &str, arguments: &Json) -> Result<Json> {
        self.begin_operation();
        let thread = self.thread(&arguments["threadId"])?;
        self.adopt_prepare_stop()?;
        self.release_prepare_suspensions_inner(false)?;
        let frame = self
            .raw_frames(thread, 0, 1)?
            .pop()
            .context("thread has no stack frame")?;
        let start = self.source_position(frame.location, false)?;
        let caller = self.source_position(frame.location, true)?;
        let inline_out = command == "stepOut" && caller != start;
        let depth = match command {
            "stepIn" => 0,
            "next" => 1,
            "stepOut" if inline_out => 1,
            "stepOut" => 2,
            _ => bail!("unknown step command"),
        };
        self.clear_step()?;
        let request = self.install_step(thread, depth)?;
        self.step = Some(Step {
            thread,
            depth,
            start,
            inline_out,
            request,
            remaining: 10_000,
        });
        self.resume()?;
        Ok(json!({}))
    }

    fn install_step(&mut self, thread: u64, depth: u32) -> Result<u32> {
        let modifier = Bytes::new()
            .byte(10)
            .id(thread, self.object_size())
            .u32(1)
            .u32(depth)
            .0;
        self.event_request(1, 2, vec![modifier])
    }

    fn clear_step(&mut self) -> Result<()> {
        if let Some(request) = self.step.as_ref().map(|step| step.request) {
            self.clear_request(1, request)?;
            // Cancellation and recoverable target errors retain the live ID.
            // Only a successful reply relinquishes ownership.
            self.step = None;
        }
        self.pending_step_clear = false;
        Ok(())
    }

    pub fn disconnect(&mut self, terminate: bool) -> Result<Json> {
        self.begin_operation();
        if self.terminated {
            return Ok(json!({}));
        }
        self.invalidate();
        if terminate {
            self.command(1, 10, Bytes::new().u32(0))?;
        } else {
            self.command(1, 6, Bytes::new())?;
        }
        self.terminated = true;
        Ok(json!({}))
    }

    pub fn poll(&mut self) -> Result<Vec<Json>> {
        self.begin_operation();
        if self.terminated {
            return self.drain_events();
        }
        if self.pending_step_clear {
            self.clear_step()?;
        }
        self.release_prepare_suspensions()?;
        self.retire_unused_prepare_request()?;
        let bytes = match self.wire.poll_event() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return self.resolve_pending_classes(),
            Err(error) if crate::wire::is_disconnected(&error) => {
                self.mark_terminated();
                return self.drain_events();
            }
            Err(error) => return Err(error),
        };
        self.poll_composite(&bytes)
    }

    fn decode_composite(&mut self, bytes: &[u8]) -> Result<(u8, Vec<CompositeEvent>)> {
        let mut r = Read::new(bytes);
        let suspend = r.byte()?;
        ensure!(suspend <= 2, "invalid JDWP composite suspend policy");
        let count = r.count(MAX_ITEMS)?;
        self.operation.items::<CompositeEvent>(count)?;
        let mut events = Vec::new();
        events.try_reserve_exact(count)?;
        for _ in 0..count {
            self.checkpoint(1)?;
            let kind = r.byte()?;
            let request = r.u32()?;
            let event = match kind {
                1 | 2 => CompositeEvent::Location {
                    kind,
                    request,
                    thread: r.id(self.object_size())?,
                    location: r.location(self.class_size(), self.method_size())?,
                },
                4 => {
                    let thread = r.id(self.object_size())?;
                    r.location(self.class_size(), self.method_size())?;
                    let exception = r.value(self.object_size())?;
                    r.location(self.class_size(), self.method_size())?;
                    CompositeEvent::Exception { thread, exception }
                }
                6 | 7 | 90 => CompositeEvent::Thread {
                    kind,
                    thread: r.id(self.object_size())?,
                },
                8 => CompositeEvent::Prepare {
                    thread: r.id(self.object_size())?,
                    class: Class {
                        tag: r.byte()?,
                        id: r.id(self.class_size())?,
                        signature: self.operation.string(r.string_ref()?)?,
                        status: r.u32()?,
                    },
                },
                99 | 100 => CompositeEvent::Death,
                _ => bail!("unsupported unsolicited JDWP event {kind}"),
            };
            events.push(event);
        }
        r.finish()?;
        Ok((suspend, events))
    }

    fn queue_real_stop(&mut self, kind: u8, thread: u64, suspend: u8) -> Result<()> {
        let thread = self.thread_handle(thread)?;
        self.queue_event(json!({"event":"stopped","body":{"reason":if kind == 1 {"step"} else {"breakpoint"},"threadId":thread,"allThreadsStopped":suspend == 2}}))
    }

    fn process_location_event(
        &mut self,
        kind: u8,
        request: u32,
        thread: u64,
        location: Location,
        suspend: u8,
        should_resume: &mut bool,
    ) -> Result<()> {
        self.suspended = suspend != 0;
        let result = (|| -> Result<bool> {
            if kind == 1 {
                if let Some(step) = self.step.as_ref() {
                    self.operation
                        .reserve(step.start.path.as_os_str().len() + 128)?;
                    let step = step.clone();
                    ensure!(
                        step.thread == thread && step.request == request,
                        "unexpected step event ownership"
                    );
                    self.clear_step()?;
                    let position = self.source_position(location, false).ok();
                    let caller = if self.terminated {
                        None
                    } else {
                        self.source_position(location, true).ok()
                    };
                    ensure!(
                        !self.terminated,
                        "target terminated during step source lookup"
                    );
                    let same = position.as_ref() == Some(&step.start);
                    let inline_pending = step.inline_out
                        && position
                            .as_ref()
                            .zip(caller.as_ref())
                            .is_some_and(|(p, c)| p != c);
                    let trampoline = self.classes.get(&location.class).is_some_and(|c| {
                        c.signature.starts_with("Lkotlin/") || c.signature.starts_with("Ljava/")
                    });
                    if same || inline_pending || trampoline {
                        ensure!(
                            step.remaining > 0,
                            "step exceeded 10000 unmapped or trampoline locations"
                        );
                        let next = self.install_step(thread, step.depth)?;
                        self.step = Some(Step {
                            request: next,
                            remaining: step.remaining - 1,
                            ..step
                        });
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })();
        match result {
            Ok(false) => Ok(()),
            Ok(true) => {
                *should_resume = false;
                self.queue_real_stop(kind, thread, suspend)
            }
            Err(error) => {
                // Clear can fail with the old ID still owned; replacement Set can
                // fail after that ID has been retired. Both are real consumed stops.
                self.pending_step_clear = self
                    .step
                    .as_ref()
                    .is_some_and(|step| step.request == request);
                *should_resume = false;
                self.queue_real_stop(kind, thread, suspend)?;
                Err(error)
            }
        }
    }

    fn process_prepare_event(&mut self, thread: u64, class: Class, suspend: u8) -> Result<()> {
        if suspend == 1 && !self.prepare_suspensions.contains(&thread) {
            self.reserve_metadata(48)?;
            self.prepare_suspensions.insert(thread);
        }
        ensure!(
            self.classes.len() < MAX_ITEMS || self.classes.contains_key(&class.id),
            "loaded class count exceeds budget"
        );
        if !self.classes.contains_key(&class.id) {
            self.reserve_metadata(class.signature.len() + 128)?;
        }
        self.classes.insert(class.id, class.clone());
        let mut breakpoints = std::mem::take(&mut self.breakpoints);
        let installed = (|| -> Result<()> {
            let metadata = if breakpoints.is_empty() {
                None
            } else {
                Some(self.source_metadata(class.id)?)
            };
            for breakpoint in &mut breakpoints {
                self.install_background_breakpoint(
                    breakpoint,
                    &class,
                    metadata
                        .as_ref()
                        .context("missing prepared source metadata")?,
                )?;
            }
            Ok(())
        })();
        self.breakpoints = breakpoints;
        if let Err(error) = installed {
            if crate::wire::error_code(&error) == Some(21) {
                self.retire_class(class.id)?;
            } else {
                self.queue_class_retry(class.id)?;
            }
            return Err(error);
        }
        Ok(())
    }

    fn poll_composite(&mut self, bytes: &[u8]) -> Result<Vec<Json>> {
        // Decode the entire bounded packet before target commands can fail. A
        // conforming same-location Step+Breakpoint set retains its later records.
        let (suspend, events) = self.decode_composite(bytes)?;
        let previous_user_suspension = if self.prepare_vm_suspensions != 0 {
            self.prepare_vm_was_suspended
        } else {
            self.suspended
        };
        let has_user_stop = events.iter().any(|event| {
            matches!(
                event,
                CompositeEvent::Location { .. } | CompositeEvent::Exception { .. }
            )
        });
        let owned_all = suspend == 2
            && !has_user_stop
            && events.iter().any(|event| {
                matches!(
                    event,
                    CompositeEvent::Prepare { .. } | CompositeEvent::Thread { kind: 90, .. }
                )
            });
        if owned_all {
            // Oracle JDWP promotes system-thread ClassPrepare (threadID0) to ALL.
            // VMStart also owns one event ALL count, separate from attach Suspend.
            ensure!(
                self.prepare_vm_suspensions < MAX_ITEMS as u32,
                "internal ALL suspension count exceeds budget"
            );
            if self.prepare_vm_suspensions == 0 {
                self.prepare_vm_was_suspended = self.suspended;
            }
            self.prepare_vm_suspensions += 1;
            self.suspended = true;
        }
        let mut should_resume = suspend != 0;
        let mut failure = None;
        let mut last_step_thread = None;
        for event in events {
            let result = match event {
                CompositeEvent::Location {
                    kind,
                    request,
                    thread,
                    location,
                } => {
                    if kind == 1 {
                        last_step_thread = Some(thread);
                    }
                    self.process_location_event(
                        kind,
                        request,
                        thread,
                        location,
                        suspend,
                        &mut should_resume,
                    )
                }
                CompositeEvent::Exception { thread, exception } => (|| {
                    self.exceptions.insert(thread, exception);
                    self.suspended = suspend != 0;
                    should_resume = false;
                    let thread = self.thread_handle(thread)?;
                    self.queue_event(json!({"event":"stopped","body":{"reason":"exception","threadId":thread,"allThreadsStopped":suspend == 2}}))
                })(),
                CompositeEvent::Thread { kind, thread } => (|| {
                    if kind != 90 {
                        let handle = self.thread_handle(thread)?;
                        self.queue_event(json!({"event":"thread","body":{"reason":if kind == 6 {"started"} else {"exited"},"threadId":handle}}))?;
                    }
                    if kind == 7 {
                        self.frames.remove_owner(thread);
                        self.children.remove_owner(thread);
                    }
                    Ok(())
                })(),
                CompositeEvent::Prepare { thread, class } => {
                    self.process_prepare_event(thread, class, suspend)
                }
                CompositeEvent::Death => {
                    self.mark_terminated();
                    should_resume = false;
                    Ok(())
                }
            };
            if let Err(error) = result {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if should_resume && suspend == 2 && !owned_all && !self.terminated {
            if let Err(error) = self.command(1, 9, Bytes::new()) {
                // An auto-step may own a replacement request already. Failed
                // Resume turns that consumed event into a visible user stop.
                if let Some(thread) = last_step_thread {
                    self.pending_step_clear = self.step.is_some();
                    self.queue_real_stop(1, thread, suspend)?;
                }
                if failure.is_none() {
                    failure = Some(error);
                }
            } else {
                // This acknowledged Resume retires only the consumed event's
                // ALL count. An independently acknowledged user Pause remains
                // owned, even if its request arrived before this queued event.
                self.invalidate();
                self.suspended = previous_user_suspension || self.prepare_vm_suspensions != 0;
            }
        }
        let resumed = if self.terminated {
            Ok(())
        } else {
            self.release_prepare_suspensions()
        };
        match (failure, resumed) {
            (Some(error), Err(cleanup)) => {
                return Err(error.context(format!(
                    "ClassPrepare suspension cleanup incomplete: {cleanup:#}"
                )));
            }
            (Some(error), Ok(())) => return Err(error),
            (None, Err(error)) => return Err(error),
            (None, Ok(())) => {}
        }
        if self.terminated {
            return self.drain_events();
        }
        self.resolve_pending_classes()
    }
}

impl JavaValue {
    fn is_object(&self) -> bool {
        matches!(self.tag, b'[' | b'L' | b's' | b't' | b'g' | b'l' | b'c')
    }
}

#[derive(Default)]
struct Bytes(Vec<u8>);
impl Bytes {
    fn new() -> Self {
        Self::default()
    }
    fn byte(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend(v.to_be_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend(v.to_be_bytes());
        self
    }
    fn id(mut self, v: u64, size: usize) -> Self {
        self.0.extend(&v.to_be_bytes()[8 - size..]);
        self
    }
    fn location(self, v: Location, class: usize, method: usize) -> Self {
        self.byte(v.tag)
            .id(v.class, class)
            .id(v.method, method)
            .u64(v.index)
    }
}

struct Read<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Read<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).context("JDWP offset overflow")?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .context("truncated JDWP reply")?;
        self.at = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn id(&mut self, size: usize) -> Result<u64> {
        ensure!((1..=8).contains(&size), "invalid JDWP ID size");
        let mut bytes = [0; 8];
        bytes[8 - size..].copy_from_slice(self.take(size)?);
        Ok(u64::from_be_bytes(bytes))
    }
    fn count(&mut self, cap: usize) -> Result<usize> {
        let count = self.u32()? as usize;
        ensure!(count <= cap, "JDWP count exceeds budget");
        Ok(count)
    }
    fn string_ref(&mut self) -> Result<&'a str> {
        let count = self.count(1_048_576)?;
        Ok(std::str::from_utf8(self.take(count)?)?)
    }
    #[cfg(test)]
    fn string(&mut self) -> Result<String> {
        Ok(self.string_ref()?.to_owned())
    }
    fn finish(self) -> Result<()> {
        ensure!(self.at == self.bytes.len(), "trailing JDWP reply bytes");
        Ok(())
    }
    fn location(&mut self, class: usize, method: usize) -> Result<Location> {
        Ok(Location {
            tag: self.byte()?,
            class: self.id(class)?,
            method: self.id(method)?,
            index: self.u64()?,
        })
    }
    fn value(&mut self, object: usize) -> Result<JavaValue> {
        let tag = self.byte()?;
        self.untagged_value(tag, object)
    }
    fn untagged_value(&mut self, tag: u8, object: usize) -> Result<JavaValue> {
        let size = match tag {
            b'B' | b'Z' => 1,
            b'C' | b'S' => 2,
            b'I' | b'F' => 4,
            b'J' | b'D' => 8,
            b'V' => 0,
            _ if (JavaValue { tag, bits: 0 }).is_object() => object,
            _ => bail!("unknown JDWP value tag {tag}"),
        };
        let mut value = [0; 8];
        value[8 - size..].copy_from_slice(self.take(size)?);
        Ok(JavaValue {
            tag,
            bits: u64::from_be_bytes(value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
    };

    fn mock_target(
        replies: Vec<(u8, u8, Vec<u8>)>,
    ) -> (u16, std::thread::JoinHandle<Vec<(u8, u8, Vec<u8>)>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut handshake = [0; 14];
            stream.read_exact(&mut handshake).unwrap();
            assert_eq!(&handshake, b"JDWP-Handshake");
            stream.write_all(&handshake).unwrap();
            let mut requests = Vec::new();
            let mut expected = vec![
                (
                    1,
                    7,
                    [8_u32; 5].into_iter().flat_map(u32::to_be_bytes).collect(),
                ),
                (1, 8, vec![]),
                (1, 3, 0_u32.to_be_bytes().to_vec()),
                (15, 1, 100_u32.to_be_bytes().to_vec()),
                (15, 1, 101_u32.to_be_bytes().to_vec()),
            ];
            expected.extend(replies);
            for (set, command, reply) in expected {
                let mut header = [0; 11];
                stream.read_exact(&mut header).unwrap();
                let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
                assert!((11..=crate::wire::MAX_PACKET_BYTES).contains(&length));
                assert_eq!((header[8], header[9], header[10]), (0, set, command));
                let mut body = vec![0; length - 11];
                stream.read_exact(&mut body).unwrap();
                requests.push((set, command, body));
                let mut packet = ((11 + reply.len()) as u32).to_be_bytes().to_vec();
                packet.extend(&header[4..8]);
                packet.extend([0x80, 0, 0]);
                packet.extend(reply);
                stream.write_all(&packet).unwrap();
            }
            requests
        });
        (port, task)
    }

    fn attach_mock(port: u16) -> Session {
        Session::attach(&json!({"hostName":"127.0.0.1","port":port,"projectRoot":"/android","sourceRoots":["/android/src"],"timeout":1000})).unwrap()
    }

    #[test]
    fn values_preserve_signed_width_and_reject_truncation() {
        let mut r = Read::new(&[b'I', 255, 255, 255, 249]);
        let v = r.value(8).unwrap();
        assert_eq!(v.bits as i32, -7);
        assert!(Read::new(&[b'J', 0]).value(8).is_err());
        assert!(Read::new(&[b'?', 0]).value(8).is_err());
    }

    #[test]
    fn counts_strings_and_locations_are_bounded() {
        assert!(Read::new(&u32::MAX.to_be_bytes()).count(MAX_ITEMS).is_err());
        assert!(Read::new(&[0, 0, 0, 3, b'a']).string().is_err());
        let expected = Location {
            tag: 1,
            class: 42,
            method: 17,
            index: u64::MAX,
        };
        let bytes = Bytes::new().location(expected, 4, 8).0;
        let mut r = Read::new(&bytes);
        assert_eq!(r.location(4, 8).unwrap(), expected);
        r.finish().unwrap();
        assert!(Read::new(&[0]).finish().is_err());
    }

    #[test]
    fn read_only_field_evaluation_has_no_invoke_and_resume_expires_handles() {
        let field = Field {
            id: 19,
            name: "field".into(),
            signature: "I".into(),
            modifiers: 0,
            owner: 5,
        };
        let mut field_reply = 1_u32.to_be_bytes().to_vec();
        field_reply.extend(field.id.to_be_bytes());
        for text in [&field.name, &field.signature] {
            field_reply.extend((text.len() as u32).to_be_bytes());
            field_reply.extend(text.as_bytes());
        }
        field_reply.extend(0_u32.to_be_bytes());
        let (port, target) = mock_target(vec![
            (16, 3, Bytes::new().byte(b'L').id(23, 8).0),
            (9, 1, Bytes::new().byte(1).id(5, 8).0),
            (2, 4, field_reply),
            (3, 1, 0_u64.to_be_bytes().to_vec()),
            (9, 2, Bytes::new().u32(1).byte(b'I').u32(7).0),
            (1, 9, vec![]),
        ]);
        let mut session = attach_mock(port);
        let frame = session
            .frames
            .insert(
                7,
                Frame {
                    thread: 7,
                    id: 9,
                    location: Location {
                        tag: 1,
                        class: 5,
                        method: 11,
                        index: 0,
                    },
                },
            )
            .unwrap();
        assert_eq!(
            session
                .evaluate(&json!({"frameId":frame,"expression":"this.field"}))
                .unwrap()["result"],
            "7"
        );
        session.continue_request(&json!({})).unwrap();
        assert!(
            session
                .evaluate(&json!({"frameId":frame,"expression":"this.field"}))
                .is_err()
        );
        let requests = target.join().unwrap();
        assert!(
            requests
                .iter()
                .all(|(set, command, _)| !matches!((*set, *command), (9, 6) | (3, 3) | (3, 4)))
        );
        let body = &requests
            .iter()
            .find(|(set, command, _)| (*set, *command) == (9, 2))
            .unwrap()
            .2;
        assert_eq!(body, &Bytes::new().id(23, 8).u32(1).id(19, 8).0);
    }

    #[test]
    fn direct_array_index_above_child_page_limit_reads_only_one_element() {
        let (port, target) = mock_target(vec![
            (13, 1, 3000_u32.to_be_bytes().to_vec()),
            (13, 2, Bytes::new().byte(b'I').u32(1).u32(42).0),
        ]);
        let mut session = attach_mock(port);
        let values = session
            .array_values(
                &JavaValue {
                    tag: b'[',
                    bits: 31,
                },
                2000,
                1,
            )
            .unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].bits, 42);
        let requests = target.join().unwrap();
        assert_eq!(
            requests.last().unwrap().2,
            Bytes::new().id(31, 8).u32(2000).u32(1).0
        );
    }

    #[test]
    fn out_of_bounds_array_index_sends_no_array_region_read() {
        let (port, target) = mock_target(vec![(13, 1, 2_u32.to_be_bytes().to_vec())]);
        let mut session = attach_mock(port);
        assert!(
            session
                .array_values(
                    &JavaValue {
                        tag: b'[',
                        bits: 31
                    },
                    1000,
                    1
                )
                .is_err()
        );
        assert!(
            target
                .join()
                .unwrap()
                .iter()
                .all(|(set, command, _)| (*set, *command) != (13, 2))
        );
    }

    #[test]
    fn detach_uses_dispose_without_terminating_or_invoking_target_code() {
        let (port, target) = mock_target(vec![(1, 6, vec![])]);
        let mut session = attach_mock(port);
        session.disconnect(false).unwrap();
        let requests = target.join().unwrap();
        assert_eq!(
            (requests.last().unwrap().0, requests.last().unwrap().1),
            (1, 6)
        );
        assert!(
            requests
                .iter()
                .all(|(set, command, _)| (*set, *command) != (1, 10))
        );
    }
    fn scripted_target(
        replies: Vec<(u8, u8, u16, Vec<u8>)>,
    ) -> (u16, std::thread::JoinHandle<Vec<(u8, u8, Vec<u8>)>>) {
        scripted_target_with_delay(replies, Duration::ZERO)
    }

    fn scripted_target_with_delay(
        replies: Vec<(u8, u8, u16, Vec<u8>)>,
        delay: Duration,
    ) -> (u16, std::thread::JoinHandle<Vec<(u8, u8, Vec<u8>)>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nodelay(true).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut handshake = [0; 14];
            stream.read_exact(&mut handshake).unwrap();
            stream.write_all(&handshake).unwrap();
            let mut base = vec![
                (
                    1,
                    7,
                    0,
                    [8_u32; 5].into_iter().flat_map(u32::to_be_bytes).collect(),
                ),
                (1, 8, 0, vec![]),
                (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 100_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 101_u32.to_be_bytes().to_vec()),
            ];
            base.extend(replies);
            let mut requests = Vec::new();
            for (set, command, error, body) in base {
                let mut header = [0; 11];
                stream.read_exact(&mut header).unwrap();
                assert_eq!((header[8], header[9], header[10]), (0, set, command));
                let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
                assert!((11..=crate::wire::MAX_PACKET_BYTES).contains(&length));
                let mut payload = vec![0; length - 11];
                stream.read_exact(&mut payload).unwrap();
                requests.push((set, command, payload));
                if requests.len() > 5 {
                    std::thread::sleep(delay);
                }
                let mut reply = ((11 + body.len()) as u32).to_be_bytes().to_vec();
                reply.extend(&header[4..8]);
                reply.push(0x80);
                reply.extend(error.to_be_bytes());
                reply.extend(body);
                stream.write_all(&reply).unwrap();
            }
            requests
        });
        (port, target)
    }

    fn jdwp_text(text: &str) -> Vec<u8> {
        let mut bytes = (text.len() as u32).to_be_bytes().to_vec();
        bytes.extend(text.as_bytes());
        bytes
    }

    fn prepared_class(id: u64, signature: &str) -> Class {
        Class {
            tag: 1,
            id,
            signature: signature.into(),
            status: 2,
        }
    }

    fn one_method() -> Vec<u8> {
        let mut body = 1_u32.to_be_bytes().to_vec();
        body.extend(7_u64.to_be_bytes());
        body.extend(jdwp_text("main"));
        body.extend(jdwp_text("()V"));
        body.extend(0_u32.to_be_bytes());
        body
    }

    fn one_line() -> Vec<u8> {
        let mut body = 0_u64.to_be_bytes().to_vec();
        body.extend(1_u64.to_be_bytes());
        body.extend(1_u32.to_be_bytes());
        body.extend(0_u64.to_be_bytes());
        body.extend(9_u32.to_be_bytes());
        body
    }

    #[test]
    fn local_pages_preserve_positions_after_one_thousand_and_receiver_offset() {
        let slots = (0..2005)
            .map(|index| Slot {
                index,
                name: format!("local{index}"),
                signature: "I".into(),
            })
            .collect::<Vec<_>>();
        let first = local_page(&slots, true, 1000, 2);
        let next = local_page(&slots, true, 1002, 2);
        assert_eq!(
            first
                .iter()
                .map(|slot| slot.unwrap().index)
                .collect::<Vec<_>>(),
            vec![999, 1000]
        );
        assert_eq!(
            next.iter()
                .map(|slot| slot.unwrap().index)
                .collect::<Vec<_>>(),
            vec![1001, 1002]
        );
        assert!(local_page(&slots, true, 0, 1)[0].is_none());
        assert_eq!(local_page(&slots, false, 2004, 2)[0].unwrap().index, 2004);
    }

    #[test]
    fn variables_fetch_only_the_requested_local_page() {
        let mut table = 0_u32.to_be_bytes().to_vec();
        table.extend(2005_u32.to_be_bytes());
        for index in 0..2005_u32 {
            table.extend(0_u64.to_be_bytes());
            table.extend(jdwp_text(&format!("local{index}")));
            table.extend(jdwp_text("I"));
            table.extend(10_u32.to_be_bytes());
            table.extend(index.to_be_bytes());
        }
        let mut receiver = vec![b'L'];
        receiver.extend(0_u64.to_be_bytes());
        let mut value = 1_u32.to_be_bytes().to_vec();
        value.push(b'I');
        value.extend(123_u32.to_be_bytes());
        let (port, target) = scripted_target(vec![
            (6, 2, 0, table),
            (16, 3, 0, receiver),
            (16, 1, 0, value),
        ]);
        let mut session = attach_mock(port);
        let frame = Frame {
            thread: 1,
            id: 2,
            location: Location {
                tag: 1,
                class: 3,
                method: 4,
                index: 1,
            },
        };
        let reference = session.children.insert(1, Children::Locals(frame)).unwrap();
        let result = session
            .variables(&json!({"variablesReference":reference,"start":1001,"count":1}))
            .unwrap();
        assert_eq!(result["variables"][0]["name"], "local1001");
        assert_eq!(result["variables"][0]["value"], "123");
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(set, command, _)| (*set, *command) == (16, 1))
                .count(),
            1
        );
        let read = &requests.last().unwrap().2;
        assert_eq!(u32::from_be_bytes(read[20..24].try_into().unwrap()), 1001);
    }

    #[test]
    fn response_and_ownership_admission_reject_before_committing_growth() {
        let mut operation = Operation::new();
        operation.response(MAX_RESPONSE_BYTES - 1).unwrap();
        let before = (operation.owned, operation.response);
        assert!(operation.response(2).is_err());
        assert_eq!((operation.owned, operation.response), before);
        assert!(operation.items::<Field>(usize::MAX).is_err());
        assert_eq!(json_string_bytes("\n\"\\").unwrap(), 8);
    }

    #[test]
    fn all_commands_and_matching_work_share_a_finite_operation_deadline() {
        let mut operation = Operation::new();
        operation.check(MAX_OPERATION_WORK).unwrap();
        assert!(operation.check(1).is_err());
        operation.end = Instant::now();
        assert!(
            operation
                .check(0)
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
        let (port, target) = mock_target(vec![]);
        let mut session = attach_mock(port);
        target.join().unwrap();
        session.operation.end = Instant::now();
        assert!(
            session
                .command(1, 4, Bytes::new())
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
    }

    #[test]
    fn more_than_8192_irrelevant_classes_do_not_exhaust_project_source_cache() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("app")).unwrap();
        let source = temp.path().join("app/Main.java");
        std::fs::write(&source, "package app; class Main {}\n").unwrap();
        let mut replies = Vec::new();
        replies.extend([
            (15, 1, 0, 102_u32.to_be_bytes().to_vec()),
            (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
        ]);
        let (port, target) = scripted_target(replies);
        let mut session = attach_mock(port);
        session.roots = vec![temp.path().to_path_buf()];
        for id in 1..=8193 {
            session
                .classes
                .insert(id, prepared_class(id, "Lforeign/Missing;"));
        }
        session
            .classes
            .insert(9000, prepared_class(9000, "Lapp/Main;"));
        let result = session
            .set_breakpoints(&json!({"source":{"path":source},"breakpoints":[{"line":9}]}))
            .unwrap();
        assert_eq!(result["breakpoints"][0]["verified"], true);
        assert_eq!(session.source_metadata.len(), 1);
        assert!(session.metadata_bytes < MAX_METADATA_BYTES);
        assert_eq!(
            target
                .join()
                .unwrap()
                .iter()
                .filter(|(set, command, _)| (*set, *command) == (15, 1))
                .count(),
            4
        );
    }

    #[test]
    fn later_malformed_metadata_rolls_back_exact_created_breakpoint_requests() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("app")).unwrap();
        let source = temp.path().join("app/Main.java");
        std::fs::write(&source, "package app; class Main {}\n").unwrap();
        let (port, target) = scripted_target(vec![
            (15, 1, 0, 102_u32.to_be_bytes().to_vec()),
            (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            (2, 12, 0, jdwp_text("SMAP\nbroken")),
            (15, 2, 0, vec![]),
            (15, 2, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temp.path().to_path_buf()];
        session.classes.insert(1, prepared_class(1, "Lapp/Main;"));
        session
            .classes
            .insert(2, prepared_class(2, "Lapp/Malformed;"));
        assert!(
            session
                .set_breakpoints(&json!({"source":{"path":source},"breakpoints":[{"line":9}]}))
                .is_err()
        );
        assert!(session.breakpoints.is_empty());
        let requests = target.join().unwrap();
        assert_eq!(requests[requests.len() - 2].2, [2, 0, 0, 3, 132]);
        assert_eq!(requests.last().unwrap().2, [8, 0, 0, 0, 102]);
    }

    #[test]
    fn failed_partial_clear_retains_request_ownership_for_later_cleanup() {
        let (port, target) = scripted_target(vec![(15, 2, 20, vec![])]);
        let mut session = attach_mock(port);
        let breakpoint = Breakpoint {
            dap_id: 1,
            path: PathBuf::from("/android/src/App.java"),
            line: 9,
            requests: vec![900],
            installed: BTreeSet::from([(1, 7, 0)]),
        };
        assert!(session.rollback_breakpoints(vec![breakpoint]).is_err());
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert_eq!(target.join().unwrap().last().unwrap().2, [2, 0, 0, 3, 132]);
    }

    #[test]
    fn duplicate_pause_then_continue_emits_only_one_suspend_and_one_resume() {
        let (port, target) = mock_target(vec![(1, 9, vec![])]);
        let mut session = attach_mock(port);
        let thread = session.thread_handle(7).unwrap();
        session.pause(&json!({"threadId":thread})).unwrap();
        session.pause(&json!({"threadId":thread})).unwrap();
        session
            .continue_request(&json!({"threadId":thread}))
            .unwrap();
        assert!(!session.suspended);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(set, command, _)| (*set, *command) == (1, 8))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(set, command, _)| (*set, *command) == (1, 9))
                .count(),
            1
        );
    }
    #[test]
    fn slow_replies_cannot_restart_the_whole_operation_deadline() {
        let (port, target) = scripted_target_with_delay(
            vec![(1, 4, 0, vec![]), (1, 4, 0, vec![])],
            Duration::from_millis(30),
        );
        let mut session = attach_mock(port);
        let start = Instant::now();
        session.operation.end = start + Duration::from_millis(45);
        session.command(1, 4, Bytes::new()).unwrap();
        assert!(session.command(1, 4, Bytes::new()).is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(target.join().unwrap().len(), 7);
    }
    #[test]
    fn partial_old_breakpoint_clear_retries_only_surviving_requests() {
        let (port, target) = scripted_target(vec![
            (15, 2, 0, vec![]),
            (15, 2, 20, vec![]),
            (15, 2, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        let path = PathBuf::from("/android/src/App.java");
        let breakpoint = Breakpoint {
            dap_id: 1,
            path: path.clone(),
            line: 9,
            requests: vec![900, 901],
            installed: BTreeSet::from([(1, 7, 0), (1, 7, 1)]),
        };
        session
            .reserve_metadata(breakpoint_bytes(&breakpoint).unwrap())
            .unwrap();
        session.breakpoints.push(breakpoint);
        let request = json!({"source":{"path":path},"breakpoints":[]});
        assert!(session.set_breakpoints(&request).is_err());
        assert_eq!(session.breakpoints[0].requests, vec![901]);
        assert_eq!(
            session.set_breakpoints(&request).unwrap()["breakpoints"],
            json!([])
        );
        assert!(session.breakpoints.is_empty());
        assert_eq!(session.metadata_bytes, 0);
        let requests = target.join().unwrap();
        let clears = requests
            .iter()
            .filter(|(set, command, _)| (*set, *command) == (15, 2))
            .collect::<Vec<_>>();
        assert_eq!(clears.len(), 3);
        assert_eq!(clears[0].2, [2, 0, 0, 3, 132]);
        assert_eq!(clears[1].2, [2, 0, 0, 3, 133]);
        assert_eq!(clears[2].2, [2, 0, 0, 3, 133]);
    }

    #[test]
    fn failed_exception_clear_retains_the_exact_request_until_acknowledgement() {
        let (port, target) = scripted_target(vec![(15, 2, 20, vec![]), (15, 2, 0, vec![])]);
        let mut session = attach_mock(port);
        session.exception_request = Some(900);
        assert!(
            session
                .set_exception_breakpoints(&json!({"filters":[]}))
                .is_err()
        );
        assert_eq!(session.exception_request, Some(900));
        session
            .set_exception_breakpoints(&json!({"filters":[]}))
            .unwrap();
        assert_eq!(session.exception_request, None);
        let requests = target.join().unwrap();
        let clears = requests
            .iter()
            .filter(|(set, command, _)| (*set, *command) == (15, 2))
            .collect::<Vec<_>>();
        assert_eq!(clears.len(), 2);
        assert_eq!(clears[0].2, [4, 0, 0, 3, 132]);
        assert_eq!(clears[1].2, [4, 0, 0, 3, 132]);
    }

    #[test]
    fn cancelled_exception_clear_keeps_ownership_without_sending_a_clear() {
        let (port, target) = scripted_target(vec![(15, 2, 0, vec![])]);
        let mut session = attach_mock(port);
        session.exception_request = Some(900);
        session.checkpoint = Some(Box::new(|| anyhow::bail!("fixture request cancelled")));
        assert!(
            session
                .set_exception_breakpoints(&json!({"filters":[]}))
                .is_err()
        );
        assert_eq!(session.exception_request, Some(900));
        session.checkpoint = None;
        session
            .set_exception_breakpoints(&json!({"filters":[]}))
            .unwrap();
        assert_eq!(session.exception_request, None);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(set, command, _)| (*set, *command) == (15, 2))
                .count(),
            1
        );
    }

    fn owned_step(request: u32) -> Step {
        Step {
            thread: 7,
            depth: 1,
            start: Position {
                path: "/android/src/App.java".into(),
                line: 9,
            },
            inline_out: false,
            request,
            remaining: 100,
        }
    }

    #[test]
    fn failed_step_clear_during_continue_retains_the_exact_id_until_retry() {
        let (port, target) = scripted_target(vec![
            (15, 2, 20, vec![]),
            (15, 2, 0, vec![]),
            (1, 9, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.step = Some(owned_step(900));
        assert!(session.continue_request(&json!({})).is_err());
        assert_eq!(session.step.as_ref().unwrap().request, 900);
        assert!(session.suspended);
        session.continue_request(&json!({})).unwrap();
        assert!(session.step.is_none());
        let requests = target.join().unwrap();
        let clears = requests
            .iter()
            .filter(|(s, c, _)| (*s, *c) == (15, 2))
            .collect::<Vec<_>>();
        assert_eq!(clears.len(), 2);
        assert!(
            clears
                .iter()
                .all(|(_, _, body)| body == &vec![1, 0, 0, 3, 132])
        );
        assert_eq!(requests.last().unwrap().0, 1);
        assert_eq!(requests.last().unwrap().1, 9);
    }

    #[test]
    fn cancelled_step_clear_retains_ownership_without_network_output() {
        let (port, target) = scripted_target(vec![(15, 2, 0, vec![])]);
        let mut session = attach_mock(port);
        session.step = Some(owned_step(901));
        session.checkpoint = Some(Box::new(|| bail!("fixture step cancellation")));
        assert!(session.clear_step().is_err());
        assert_eq!(session.step.as_ref().unwrap().request, 901);
        session.checkpoint = None;
        session.clear_step().unwrap();
        assert!(session.step.is_none());
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .count(),
            1
        );
        assert_eq!(requests.last().unwrap().2, [1, 0, 0, 3, 133]);
    }

    #[test]
    fn primitive_and_null_field_types_charge_one_string_at_the_page_boundary() {
        let signatures = ["I", "Ljava/lang/String;", "Z", "Lapp/Optional;"];
        let charge = signatures
            .iter()
            .map(|s| json_string_bytes(s).unwrap())
            .sum::<usize>();
        let mut operation = Operation::new();
        operation.response(MAX_RESPONSE_BYTES - charge).unwrap();
        let mut page = vec![
            json!({"value":"7"}),
            json!({"value":"null"}),
            json!({"value":"false"}),
            json!({"value":"null"}),
        ];
        for (variable, signature) in page.iter_mut().zip(signatures) {
            assign_variable_type(&mut operation, variable, signature.into()).unwrap();
        }
        assert_eq!(operation.response, MAX_RESPONSE_BYTES);
        assert_eq!(page[0]["type"], "I");
        assert_eq!(page[1]["type"], "Ljava/lang/String;");
        assert_eq!(page[3]["value"], "null");
        assert!(operation.response(1).is_err());
    }

    #[test]
    fn delayed_large_vm_prioritizes_the_source_and_reuses_metadata_on_edits() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("app")).unwrap();
        let source = temp.path().join("app/Main.java");
        std::fs::write(&source, "package app; class Main {}\n").unwrap();
        let (port, target) = scripted_target_with_delay(
            vec![
                (15, 1, 0, 102_u32.to_be_bytes().to_vec()),
                (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
                (2, 12, 101, vec![]),
                (2, 7, 0, jdwp_text("Main.java")),
                (2, 5, 0, one_method()),
                (6, 1, 0, one_line()),
                (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
                (15, 2, 0, vec![]),
                (15, 1, 0, 901_u32.to_be_bytes().to_vec()),
            ],
            Duration::from_millis(2),
        );
        let mut session = attach_mock(port);
        session.roots = vec![temp.path().to_path_buf()];
        for id in 1..=8193 {
            session
                .classes
                .insert(id, prepared_class(id, "Lframework/Unrelated;"));
        }
        session
            .classes
            .insert(9000, prepared_class(9000, "Lapp/Main;"));
        let started = Instant::now();
        let arguments = json!({"source":{"path":source},"breakpoints":[{"line":9}]});
        assert_eq!(
            session.set_breakpoints(&arguments).unwrap()["breakpoints"][0]["verified"],
            true
        );
        assert_eq!(
            session.set_breakpoints(&arguments).unwrap()["breakpoints"][0]["verified"],
            true
        );
        assert!(started.elapsed() < OPERATION_TIMEOUT);
        assert_eq!(session.class_work.len(), 8193);
        assert!(session.metadata_bytes < MAX_METADATA_BYTES);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| *s == 2 && matches!(*c, 7 | 12))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 1))
                .count(),
            5
        );
    }

    #[test]
    fn negative_source_index_reuses_absent_metadata_and_new_sources_invalidate_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Missing.java");
        let (port, target) = scripted_target(vec![(2, 12, 101, vec![]), (2, 7, 101, vec![])]);
        let mut session = attach_mock(port);
        session.roots = vec![temp.path().to_path_buf()];
        session.classes.insert(1, prepared_class(1, "LMissing;"));
        session.schedule_classes(&path).unwrap();
        assert!(session.source_metadata(1).unwrap().file.is_none());
        assert!(session.source_metadata(1).unwrap().file.is_none());
        assert_eq!(session.source_indexed.len(), 1);
        session.schedule_classes(&path).unwrap();
        assert!(session.class_work.is_empty());
        std::fs::write(&path, "class Missing {}\n").unwrap();
        session.schedule_classes(&path).unwrap();
        assert!(session.source_indexed.is_empty());
        assert_eq!(session.class_work.front(), Some(&1));
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| *s == 2 && matches!(*c, 7 | 12))
                .count(),
            2
        );
    }

    #[test]
    fn class_prepare_request_is_event_thread_only_and_absent_without_breakpoints() {
        let (port, target) = scripted_target(vec![
            (15, 1, 0, 500_u32.to_be_bytes().to_vec()),
            (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
            (15, 2, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        assert!(session.prepare_request.is_none());
        session.update_prepare_request(true).unwrap();
        session.update_prepare_request(true).unwrap();
        assert_eq!(session.prepare_request, Some(500));
        session.update_prepare_request(false).unwrap();
        assert!(session.prepare_request.is_none());
        let requests = target.join().unwrap();
        let prepares = requests
            .iter()
            .filter(|(s, c, body)| (*s, *c) == (15, 1) && body[0] == 8)
            .collect::<Vec<_>>();
        assert_eq!(prepares.len(), 1);
        assert_eq!(prepares[0].2, [8, 1, 0, 0, 0, 0]);
        assert_eq!(requests.last().unwrap().2, [8, 0, 0, 1, 244]);
    }

    fn prepare_event(policy: u8, signature: &str) -> Vec<u8> {
        let mut bytes = Bytes::new()
            .byte(policy)
            .u32(1)
            .byte(8)
            .u32(500)
            .id(77, 8)
            .byte(1)
            .id(99, 8)
            .0;
        bytes.extend(jdwp_text(signature));
        bytes.extend(2_u32.to_be_bytes());
        bytes
    }

    fn inline_breakpoint(session: &mut Session, path: PathBuf, id: u64, installed: bool) {
        let breakpoint = Breakpoint {
            dap_id: id,
            path,
            line: 9,
            requests: if installed { vec![900] } else { vec![] },
            installed: if installed {
                BTreeSet::from([(99, 7, 0)])
            } else {
                BTreeSet::new()
            },
        };
        session
            .reserve_metadata(breakpoint_bytes(&breakpoint).unwrap())
            .unwrap();
        session.breakpoints.push(breakpoint);
    }

    #[test]
    fn cross_package_inline_prepare_resolves_once_for_all_breakpoints_and_resumes_only_its_thread()
    {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Inline.kt");
        std::fs::write(&path, "package app\ninline fun value() = 9\n").unwrap();
        let smap =
            "SMAP\nCaller.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Inline.kt\napp/InlineKt\n*L\n9#1:9\n*E\n";
        let (port, target) = scripted_target(vec![
            (2, 12, 0, jdwp_text(smap)),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            (15, 1, 0, 901_u32.to_be_bytes().to_vec()),
            (11, 3, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        inline_breakpoint(&mut session, path.clone(), 1, false);
        inline_breakpoint(&mut session, path, 2, false);
        let output = session
            .poll_composite(&prepare_event(1, "Lforeign/CallerKt;"))
            .unwrap();
        assert_eq!(output.len(), 2);
        assert!(
            output
                .iter()
                .all(|event| event["body"]["breakpoint"]["verified"] == true)
        );
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert_eq!(session.breakpoints[1].requests, vec![901]);
        assert!(session.prepare_suspensions.is_empty());
        assert!(
            session.suspended,
            "the explicit attach suspend is independent"
        );
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (2, 12))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (11, 3))
                .count(),
            1
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (1, 9)));
        assert_eq!(requests.last().unwrap().2, 77_u64.to_be_bytes());
    }

    #[test]
    fn a_real_breakpoint_in_an_all_suspended_prepare_composite_is_never_resumed() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Inline.kt");
        std::fs::write(&path, "package app\ninline fun value() = 9\n").unwrap();
        let smap =
            "SMAP\nCaller.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Inline.kt\napp/InlineKt\n*L\n9#1:9\n*E\n";
        let (port, target) = scripted_target(vec![
            (2, 12, 0, jdwp_text(smap)),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        inline_breakpoint(&mut session, path, 1, true);
        let location = Location {
            tag: 1,
            class: 99,
            method: 7,
            index: 0,
        };
        let mut event = Bytes::new()
            .byte(2)
            .u32(2)
            .byte(2)
            .u32(900)
            .id(77, 8)
            .location(location, 8, 8)
            .0;
        event.extend(&prepare_event(2, "Lforeign/CallerKt;")[5..]);
        let output = session.poll_composite(&event).unwrap();
        assert_eq!(output[0]["event"], "stopped");
        assert_eq!(output[0]["body"]["allThreadsStopped"], true);
        assert!(session.suspended);
        assert!(session.prepare_suspensions.is_empty());
        assert!(
            target
                .join()
                .unwrap()
                .iter()
                .all(|(s, c, _)| !matches!((*s, *c), (11, 3) | (1, 9)))
        );
    }

    #[test]
    fn malformed_prepare_metadata_still_releases_only_the_owned_event_thread() {
        let (port, target) = scripted_target(vec![
            (2, 12, 0, jdwp_text("SMAP\nbroken")),
            (11, 3, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        inline_breakpoint(&mut session, "/android/src/Inline.kt".into(), 1, false);
        assert!(
            session
                .poll_composite(&prepare_event(1, "Lforeign/CallerKt;"))
                .is_err()
        );
        assert!(session.prepare_suspensions.is_empty());
        assert!(session.breakpoints[0].requests.is_empty());
        assert!(session.suspended);
        let requests = target.join().unwrap();
        assert_eq!(
            (requests.last().unwrap().0, requests.last().unwrap().1),
            (11, 3)
        );
        assert_eq!(requests.last().unwrap().2, 77_u64.to_be_bytes());
    }

    #[test]
    fn failed_prepare_thread_resume_retains_the_exact_suspension_for_retry() {
        let (port, target) = scripted_target(vec![(11, 3, 20, vec![]), (11, 3, 0, vec![])]);
        let mut session = attach_mock(port);
        session.reserve_metadata(48).unwrap();
        session.prepare_suspensions.insert(77);
        assert!(session.release_prepare_suspensions().is_err());
        assert_eq!(session.prepare_suspensions, BTreeSet::from([77]));
        session.release_prepare_suspensions().unwrap();
        assert!(session.prepare_suspensions.is_empty());
        assert_eq!(session.metadata_bytes, 0);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (11, 3))
                .count(),
            2
        );
        assert!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (11, 3))
                .all(|(_, _, b)| b == &77_u64.to_be_bytes())
        );
    }

    #[test]
    fn failed_step_event_clear_preserves_the_old_request_until_acknowledged() {
        let (port, target) = scripted_target(vec![
            (15, 2, 20, vec![]),
            (15, 2, 0, vec![]),
            (6, 1, 101, vec![]),
            (6, 1, 101, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.step = Some(owned_step(900));
        let location = Location {
            tag: 1,
            class: 3,
            method: 4,
            index: 0,
        };
        let event = Bytes::new()
            .byte(2)
            .u32(1)
            .byte(1)
            .u32(900)
            .id(7, 8)
            .location(location, 8, 8)
            .0;
        assert!(session.poll_composite(&event).is_err());
        assert_eq!(session.step.as_ref().unwrap().request, 900);
        let output = session.poll_composite(&event).unwrap();
        assert!(session.step.is_none());
        assert_eq!(output[0]["body"]["reason"], "step");
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .count(),
            2
        );
        assert!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .all(|(_, _, b)| b == &vec![1, 0, 0, 3, 132])
        );
    }

    #[test]
    fn failed_configuration_resume_leaves_configuration_retryable() {
        let (port, target) = scripted_target(vec![(1, 9, 20, vec![]), (1, 9, 0, vec![])]);
        let mut session = attach_mock(port);
        assert!(session.configure().is_err());
        assert!(!session.configured);
        assert!(session.suspended);
        session.configure().unwrap();
        assert!(session.configured);
        assert!(!session.suspended);
        assert!(session.configure().is_err());
        assert_eq!(
            target
                .join()
                .unwrap()
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            2
        );
    }

    #[test]
    fn cancelled_configuration_is_retryable_without_a_premature_resume() {
        let (port, target) = scripted_target(vec![(1, 9, 0, vec![])]);
        let mut session = attach_mock(port);
        session.checkpoint = Some(Box::new(|| bail!("fixture configure cancellation")));
        assert!(session.configure().is_err());
        assert!(!session.configured);
        assert!(session.suspended);
        session.checkpoint = None;
        session.configure().unwrap();
        assert!(session.configured);
        assert_eq!(
            target
                .join()
                .unwrap()
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            1
        );
    }

    #[test]
    fn a_background_class_slice_is_bounded_and_reuses_negative_results() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("Main.java");
        let mut replies = Vec::new();
        for _ in 0..CLASS_SLICE {
            replies.extend([(2, 12, 101, vec![]), (2, 7, 101, vec![])]);
        }
        let (port, target) = scripted_target_with_delay(replies, Duration::from_millis(1));
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        for id in 1..=128 {
            session
                .classes
                .insert(id, prepared_class(id, "Lforeign/Missing;"));
        }
        session.schedule_classes(&path).unwrap();
        inline_breakpoint(&mut session, path.clone(), 1, false);
        session.resolve_pending_classes().unwrap();
        let completed = session.source_indexed.len();
        assert!(completed > 0 && completed <= CLASS_SLICE);
        assert_eq!(session.class_work.len(), 128 - completed);
        // Finish the bounded scripted transcript if scheduling consumed the
        // wall-time slice early; the slice assertions above remain unchanged.
        for id in 1..=CLASS_SLICE as u64 {
            session.source_metadata(id).unwrap();
        }
        session.schedule_classes(&path).unwrap();
        assert_eq!(session.class_work.len(), 128 - CLASS_SLICE);
        for id in 1..=CLASS_SLICE as u64 {
            assert!(session.source_metadata(id).unwrap().file.is_none());
        }
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| *s == 2 && matches!(*c, 7 | 12))
                .count(),
            CLASS_SLICE * 2
        );
    }

    #[test]
    fn rejected_background_set_does_not_commit_a_location_or_consume_its_retry() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Main.java");
        std::fs::write(&path, "package app; class Main {}\n").unwrap();
        let (port, target) = scripted_target(vec![
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 20, vec![]),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        session.classes.insert(99, prepared_class(99, "Lapp/Main;"));
        inline_breakpoint(&mut session, path.clone(), 1, false);
        session.schedule_classes(&path).unwrap();
        let error = session.resolve_pending_classes().unwrap_err();
        assert_eq!(crate::wire::error_code(&error), Some(20));
        assert!(session.breakpoints[0].installed.is_empty());
        assert!(session.breakpoints[0].requests.is_empty());
        assert_eq!(session.class_work.front(), Some(&99));
        assert!(session.take_pending_event().is_none());
        session.begin_operation();
        let events = session.resolve_pending_classes().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["body"]["breakpoint"]["id"], 1);
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert_eq!(
            session.breakpoints[0].installed,
            BTreeSet::from([(99, 7, 0)])
        );
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (15, 1) && b[0] == 2)
                .count(),
            2
        );
        assert_eq!(requests[requests.len() - 2].2, requests.last().unwrap().2);
    }

    #[test]
    fn a_partially_installed_breakpoint_commits_one_notification_before_a_later_set_error() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Main.java");
        std::fs::write(&path, "package app; class Main {}\n").unwrap();
        let lines = Bytes::new()
            .u64(0)
            .u64(1)
            .u32(2)
            .u64(0)
            .u32(9)
            .u64(1)
            .u32(9)
            .0;
        let (port, target) = scripted_target(vec![
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (2, 5, 0, one_method()),
            (6, 1, 0, lines),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            (15, 1, 20, vec![]),
            (15, 1, 0, 901_u32.to_be_bytes().to_vec()),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        session.classes.insert(99, prepared_class(99, "Lapp/Main;"));
        inline_breakpoint(&mut session, path.clone(), 1, false);
        session.schedule_classes(&path).unwrap();
        assert!(session.resolve_pending_classes().is_err());
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert_eq!(
            session.breakpoints[0].installed,
            BTreeSet::from([(99, 7, 0)])
        );
        let event = session.take_pending_event().unwrap();
        assert_eq!(event["body"]["breakpoint"]["verified"], true);
        assert!(session.take_pending_event().is_none());
        session.begin_operation();
        assert!(session.resolve_pending_classes().unwrap().is_empty());
        assert_eq!(session.breakpoints[0].requests, vec![900, 901]);
        assert_eq!(session.breakpoints[0].installed.len(), 2);
        let requests = target.join().unwrap();
        assert_eq!(requests[requests.len() - 2].2, requests.last().unwrap().2);
    }

    #[test]
    fn prepare_partial_success_preserves_verified_events_and_queues_the_unfinished_class() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Main.java");
        std::fs::write(&path, "package app; class Main {}\n").unwrap();
        let (port, target) = scripted_target(vec![
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            (15, 1, 20, vec![]),
            (11, 3, 0, vec![]),
            (15, 1, 0, 901_u32.to_be_bytes().to_vec()),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        inline_breakpoint(&mut session, path.clone(), 1, false);
        inline_breakpoint(&mut session, path, 2, false);
        assert!(
            session
                .poll_composite(&prepare_event(1, "Lapp/Main;"))
                .is_err()
        );
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert!(session.breakpoints[1].installed.is_empty());
        assert_eq!(session.class_work.front(), Some(&99));
        assert!(session.prepare_suspensions.is_empty());
        assert_eq!(
            session.take_pending_event().unwrap()["body"]["breakpoint"]["id"],
            1
        );
        assert!(session.take_pending_event().is_none());
        session.begin_operation();
        let events = session.resolve_pending_classes().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["body"]["breakpoint"]["id"], 2);
        assert_eq!(session.breakpoints[1].requests, vec![901]);
        assert!(session.class_work.is_empty());
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (11, 3))
                .count(),
            1
        );
    }

    #[test]
    fn a_real_stop_survives_failure_in_later_background_source_indexing() {
        let (port, target) = scripted_target(vec![(2, 12, 20, vec![])]);
        let mut session = attach_mock(port);
        inline_breakpoint(&mut session, "/android/src/Inline.kt".into(), 1, true);
        session
            .classes
            .insert(99, prepared_class(99, "Lforeign/CallerKt;"));
        session.queue_class_retry(99).unwrap();
        let event = Bytes::new()
            .byte(2)
            .u32(1)
            .byte(2)
            .u32(900)
            .id(77, 8)
            .location(
                Location {
                    tag: 1,
                    class: 99,
                    method: 7,
                    index: 0,
                },
                8,
                8,
            )
            .0;
        let error = session.poll_composite(&event).unwrap_err();
        assert!(session.poll_error_is_recoverable(&error));
        assert!(session.suspended);
        let stopped = session.take_pending_event().unwrap();
        assert_eq!(stopped["event"], "stopped");
        assert_eq!(stopped["body"]["reason"], "breakpoint");
        assert_eq!(stopped["body"]["allThreadsStopped"], true);
        assert!(session.take_pending_event().is_none());
        assert_eq!(session.class_work.front(), Some(&99));
        assert!(
            target
                .join()
                .unwrap()
                .iter()
                .all(|(s, c, _)| !matches!((*s, *c), (1, 9) | (11, 3)))
        );
    }

    #[test]
    fn cancellation_after_prepare_set_acknowledgement_cannot_veto_watcher_cleanup() {
        let (port, target) = scripted_target(vec![
            (15, 1, 0, 500_u32.to_be_bytes().to_vec()),
            (15, 2, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let checkpoint_calls = calls.clone();
        session.set_checkpoint(Some(Box::new(move || {
            let call = checkpoint_calls.get() + 1;
            checkpoint_calls.set(call);
            ensure!(
                call < 3,
                "fixture cancellation after watcher acknowledgement"
            );
            Ok(())
        })));
        let error = session
            .set_breakpoints(
                &json!({"source":{"path":"/android/src/Main.java"},"breakpoints":[{"line":9}]}),
            )
            .unwrap_err();
        assert!(error.to_string().contains("fixture cancellation"));
        assert_eq!(calls.get(), 3);
        assert!(session.breakpoints.is_empty());
        assert!(session.prepare_request.is_none());
        assert!(session.checkpoint.is_some());
        let requests = target.join().unwrap();
        assert_eq!(requests.last().unwrap().2, [8, 0, 0, 1, 244]);
    }

    #[test]
    fn failed_unused_watcher_cleanup_retains_its_id_and_retries_from_actual_poll() {
        let (port, target) = scripted_target(vec![(15, 2, 20, vec![]), (15, 2, 0, vec![])]);
        let mut session = attach_mock(port);
        session.prepare_request = Some(500);
        let error = session.retire_unused_prepare_request().unwrap_err();
        assert!(session.poll_error_is_recoverable(&error));
        assert_eq!(session.prepare_request, Some(500));
        let events = session.poll().unwrap();
        assert!(session.prepare_request.is_none());
        assert!(events.iter().all(|event| event["event"] == "terminated"));
        let requests = target.join().unwrap();
        assert_eq!(requests[requests.len() - 2].2, requests.last().unwrap().2);
    }

    #[test]
    fn retained_notifications_are_bounded_and_failed_admission_preserves_the_outbox() {
        let (port, target) = mock_target(vec![]);
        let mut session = attach_mock(port);
        for index in 0..MAX_PENDING_EVENT_BYTES / EVENT_BYTES {
            session
                .queue_event(json!({"event":"thread","body":{"threadId":index}}))
                .unwrap();
        }
        let before = (
            session.pending_events.len(),
            session.pending_event_bytes,
            session.metadata_bytes,
        );
        assert!(
            session
                .queue_event(json!({"event":"thread","body":{}}))
                .is_err()
        );
        assert_eq!(
            (
                session.pending_events.len(),
                session.pending_event_bytes,
                session.metadata_bytes
            ),
            before
        );
        session.begin_operation();
        assert_eq!(
            session.drain_events().unwrap().len(),
            MAX_PENDING_EVENT_BYTES / EVENT_BYTES
        );
        assert_eq!(session.pending_event_bytes, 0);
        assert_eq!(session.metadata_bytes, 0);
        target.join().unwrap();
    }

    #[test]
    fn all_8193_foreign_classes_are_indexed_in_background_without_exhausting_owned_cache() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let source = temporary.path().join("app/Main.java");
        std::fs::write(&source, "package app; class Main {}\n").unwrap();
        let mut replies = vec![
            (15, 1, 0, 500_u32.to_be_bytes().to_vec()),
            (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
        ];
        for _ in 0..8193 {
            replies.extend([(2, 12, 101, vec![]), (2, 7, 0, jdwp_text("Missing.java"))]);
        }
        let (port, target) = scripted_target(replies);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        for id in 1..=8193 {
            session
                .classes
                .insert(id, prepared_class(id, "Lforeign/Missing;"));
        }
        session
            .classes
            .insert(9000, prepared_class(9000, "Lapp/Main;"));
        let result = session
            .set_breakpoints(&json!({"source":{"path":source},"breakpoints":[{"line":9}]}))
            .unwrap();
        assert_eq!(result["breakpoints"][0]["verified"], true);
        assert_eq!(session.class_work.len(), 8193);
        let mut turns = 0;
        while !session.class_work.is_empty() {
            let before = session.class_work.len();
            session.begin_operation();
            assert!(session.resolve_pending_classes().unwrap().is_empty());
            assert!(session.class_work.len() < before);
            assert!(before - session.class_work.len() <= CLASS_SLICE);
            turns += 1;
            assert!(turns <= 8193);
        }
        assert_eq!(session.source_indexed.len(), 8194);
        assert_eq!(session.source_metadata.len(), 1);
        assert!(session.metadata_bytes < MAX_METADATA_BYTES);
        session.begin_operation();
        session.schedule_classes(&source).unwrap();
        assert_eq!(session.class_work.len(), 1);
        assert!(session.resolve_pending_classes().unwrap().is_empty());
        assert_eq!(session.source_indexed.len(), 8194);
        assert_eq!(session.source_metadata.len(), 1);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| *s == 2 && matches!(*c, 7 | 12))
                .count(),
            8194 * 2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 1))
                .count(),
            4
        );
    }

    #[test]
    fn an_actual_consumed_step_packet_reports_the_stop_and_retries_only_its_clear() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.set_nodelay(true).unwrap();
            let mut handshake = [0; 14];
            stream.read_exact(&mut handshake).unwrap();
            assert_eq!(&handshake, b"JDWP-Handshake");
            stream.write_all(&handshake).unwrap();
            let base = vec![
                (
                    1,
                    7,
                    0,
                    [8_u32; 5]
                        .into_iter()
                        .flat_map(u32::to_be_bytes)
                        .collect::<Vec<_>>(),
                ),
                (1, 8, 0, vec![]),
                (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 100_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 101_u32.to_be_bytes().to_vec()),
                (15, 2, 20, vec![]),
                (15, 2, 0, vec![]),
            ];
            let mut requests = Vec::new();
            for (turn, (set, command, error, body)) in base.into_iter().enumerate() {
                let mut header = [0; 11];
                stream.read_exact(&mut header).unwrap();
                assert_eq!((header[9], header[10]), (set, command));
                let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
                assert!((11..=crate::wire::MAX_PACKET_BYTES).contains(&length));
                let mut bytes = vec![0; length - 11];
                stream.read_exact(&mut bytes).unwrap();
                requests.push((set, command, bytes));
                let mut reply = ((11 + body.len()) as u32).to_be_bytes().to_vec();
                reply.extend(&header[4..8]);
                reply.push(0x80);
                reply.extend((error as u16).to_be_bytes());
                reply.extend(body);
                stream.write_all(&reply).unwrap();
                if turn == 4 {
                    let body = Bytes::new()
                        .byte(2)
                        .u32(1)
                        .byte(1)
                        .u32(900)
                        .id(7, 8)
                        .location(
                            Location {
                                tag: 1,
                                class: 3,
                                method: 4,
                                index: 0,
                            },
                            8,
                            8,
                        )
                        .0;
                    let mut packet = ((11 + body.len()) as u32).to_be_bytes().to_vec();
                    packet.extend(1000_u32.to_be_bytes());
                    packet.extend([0, 64, 100]);
                    packet.extend(body);
                    stream.write_all(&packet).unwrap();
                }
            }
            requests
        });
        let mut session = attach_mock(port);
        session.step = Some(owned_step(900));
        let error = session.poll().unwrap_err();
        assert!(session.poll_error_is_recoverable(&error));
        assert_eq!(session.step.as_ref().unwrap().request, 900);
        assert!(session.pending_step_clear);
        assert!(session.suspended);
        let event = session.take_pending_event().unwrap();
        assert_eq!(event["event"], "stopped");
        assert_eq!(event["body"]["reason"], "step");
        assert_eq!(event["body"]["allThreadsStopped"], true);
        assert!(session.take_pending_event().is_none());
        let events = session.poll().unwrap();
        assert!(session.step.is_none());
        assert!(!session.pending_step_clear);
        assert!(events.iter().all(|event| event["event"] != "stopped"));
        let requests = target.join().unwrap();
        assert_eq!(requests[requests.len() - 2].2, vec![1, 0, 0, 3, 132]);
        assert_eq!(requests.last().unwrap().2, vec![1, 0, 0, 3, 132]);
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .count(),
            2
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (1, 9)));
    }

    #[test]
    fn an_unloaded_front_class_cannot_starve_a_later_cross_package_inline_owner() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Inline.kt");
        std::fs::write(&path, "package app\ninline fun value() = 9\n").unwrap();
        let smap =
            "SMAP\nCaller.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Inline.kt\napp/InlineKt\n*L\n9#1:9\n*E\n";
        let (port, target) = scripted_target(vec![
            (2, 12, 21, vec![]),
            (2, 12, 0, jdwp_text(smap)),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        session
            .classes
            .insert(1, prepared_class(1, "Lforeign/Unloaded;"));
        session
            .classes
            .insert(2, prepared_class(2, "Lother/CallerKt;"));
        inline_breakpoint(&mut session, path.clone(), 1, false);
        session.queue_class_retry(1).unwrap();
        session.queue_class_retry(2).unwrap();
        let events = session.resolve_pending_classes().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["event"] == "breakpoint")
                .count(),
            1
        );
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert_eq!(session.retired_classes, BTreeSet::from([1]));
        assert!(session.class_work.is_empty());
        assert_eq!(session.source_metadata.len(), 1);
        assert!(session.metadata_bytes < MAX_METADATA_BYTES);
        session.begin_operation();
        session.schedule_classes(&path).unwrap();
        assert_eq!(session.class_work, VecDeque::from([2]));
        assert!(session.resolve_pending_classes().unwrap().is_empty());
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (2, 12) && b == &1_u64.to_be_bytes())
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (2, 12) && b == &2_u64.to_be_bytes())
                .count(),
            1
        );
    }

    #[test]
    fn transient_class_errors_rotate_without_losing_later_verification_or_request_ownership() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let path = temporary.path().join("app/Inline.kt");
        std::fs::write(&path, "package app\ninline fun value() = 9\n").unwrap();
        let smap =
            "SMAP\nCaller.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Inline.kt\napp/InlineKt\n*L\n9#1:9\n*E\n";
        let (port, target) = scripted_target(vec![
            (2, 12, 113, vec![]),
            (2, 12, 0, jdwp_text(smap)),
            (2, 5, 0, one_method()),
            (6, 1, 0, one_line()),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            (2, 12, 113, vec![]),
            (2, 12, 113, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        session
            .classes
            .insert(1, prepared_class(1, "Lforeign/TemporarilyUnavailable;"));
        session
            .classes
            .insert(2, prepared_class(2, "Lother/CallerKt;"));
        inline_breakpoint(&mut session, path, 1, false);
        session.queue_class_retry(1).unwrap();
        session.queue_class_retry(2).unwrap();
        assert!(session.resolve_pending_classes().is_err());
        assert_eq!(session.class_work, VecDeque::from([2, 1]));
        session.begin_operation();
        assert!(session.resolve_pending_classes().is_err());
        assert_eq!(session.breakpoints[0].requests, vec![900]);
        assert_eq!(
            session.take_pending_event().unwrap()["body"]["breakpoint"]["verified"],
            true
        );
        assert_eq!(session.class_work, VecDeque::from([1]));
        assert!(session.retired_classes.is_empty());
        session.begin_operation();
        assert!(session.resolve_pending_classes().is_err());
        assert!(session.take_pending_event().is_none());
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (2, 12) && b == &1_u64.to_be_bytes())
                .count(),
            3
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (2, 12) && b == &2_u64.to_be_bytes())
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (15, 1) && b[0] == 2)
                .count(),
            1
        );
    }

    #[test]
    fn promoted_system_thread_prepare_failure_releases_only_its_all_count() {
        let (port, target) = scripted_target(vec![(2, 12, 113, vec![]), (1, 9, 0, vec![])]);
        let mut session = attach_mock(port);
        inline_breakpoint(&mut session, "/android/src/Inline.kt".into(), 1, false);
        let mut event = prepare_event(2, "Lother/CallerKt;");
        event[10..18].fill(0);
        let error = session.poll_composite(&event).unwrap_err();
        assert!(session.poll_error_is_recoverable(&error));
        assert_eq!(session.prepare_vm_suspensions, 0);
        assert!(session.suspended, "explicit attach Suspend remains owned");
        assert!(session.prepare_suspensions.is_empty());
        assert_eq!(session.class_work, VecDeque::from([99]));
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            1
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (11, 3)));
    }

    #[test]
    fn failed_promoted_all_resume_retains_its_count_and_reports_then_recovers_the_stop() {
        let (port, target) = scripted_target(vec![(1, 9, 113, vec![]), (1, 9, 0, vec![])]);
        let mut session = attach_mock(port);
        session.suspended = false;
        let mut event = prepare_event(2, "Lother/CallerKt;");
        event[10..18].fill(0);
        let error = session.poll_composite(&event).unwrap_err();
        assert!(session.poll_error_is_recoverable(&error));
        assert_eq!(session.prepare_vm_suspensions, 1);
        assert!(session.suspended);
        assert_eq!(session.take_pending_event().unwrap()["event"], "stopped");
        assert!(session.take_pending_event().is_none());
        session.release_prepare_suspensions().unwrap();
        assert_eq!(session.prepare_vm_suspensions, 0);
        assert!(!session.suspended);
        let continued = session.take_pending_event().unwrap();
        assert_eq!(continued["event"], "continued");
        assert_eq!(continued["body"]["allThreadsContinued"], true);
        assert!(session.take_pending_event().is_none());
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            2
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (11, 3)));
    }

    #[test]
    fn user_pause_gains_its_own_count_while_promoted_prepare_cleanup_is_pending() {
        let (port, target) = scripted_target(vec![
            (1, 9, 0, vec![]),
            (1, 9, 113, vec![]),
            (1, 8, 0, vec![]),
            (1, 9, 0, vec![]),
            (1, 9, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.configure().unwrap();
        let mut event = prepare_event(2, "Lother/CallerKt;");
        event[10..18].fill(0);
        assert!(session.poll_composite(&event).is_err());
        assert_eq!(session.prepare_vm_suspensions, 1);
        assert_eq!(session.take_pending_event().unwrap()["event"], "stopped");
        let thread = session.thread_handle(7).unwrap();
        session.pause(&json!({"threadId":thread})).unwrap();
        session.pause(&json!({"threadId":thread})).unwrap();
        assert!(session.prepare_vm_was_suspended);
        session.release_prepare_suspensions().unwrap();
        assert!(session.suspended);
        assert_eq!(session.prepare_vm_suspensions, 0);
        session
            .continue_request(&json!({"threadId":thread}))
            .unwrap();
        assert!(!session.suspended);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 8))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            4
        );
    }

    #[test]
    fn continue_from_an_internal_all_stop_does_not_resume_an_unowned_extra_count() {
        let (port, target) = scripted_target(vec![
            (1, 9, 0, vec![]),
            (1, 9, 113, vec![]),
            (1, 9, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.configure().unwrap();
        let mut event = prepare_event(2, "Lother/CallerKt;");
        event[10..18].fill(0);
        assert!(session.poll_composite(&event).is_err());
        assert_eq!(session.take_pending_event().unwrap()["event"], "stopped");
        session.continue_request(&json!({})).unwrap();
        assert!(!session.suspended);
        assert_eq!(session.prepare_vm_suspensions, 0);
        assert!(session.take_pending_event().is_none());
        assert_eq!(
            target
                .join()
                .unwrap()
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            3
        );
    }

    #[test]
    fn a_failed_thread_resume_does_not_starve_another_thread_or_a_promoted_all_count() {
        let (port, target) = scripted_target(vec![
            (11, 3, 113, vec![]),
            (11, 3, 0, vec![]),
            (1, 9, 0, vec![]),
            (11, 3, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.reserve_metadata(96).unwrap();
        session.prepare_suspensions = BTreeSet::from([7, 8]);
        session.prepare_vm_suspensions = 1;
        session.prepare_vm_was_suspended = true;
        assert!(session.release_prepare_suspensions().is_err());
        assert_eq!(session.prepare_suspensions, BTreeSet::from([7]));
        assert_eq!(session.prepare_vm_suspensions, 0);
        assert!(session.suspended);
        session.release_prepare_suspensions().unwrap();
        assert!(session.prepare_suspensions.is_empty());
        let requests = target.join().unwrap();
        let tail = &requests[requests.len() - 4..];
        assert_eq!(
            (tail[0].0, tail[0].1, tail[0].2.clone()),
            (11, 3, 7_u64.to_be_bytes().to_vec())
        );
        assert_eq!(
            (tail[1].0, tail[1].1, tail[1].2.clone()),
            (11, 3, 8_u64.to_be_bytes().to_vec())
        );
        assert_eq!((tail[2].0, tail[2].1), (1, 9));
        assert_eq!(
            (tail[3].0, tail[3].1, tail[3].2.clone()),
            (11, 3, 7_u64.to_be_bytes().to_vec())
        );
    }

    #[test]
    fn acknowledged_configuration_releases_attach_ownership_while_an_internal_all_count_is_pending()
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nodelay(true).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut handshake = [0; 14];
            stream.read_exact(&mut handshake).unwrap();
            assert_eq!(&handshake, b"JDWP-Handshake");
            stream.write_all(&handshake).unwrap();
            let mut threads = 1_u32.to_be_bytes().to_vec();
            threads.extend(7_u64.to_be_bytes());
            let replies = [
                (
                    1,
                    7,
                    0,
                    [8_u32; 5].into_iter().flat_map(u32::to_be_bytes).collect(),
                ),
                (1, 8, 0, vec![]),
                (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 100_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 101_u32.to_be_bytes().to_vec()),
                (15, 1, 0, 500_u32.to_be_bytes().to_vec()),
                (1, 3, 0, 0_u32.to_be_bytes().to_vec()),
                (2, 12, 101, vec![]),
                (2, 7, 101, vec![]),
                (1, 9, 113, vec![]), // Internal ALL cleanup fails.
                (1, 9, 113, vec![]), // Configuration fails; attach stays owned.
                (1, 9, 0, vec![]),   // Configuration acknowledges attach release.
                (1, 9, 0, vec![]),   // Internal cleanup acknowledges its own count.
                (1, 4, 0, threads),
                (11, 1, 0, jdwp_text("main")),
                (1, 8, 0, vec![]),
                (1, 9, 0, vec![]), // Real user Pause, then Continue.
            ];
            let mut requests = Vec::new();
            for (turn, (set, command, error, body)) in replies.into_iter().enumerate() {
                let mut header = [0; 11];
                stream.read_exact(&mut header).unwrap();
                assert_eq!((header[8], header[9], header[10]), (0, set, command));
                let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
                assert!((11..=crate::wire::MAX_PACKET_BYTES).contains(&length));
                let mut request = vec![0; length - 11];
                stream.read_exact(&mut request).unwrap();
                requests.push((set, command, request));
                let mut reply = ((11 + body.len()) as u32).to_be_bytes().to_vec();
                reply.extend(&header[4..8]);
                reply.push(0x80);
                reply.extend((error as u16).to_be_bytes());
                reply.extend(body);
                if turn == 6 {
                    // A separate legal system-thread prepare is promoted from
                    // the requested EVENT_THREAD policy to ALL by JDWP.
                    let mut prepare = vec![2];
                    prepare.extend(1_u32.to_be_bytes());
                    prepare.push(8);
                    prepare.extend(500_u32.to_be_bytes());
                    prepare.extend(0_u64.to_be_bytes());
                    prepare.push(1);
                    prepare.extend(99_u64.to_be_bytes());
                    prepare.extend(jdwp_text("Lforeign/Other;"));
                    prepare.extend(2_u32.to_be_bytes());
                    let mut packet = ((11 + prepare.len()) as u32).to_be_bytes().to_vec();
                    packet.extend(1000_u32.to_be_bytes());
                    packet.extend([0, 64, 100]);
                    packet.extend(prepare);
                    reply.extend(packet);
                }
                stream.write_all(&reply).unwrap();
            }
            requests
        });
        let mut session = attach_mock(port);
        assert_eq!(
            session
                .set_breakpoints(
                    &json!({"source":{"path":"/android/Main.java"},"breakpoints":[{"line":9}]})
                )
                .unwrap()["breakpoints"][0]["verified"],
            false
        );
        let error = session.poll().unwrap_err();
        assert!(session.poll_error_is_recoverable(&error));
        assert_eq!(session.prepare_vm_suspensions, 1);
        assert!(session.prepare_vm_was_suspended);
        assert!(session.suspended);
        assert_eq!(session.take_pending_event().unwrap()["event"], "stopped");
        assert!(session.configure().is_err());
        assert!(!session.configured);
        assert_eq!(session.prepare_vm_suspensions, 1);
        assert!(session.prepare_vm_was_suspended);
        assert!(session.suspended);
        session.configure().unwrap();
        assert!(session.configured);
        assert_eq!(session.prepare_vm_suspensions, 1);
        assert!(!session.prepare_vm_was_suspended);
        assert!(session.suspended);
        session.release_prepare_suspensions().unwrap();
        assert_eq!(session.prepare_vm_suspensions, 0);
        assert!(!session.suspended);
        let continued = session.take_pending_event().unwrap();
        assert_eq!(continued["event"], "continued");
        assert_eq!(continued["body"]["allThreadsContinued"], true);
        let threads = session.threads().unwrap();
        assert_eq!(threads["threads"][0]["id"], 1);
        session.pause(&json!({"threadId":1})).unwrap();
        assert!(session.suspended);
        session.pause(&json!({"threadId":1})).unwrap();
        session.continue_request(&json!({"threadId":1})).unwrap();
        assert!(!session.suspended);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 8))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            5
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (11, 3)));
    }

    #[test]
    fn filtered_event_resume_preserves_a_separately_acknowledged_user_pause() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let source = temporary.path().join("app/Main.java");
        std::fs::write(&source, "package app; class Main {}\n").unwrap();
        let (port, target) = scripted_target(vec![
            (1, 9, 0, vec![]),
            (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            (1, 8, 0, vec![]),
            (15, 2, 0, vec![]),
            (6, 1, 0, one_line()),
            (2, 12, 101, vec![]),
            (2, 7, 0, jdwp_text("Main.java")),
            (15, 1, 0, 902_u32.to_be_bytes().to_vec()),
            (1, 9, 0, vec![]),
            (15, 2, 0, vec![]),
            (1, 9, 0, vec![]),
        ]);
        let mut session = attach_mock(port);
        session.roots = vec![temporary.path().to_path_buf()];
        session.classes.insert(3, prepared_class(3, "Lapp/Main;"));
        session.configure().unwrap();
        assert!(!session.suspended);
        let request = session.install_step(7, 1).unwrap();
        assert_eq!(request, 900);
        session.step = Some(Step {
            start: Position {
                path: source,
                line: 9,
            },
            ..owned_step(request)
        });
        let thread = session.thread_handle(7).unwrap();
        // A user Pause can be acknowledged before an already-queued step event
        // reaches the adapter. Its count is separate from that ALL event count.
        session.pause(&json!({"threadId":thread})).unwrap();
        assert!(session.suspended);
        let event = Bytes::new()
            .byte(2)
            .u32(1)
            .byte(1)
            .u32(900)
            .id(7, 8)
            .location(
                Location {
                    tag: 1,
                    class: 3,
                    method: 7,
                    index: 0,
                },
                8,
                8,
            )
            .0;
        let events = session.poll_composite(&event).unwrap();
        assert!(events.is_empty());
        assert!(session.suspended);
        assert_eq!(session.step.as_ref().unwrap().request, 902);
        session.pause(&json!({"threadId":thread})).unwrap();
        session
            .continue_request(&json!({"threadId":thread}))
            .unwrap();
        assert!(!session.suspended);
        let requests = target.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 8))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            3
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .count(),
            2
        );
    }
}
