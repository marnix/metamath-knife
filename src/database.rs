//! Main API entry point for reading and manipulating Metamath databases.
//!
//! A variable of type `Database` represents a loaded database.  You can
//! construct a `Database` object, then cause it to represent a database from a
//! disk file using the `parse` method, then query various analysis results
//! which will be computed on demand.  You can call `parse` again to reload
//! data; the implementation expects there to be minor changes, and optimizes
//! with incremental recomputation.
//!
//! It is also possible to modify a loaded database by opening it (details TBD);
//! while the database is open most analyses cannot be used, but it is permitted
//! to call `Clone::clone` on a `Database` and the type is designed to make that
//! relatively efficient (currently requires the duplication of three large hash
//! tables, this can be optimized).
//!
//! ## On segmentation
//!
//! Existing Metamath verifiers which attempt to maintain a DOM represent it as
//! a flat list of statements.  In order to permit incremental and parallel
//! operation across _all_ phases, we split the list into one or more segments.
//! A **segment** is a run of statements which are parsed together and will
//! always remain contiguous in the logical system.  Segments are generated by
//! the parsing process, and are the main unit of recalculation and parallelism
//! for subsequent passes.  We do not allow grouping constructs to span segment
//! boundaries; since we also disallow top-level `$e` statements, this means
//! that the scope of an `$e` statement is always limited to a single segment.
//!
//! A source file without include statements will be treated as a single segment
//! (except for splitting, see below).  A source file with N include statements
//! will generate N + 1 segments; a new segment is started immediately after
//! each include to allow any segment(s) from the included file to be slotted
//! into the correct order.  Thus segments cannot quite be the parallelism
//! granularity for parsing, because during the parse we don't know the final
//! number of segments; instead each source file is parsed independently, gating
//! rereading and reparsing on the file modification time.
//!
//! As an exception to support parallel processing of large single files (like
//! set.mm at the time of writing), source files larger than 1MiB are
//! automatically split into multiple pieces before parsing.  Each piece tracks
//! the need to recalculate independently, and each piece may generate or or
//! more segments as above.  Pieces are identified using chapter header
//! comments, and are located using a simple word-at-a-time Boyer-Moore search
//! that is much faster than the actual parser (empirically, it is limited by
//! main memory sequential read speed).  _Note that this means that for large
//! files, chapter header comments are effectively illegal inside of grouping
//! statements.  set.mm is fine with that restriction, but it does not match the
//! spec._
//!
//! Each loaded segment is assigned an ID (of type `SegmentId`, an opacified
//! 32-bit integer).  These IDs are **reused** when a segment is replaced with
//! another segment with the same logical sequence position; this allows
//! subsequent passes to interpret the new segment as the inheritor of the
//! previous segment, and reuse caches as applicable.  It then becomes necessary
//! to decide which of two segments is earlier in the logical order; it is not
//! possible to simply use numeric order, as a new segment might need to be
//! added between any two existing segments.  This is the well-studied
//! [order-maintenance problem][OMP]; we currently have a naive algorithm in the
//! `parser::SegmentOrder` structure, but a more sophisticated one could be
//! added later.  We never reuse a `SegmentId` in a way which would cause the
//! relative position of two `SegmentId` values to change; this means that after
//! many edits and incremental reloads the `SegmentOrder` will grow, and it may
//! become necessary to add code later to trigger a global renumbering (which
//! would necesssarily entail recomputation of all passes for all segments, but
//! the amortized complexity need not be bad).
//!
//! [OMP]: https://en.wikipedia.org/wiki/Order-maintenance_problem
//!
//! ## Incremental processing: Readers and Usages
//!
//! A pass will be calculated when its result is needed.  Operation is currently
//! lazy at a pass level, so it is not possible to verify only one segment,
//! although that _might_ change.  The results of a pass are stored in a data
//! structure indexed by some means, each element of which has an associated
//! version number.  When another pass needs to use the result of the first
//! pass, it tracks which elements of the first pass's result are used for each
//! segment, and their associated version numbers; this means that if a small
//! database change is made and the second pass is rerun, it can quickly abort
//! on most segments by checking if the dependencies _of that segment_ have
//! changed, using only the version numbers.
//!
//! This is not yet a rigidly systematized thing; for an example, nameck
//! generates its result as a `nameck::Nameset`, and implements
//! `nameck::NameUsage` objects which scopeck can use to record which names were
//! used scoping a given segment; it also provides `nameck::NameReader` objects
//! which can be used to access the nameset while simultaneously building a
//! usage object that can be used for future checking.
//!
//! ## Parallelism and promises
//!
//! The current parallel processing implementation is fairly simplistic.  If you
//! want to run a number of code fragments in parallel, get a reference to the
//! `Executor` object for the current database, then use it to queue a closure
//! for each task you want to run; the queueing step returns a `Promise` object
//! which can be used to wait for the task to complete.  Generally you want to
//! queue everything, then wait for everything.
//!
//! To improve packing efficiency, jobs are dispatched in descending order of
//! estimated runtime.  This requires an additional argument when queueing.

