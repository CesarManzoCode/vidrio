//! The HTML state of a document that belongs (or belonged) to a browsing
//! context: its current readiness (HTML §3.1.10), the parser that loads it
//! and the script handling of the "text" insertion mode (HTML §13.2.6.4.8),
//! "the end" (HTML §13.2.7) with `DOMContentLoaded`, `load` and
//! `pageshow`, dynamic markup insertion (`document.open`, `write`,
//! `close`, HTML §8.4) and the HTML steps of tree changes the DOM recorded
//! ([`crate::dom::HtmlChange`]): the insertion steps of `script` and
//! `iframe`, attribute change steps and event handler content attributes.
//!
//! The parser runs inside tasks: a networking task feeds it what arrived,
//! and it runs until it needs more input, a parsing-blocking script is not
//! ready yet, or the input ends. A script becoming ready queues a task
//! that resumes it.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vjs::Agent;
use vjs::gc::Realm;

use super::UserAgent;
use super::event_loop::{TaskSource, queue_task};
use super::navigable::{ContextId, fire_simple_event, fire_window_event};
use super::script::ScriptId;
use crate::dom::events::{EventInit, EventInterface, Target};
use crate::dom::{DocRef, Document, DomException, HtmlChange, NodeId, QuirksMode};
use crate::html::{ParseStatus, Parser};
use crate::url::Url;

/// The current document readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Loading,
    Interactive,
    Complete,
}

impl Readiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Readiness::Loading => "loading",
            Readiness::Interactive => "interactive",
            Readiness::Complete => "complete",
        }
    }
}

/// Where "the end" is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndStep {
    NotStarted,
    /// Running the scripts that execute when the document has finished
    /// parsing (`defer` and module scripts).
    DeferScripts,
    /// `DOMContentLoaded` is queued; waiting for the "as soon as possible"
    /// scripts and for whatever delays the load event.
    AsapScripts,
    /// The `load` task is queued.
    Done,
}

/// What the user agent keeps per document.
pub struct DocInfo {
    pub doc: Weak<RefCell<Document>>,
    pub context: Cell<Option<ContextId>>,
    pub realm: Cell<Option<Realm>>,
    pub readiness: Cell<Readiness>,
    pub is_initial_about_blank: Cell<bool>,
    pub completely_loaded: Cell<bool>,
    pub page_showing: Cell<bool>,
    pub unloaded: Cell<bool>,
    pub unload_counter: Cell<u32>,
    pub discarded: Cell<bool>,
    /// Things that delay the load event besides scripts (iframes whose
    /// documents are loading).
    pub load_delay: Cell<u32>,
    pub loading_iframes: RefCell<Vec<NodeId>>,
    /// The `iframe` elements the document ever had connected.
    pub iframes: RefCell<Vec<NodeId>>,
    /// The `src` and `srcdoc` of each iframe when it last navigated.
    pub iframe_attrs: RefCell<Vec<(NodeId, Option<String>, Option<String>)>>,
    /// The active parser.
    pub parser: RefCell<Option<Rc<RefCell<Parser>>>>,
    /// The active parser was made by `document.open()`.
    pub script_created_parser: Cell<bool>,
    /// Increases with every new parser, so input for an older one (the
    /// network parser `document.open()` replaced) is dropped.
    pub parser_generation: Cell<u64>,
    /// The parser loop is running (it never runs nested).
    pub parsing: Cell<bool>,
    pub active_parser_was_aborted: Cell<bool>,
    pub ignore_destructive_writes: Cell<u32>,
    pub throw_on_dynamic_markup_insertion: Cell<u32>,
    /// `document.currentScript`.
    pub current_script: Cell<Option<NodeId>>,
    /// The pending parsing-blocking script.
    pub pending_parsing_blocking: Cell<Option<ScriptId>>,
    /// The list of scripts that will execute when the document has
    /// finished parsing.
    pub defer_scripts: RefCell<Vec<ScriptId>>,
    /// The list of scripts that will execute in order as soon as possible.
    pub in_order_scripts: RefCell<Vec<ScriptId>>,
    /// The set of scripts that will execute as soon as possible.
    pub asap_scripts: RefCell<Vec<ScriptId>>,
    pub end_step: Cell<EndStep>,
    /// `document.referrer`.
    pub referrer: RefCell<String>,
    /// `document.cookie` (kept per document; no cookie store yet).
    pub cookie: RefCell<String>,
    /// `document.lastModified`.
    pub last_modified: RefCell<String>,
    /// The planned navigation of each form that submitted (HTML
    /// §4.10.21.3): the form and the plan its queued task carries.
    pub planned_navigations: RefCell<Vec<(NodeId, u64)>>,
}

