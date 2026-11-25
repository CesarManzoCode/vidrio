//! The event loop of a window agent (HTML §8.1.7): task queues by task
//! source, the processing model (one task, a microtask checkpoint, then
//! "update the rendering" at a rendering opportunity), timers (HTML §8.6)
//! and animation frame callbacks (HTML §8.10).
//!
//! The loop keeps one FIFO of tasks: picking the oldest runnable task of
//! any queue is a valid choice of the processing model, and it keeps the
//! relative order of tasks from different sources the way they were
//! queued. A task is runnable when it has no document or its document is
//! fully active; the tasks of a document that was discarded are dropped.
//!
//! Rendering opportunities come at 60 Hz of the loop's clock. The clock
//! is either real time or virtual: a virtual clock only moves when
//! nothing is runnable and no network fetch is pending, and then it jumps
//! to the next timer or rendering opportunity, so pages run as fast as
//! the machine allows with the same ordering as in real time.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use vjs::gc::{Realm, Trace, Tracer};
use vjs::{Agent, Throw, Value};

use super::UserAgent;
use crate::dom::DocRef;

/// The interval between rendering opportunities (60 Hz).
pub const FRAME_INTERVAL: f64 = 1000.0 / 60.0;

/// Timers nested deeper than this are clamped to [`TIMER_CLAMP`] ms.
pub const TIMER_NESTING_LIMIT: u32 = 5;
pub const TIMER_CLAMP: f64 = 4.0;

/// The task sources of HTML §8.1.7.4 that vidrio uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskSource {
    DomManipulation,
    UserInteraction,
    Networking,
    Navigation,
    HistoryTraversal,
    Timer,
    PostedMessage,
    Rendering,
    Other,
}

/// The work of a task. Its values are roots while it is queued.
pub trait TaskRun: Trace {
    fn run(self: Box<Self>, agent: &mut Agent);
}

/// A task whose closure holds no JavaScript value.
pub struct NativeTask<F>(pub F);

impl<F> Trace for NativeTask<F> {
    fn trace(&self, _tracer: &mut Tracer) {}
}

impl<F: FnOnce(&mut Agent) + 'static> TaskRun for NativeTask<F> {
    fn run(self: Box<Self>, agent: &mut Agent) {
        (self.0)(agent)
    }
}

/// A task that holds JavaScript values (traced) for its closure.
pub struct ValueTask {
    pub values: Vec<Value>,
    #[allow(clippy::type_complexity)]
    pub run: Box<dyn FnOnce(&mut Agent, Vec<Value>)>,
}

impl Trace for ValueTask {
    fn trace(&self, tracer: &mut Tracer) {
        self.values.trace(tracer);
    }
}

impl TaskRun for ValueTask {
    fn run(self: Box<Self>, agent: &mut Agent) {
        (self.run)(agent, self.values)
    }
}

/// A queued task.
pub struct Task {
    pub source: TaskSource,
    /// The document the task belongs to (`None`: always runnable).
    pub document: Option<DocRef>,
    /// For tasks of the timer initialization steps: their nesting level.
    pub timer_nesting: Option<u32>,
    pub run: Box<dyn TaskRun>,
}

/// How the loop's clock moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockMode {
    Real,
    Virtual,
}

/// The clock of the event loop, in milliseconds since the user agent
/// started.
pub struct Clock {
    pub mode: ClockMode,
    start: Instant,
    virtual_now: Cell<f64>,
}

impl Clock {
    pub fn new(mode: ClockMode) -> Clock {
        Clock {
            mode,
            start: Instant::now(),
            virtual_now: Cell::new(0.0),
        }
    }

    pub fn now(&self) -> f64 {
        match self.mode {
            ClockMode::Real => self.start.elapsed().as_secs_f64() * 1000.0,
            ClockMode::Virtual => self.virtual_now.get(),
        }
    }

    /// Moves a virtual clock forward to `t` (never backwards).
    pub fn advance_to(&self, t: f64) {
        if self.mode == ClockMode::Virtual && t > self.virtual_now.get() {
            self.virtual_now.set(t);
        }
    }
}

/// What a timer runs.
#[derive(Clone, Debug)]
pub enum TimerHandler {
    Function(Value),
    Source(String),
}

/// An entry of a global's map of active timers, with what its task needs.
pub struct Timer {
    pub realm: Realm,
    pub id: i32,
    /// The unique internal value of the map entry.
    pub handle: u64,
    pub handler: TimerHandler,
    pub args: Vec<Value>,
    pub timeout: f64,
    pub repeat: bool,
    pub nesting: u32,
    pub deadline: f64,
    pub seq: u64,
    /// The timer's task is queued and has not run yet.
    pub queued: bool,
}