use crate::diag;
use crate::diag::DiagnosticClass;
use crate::diag::Notation;
use crate::export;
use crate::formula::Label;
use crate::grammar;
use crate::grammar::Grammar;
use crate::grammar::StmtParse;
use crate::nameck::Nameset;
use crate::outline::OutlineNode;
use crate::parser::StatementRef;
use crate::scopeck;
use crate::scopeck::ScopeResult;
use crate::segment_set::SegmentSet;
use crate::verify;
use crate::verify::VerifyResult;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt;
use std::fmt::Debug;
use std::fs::File;
use std::panic;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

/// Structure for options that affect database processing, and must be constant
/// for the lifetime of the database container.
///
/// Some of these could theoretically support modification.
#[derive(Copy, Clone, Debug)]
pub struct DbOptions {
    /// If true, the automatic splitting of large files described above is
    /// enabled, with the caveat about chapter comments inside grouping
    /// statements.
    pub autosplit: bool,
    /// If true, time in milliseconds is printed after the completion of each
    /// pass.
    pub timing: bool,
    /// True to print names (determined by a very simple heuristic, see
    /// `parser::guess_buffer_name`) of segments which are recalculated in each
    /// pass.
    pub trace_recalc: bool,
    /// True to record detailed usage data needed for incremental operation.
    ///
    /// This will slow down the initial analysis, so don't set it if you won't
    /// use it.  If this is false, any reparse will result in a full
    /// recalculation, so it is always safe but different settings will be
    /// faster for different tasks.
    pub incremental: bool,
    /// Number of jobs to run in parallel at any given time.
    pub jobs: usize,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            autosplit: false,
            timing: false,
            trace_recalc: false,
            incremental: false,
            jobs: 1,
        }
    }
}

/// Wraps a heap-allocated closure with a difficulty score which can be used for
/// sorting; this might belong in the standard library as `CompareFirst` or such.
struct Job(usize, Box<dyn FnMut() + Send>);
impl PartialEq for Job {
    fn eq(&self, other: &Job) -> bool {
        self.0 == other.0
    }
}
impl Eq for Job {}
impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Job) -> Option<Ordering> {
        Some(self.0.cmp(&other.0))
    }
}
impl Ord for Job {
    fn cmp(&self, other: &Job) -> Ordering {
        self.0.cmp(&other.0)
    }
}

/// Object which holds the state of the work queue and allows queueing tasks to
/// run on the thread pool.
#[derive(Clone)]
pub struct Executor {
    concurrency: usize,
    // Jobs are kept in a heap so that we can dispatch the biggest one first.
    mutex: Arc<Mutex<BinaryHeap<Job>>>,
    // Condvar used to notify work threads of new work.
    work_cv: Arc<Condvar>,
}

/// Debug printing for `Executor` displays the current count of queued but not
/// dispatched tasks.
impl fmt::Debug for Executor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let g = self.mutex.lock().unwrap();
        write!(f, "Executor(active={})", g.len())
    }
}