impl DocInfo {
    pub fn new(doc: &DocRef) -> DocInfo {
        DocInfo {
            doc: Rc::downgrade(doc),
            context: Cell::new(None),
            realm: Cell::new(None),
            readiness: Cell::new(Readiness::Loading),
            is_initial_about_blank: Cell::new(false),
            completely_loaded: Cell::new(false),
            page_showing: Cell::new(false),
            unloaded: Cell::new(false),
            unload_counter: Cell::new(0),
            discarded: Cell::new(false),
            load_delay: Cell::new(0),
            loading_iframes: RefCell::new(Vec::new()),
            iframes: RefCell::new(Vec::new()),
            iframe_attrs: RefCell::new(Vec::new()),
            parser: RefCell::new(None),
            script_created_parser: Cell::new(false),
            parser_generation: Cell::new(0),
            parsing: Cell::new(false),
            active_parser_was_aborted: Cell::new(false),
            ignore_destructive_writes: Cell::new(0),
            throw_on_dynamic_markup_insertion: Cell::new(0),
            current_script: Cell::new(None),
            pending_parsing_blocking: Cell::new(None),
            defer_scripts: RefCell::new(Vec::new()),
            in_order_scripts: RefCell::new(Vec::new()),
            asap_scripts: RefCell::new(Vec::new()),
            end_step: Cell::new(EndStep::NotStarted),
            referrer: RefCell::new(String::new()),
            cookie: RefCell::new(String::new()),
            last_modified: RefCell::new(last_modified_now()),
            planned_navigations: RefCell::new(Vec::new()),
        }
    }

    pub fn document(&self) -> Option<DocRef> {
        self.doc.upgrade()
    }

    /// The active parser, if any.
    pub fn active_parser(&self) -> Option<Rc<RefCell<Parser>>> {
        self.parser.borrow().clone()
    }
}

/// `document.lastModified` for a document without a `Last-Modified`
/// header: the current time, as `MM/DD/YYYY hh:mm:ss` (UTC).
fn last_modified_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{m:02}/{d:02}/{y:04} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

// ----- URLs ------------------------------------------------------------------------

/// The fallback base URL of `doc` (HTML §2.4.1): the container's base URL
/// for `about:srcdoc`, the creator's for an `about:blank` with one,
/// otherwise the document's URL.
pub fn fallback_base_url(doc: &DocRef) -> Url {
    let (url, srcdoc) = {
        let d = doc.borrow();
        (d.url.clone(), d.is_srcdoc)
    };
    let about_blank = url.href() == "about:blank";
    if srcdoc || about_blank {
        if let Some(ua) = super::ua() {
            let ctx = ua
                .doc_info(doc)
                .and_then(|i| i.context.get())
                .and_then(|c| ua.context(c));
            if let Some(ctx) = ctx {
                let creator = ctx.container_document().or_else(|| {
                    ctx.opener
                        .get()
                        .and_then(|o| ua.context(o))
                        .and_then(|c| c.active_document())
                });
                if let Some(parent) = creator {
                    if !Rc::ptr_eq(&parent, doc) {
                        return base_url(&parent);
                    }
                }
            }
        }
    }
    url
}