impl Trace for Timer {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.realm(self.realm);
        if let TimerHandler::Function(f) = &self.handler {
            tracer.value(*f);
        }
        self.args.trace(tracer);
    }
}

/// K4's part of "update the rendering": style, layout and painting.
pub trait RenderingHook {
    /// Whether `doc` has changes that need a rendering update.
    fn needs_update(&self, doc: &DocRef) -> bool;
    /// Recomputes style and layout and paints `doc`.
    fn update_rendering(&self, agent: &mut Agent, doc: &DocRef);
}

/// The event loop.
pub struct EventLoop {
    pub clock: Clock,
    pub tasks: RefCell<VecDeque<Task>>,
    pub timers: RefCell<Vec<Timer>>,
    next_timer_id: Cell<i32>,
    next_handle: Cell<u64>,
    next_seq: Cell<u64>,
    /// The time of the last rendering opportunity taken.
    last_frame: Cell<f64>,
    pub performing_microtask_checkpoint: Cell<bool>,
    /// The timer nesting level of the task that is running, if it came
    /// from the timer initialization steps.
    pub current_timer_nesting: Cell<Option<u32>>,
    pub rendering_hook: RefCell<Option<Rc<dyn RenderingHook>>>,
}

impl Trace for EventLoop {
    fn trace(&self, tracer: &mut Tracer) {
        for t in self.tasks.borrow().iter() {
            t.run.trace(tracer);
        }
        for t in self.timers.borrow().iter() {
            t.trace(tracer);
        }
    }
}

impl EventLoop {
    pub fn new(mode: ClockMode) -> EventLoop {
        EventLoop {
            clock: Clock::new(mode),
            tasks: RefCell::new(VecDeque::new()),
            timers: RefCell::new(Vec::new()),
            next_timer_id: Cell::new(1),
            next_handle: Cell::new(1),
            next_seq: Cell::new(1),
            last_frame: Cell::new(f64::NEG_INFINITY),
            performing_microtask_checkpoint: Cell::new(false),
            current_timer_nesting: Cell::new(None),
            rendering_hook: RefCell::new(None),
        }
    }

    /// "Queue a task" on `source` for `document`.
    pub fn queue(&self, source: TaskSource, document: Option<DocRef>, run: Box<dyn TaskRun>) {
        self.tasks.borrow_mut().push_back(Task {
            source,
            document,
            timer_nesting: None,
            run,
        });
    }

    fn seq(&self) -> u64 {
        let s = self.next_seq.get();
        self.next_seq.set(s + 1);
        s
    }

    /// The deadline of the earliest timer whose task is not queued.
    fn next_timer_deadline(&self) -> Option<f64> {
        self.timers
            .borrow()
            .iter()
            .filter(|t| !t.queued)
            .map(|t| t.deadline)
            .min_by(|a, b| a.total_cmp(b))
    }
}

/// "Queue a task" on `source` for `document` that runs `f`.
pub fn queue_task(
    source: TaskSource,
    document: Option<DocRef>,
    f: impl FnOnce(&mut Agent) + 'static,
) {
    if let Some(ua) = super::ua() {
        ua.event_loop
            .queue(source, document, Box::new(NativeTask(f)));
    }
}

/// "Queue a task" whose closure needs JavaScript `values` (kept alive
/// while the task waits).
pub fn queue_value_task(
    source: TaskSource,
    document: Option<DocRef>,
    values: Vec<Value>,
    f: impl FnOnce(&mut Agent, Vec<Value>) + 'static,
) {
    if let Some(ua) = super::ua() {
        ua.event_loop.queue(
            source,
            document,
            Box::new(ValueTask {
                values,
                run: Box::new(f),
            }),
        );
    }
}

// ----- microtasks ----------------------------------------------------------

/// "Perform a microtask checkpoint" (HTML §8.1.7.3).
pub fn perform_microtask_checkpoint(agent: &mut Agent) {
    let Some(ua) = super::ua() else {
        return;
    };
    if ua.event_loop.performing_microtask_checkpoint.get() {
        return;
    }
    ua.event_loop.performing_microtask_checkpoint.set(true);
    loop {
        let errors = agent.run_jobs();
        for Throw(e) in errors {
            super::errors::report_exception_in_current(agent, e);
        }
        if !agent.has_pending_jobs() {
            break;
        }
    }
    super::errors::notify_about_rejected_promises(agent);
    ua.event_loop.performing_microtask_checkpoint.set(false);
}