fn queue_work(exec: &Executor, estimate: usize, mut f: Box<dyn FnMut() + Send>) {
    if exec.concurrency <= 1 {
        f();
        return;
    }
    let mut wq = exec.mutex.lock().unwrap();
    wq.push(Job(estimate, f));
    exec.work_cv.notify_one();
}

impl Executor {
    /// Instantiates a new work queue and creates the threads to service it.
    ///
    /// The threads will exit when the `Executor` goes out of scope (not yet
    /// implemented).  In the future, we *may* have process-level coordination
    /// to allow different `Executor`s to share a thread pool, and use per-job
    /// concurrency limits.
    #[must_use]
    pub fn new(concurrency: usize) -> Executor {
        let mutex = Arc::new(Mutex::new(BinaryHeap::new()));
        let cv = Arc::new(Condvar::new());

        if concurrency > 1 {
            for _ in 0..concurrency {
                let mutex = mutex.clone();
                let cv = cv.clone();
                thread::spawn(move || loop {
                    let mut task: Job = {
                        let mut mutexg = mutex.lock().unwrap();
                        while mutexg.is_empty() {
                            mutexg = cv.wait(mutexg).unwrap();
                        }
                        mutexg.pop().unwrap()
                    };
                    (task.1)();
                });
            }
        }

        Executor {
            concurrency,
            mutex,
            work_cv: cv,
        }
    }

    /// Queue a job on this work queue.
    ///
    /// The estimate is meaningless in isolation but jobs with a higher estimate
    /// will be dispatched first, so it should be comparable among jobs that
    /// could simultaneously be in the work queue.
    ///
    /// Returns a `Promise` that can be used to wait for completion of the
    /// queued work.  If the provided task panics, the error will be stored and
    /// rethrown when the promise is awaited.
    pub fn exec<TASK, RV>(&self, estimate: usize, task: TASK) -> Promise<RV>
    where
        TASK: FnOnce() -> RV + Send + 'static,
        RV: Send + 'static,
    {
        let parts = Arc::new((Mutex::new(None), Condvar::new()));

        let partsc = parts.clone();
        let mut task_o = Some(task);
        queue_work(
            self,
            estimate,
            Box::new(move || {
                let mut g = partsc.0.lock().unwrap();
                let task_f =
                    panic::AssertUnwindSafe(task_o.take().expect("should only be called once"));
                *g = Some(panic::catch_unwind(task_f));
                partsc.1.notify_one();
            }),
        );

        Promise::new_once(move || {
            let mut g = parts.0.lock().unwrap();
            while g.is_none() {
                g = parts.1.wait(g).unwrap();
            }
            g.take().unwrap().unwrap()
        })
    }
}

/// A handle for a value which will be available later.
///
/// Promises are normally constructed using `Executor::exec`, which moves
/// computation to a thread pool.  There are several other methods to attach
/// code to promises; these do **not** parallelize, and are intended to do very
/// cheap tasks for interface consistency purposes only.
pub struct Promise<T>(Box<dyn FnMut() -> T + Send>);

impl<T> Debug for Promise<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Promise(..)")
    }
}

impl<T> Promise<T> {
    /// Wait for a value to be available and return it, rethrowing any panic.
    #[must_use]
    pub fn wait(mut self) -> T {
        (self.0)()
    }

    /// Construct a promise which uses a provided closure to wait for the value
    /// when necessary.
    ///
    /// This does **not** do any parallelism; the provided closure will be
    /// invoked when `wait` is called, on the thread where `wait` is called.  If
    /// you want to run code in parallel, use `Executor::exec`.
    pub fn new_once<FN>(fun: FN) -> Promise<T>
    where
        FN: FnOnce() -> T + Send + 'static,
    {
        let mut funcell = Some(fun);
        // the take hack works around the lack of stable FnBox
        Promise(Box::new(move || (funcell.take().unwrap())()))
    }