/// The document base URL (HTML §2.4.1): the frozen base URL of the first
/// `base` element with an `href`, or the fallback base URL.
pub fn base_url(doc: &DocRef) -> Url {
    let href = {
        let d = doc.borrow();
        d.inclusive_descendants(NodeId::DOCUMENT)
            .into_iter()
            .filter(|&n| d.is_html_element(n, "base"))
            .find_map(|n| d.get_attribute(n, "href").map(str::to_string))
    };
    let fallback = fallback_base_url(doc);
    match href {
        Some(h) => Url::parse(&h, Some(&fallback)).unwrap_or(fallback),
        None => fallback,
    }
}

// ----- readiness and lifecycle events ---------------------------------------------------

/// "Update the current document readiness" to `readiness`: fires
/// `readystatechange` at the document when it changes.
pub fn update_readiness(agent: &mut Agent, doc: &DocRef, info: &DocInfo, readiness: Readiness) {
    if info.readiness.get() == readiness {
        return;
    }
    info.readiness.set(readiness);
    fire_simple_event(
        agent,
        doc,
        Target::Node(NodeId::DOCUMENT),
        "readystatechange",
        EventInit::default(),
    );
    super::event_loop::clean_up_after_running_script(agent);
}

// ----- the parser -----------------------------------------------------------------------

/// Creates the active parser of `doc` (a network parser, or with
/// `script_created` the parser of `document.open()`).
pub fn create_parser(
    ua: &UserAgent,
    doc: &DocRef,
    info: &DocInfo,
    script_created: bool,
) -> Rc<RefCell<Parser>> {
    let scripting = ua.scripting_enabled(doc);
    let parser = Rc::new(RefCell::new(Parser::new(doc.clone(), scripting)));
    if script_created {
        parser.borrow_mut().set_insertion_point_here();
    }
    *info.parser.borrow_mut() = Some(parser.clone());
    info.script_created_parser.set(script_created);
    info.parser_generation.set(info.parser_generation.get() + 1);
    info.end_step.set(EndStep::NotStarted);
    parser
}

/// Appends decoded network input for the parser of generation
/// `generation` and runs it.
pub fn feed_parser(agent: &mut Agent, doc: &DocRef, generation: u64, text: &str) {
    let Some(info) = super::ua().and_then(|ua| ua.doc_info(doc)) else {
        return;
    };
    if info.parser_generation.get() != generation {
        return;
    }
    let Some(parser) = info.active_parser() else {
        return;
    };
    parser.borrow_mut().feed(text);
    run_parser(agent, doc);
}

/// The network input of the parser of generation `generation` ended.
pub fn end_parser_input(agent: &mut Agent, doc: &DocRef, generation: u64) {
    let Some(info) = super::ua().and_then(|ua| ua.doc_info(doc)) else {
        return;
    };
    if info.parser_generation.get() != generation {
        return;
    }
    let Some(parser) = info.active_parser() else {
        return;
    };
    parser.borrow_mut().end();
    run_parser(agent, doc);
}

/// Runs the active parser of `doc` as far as it can go: executes the
/// pending parsing-blocking script once it is ready, stops when input
/// runs out or a script is not ready, and runs "the end" at the end of
/// the input.
pub fn run_parser(agent: &mut Agent, doc: &DocRef) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    if info.parsing.get() || info.discarded.get() {
        return;
    }
    info.parsing.set(true);
    loop {
        let Some(parser) = info.active_parser() else {
            break;
        };
        if let Some(id) = info.pending_parsing_blocking.get() {
            // The script also waits for style sheets that block scripts.
            if !super::script::is_ready(&ua, id) || crate::css::sheets::has_blocking(doc) {
                break;
            }
            // "Execute the pending parsing-blocking script."
            info.pending_parsing_blocking.set(None);
            parser.borrow_mut().set_insertion_point_here();
            parser.borrow_mut().increment_script_nesting_level();
            super::script::execute_script_element(agent, id);
            parser.borrow_mut().decrement_script_nesting_level();
            parser.borrow_mut().clear_insertion_point();
            process_html_changes(agent, doc);
            continue;
        }
        let status = parser.borrow_mut().run();
        process_html_changes(agent, doc);
        match status {
            ParseStatus::Script(element) => {
                handle_script_end_tag(agent, doc, &info, &parser, element);
            }
            ParseStatus::Done => {
                if info
                    .active_parser()
                    .is_some_and(|p| Rc::ptr_eq(&p, &parser))
                {
                    the_end(agent, doc, &info);
                }
                break;
            }
            ParseStatus::NeedInput | ParseStatus::Paused | ParseStatus::ReachedInsertionPoint => {
                break;
            }
        }
    }
    info.parsing.set(false);
}