/// "Clean up after running script" (and after running a callback): a
/// microtask checkpoint when the JavaScript execution context stack is
/// empty.
pub fn clean_up_after_running_script(agent: &mut Agent) {
    if agent.running_context().is_none() {
        perform_microtask_checkpoint(agent);
    }
}

/// A microtask that calls a callback and reports its exception
/// (`queueMicrotask`).
pub struct CallbackMicrotask {
    pub callback: Value,
    pub realm: Realm,
}

impl Trace for CallbackMicrotask {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(self.callback);
        tracer.realm(self.realm);
    }
}

impl vjs::agent::JobTask for CallbackMicrotask {
    fn run(self: Box<Self>, agent: &mut Agent) -> vjs::JsResult<()> {
        if let Err(Throw(e)) = vjs::ops::call(agent, self.callback, Value::Undefined, &[]) {
            if let Some(state) = crate::bindings::state_of(agent, self.realm) {
                super::errors::report_exception(agent, &state, e);
            }
        }
        Ok(())
    }
}

/// Queues `callback` as a microtask of `realm`.
pub fn queue_microtask(agent: &mut Agent, realm: Realm, callback: Value) {
    agent.enqueue_job(vjs::agent::Job {
        realm: Some(realm),
        task: Box::new(CallbackMicrotask { callback, realm }),
    });
}

// ----- timers --------------------------------------------------------------

/// The timer initialization steps (HTML §8.6): returns the timer's id.
/// `previous` is the id (and entry) of a repeating timer being re-armed.
#[allow(clippy::too_many_arguments)]
pub fn initialize_timer(
    ua: &UserAgent,
    realm: Realm,
    handler: TimerHandler,
    timeout: f64,
    args: Vec<Value>,
    repeat: bool,
    previous: Option<i32>,
) -> i32 {
    let el = &ua.event_loop;
    let id = match previous {
        Some(id) => id,
        None => {
            let id = el.next_timer_id.get();
            el.next_timer_id
                .set(if id == i32::MAX { 1 } else { id + 1 });
            id
        }
    };
    let mut nesting = el.current_timer_nesting.get().unwrap_or(0);
    let mut timeout = if timeout.is_nan() || timeout < 0.0 {
        0.0
    } else {
        timeout
    };
    if nesting > TIMER_NESTING_LIMIT && timeout < TIMER_CLAMP {
        timeout = TIMER_CLAMP;
    }
    nesting += 1;
    let handle = el.next_handle.get();
    el.next_handle.set(handle + 1);
    let timer = Timer {
        realm,
        id,
        handle,
        handler,
        args,
        timeout,
        repeat,
        nesting,
        deadline: el.clock.now() + timeout,
        seq: el.seq(),
        queued: false,
    };
    let mut timers = el.timers.borrow_mut();
    timers.retain(|t| !(t.realm == realm && t.id == id));
    timers.push(timer);
    id
}

/// `clearTimeout`/`clearInterval`: removes `id` from the map of active
/// timers of `realm`'s global.
pub fn clear_timer(ua: &UserAgent, realm: Realm, id: i32) {
    ua.event_loop
        .timers
        .borrow_mut()
        .retain(|t| !(t.realm == realm && t.id == id));
}

/// Removes every timer of `realm` (its document was discarded).
pub fn clear_timers_of(ua: &UserAgent, realm: Realm) {
    ua.event_loop
        .timers
        .borrow_mut()
        .retain(|t| t.realm != realm);
}

/// Queues the tasks of the timers whose time has come, earliest first.
fn queue_due_timers(ua: &UserAgent) {
    let el = &ua.event_loop;
    let now = el.clock.now();
    let mut due: Vec<(f64, u64, Realm, i32, u64, u32)> = el
        .timers
        .borrow_mut()
        .iter_mut()
        .filter(|t| !t.queued && t.deadline <= now)
        .map(|t| {
            t.queued = true;
            (t.deadline, t.seq, t.realm, t.id, t.handle, t.nesting)
        })
        .collect();
    due.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, _, realm, id, handle, nesting) in due {
        let doc = super::window::document_of_realm(ua, realm);
        el.tasks.borrow_mut().push_back(Task {
            source: TaskSource::Timer,
            document: doc,
            timer_nesting: Some(nesting),
            run: Box::new(NativeTask(move |agent: &mut Agent| {
                run_timer(agent, realm, id, handle)
            })),
        });
    }
}