    /// Wrap a value which is available now in a promise.
    pub fn new(value: T) -> Self
    where
        T: Send + 'static,
    {
        Promise::new_once(move || value)
    }

    /// Modify a promise with a function, which will be called at `wait` time on
    /// the `wait` thread.
    pub fn map<FN, RV>(self, fun: FN) -> Promise<RV>
    where
        T: 'static,
        FN: Send + FnOnce(T) -> RV + 'static,
    {
        Promise::new_once(move || fun(self.wait()))
    }

    /// Convert a collection of promises into a single promise, which waits for
    /// all of its parts.
    #[must_use]
    pub fn join(promises: Vec<Promise<T>>) -> Promise<Vec<T>>
    where
        T: 'static,
    {
        Promise::new_once(move || promises.into_iter().map(Promise::wait).collect())
    }
}

/// Master type of database containers.
///
/// A variable of type `Database` holds a database, i.e. an ordered collection
/// of segments and analysis results for that collection.  Analysis results are
/// generated lazily for each database, and are invalidated on any edit to the
/// database's segments.  If you need to refer to old analysis results while
/// making a sequence of edits, call `Clone::clone` on the database first; this
/// is intended to be a relatively cheap operation.
///
/// More specifically, cloning a `Database` object does essentially no work
/// until it is necessary to run an analysis pass on one clone or the other;
/// then if the analysis pass has a result index which is normally updated in
/// place, such as the hash table of statement labels constructed by nameck,
/// that table must be duplicated so that it can be updated for one database
/// without affecting the other.
#[derive(Debug)]
pub struct Database {
    options: Arc<DbOptions>,
    segments: Arc<SegmentSet>,
    /// We track the "current" and "previous" for all known passes, so that each
    /// pass can use its most recent results for optimized incremental
    /// processing.  Any change to the segment vector zeroizes the current
    /// fields but not the previous fields.
    prev_nameset: Option<Arc<Nameset>>,
    nameset: Option<Arc<Nameset>>,
    prev_scopes: Option<Arc<ScopeResult>>,
    scopes: Option<Arc<ScopeResult>>,
    prev_verify: Option<Arc<VerifyResult>>,
    verify: Option<Arc<VerifyResult>>,
    outline: Option<Arc<OutlineNode>>,
    grammar: Option<Arc<Grammar>>,
    stmt_parse: Option<Arc<StmtParse>>,
}

impl Default for Database {
    fn default() -> Self {
        Self::new(DbOptions::default())
    }
}

fn time<R, F: FnOnce() -> R>(opts: &DbOptions, name: &str, f: F) -> R {
    let now = Instant::now();
    let ret = f();
    if opts.timing {
        // no as_msecs :(
        println!("{} {}ms", name, (now.elapsed() * 1000).as_secs());
    }
    ret
}

impl Drop for Database {
    fn drop(&mut self) {
        time(&self.options.clone(), "free", move || {
            self.prev_verify = None;
            self.verify = None;
            self.prev_scopes = None;
            self.scopes = None;
            self.prev_nameset = None;
            self.nameset = None;
            Arc::make_mut(&mut self.segments).clear();
            self.outline = None;
        });
    }
}

impl Database {
    /// Constructs a new database object representing an empty set of segments.
    ///
    /// Use `parse` to load it with data.  Currently this eagerly starts the
    /// threadpool, but that may change.
    #[must_use]
    pub fn new(options: DbOptions) -> Database {
        let options = Arc::new(options);
        let exec = Executor::new(options.jobs);
        Database {
            segments: Arc::new(SegmentSet::new(options.clone(), &exec)),
            options,
            nameset: None,
            scopes: None,
            verify: None,
            outline: None,
            grammar: None,
            stmt_parse: None,
            prev_nameset: None,
            prev_scopes: None,
            prev_verify: None,
        }
    }