/// The handling of a `script` end tag in the "text" insertion mode: the
/// parser already incremented the script nesting level.
fn handle_script_end_tag(
    agent: &mut Agent,
    doc: &DocRef,
    info: &DocInfo,
    parser: &Rc<RefCell<Parser>>,
    element: NodeId,
) {
    if agent.running_context().is_none() {
        super::event_loop::perform_microtask_checkpoint(agent);
    }
    super::script::prepare_script(agent, doc, element);
    process_html_changes(agent, doc);
    parser.borrow_mut().script_finished();
    if info.pending_parsing_blocking.get().is_some() && parser.borrow().script_nesting_level() > 0 {
        // A document.write() call: the outer invocation of the tokenizer
        // goes on once the script that called it returns.
        parser.borrow_mut().set_paused(true);
    }
}

/// The `document.write` step "have the HTML parser process the characters
/// that were inserted, one at a time, until it reaches the insertion point
/// or the parser pause flag is set".
fn process_written(agent: &mut Agent, doc: &DocRef, info: &DocInfo, parser: &Rc<RefCell<Parser>>) {
    loop {
        let status = parser.borrow_mut().run_until_insertion_point();
        process_html_changes(agent, doc);
        match status {
            ParseStatus::Script(element) => {
                handle_script_end_tag(agent, doc, info, parser, element);
                if parser.borrow().is_paused() {
                    return;
                }
            }
            ParseStatus::Done => {
                if info.active_parser().is_some_and(|p| Rc::ptr_eq(&p, parser)) {
                    the_end(agent, doc, info);
                }
                return;
            }
            _ => return,
        }
    }
}

/// Queues a networking task that resumes the parser of `doc` (a
/// parsing-blocking script became ready).
pub fn resume_parser_soon(doc: &DocRef) {
    let d = doc.clone();
    queue_task(TaskSource::Networking, Some(doc.clone()), move |agent| {
        run_parser(agent, &d);
    });
}

// ----- the end ---------------------------------------------------------------------------

/// "The end" (HTML §13.2.7), from its first step.
pub fn the_end(agent: &mut Agent, doc: &DocRef, info: &DocInfo) {
    if info.end_step.get() != EndStep::NotStarted {
        return;
    }
    // The parser is no longer active.
    *info.parser.borrow_mut() = None;
    info.script_created_parser.set(false);
    update_readiness(agent, doc, info, Readiness::Interactive);
    info.end_step.set(EndStep::DeferScripts);
    continue_the_end(agent, doc);
}