/// The task of a timer (step 8 of the timer initialization steps).
fn run_timer(agent: &mut Agent, realm: Realm, id: i32, handle: u64) {
    let Some(ua) = super::ua() else {
        return;
    };
    let (handler, args, timeout, repeat) = {
        let timers = ua.event_loop.timers.borrow();
        let Some(t) = timers
            .iter()
            .find(|t| t.realm == realm && t.id == id && t.handle == handle)
        else {
            return;
        };
        (t.handler.clone(), t.args.clone(), t.timeout, t.repeat)
    };
    let Some(state) = crate::bindings::state_of(agent, realm) else {
        return;
    };
    match &handler {
        TimerHandler::Function(f) => {
            let this = state
                .html
                .proxy
                .get()
                .or(state.global.get())
                .map_or(Value::Undefined, Value::Object);
            // The callback's realm keeps the handler alive across the call.
            let keep = Rc::new(super::window::Roots(vec![*f]));
            let id_root = agent.heap.add_root_set(keep.clone() as Rc<dyn Trace>);
            let result = vjs::ops::call(agent, *f, this, &args);
            agent.heap.remove_root_set(id_root);
            if let Err(Throw(e)) = result {
                super::errors::report_exception(agent, &state, e);
            }
            clean_up_after_running_script(agent);
        }
        TimerHandler::Source(source) => {
            super::script::run_classic_source(agent, &state, source, None);
        }
    }
    // The entry may have been cleared or replaced meanwhile.
    let still = ua
        .event_loop
        .timers
        .borrow()
        .iter()
        .any(|t| t.realm == realm && t.id == id && t.handle == handle);
    if !still {
        return;
    }
    if repeat {
        initialize_timer(&ua, realm, handler, timeout, args, true, Some(id));
    } else {
        clear_timer(&ua, realm, id);
    }
}

// ----- the processing model --------------------------------------------------

/// Pops the oldest runnable task; drops tasks of discarded documents.
fn take_runnable(ua: &UserAgent) -> Option<Task> {
    let mut tasks = ua.event_loop.tasks.borrow_mut();
    let mut i = 0;
    while i < tasks.len() {
        let runnable = match &tasks[i].document {
            None => true,
            Some(d) => ua.is_fully_active(d),
        };
        if runnable {
            return tasks.remove(i);
        }
        let discarded = match &tasks[i].document {
            Some(d) => ua.doc_info(d).is_none_or(|info| info.discarded.get()),
            None => false,
        };
        if discarded {
            tasks.remove(i);
        } else {
            i += 1;
        }
    }
    None
}

/// Runs one task and what follows it in the processing model; false if
/// nothing was runnable.
pub fn run_once(agent: &mut Agent) -> bool {
    let Some(ua) = super::ua() else {
        return false;
    };
    super::loader::poll_network(&ua, None);
    queue_due_timers(&ua);
    if let Some(task) = take_runnable(&ua) {
        ua.event_loop.current_timer_nesting.set(task.timer_nesting);
        task.run.run(agent);
        ua.event_loop.current_timer_nesting.set(None);
        perform_microtask_checkpoint(agent);
        agent.safepoint(&[]);
        maybe_update_rendering(agent, &ua);
        return true;
    }
    if let Some(result) = agent.run_host_task() {
        if let Err(Throw(e)) = result {
            super::errors::report_exception_in_current(agent, e);
        }
        perform_microtask_checkpoint(agent);
        return true;
    }
    maybe_update_rendering(agent, &ua)
}

/// Whether a rendering update is wanted: animation frame callbacks are
/// waiting or the rendering hook has changes.
fn frame_requested(agent: &Agent, ua: &UserAgent) -> bool {
    let hook = ua.event_loop.rendering_hook.borrow().clone();
    for doc in super::navigable::fully_active_documents(ua) {
        if let Some(state) = super::window::state_of_document(agent, ua, &doc) {
            if !state.html.animation_frames.borrow().is_empty() {
                return true;
            }
        }
        if hook.as_ref().is_some_and(|h| h.needs_update(&doc)) {
            return true;
        }
    }
    false
}

/// The time of the next rendering opportunity after the last one.
fn next_frame_time(ua: &UserAgent) -> f64 {
    let last = ua.event_loop.last_frame.get();
    if last == f64::NEG_INFINITY {
        return ua.event_loop.clock.now();
    }
    ((last / FRAME_INTERVAL).floor() + 1.0) * FRAME_INTERVAL
}

