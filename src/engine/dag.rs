//! The Dag
//!
//! # [`Dag`] is dagrs's main body.
//!
//! [`Dag`] embodies the scheduling logic of tasks written by users or tasks in a given configuration file.
//! A Dag contains multiple tasks. This task can be added to a Dag as long as it implements
//! the [`Task`] trait, and the user needs to define specific execution logic for the task, that is,
//! implement the [`Action`] trait and override the `run` method.
//!
//! The execution process of Dag is roughly as follows:
//! - The user gives a list of tasks `tasks`. These tasks can be parsed from configuration files, or provided
//! by user programming implementations.
//! - Internally generate [`Graph`] based on task dependencies, and generate execution sequences based on `rely_graph`.
//! - The task is scheduled to start executing asynchronously.
//! - The task will wait to get the result `execute_states` generated by the execution of the predecessor task.
//! - If the result of the predecessor task can be obtained, check the continuation status `can_continue`, if it
//! is true, continue to execute the defined logic, if it is false, trigger `handle_error`, and cancel the
//! execution of the subsequent task.
//! - After all tasks are executed, set the continuation status to false, which means that the tasks of the dag
//! cannot be scheduled for execution again.
//!
//!  # Example
//! ```rust
//! use dagrs::{log,LogLevel,Dag, DefaultTask, gen_task, Output,Input,EnvVar,RunningError,Action};
//! use std::sync::Arc;
//! log::init_logger(LogLevel::Info,None);
//! let task=gen_task!("Simple Task",|input,_env|{
//!     Ok(Output::new(1))
//! });
//! let mut dag=Dag::with_tasks(vec![task]);
//! assert!(dag.start().unwrap())
//!
//! ```

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anymap2::any::CloneAnySendSync;
use tokio::task::JoinHandle;

use crate::{
    parser::{Parser, YamlParser},
    task::{ExecState, Input, Task},
    utils::{log, EnvVar},
};

use super::{error::DagError, graph::Graph};

/// dagrs's function is wrapped in Dag struct.
#[derive(Debug)]
pub struct Dag {
    /// Store all tasks' infos.
    ///
    /// Arc but no mutex, because only one thread will change [`TaskWrapper`]at a time.
    /// And no modification to [`TaskWrapper`] happens during the execution of it.
    tasks: HashMap<usize, Arc<Box<dyn Task>>>,
    /// Store dependency relations.
    rely_graph: Graph,
    /// Store a task's running result.Execution results will be read and written asynchronously by several threads.
    execute_states: HashMap<usize, Arc<ExecState>>,
    /// Global environment variables for this Dag job. It should be set before the Dag job runs.
    env: Arc<EnvVar>,
    /// Mark whether the Dag task can continue to execute.
    /// When an error occurs during the execution of any task, this flag will be set to false, and
    /// subsequent tasks will be canceled.
    /// when all tasks in the dag are executed, the flag will also be set to false, indicating that
    /// the task cannot be run repeatedly.
    can_continue: Arc<AtomicBool>,
    /// The execution sequence of tasks.
    exe_sequence: Vec<usize>,
}

impl Dag {
    /// Create a dag. This function is not open to the public. There are three ways to create a new
    /// dag, corresponding to three functions: `with_tasks`, `with_yaml`, `with_config_file_and_parser`.
    fn new() -> Dag {
        Dag {
            tasks: HashMap::new(),
            rely_graph: Graph::new(),
            execute_states: HashMap::new(),
            env: Arc::new(EnvVar::new()),
            can_continue: Arc::new(AtomicBool::new(true)),
            exe_sequence: Vec::new(),
        }
    }

    /// Create a dag by adding a series of tasks.
    pub fn with_tasks(tasks: Vec<impl Task + 'static>) -> Dag {
        let mut dag = Dag::new();
        tasks.into_iter().for_each(|task| {
            let task = Box::new(task) as Box<dyn Task>;
            dag.tasks.insert(task.id(), Arc::new(task));
        });
        dag
    }