    /// Replaces the content of a database in memory with the parsed content of
    /// one or more input files.
    ///
    /// To load data from disk files, pass the pathname as `start` and leave
    /// `text` empty.  `start` and any references arising from file inclusions
    /// will be processed relative to the current directory; we _may_ add a base
    /// directory option later.
    ///
    /// The database object will remember the name and OS modification time of
    /// all files read to construct its current state, and will skip rereading
    /// them if the modification change has not changed on the next call to
    /// `parse`.  If your filesystem has poor modification time granulatity,
    /// beware of possible lost updates if you modify a file and the timestamp
    /// does not change.
    ///
    /// To parse data already resident in program memory, pass an arbitrary name
    /// as `start` and then pass a pair in `text` mapping that name to the
    /// buffer to parse.  Any file inclusions found in the buffer can be
    /// resolved from additional pairs in `text`; file inclusions which are
    /// _not_ found in `text` will be resolved on disk relative to the current
    /// directory as above (this feature has [an uncertain future][FALLBACK]).
    ///
    /// [FALLBACK]: https://github.com/sorear/smetamath-rs/issues/18
    ///
    /// All analysis passes will be invalidated; they will not immediately be
    /// rerun, but will be when next requested.  If the database is not
    /// currently empty, the files loaded are assumed to be similar to the
    /// current database content and incremental processing will be used as
    /// appropriate.
    pub fn parse(&mut self, start: String, text: Vec<(String, Vec<u8>)>) {
        time(&self.options.clone(), "parse", || {
            Arc::make_mut(&mut self.segments).read(start, text);
            self.nameset = None;
            self.scopes = None;
            self.verify = None;
            self.outline = None;
            self.grammar = None;
        });
    }

    /// Obtains a reference to the current parsed data.
    pub(crate) const fn parse_result(&self) -> &Arc<SegmentSet> {
        &self.segments
    }

    /// Calculates and returns the name to definition lookup table.
    pub fn name_pass(&mut self) -> &Arc<Nameset> {
        if self.nameset.is_none() {
            time(&self.options.clone(), "nameck", || {
                let mut ns = self.prev_nameset.take().unwrap_or_default();
                let pr = self.parse_result();
                Arc::make_mut(&mut ns).update(pr);
                self.prev_nameset = Some(ns.clone());
                self.nameset = Some(ns);
            });
        }

        self.name_result()
    }

    /// Returns the name to definition lookup table.
    /// Panics if [`Database::name_pass`] was not previously called.
    #[inline]
    #[must_use]
    pub fn name_result(&self) -> &Arc<Nameset> {
        self.nameset.as_ref().unwrap()
    }

    /// Calculates and returns the frames for this database, i.e. the actual
    /// logical system.
    ///
    /// All logical properties of the database (as opposed to surface syntactic
    /// properties) can be obtained from this object.
    pub fn scope_pass(&mut self) -> &Arc<ScopeResult> {
        if self.scopes.is_none() {
            self.name_pass();
            time(&self.options.clone(), "scopeck", || {
                let mut sc = self.prev_scopes.take().unwrap_or_default();
                let parse = self.parse_result();
                let name = self.name_result();
                scopeck::scope_check(Arc::make_mut(&mut sc), parse, name);
                self.prev_scopes = Some(sc.clone());
                self.scopes = Some(sc);
            });
        }
        self.scope_result()
    }

    /// Returns the frames for this database, i.e. the actual logical system.
    /// Panics if [`Database::scope_pass`] was not previously called.
    ///
    /// All logical properties of the database (as opposed to surface syntactic
    /// properties) can be obtained from this object.
    #[inline]
    #[must_use]
    pub fn scope_result(&self) -> &Arc<ScopeResult> {
        self.scopes.as_ref().unwrap()
    }