/// "Update the rendering" if a rendering opportunity has come and a frame
/// is wanted; whether it ran.
fn maybe_update_rendering(agent: &mut Agent, ua: &Rc<UserAgent>) -> bool {
    if !frame_requested(agent, ua) {
        return false;
    }
    let now = ua.event_loop.clock.now();
    if now < next_frame_time(ua) {
        return false;
    }
    ua.event_loop.last_frame.set(now);
    update_the_rendering(agent, ua, now);
    true
}

/// "Update the rendering" (HTML §8.1.7.3, steps 6–16): animation frame
/// callbacks of every fully active document, then K4's style, layout and
/// painting.
fn update_the_rendering(agent: &mut Agent, ua: &Rc<UserAgent>, now: f64) {
    let docs = super::navigable::fully_active_documents(ua);
    for doc in &docs {
        if let Some(state) = super::window::state_of_document(agent, ua, doc) {
            let origin = state.html.time_origin.get();
            run_animation_frame_callbacks(agent, &state, now - origin);
        }
    }
    // New `@font-face` rules start their loads before style and layout.
    for doc in &docs {
        crate::bindings::fonts::update_document(agent, doc);
    }
    let hook = ua.event_loop.rendering_hook.borrow().clone();
    if let Some(hook) = hook {
        for doc in &docs {
            if ua.is_fully_active(doc) {
                hook.update_rendering(agent, doc);
            }
        }
    }
}

/// "Run the animation frame callbacks" of a window with timestamp `now`.
pub fn run_animation_frame_callbacks(
    agent: &mut Agent,
    state: &Rc<crate::bindings::RealmState>,
    now: f64,
) {
    let handles: Vec<u32> = state
        .html
        .animation_frames
        .borrow()
        .iter()
        .map(|(h, _)| *h)
        .collect();
    for handle in handles {
        let callback = {
            let mut frames = state.html.animation_frames.borrow_mut();
            match frames.iter().position(|(h, _)| *h == handle) {
                Some(i) => frames.remove(i).1,
                None => continue,
            }
        };
        let keep = Rc::new(super::window::Roots(vec![callback]));
        let id = agent.heap.add_root_set(keep.clone() as Rc<dyn Trace>);
        let result = vjs::ops::call(agent, callback, Value::Undefined, &[Value::Number(now)]);
        agent.heap.remove_root_set(id);
        if let Err(Throw(e)) = result {
            super::errors::report_exception(agent, state, e);
        }
        clean_up_after_running_script(agent);
    }
}

/// Runs the loop until `done` is true, nothing is left, or `limit` of
/// clock time passes; returns false only when the limit stopped it.
pub fn run_until(
    agent: &mut Agent,
    limit: Duration,
    done: &mut dyn FnMut(&mut Agent) -> bool,
) -> bool {
    let Some(ua) = super::ua() else {
        return true;
    };
    let limit_ms = limit.as_secs_f64() * 1000.0;
    let start = ua.event_loop.clock.now();
    let real_start = Instant::now();
    loop {
        if done(agent) {
            return true;
        }
        if ua.event_loop.clock.now() - start > limit_ms || real_start.elapsed() > limit {
            return false;
        }
        if run_once(agent) {
            continue;
        }
        // Nothing runnable: wait for the network, a timer or a frame.
        let mut deadline = ua.event_loop.next_timer_deadline();
        if frame_requested(agent, &ua) {
            let f = next_frame_time(&ua);
            deadline = Some(deadline.map_or(f, |d| d.min(f)));
        }
        let pending_network = ua.net.has_pending();
        let now = ua.event_loop.clock.now();
        match ua.event_loop.clock.mode {
            ClockMode::Virtual => {
                if pending_network {
                    // The network goes first: virtual time stands still
                    // until every fetch has answered.
                    let remaining = limit.saturating_sub(real_start.elapsed());
                    super::loader::poll_network(&ua, Some(remaining.min(Duration::from_secs(1))));
                    continue;
                }
                match deadline {
                    Some(d) => ua.event_loop.clock.advance_to(d.max(now)),
                    None => return true,
                }
            }
            ClockMode::Real => {
                let wait = match deadline {
                    Some(d) => Duration::from_secs_f64(((d - now).max(0.0)) / 1000.0),
                    None if pending_network => Duration::from_millis(50),
                    None => return true,
                };
                let wait = wait.min(limit.saturating_sub(real_start.elapsed()));
                if pending_network {
                    super::loader::poll_network(&ua, Some(wait));
                } else if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
            }
        }
    }
}