    /// Given a yaml configuration file parsing task to generate a dag.
    pub fn with_yaml(file: &str) -> Result<Dag, DagError> {
        Dag::read_tasks(file, None)
    }

    /// Generates a dag with the user given path to a custom parser and task config file.
    pub fn with_config_file_and_parser(
        file: &str,
        parser: Box<dyn Parser>,
    ) -> Result<Dag, DagError> {
        Dag::read_tasks(file, Some(parser))
    }

    /// Parse the content of the configuration file into a series of tasks and generate a dag.
    fn read_tasks(file: &str, parser: Option<Box<dyn Parser>>) -> Result<Dag, DagError> {
        let mut dag = Dag::new();
        let tasks = match parser {
            Some(p) => p.parse_tasks(file)?,
            None => {
                let parser = YamlParser;
                parser.parse_tasks(file)?
            }
        };
        tasks.into_iter().for_each(|task| {
            dag.tasks.insert(task.id(), Arc::new(task));
        });
        Ok(dag)
    }

    /// create rely map between tasks.
    ///
    /// This operation will initialize `dagrs.rely_graph` if no error occurs.
    fn create_graph(&mut self) -> Result<(), DagError> {
        let size = self.tasks.len();
        self.rely_graph.set_graph_size(size);

        // Add Node (create id - index mapping)
        self.tasks
            .iter()
            .map(|(&n, _)| self.rely_graph.add_node(n))
            .count();

        // Form Graph
        for (&id, task) in self.tasks.iter() {
            let index = self.rely_graph.find_index_by_id(&id).unwrap();

            for rely_task_id in task.predecessors() {
                // Rely task existence check
                let rely_index = self
                    .rely_graph
                    .find_index_by_id(rely_task_id)
                    .ok_or(DagError::RelyTaskIllegal(task.name()))?;

                self.rely_graph.add_edge(rely_index, index);
            }
        }

        Ok(())
    }

    /// Initialize dags. The initialization process completes three actions:
    /// - Initialize the status of each task execution result.
    /// - Create a graph from task dependencies.
    /// - Generate task heart sequence according to topological sorting of graph.
    pub(crate) fn init(&mut self) -> Result<(), DagError> {
        self.tasks.keys().for_each(|id| {
            self.execute_states
                .insert(*id, Arc::new(ExecState::new(*id)));
        });

        self.create_graph()?;

        match self.rely_graph.topo_sort() {
            Some(seq) => {
                if seq.is_empty() {
                    return Err(DagError::EmptyJob);
                }
                let exe_seq: Vec<usize> = seq
                    .into_iter()
                    .map(|index| self.rely_graph.find_id_by_index(index).unwrap())
                    .collect();
                self.exe_sequence = exe_seq;
                Ok(())
            }
            None => Err(DagError::LoopGraph),
        }
    }

    /// This function is used for the execution of a single dag.
    pub fn start(&mut self) -> Result<bool, DagError> {
        // If the current continuable state is false, the task will start failing.
        if self.can_continue.load(Ordering::Acquire) {
            self.init().map_or_else(Err, |_| {
                Ok(tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(async { self.run().await }))
            })
        } else {
            Ok(false)
        }
    }

    /// Execute tasks sequentially according to the execution sequence given by
    /// topological sorting, and cancel the execution of subsequent tasks if an
    /// error is encountered during task execution.
    pub(crate) async fn run(&self) -> bool {
        let mut exe_seq = String::from("[Start]");
        self.exe_sequence
            .iter()
            .for_each(|id| exe_seq.push_str(&format!(" -> {}", self.tasks[id].name())));
        log::info(format!("{} -> [End]", exe_seq));
        let mut handles = Vec::new();
        self.exe_sequence.iter().for_each(|id| {
            handles.push((*id, self.execute_task(self.tasks[id].clone())));
        });
        // Wait for the status of each task to execute. If there is an error in the execution of a task,
        // the engine will fail to execute and give up executing tasks that have not yet been executed.
        let mut exe_success = true;
        for handle in handles {
            let complete = handle.1.await.map_or_else(
                |err| {
                    log::error(format!(
                        "Task execution encountered an unexpected error! {}",
                        err
                    ));
                    false
                },
                |state| state,
            );
            if !complete {
                log::error(format!(
                    "Task execution failed! [{}]",
                    self.tasks[&handle.0].name()
                ));
                self.handle_error(&handle.0).await;
                exe_success = false;
            }
        }
        self.can_continue.store(false, Ordering::Release);
        exe_success
    }