    /// Calculates and returns verification information for the database.
    ///
    /// This is an optimized verifier which returns no useful information other
    /// than error diagnostics.  It does not save any parsed proof data.
    pub fn verify_pass(&mut self) -> &Arc<VerifyResult> {
        if self.verify.is_none() {
            self.name_pass();
            self.scope_pass();
            time(&self.options.clone(), "verify", || {
                let mut ver = self.prev_verify.take().unwrap_or_default();
                let parse = self.parse_result();
                let scope = self.scope_result();
                let name = self.name_result();
                verify::verify(Arc::make_mut(&mut ver), parse, name, scope);
                self.prev_verify = Some(ver.clone());
                self.verify = Some(ver);
            });
        }
        self.verify_result()
    }

    /// Returns verification information for the database.
    /// Panics if [`Database::verify_pass`] was not previously called.
    ///
    /// This is an optimized verifier which returns no useful information other
    /// than error diagnostics.  It does not save any parsed proof data.
    #[inline]
    #[must_use]
    pub fn verify_result(&self) -> &Arc<VerifyResult> {
        self.verify.as_ref().unwrap()
    }

    /// Computes and returns the root node of the outline.
    pub fn outline_pass(&mut self) -> &Arc<OutlineNode> {
        if self.outline.is_none() {
            time(&self.options.clone(), "outline", || {
                let parse = self.parse_result().clone();
                let mut outline = OutlineNode::default();
                parse.build_outline(&mut outline);
                self.outline = Some(Arc::new(outline));
            })
        }
        self.outline_result()
    }

    /// Returns the root node of the outline.
    /// Panics if [`Database::outline_pass`] was not previously called.
    #[inline]
    #[must_use]
    pub fn outline_result(&self) -> &Arc<OutlineNode> {
        self.outline.as_ref().unwrap()
    }

    /// Builds and returns the grammar.
    pub fn grammar_pass(&mut self) -> &Arc<Grammar> {
        if self.grammar.is_none() {
            self.name_pass();
            self.scope_pass();
            time(&self.options.clone(), "grammar", || {
                self.grammar = Some(Arc::new(Grammar::new(self)));
            })
        }
        self.grammar_result()
    }

    /// Returns the grammar.
    /// Panics if [`Database::grammar_pass`] was not previously called.
    #[inline]
    #[must_use]
    pub fn grammar_result(&self) -> &Arc<Grammar> {
        self.grammar.as_ref().unwrap()
    }

    /// Parses the statements using the grammar.
    pub fn stmt_parse_pass(&mut self) -> &Arc<StmtParse> {
        if self.stmt_parse.is_none() {
            self.name_pass();
            self.scope_pass();
            self.grammar_pass();
            time(&self.options.clone(), "stmt_parse", || {
                let parse = self.parse_result();
                let name = self.name_result();
                let grammar = self.grammar_result();
                let mut stmt_parse = StmtParse::default();
                grammar::parse_statements(&mut stmt_parse, parse, name, grammar);
                self.stmt_parse = Some(Arc::new(stmt_parse));
            })
        }
        self.stmt_parse_result()
    }

    /// Returns the statements parsed using the grammar.
    /// Panics if [`Database::stmt_parse_pass`] was not previously called.
    #[inline]
    #[must_use]
    pub fn stmt_parse_result(&self) -> &Arc<StmtParse> {
        self.stmt_parse.as_ref().unwrap()
    }

    /// A getter method which does not build the outline.
    #[inline]
    #[must_use]
    pub const fn get_outline(&self) -> Option<&Arc<OutlineNode>> {
        self.outline.as_ref()
    }