/// Goes on with "the end" of `doc` as far as its scripts and the things
/// that delay its load event allow.
pub fn continue_the_end(agent: &mut Agent, doc: &DocRef) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    loop {
        match info.end_step.get() {
            EndStep::NotStarted | EndStep::Done => return,
            EndStep::DeferScripts => {
                let first = info.defer_scripts.borrow().first().copied();
                match first {
                    Some(id) if super::script::is_ready(&ua, id) => {
                        info.defer_scripts.borrow_mut().remove(0);
                        super::script::execute_script_element(agent, id);
                        process_html_changes(agent, doc);
                    }
                    Some(_) => return,
                    None => {
                        info.end_step.set(EndStep::AsapScripts);
                        let d = doc.clone();
                        queue_task(
                            TaskSource::DomManipulation,
                            Some(doc.clone()),
                            move |agent| {
                                let init = EventInit {
                                    bubbles: true,
                                    ..EventInit::default()
                                };
                                fire_simple_event(
                                    agent,
                                    &d,
                                    Target::Node(NodeId::DOCUMENT),
                                    "DOMContentLoaded",
                                    init,
                                );
                            },
                        );
                    }
                }
            }
            EndStep::AsapScripts => {
                let waiting = !info.asap_scripts.borrow().is_empty()
                    || !info.in_order_scripts.borrow().is_empty()
                    || info.load_delay.get() > 0;
                if waiting {
                    return;
                }
                info.end_step.set(EndStep::Done);
                let d = doc.clone();
                queue_task(
                    TaskSource::DomManipulation,
                    Some(doc.clone()),
                    move |agent| {
                        load_task(agent, &d);
                    },
                );
                return;
            }
        }
    }
}

/// Step 7 of "the end": readiness `complete`, `load` at the window,
/// `pageshow`, then "completely finish loading".
fn load_task(agent: &mut Agent, doc: &DocRef) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    update_readiness(agent, doc, &info, Readiness::Complete);
    let Some(ctx) = info.context.get().and_then(|c| ua.context(c)) else {
        return;
    };
    let Some(w) = ctx.active_window() else {
        return;
    };
    if !Rc::ptr_eq(&w.doc, doc) {
        return;
    }
    fire_window_event(
        agent,
        &w.state,
        "load",
        EventInterface::Event,
        EventInit::default(),
        &[],
        true,
    );
    super::event_loop::clean_up_after_running_script(agent);
    if !info.page_showing.get() {
        info.page_showing.set(true);
        fire_window_event(
            agent,
            &w.state,
            "pageshow",
            EventInterface::Other("PageTransitionEvent"),
            EventInit::default(),
            &[("persisted", vjs::Value::Boolean(false))],
            true,
        );
        super::event_loop::clean_up_after_running_script(agent);
    }
    completely_finish_loading(&ua, doc, &info, &ctx);
}

/// "Completely finish loading": the document is completely loaded and
/// its container, if an iframe, runs the iframe load event steps.
pub fn completely_finish_loading(
    _ua: &UserAgent,
    doc: &DocRef,
    info: &DocInfo,
    ctx: &Rc<super::navigable::BrowsingContext>,
) {
    info.completely_loaded.set(true);
    if let Some((parent_doc, _)) = ctx.container_element() {
        let c = ctx.clone();
        let _ = doc;
        queue_task(
            TaskSource::DomManipulation,
            Some(parent_doc),
            move |agent| {
                super::navigable::child_finished_loading(agent, &c);
            },
        );
    }
}

// ----- dynamic markup insertion ------------------------------------------------------------

/// The document open steps (HTML §8.4.1).
pub fn document_open(agent: &mut Agent, doc: &DocRef) -> Result<(), DomException> {
    if !doc.borrow().is_html() {
        return Err(DomException::InvalidStateError);
    }
    let Some(ua) = super::ua() else {
        return Err(DomException::InvalidStateError);
    };
    let Some(info) = ua.doc_info(doc) else {
        // A document without a browsing context: a parser that never runs
        // scripts.
        return Err(DomException::InvalidStateError);
    };
    if info.throw_on_dynamic_markup_insertion.get() > 0 {
        return Err(DomException::InvalidStateError);
    }
    if info
        .active_parser()
        .is_some_and(|p| p.borrow().script_nesting_level() > 0)
    {
        return Ok(());
    }
    if info.unload_counter.get() > 0 || info.active_parser_was_aborted.get() {
        return Ok(());
    }
    // Stop an ongoing navigation of the document's browsing context.
    if let Some(ctx) = info.context.get().and_then(|c| ua.context(c)) {
        ctx.navigation.set(0);
    }
    // Erase the event listeners of every node and of the window.
    let nodes = doc.borrow().inclusive_descendants(NodeId::DOCUMENT);
    if let Some(state) = super::window::state_of_document(agent, &ua, doc) {
        let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
        for n in nodes {
            crate::dom::events::remove_all_listeners(
                &mut doc.borrow_mut(),
                &mut host,
                Target::Node(n),
            );
        }
        if state.document().is_some_and(|d| Rc::ptr_eq(&d, doc)) {
            crate::dom::events::remove_all_listeners(
                &mut doc.borrow_mut(),
                &mut host,
                Target::Window,
            );
        }
    }
    {
        let mut d = doc.borrow_mut();
        d.replace_all(None, NodeId::DOCUMENT);
        d.mode = QuirksMode::NoQuirks;
        d.html_changes.clear();
    }
    info.is_initial_about_blank.set(false);
    info.pending_parsing_blocking.set(None);
    create_parser(&ua, doc, &info, true);
    update_readiness(agent, doc, &info, Readiness::Loading);
    Ok(())
}