    /// Execute a given task asynchronously.
    fn execute_task(&self, task: Arc<Box<dyn Task>>) -> JoinHandle<bool> {
        let env = self.env.clone();
        let task_id = task.id();
        let task_name = task.name();
        let execute_state = self.execute_states[&task_id].clone();
        let task_out_degree = self.rely_graph.get_node_out_degree(&task_id);
        let wait_for_input: Vec<Arc<ExecState>> = task
            .predecessors()
            .iter()
            .map(|id| self.execute_states[id].clone())
            .collect();
        let action = task.action();
        let can_continue = self.can_continue.clone();
        tokio::spawn(async move {
            // Wait for the execution result of the predecessor task
            let mut inputs = Vec::new();
            for wait_for in wait_for_input {
                wait_for.semaphore().acquire().await.unwrap().forget();
                // When the task execution result of the predecessor can be obtained, judge whether
                // the continuation flag is set to false, if it is set to false, cancel the specific
                // execution logic of the task and return immediately.
                if !can_continue.load(Ordering::Acquire) {
                    return true;
                }
                if let Some(content) = wait_for.get_output() {
                    if !content.is_empty() {
                        inputs.push(content);
                    }
                }
            }
            log::info(format!("Executing Task[name: {}]", task_name));
            // Concrete logical behavior for performing tasks.
            match action.run(Input::new(inputs), env) {
                Ok(out) => {
                    // Store execution results
                    execute_state.set_output(out);
                    execute_state.semaphore().add_permits(task_out_degree);
                    log::info(format!("Task executed successfully. [name: {}]",task_name));
                    true
                }
                Err(err) => {
                    log::error(format!("Task failed[name: {}]. {}", task_name, err));
                    false
                }
            }
        })
    }

    /// error handling.
    /// When a task execution error occurs, the error handling logic is:
    /// First, set the continuation status to false, and then release the semaphore of the
    /// error task and the tasks after the error task, so that subsequent tasks can quickly
    /// know that some tasks have errors and cannot continue to execute.
    /// After that, the follow-up task finds that the flag that can continue to execute is set
    /// to false, and the specific behavior of executing the task will be cancelled.
    async fn handle_error(&self, error_task_id: &usize) {
        self.can_continue.store(false, Ordering::Release);
        // Find the position of the faulty task in the execution sequence.
        let index = self
            .exe_sequence
            .iter()
            .position(|tid| *tid == *error_task_id)
            .unwrap();

        for i in index..self.exe_sequence.len() {
            let tid = self.exe_sequence.get(i).unwrap();
            let out_degree = self.rely_graph.get_node_out_degree(tid);
            self.execute_states
                .get(tid)
                .unwrap()
                .semaphore()
                .add_permits(out_degree);
        }
    }

    /// Get the final execution result.
    pub fn get_result<T: CloneAnySendSync + Send + Sync>(&self) -> Option<T> {
        if self.exe_sequence.is_empty() {
            None
        } else {
            let last_id = self.exe_sequence.last().unwrap();
            match self.execute_states[last_id].get_output() {
                Some(ref content) => content.clone().remove(),
                None => None,
            }
        }
    }

    /// Before the dag starts executing, set the dag's global environment variable.
    pub fn set_env(&mut self, env: EnvVar) {
        self.env = Arc::new(env);
    }
}
