use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::executor::ThreadPool;
use futures::StreamExt;
use rand::{thread_rng, RngCore};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    utils::{check_match, MatchResult},
    HashCountSender, Job, ResultSender,
};

async fn task(pre: u8, mask: u8, job: Job, tx: ResultSender, hr: HashCountSender) {
    //println!("task pre {:x} mask {:x}", pre, mask);
    let mut rng = thread_rng();
    let mut count = 0;
    let mut previous_result = None;
    loop {
        count += 1;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        if tx.is_closed() || now + 2000 > job.next_block_time as u128 {
            break;
        };
        let mut nonce: [u8; 16] = [0; 16];
        rng.fill_bytes(&mut nonce);
        nonce[0] = (nonce[0] & mask) | pre;
        let result = check_match(nonce, &job);
        if result.better_than(previous_result.as_ref()) {
            previous_result = Some(result.clone());
            let _ = tx.unbounded_send(Some(result)).ok();
        }
    }
    let _ = hr.unbounded_send(count).ok();
    //println!("task pre {:x} done", pre);
}

#[derive(Clone)]
struct Hasher {
    pool: ThreadPool,
}

pub fn setup() -> impl super::Hasher {
    let pool = ThreadPool::new().unwrap();
    Hasher { pool }
}

impl super::Hasher for Hasher {
    fn start_hashing(
        &self,
        job: Job,
        on_termination: UnboundedReceiver<()>,
    ) -> (
        UnboundedReceiver<u64>,
        UnboundedReceiver<Option<MatchResult>>,
    ) {
        let (tx, rx) = unbounded();
        let (metrics_tx, metrics_rx) = unbounded();
        let mut bits = 0;
        let mut max_threads = 1;
        while max_threads * 2 < num_cpus::get() {
            bits += 1;
            max_threads = max_threads * 2;
        }
        println!("bits = {}, max_threads = {}", bits, max_threads);
        for i in 0..max_threads {
            let pre = (i as u8) << (8 - bits);
            let mask = (1 << (8 - bits)) - 1;
            self.pool
                .spawn_ok(task(pre, mask, job.clone(), tx.clone(), metrics_tx.clone()))
        }
        self.pool.spawn_ok(monitor_termination(on_termination, tx));
        (metrics_rx, rx)
    }
}

async fn monitor_termination(mut on_termination: UnboundedReceiver<()>, tx: ResultSender) {
    on_termination.next().await;
    println!("termination detected");
    let _ = tx.unbounded_send(None).ok();
}

#[async_std::test]
async fn test_start_hashing() {
    use super::Hasher;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let job = super::JobDesc {
        buffer: "0100000001ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b9670000000000fdffffff034905000000000000225120096d50b1af027bd0efd97c0ed9b8ba8cdc8e0678c2ed93c3c4640c849382a38f0000000000000000126a109d4b1212d0c917e668e55bbeb5eda7171b51010000000000225120bf247ef829e040cd0a4f4a879aeb2ac7fd3adfd23fbdda7760a9fbfdc781a0ba00000000".to_string(),
        hex_start: 101,
        remote_hash:"ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b967".to_string(),
        pre: 5,
        post_hex: "7".to_string(),
        next_block_time: now + 1000 * 60,
        block_height: 123,
        submitted: false,
    };
    let (_tx, rx) = unbounded();
    let hasher = setup();
    let (_, mut nonce_rx) = hasher.start_hashing(job.into(), rx);
    let mut last_result = None;
    while let Some(Some(result)) = nonce_rx.next().await {
        if result.better_than(last_result.as_ref()) {
            last_result = Some(result.clone());
        }
        if result.fully_matched {
            break;
        }
    }
    assert!(last_result.is_some());
    assert!(last_result.unwrap().fully_matched);
}