/// `document.write(text)` (and `writeln` with `newline`).
pub fn document_write(
    agent: &mut Agent,
    doc: &DocRef,
    text: &str,
    newline: bool,
) -> Result<(), DomException> {
    if !doc.borrow().is_html() {
        return Err(DomException::InvalidStateError);
    }
    let Some(info) = super::ua().and_then(|ua| ua.doc_info(doc)) else {
        return Err(DomException::InvalidStateError);
    };
    if info.throw_on_dynamic_markup_insertion.get() > 0 {
        return Err(DomException::InvalidStateError);
    }
    if info.active_parser_was_aborted.get() {
        return Ok(());
    }
    let has_insertion_point = info
        .active_parser()
        .is_some_and(|p| p.borrow().has_insertion_point());
    if !has_insertion_point {
        if info.unload_counter.get() > 0 || info.ignore_destructive_writes.get() > 0 {
            return Ok(());
        }
        document_open(agent, doc)?;
    }
    let Some(parser) = info.active_parser() else {
        return Ok(());
    };
    let mut input = text.to_string();
    if newline {
        input.push('\n');
    }
    parser.borrow_mut().write(&input);
    if info.pending_parsing_blocking.get().is_none() {
        process_written(agent, doc, &info, &parser);
    }
    Ok(())
}

/// `document.close()`.
pub fn document_close(agent: &mut Agent, doc: &DocRef) -> Result<(), DomException> {
    if !doc.borrow().is_html() {
        return Err(DomException::InvalidStateError);
    }
    let Some(info) = super::ua().and_then(|ua| ua.doc_info(doc)) else {
        return Err(DomException::InvalidStateError);
    };
    if info.throw_on_dynamic_markup_insertion.get() > 0 {
        return Err(DomException::InvalidStateError);
    }
    if !info.script_created_parser.get() {
        return Ok(());
    }
    let Some(parser) = info.active_parser() else {
        return Ok(());
    };
    parser.borrow_mut().end();
    if info.pending_parsing_blocking.get().is_some() {
        return Ok(());
    }
    if info.parsing.get() {
        // Called from a script the parser runs: the loop goes on after it.
        return Ok(());
    }
    run_parser(agent, doc);
    Ok(())
}

// ----- HTML steps of tree changes ------------------------------------------------------------

