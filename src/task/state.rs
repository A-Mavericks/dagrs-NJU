//! Task input and output
//!
//! [`Output`] and [`Input`] represent the output and input of the task respectively.
//!
//! # [`Output`]
//!
//! Users should consider the output results of the task when defining the specific
//! behavior of the task. The input results may be: normal output, no output, or task
//! execution error message.
//! It should be noted that the content stored in [`Output`] must implement the [`Clone`] trait.
//!
//! # Example
//! In general, a task may produce output or no output:
//! ```rust
//! use dagrs::Output;
//! let out=Output::new(10);
//! let non_out=Output::empty();
//! ```
//! In some special cases, when a predictable error occurs in the execution of a task's
//! specific behavior, the user can choose to return the error message as the output of
//! the task. Of course, this will cause subsequent tasks to abandon execution.
//!
//! ```rust
//! use dagrs::Output;
//! use dagrs::task::Content;
//! let err_out = Output::Err("some error messages!".to_string());
//! ```
//!
//! # [`Input`]
//!
//! [`Input`] represents the input required by the task. The input comes from the output
//! generated by multiple predecessor tasks of the task. If a predecessor task does not produce
//! output, the output will not be stored in [`Input`].
//! [`Input`] will be used directly by the user without user construction. [`Input`] is actually
//! constructed by cloning multiple [`Output`]. Users can obtain the content stored in [`Input`]
//! to implement the logic of the program.

use std::{
    any::Any,
    slice::Iter,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex
    }
};

use tokio::sync::Semaphore;

/// Container type to store task output.
#[derive(Debug, Clone)]
pub struct Content {
    content: Arc<dyn Any + Send + Sync>,
}

impl Content {
    /// Construct a new [`Content`].
    pub fn new<H: Send + Sync + 'static>(val: H) -> Self {
        Self {
            content: Arc::new(val),
        }
    }

    pub fn from_arc<H: Send + Sync + 'static>(val: Arc<H>) -> Self {
        Self { content: val }
    }

    pub fn get<H: 'static>(&self) -> Option<&H> {
        self.content.downcast_ref::<H>()
    }

    pub fn into_inner<H: Send + Sync + 'static>(self) -> Option<Arc<H>> {
        self.content.downcast::<H>().ok()
    }
}

/// [`ExeState`] internally stores [`Output`], which represents whether the execution of
/// the task is successful, and its internal semaphore is used to synchronously obtain
/// the output of the predecessor task as the input of this task.
#[derive(Debug)]
pub(crate) struct ExecState {
    /// The execution succeed or not.
    success: AtomicBool,
    /// Output produced by a task.
    output: Arc<Mutex<Output>>,
    /// The semaphore is used to control the synchronous blocking of subsequent tasks to obtain the
    /// execution results of this task.
    /// When a task is successfully executed, the permits inside the semaphore will be increased to
    /// n (n represents the number of successor tasks of this task or can also be called the output
    /// of the node), which means that the output of the task is available, and then each successor
    /// The task will obtain a permits synchronously (the permit will not be returned), which means
    /// that the subsequent task has obtained the execution result of this task.
    semaphore: Semaphore,
}

/// Output produced by a task.
#[derive(Clone,Debug)]
pub enum Output {
    Out(Option<Content>),
    Err(String),
    ErrWithExitCode(Option<i32>, Option<Content>),
}

/// Task's input value.
#[derive(Debug)]
pub struct Input(Vec<Content>);

impl ExecState {
    /// Construct a new [`ExeState`].
    pub(crate) fn new() -> Self {
        // initialize the task to failure without output.
        Self {
            success: AtomicBool::new(false),
            output: Arc::new(Mutex::new(Output::empty())),
            semaphore: Semaphore::new(0),
        }
    }

    /// After the task is successfully executed, set the execution result.
    pub(crate) fn set_output(&self, output: Output) {
        self.success.store(true, Ordering::Relaxed);
        *self.output.lock().unwrap() = output;
    }

    /// [`Output`] for fetching internal storage.
    /// This function is generally not called directly, but first uses the semaphore for synchronization control.
    pub(crate) fn get_output(&self) -> Option<Content> {
        self.output.lock().unwrap().get_out()
    }
    pub(crate) fn get_full_output(&self) -> Output {
        self.output.lock().unwrap().clone()
    }

    /// The task execution succeed or not.
    /// `true` means no panic occurs.
    pub(crate) fn success(&self) -> bool {
        self.success.load(Ordering::Relaxed)
    }

    pub(crate) fn exe_success(&self) {
        self.success.store(true, Ordering::Relaxed)
    }

    pub(crate) fn exe_fail(&self) {
        self.success.store(false, Ordering::Relaxed)
    }

    /// The semaphore is used to control the synchronous acquisition of task output results.
    /// Under normal circumstances, first use the semaphore to obtain a permit, and then call
    /// the `get_output` function to obtain the output. If the current task is not completed
    /// (no output is generated), the subsequent task will be blocked until the current task
    /// is completed and output is generated.
    pub(crate) fn semaphore(&self) -> &Semaphore {
        &self.semaphore
    }
}

impl Output {
    /// Construct a new [`Output`].
    ///
    /// Since the return value may be transferred between threads,
    /// [`Send`], [`Sync`] is needed.
    pub fn new<H: Send + Sync + 'static>(val: H) -> Self {
        Self::Out(Some(Content::new(val)))
    }

    /// Construct an empty [`Output`].
    pub fn empty() -> Self {
        Self::Out(None)
    }

    /// Construct an [`Output`]` with an error message.
    pub fn error(msg: String) -> Self {
        Self::Err(msg)
    }

    /// Construct an [`Output`]` with an exit code and an optional error message.
    pub fn error_with_exit_code(code: Option<i32>, msg: Option<Content>) -> Self {
        Self::ErrWithExitCode(code, msg)
    }

    /// Determine whether [`Output`] stores error information.
    pub(crate) fn is_err(&self) -> bool {
        match self {
            Self::Err(_) | Self::ErrWithExitCode(_, _) => true,
            Self::Out(_) => false,
        }
    }

    /// Get the contents of [`Output`].
    pub(crate) fn get_out(&self) -> Option<Content> {
        match self {
            Self::Out(ref out) => out.clone(),
            Self::Err(_) | Self::ErrWithExitCode(_, _) => None,
        }
    }

    /// Get error information stored in [`Output`].
    pub(crate) fn get_err(&self) -> Option<String> {
        match self {
            Self::Out(_) => None,
            Self::Err(err) => Some(err.to_string()),
            Self::ErrWithExitCode(_, err) => {
                if let Some(e) = err {
                    Some(e.get::<String>()?.to_string())
                } else {
                    None
                }
            }
        }
    }
}

impl Input {
    /// Constructs input using output produced by a non-empty predecessor task.
    pub fn new(input: Vec<Content>) -> Self {
        Self(input)
    }

    /// Since [`Input`] can contain multi-input values, and it's implemented
    /// by [`Vec`] actually, of course it can be turned into a iterator.
    pub fn get_iter(&self) -> Iter<Content> {
        self.0.iter()
    }
}
