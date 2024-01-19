use futures::future::BoxFuture;
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};
// A utility that allows us to implement a `std::task::Waker` without having to
// use `unsafe` code.
use futures::task::{self, ArcWake};
// Used as a channel to queue scheduled tasks.
use crossbeam::channel;

struct Task {
    future: Mutex<BoxFuture<'static, ()>>,
    executor: channel::Sender<Arc<Task>>,
}
impl Task {
    fn schedule(self: &Arc<Self>) {
        self.executor.send(self.clone());
    }

    fn spawn<F>(future: F, sender: &channel::Sender<Arc<Task>>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = Arc::new(Task {
            future: Mutex::new(Box::pin(future)),
            executor: sender.clone(),
        });

        let _ = sender.send(task);
    }
    fn poll(self: Arc<Self>) {
        let waker = task::waker(self.clone());
        let mut cx = Context::from_waker(&waker);
        let mut future = self.future.try_lock().unwrap();
        let _ = future.as_mut().poll(&mut cx);
    }
}
impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        let _ = arc_self.executor.send(arc_self.clone());
    }
}
thread_local! {
    static CURRENT: RefCell<Option<channel::Sender<Arc<Task>>>> =
        RefCell::new(None);
}
struct MiniTokio {
    scheduled: channel::Receiver<Arc<Task>>,
    sender: channel::Sender<Arc<Task>>,
}
impl MiniTokio {
    fn new() -> MiniTokio {
        let (st, rt) = channel::unbounded();
        MiniTokio {
            scheduled: rt,
            sender: st,
        }
    }
    fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Task::spawn(future, &self.sender);
    }
    // run executor
    fn run(&self) {
        CURRENT.with(|cell| {
            *cell.borrow_mut() = Some(self.sender.clone());
        });

        while let Ok(task) = self.scheduled.recv() {
            task.poll();
        }
    }
}
pub fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    CURRENT.with(|cell| {
        let borrow = cell.borrow();
        let sender = borrow.as_ref().unwrap();
        Task::spawn(future, sender);
    });
}

async fn delay(dur: Duration) {
    struct Delay {
        when: Instant,
        waker: Option<Arc<Mutex<Waker>>>,
    }
    impl Future for Delay {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if let Some(waker) = &self.waker {
                let mut waker = waker.lock().unwrap();

                if !waker.will_wake(cx.waker()) {
                    *waker = cx.waker().clone();
                }
            } else {
                let when = self.when;
                let waker = Arc::new(Mutex::new(cx.waker().clone()));
                self.waker = Some(waker.clone());

                thread::spawn(move || {
                    let now = Instant::now();

                    if now < when {
                        thread::sleep(when - now);
                    }
                    let waker = waker.lock().unwrap();
                    waker.wake_by_ref();
                });
            }
            if Instant::now() >= self.when {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
    let future = Delay {
        when: Instant::now() + dur,
        waker: None,
    };
    future.await;
}
fn main() {
    // Create the mini-tokio instance.
    let mut mini_tokio = MiniTokio::new();

    // Spawn the root task. All other tasks are spawned from the context of this
    // root task. No work happens until `mini_tokio.run()` is called.
    mini_tokio.spawn(async {
        // Spawn a task
        spawn(async {
            // Wait for a little bit of time so that "world" is printed after
            // "hello"
            delay(Duration::from_millis(100)).await;
            println!("world");
        });

        // Spawn a second task
        spawn(async {
            println!("hello");
        });

        // We haven't implemented executor shutdown, so force the process to exit.
        delay(Duration::from_millis(200)).await;
        std::process::exit(0);
    });

    // Start the mini-tokio executor loop. Scheduled tasks are received and
    // executed.
    mini_tokio.run();
}