    /// Get a statement by label. Requires: [`Database::name_pass`]
    #[must_use]
    pub fn statement(&self, name: &str) -> Option<StatementRef<'_>> {
        let lookup = self.name_result().lookup_label(name.as_bytes())?;
        Some(self.parse_result().statement(lookup.address))
    }

    /// Get a statement by label atom.
    #[must_use]
    pub fn statement_by_label(&self, label: Label) -> Option<StatementRef<'_>> {
        let token = self.name_result().atom_name(label);
        let lookup = self.name_result().lookup_label(token)?;
        Some(self.parse_result().statement(lookup.address))
    }

    /// Iterates over all the statements
    pub fn statements(&self) -> impl Iterator<Item = StatementRef<'_>> + '_ {
        self.segments.segments().into_iter().flatten()
    }

    /// Export an mmp file for a given statement.
    /// Requires: [`Database::name_pass`], [`Database::scope_pass`]
    pub fn export(&self, stmt: &str) {
        time(&self.options, "export", || {
            let sref = self.statement(stmt).unwrap_or_else(|| {
                panic!("Label {} did not correspond to an existing statement", stmt)
            });

            File::create(format!("{}.mmp", stmt))
                .map_err(export::ExportError::Io)
                .and_then(|mut file| self.export_mmp(sref, &mut file))
                .unwrap()
        })
    }

    /// Export the grammar of this database in DOT format.
    /// Requires: [`Database::name_pass`], [`Database::grammar_pass`]
    #[cfg(feature = "dot")]
    pub fn export_grammar_dot(&self) {
        time(&self.options, "export_grammar_dot", || {
            let name = self.name_result();
            let grammar = self.grammar_result();

            File::create("grammar.dot")
                .map_err(export::ExportError::Io)
                .and_then(|mut file| grammar.export_dot(name, &mut file))
                .unwrap()
        })
    }

    /// Dump the grammar of this database.
    /// Requires: [`Database::name_pass`], [`Database::grammar_pass`]
    pub fn print_grammar(&self) {
        time(&self.options, "print_grammar", || {
            self.grammar_result().dump(self);
        })
    }

    /// Dump the formulas of this database.
    /// Requires: [`Database::name_pass`], [`Database::stmt_parse_pass`]
    pub fn print_formula(&self) {
        time(&self.options, "print_formulas", || {
            self.stmt_parse_result().dump(self);
        })
    }

    /// Verify that printing the formulas of this database gives back the original formulas.
    /// Requires: [`Database::name_pass`], [`Database::stmt_parse_pass`]
    pub fn verify_parse_stmt(&self) {
        time(&self.options, "verify_parse_stmt", || {
            if let Err(diag) = self.stmt_parse_result().verify(self) {
                drop(diag::to_annotations(self.parse_result(), vec![diag]));
            }
        })
    }

    /// Dump the outline of this database.
    /// Requires: [`Database::outline_pass`]
    pub fn print_outline(&self) {
        time(&self.options, "print_outline", || {
            let root_node = self.outline_result();
            self.print_outline_node(root_node, 0);
        })
    }

    /// Dump the outline of this database.
    fn print_outline_node(&self, node: &OutlineNode, indent: usize) {
        // let indent = (node.level as usize) * 3
        println!(
            "{:indent$} {:?} {:?}",
            "",
            node.level,
            node.get_name(),
            indent = indent
        );
        for child in &node.children {
            self.print_outline_node(child, indent + 1);
        }
    }

    /// Collects and returns all errors generated by the passes run.
    ///
    /// Passes are identified by the `types` argument and are not inclusive; if
    /// you ask for Verify, you will not get Parse unless you specifically ask
    /// for that as well.
    ///
    /// Currently there is no way to incrementally fetch diagnostics, so this
    /// will be a bit slow if there are thousands of errors.
    pub fn diag_notations(&mut self, types: &[DiagnosticClass]) -> Vec<Notation> {
        let mut diags = Vec::new();
        if types.contains(&DiagnosticClass::Parse) {
            diags.extend(self.parse_result().parse_diagnostics());
        }
        if types.contains(&DiagnosticClass::Scope) {
            diags.extend(self.scope_pass().diagnostics());
        }
        if types.contains(&DiagnosticClass::Verify) {
            diags.extend(self.verify_pass().diagnostics());
        }
        if types.contains(&DiagnosticClass::Grammar) {
            diags.extend(self.grammar_pass().diagnostics());
        }
        if types.contains(&DiagnosticClass::StmtParse) {
            diags.extend(self.stmt_parse_pass().diagnostics());
        }
        time(&self.options.clone(), "diag", || {
            diag::to_annotations(self.parse_result(), diags)
        })
    }
}