/// Runs the HTML steps of the changes the DOM recorded in `doc`: event
/// handler content attributes, the insertion and attribute change steps of
/// `script` and `iframe` elements, and iframe removal.
pub fn process_html_changes(agent: &mut Agent, doc: &DocRef) {
    let changes = match doc.try_borrow_mut() {
        Ok(mut d) if !d.html_changes.is_empty() => std::mem::take(&mut d.html_changes),
        _ => return,
    };
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    let mut iframes_changed = false;
    for change in changes {
        match change {
            HtmlChange::Inserted(node) => {
                let nodes = doc.borrow().inclusive_descendants(node);
                for n in nodes {
                    super::handlers::element_inserted(agent, doc, n);
                    let (connected, script, iframe, parser_inserted) = {
                        let d = doc.borrow();
                        let e = d.element(n);
                        (
                            d.is_connected(n),
                            d.is_html_element(n, "script"),
                            d.is_html_element(n, "iframe"),
                            e.is_some_and(|e| e.script.parser_inserted),
                        )
                    };
                    if iframe && connected {
                        let mut list = info.iframes.borrow_mut();
                        if !list.contains(&n) {
                            list.push(n);
                        }
                        iframes_changed = true;
                    }
                    if script && connected && !parser_inserted {
                        super::script::prepare_script(agent, doc, n);
                    }
                    let (link, style) = {
                        let d = doc.borrow();
                        (d.is_html_element(n, "link"), d.is_html_element(n, "style"))
                    };
                    if link && connected {
                        super::style::link_changed(doc, n);
                    }
                    if style && connected {
                        super::style::style_changed(doc, n);
                    }
                }
                // The children changed steps of a style element.
                let style_parent = {
                    let d = doc.borrow();
                    d.parent(node)
                        .filter(|&p| d.is_html_element(p, "style") && d.is_connected(p))
                };
                if let Some(p) = style_parent {
                    super::style::style_changed(doc, p);
                }
                // The children changed steps of a script element.
                let parent = {
                    let d = doc.borrow();
                    d.parent(node).filter(|&p| {
                        d.is_html_element(p, "script")
                            && d.is_connected(p)
                            && d.element(p).is_some_and(|e| !e.script.parser_inserted)
                    })
                };
                if let Some(p) = parent {
                    super::script::prepare_script(agent, doc, p);
                }
            }
            HtmlChange::Removed(_) => {
                if !info.iframes.borrow().is_empty() {
                    iframes_changed = true;
                }
            }
            HtmlChange::Attribute(element, local) => {
                let name = local.as_str().to_string();
                if name.starts_with("on") {
                    super::handlers::content_attribute_changed(agent, doc, element, &name);
                }
                let (script, iframe, connected, parser_inserted) = {
                    let d = doc.borrow();
                    (
                        d.is_html_element(element, "script"),
                        d.is_html_element(element, "iframe"),
                        d.is_connected(element),
                        d.element(element).is_some_and(|e| e.script.parser_inserted),
                    )
                };
                let is_link = doc.borrow().is_html_element(element, "link");
                if is_link && matches!(name.as_str(), "rel" | "href" | "media" | "type") {
                    super::style::link_changed(doc, element);
                }
                if iframe && connected && (name == "src" || name == "srcdoc") {
                    iframes_changed = true;
                }
                if script && name == "src" && connected && !parser_inserted {
                    let has_src = doc.borrow().get_attribute(element, "src").is_some();
                    if has_src {
                        super::script::prepare_script(agent, doc, element);
                    }
                }
            }
        }
    }
    if iframes_changed {
        super::navigable::reconcile_iframes(agent, &ua, doc, &info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_strings() {
        assert_eq!(Readiness::Loading.as_str(), "loading");
        assert_eq!(Readiness::Interactive.as_str(), "interactive");
        assert_eq!(Readiness::Complete.as_str(), "complete");
    }

    #[test]
    fn last_modified_has_the_legacy_shape() {
        let s = last_modified_now();
        let b = s.as_bytes();
        assert_eq!(s.len(), 19);
        assert_eq!(
            (b[2], b[5], b[10], b[13], b[16]),
            (b'/', b'/', b' ', b':', b':')
        );
    }

    #[test]
    fn base_element_overrides_the_document_url() {
        let mut d = Document::new_html();
        d.url = Url::parse("http://a.test/dir/page.html", None).unwrap();
        crate::html::parse_into(&mut d, "<base href='/other/'><p>", true);
        let doc: DocRef = Rc::new(RefCell::new(d));
        assert_eq!(base_url(&doc).href(), "http://a.test/other/");
    }
}
