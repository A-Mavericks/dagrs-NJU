//! Some tests of the dag engine.

use std::{collections::HashMap, env::set_var, sync::Arc};

use dagrs::{
    task::{reset_id_allocator, Content},
    Complex, Dag, DagError, DefaultTask, EnvVar, Input, Output,
};
use pretty_assertions::assert_eq;

#[test]
fn yaml_task_correct_execute() {
    let mut job = Dag::with_yaml("tests/config/correct.yaml", HashMap::new()).unwrap();
    assert!(job.start().unwrap());
}

#[test]
fn yaml_task_loop_graph() {
    let res = Dag::with_yaml("tests/config/loop_error.yaml", HashMap::new())
        .unwrap()
        .start();
    assert!(matches!(res, Err(DagError::LoopGraph)))
}

#[test]
fn yaml_task_self_loop_graph() {
    let res = Dag::with_yaml("tests/config/self_loop_error.yaml", HashMap::new())
        .unwrap()
        .start();
    assert!(matches!(res, Err(DagError::LoopGraph)))
}

#[test]
fn yaml_task_failed_execute() {
    let res = Dag::with_yaml("tests/config/script_run_failed.yaml", HashMap::new())
        .unwrap()
        .start();
    assert!(!res.unwrap());
}

#[test]
fn task_loop_graph() {
    let mut a = DefaultTask::with_closure("a", |_, _| Output::empty());
    let mut b = DefaultTask::with_closure("b", |_, _| Output::empty());
    let mut c = DefaultTask::with_closure("c", |_, _| Output::empty());
    a.set_predecessors(&[&b]);
    b.set_predecessors(&[&c]);
    c.set_predecessors(&[&a]);

    let mut env = EnvVar::new();
    env.set("base", 2usize);

    let mut job = Dag::with_tasks(vec![a, b, c]);
    job.set_env(env);
    let res = job.start();
    assert!(matches!(res, Err(DagError::LoopGraph)));
}

#[test]
fn non_job() {
    let tasks: Vec<DefaultTask> = Vec::new();
    let res = Dag::with_tasks(tasks).start();
    assert!(res.is_err());
}

struct FailedActionC(usize);

impl Complex for FailedActionC {
    fn run(&self, _input: Input, env: Arc<EnvVar>) -> Output {
        let base = env.get::<usize>("base").unwrap();
        Output::new(base / self.0)
    }
}

struct FailedActionD(usize);

impl Complex for FailedActionD {
    fn run(&self, _input: Input, _env: Arc<EnvVar>) -> Output {
        Output::Err(None, Some(Content::new("error".to_string())))
    }
}

macro_rules! generate_task {
    ($task:ident($val:expr),$name:literal) => {{
        pub struct $task(usize);
        impl Complex for $task {
            fn run(&self, input: Input, env: Arc<EnvVar>) -> Output {
                let base = env.get::<usize>("base").unwrap();
                let mut sum = self.0;
                std::thread::sleep(std::time::Duration::from_millis(100));
                input
                    .get_iter()
                    .for_each(|i| sum += i.get::<usize>().unwrap() * base);
                Output::new(sum)
            }
        }
        DefaultTask::with_action($name, $task($val))
    }};
}

fn test_dag(keep_going: bool, expected_outputs: Vec<(usize, Option<Arc<usize>>)>) {
    // tests are independent, so reset the id allocator
    unsafe {
        reset_id_allocator();
    }

    let a = generate_task!(A(1), "Compute A");
    let mut b = generate_task!(B(2), "Compute B");
    let mut c = DefaultTask::with_action("Compute C", FailedActionC(0));
    let mut d = DefaultTask::with_action("Compute D", FailedActionD(1));
    let mut e: DefaultTask = generate_task!(E(16), "Compute E");
    let mut f = generate_task!(F(32), "Compute F");
    let mut g = generate_task!(G(64), "Compute G");
    let h = generate_task!(H(64), "Compute H");
    let i = generate_task!(I(64), "Compute I");
    let j = generate_task!(J(64), "Compute J");
    let k = generate_task!(K(64), "Compute K");
    let l = generate_task!(L(64), "Compute L");
    let m = generate_task!(M(64), "Compute M");

    b.set_predecessors(&[&a]);
    c.set_predecessors(&[&a]);
    d.set_predecessors(&[&a]);
    e.set_predecessors(&[&b, &c]);
    f.set_predecessors(&[&c, &d]);
    g.set_predecessors(&[&b, &e, &f]);

    set_var("TOKIO_WORKER_THREADS", "2");

    let mut env = EnvVar::new();
    env.set("base", 2usize);

    let mut job = Dag::with_tasks(vec![a, b, c, d, e, f, g, h, i, j, k, l, m]);
    if keep_going {
        job = job.keep_going();
    }

    job.set_env(env);
    assert!(!job.start().unwrap()); // reports a failure

    // but the results for independent tasks are still available
    let output = job.get_results::<usize>();

    // sort the results
    let mut output_ordered = output.into_iter().collect::<Vec<(_, _)>>();
    output_ordered.sort_by_key(|(k, _)| *k);

    assert_eq!(output_ordered, expected_outputs);
}

#[test]
fn task_failed_execute() {
    let expected_output = vec![
        (1, Some(Arc::new(1))),
        (2, Some(Arc::new(4))),
        (3, None),
        (4, None),
        (5, None),
        (6, None),
        (7, None),
        (8, Some(Arc::new(64))),
        (9, Some(Arc::new(64))),
        (10, Some(Arc::new(64))),
        (11, Some(Arc::new(64))),
        (12, Some(Arc::new(64))),
        (13, Some(Arc::new(64))),
    ];

    test_dag(false, expected_output);
}

#[test]
fn task_keep_going() {
    let expected_output = vec![
        (1, Some(Arc::new(1))),
        (2, Some(Arc::new(4))),
        (3, None),
        (4, None),
        (5, None),
        (6, None),
        (7, None),
        (8, Some(Arc::new(64))),
        (9, Some(Arc::new(64))),
        (10, Some(Arc::new(64))),
        (11, Some(Arc::new(64))),
        (12, Some(Arc::new(64))),
        (13, Some(Arc::new(64))),
    ];

    test_dag(true, expected_output);
}
