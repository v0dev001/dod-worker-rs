use super::{HashCountSender, Job, OnTermination, ResultSender};
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use ocl::{
    enums::{DeviceInfo, DeviceInfoResult},
    flags::DeviceType,
    Context, MemFlags, ProQue,
};
use rand::{thread_rng, RngCore};
use std::time::{SystemTime, UNIX_EPOCH};

const HASH_BYTES: usize = 32;
const NONCE_BYTES: usize = 16;
const ITERATIONS: u32 = 1000;

fn opencl_source() -> String {
    let common = include_str!("./common.cl");
    let sha2 = include_str!("./sha2.cl");
    let kiss09 = include_str!("./kiss09.cl");
    let hasher = include_str!("./hasher.cl");
    return [common, sha2, kiss09, hasher].join("\n");
}

#[derive(Clone)]
struct Hasher {
    pro_ques: Vec<ProQue>,
}

pub fn setup() -> ocl::Result<impl super::Hasher> {
    let ctx = Context::builder().devices(DeviceType::GPU).build()?;
    let mut pro_ques = Vec::new();
    for (i, device) in ctx.devices().iter().enumerate() {
        let compute_units = match device.info(DeviceInfo::MaxComputeUnits)? {
            DeviceInfoResult::MaxComputeUnits(c) => c,
            _ => panic!("..."),
        };
        let work_group_size = match device.info(DeviceInfo::MaxWorkGroupSize)? {
            DeviceInfoResult::MaxWorkGroupSize(c) => c,
            _ => panic!("..."),
        };
        println!(
            "GPU device {}, compute_units = {} work_group_size = {}",
            i, compute_units, work_group_size
        );
        let batch_size = compute_units as usize * work_group_size;
        pro_ques.push(
            ProQue::builder()
                .src(opencl_source())
                .device(device)
                .dims(batch_size)
                .build()?,
        );
    }
    if pro_ques.is_empty() {
        panic!("No GPU detected");
    }
    Ok(Hasher { pro_ques })
}

fn task(
    job: Job,
    pro_que: ProQue,
    on_termination: OnTermination,
    tx: ResultSender,
    hr: HashCountSender,
) -> ocl::Result<()> {
    let mut count = 0;
    let mut rng = thread_rng();
    let dims = pro_que.dims().to_len();
    let transaction_buffer = pro_que
        .buffer_builder::<u8>()
        .flags(MemFlags::READ_ONLY)
        .len(job.buffer.len())
        .copy_host_slice(&job.buffer)
        .build()?;
    let remote_hash_buffer = pro_que
        .buffer_builder::<u8>()
        .flags(MemFlags::READ_ONLY)
        .len(job.remote_hash.len())
        .copy_host_slice(job.remote_hash.as_bytes())
        .build()?;
    let result_nonces_buffer = pro_que
        .buffer_builder::<u8>()
        .flags(MemFlags::WRITE_ONLY)
        .len(dims * NONCE_BYTES)
        .build()?;
    let result_hashes_buffer = pro_que
        .buffer_builder::<u8>()
        .flags(MemFlags::WRITE_ONLY)
        .len(dims * HASH_BYTES)
        .build()?;
    let matched_buffer = pro_que
        .buffer_builder::<u8>()
        .flags(MemFlags::WRITE_ONLY)
        .len(dims)
        .build()?;
    let mut previous_result: Option<super::utils::MatchResult> = None;
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        if on_termination.is_closed() || now + 2000 > job.next_block_time as u128 {
            println!("break");
            break;
        };
        let rng_seed: u64 = rng.next_u64();
        // println!("seed = {}", rng_seed);
        let kernel = pro_que
            .kernel_builder("hasher")
            .arg_named("transaction", &transaction_buffer)
            .arg_named("tx_len", job.buffer.len() as u32)
            .arg_named("hex_start", job.hex_start as u32)
            .arg_named("rng_seed", rng_seed)
            .arg_named("iterations", ITERATIONS)
            .arg_named("remote_hash", &remote_hash_buffer)
            .arg_named("result_nonces", &result_nonces_buffer)
            .arg_named("result_hashes", &result_hashes_buffer)
            .arg_named("result_matched", &matched_buffer)
            .build()?;

        //println!("before run");
        unsafe {
            kernel.enq()?;
        }
        //println!("after run");
        count += dims as u64 * ITERATIONS as u64;

        let mut nonces = vec![0; dims * NONCE_BYTES];
        let mut hashes = vec![0; dims * HASH_BYTES];
        let mut matched = vec![0; dims];
        result_nonces_buffer.read(&mut nonces).enq()?;
        result_hashes_buffer.read(&mut hashes).enq()?;
        matched_buffer.read(&mut matched).enq()?;
        for (i, v) in matched.iter().enumerate() {
            if *v as usize
                >= previous_result
                    .as_ref()
                    .map(|r| r.matched_length)
                    .unwrap_or_default()
            {
                let mut hash = [0; HASH_BYTES];
                hash.copy_from_slice(&hashes[i * HASH_BYTES..(i + 1) * HASH_BYTES]);
                hash.reverse();
                let mut nonce = [0; NONCE_BYTES];
                nonce.copy_from_slice(&nonces[i * 16..(i + 1) * 16]);
                // println!( "{} matched = {} found = {} nonce = {}", i, v, hex::encode(&hash), hex::encode(&nonce));
                let result = super::utils::check_match(nonce, &job);
                if result.better_than(previous_result.as_ref()) {
                    previous_result = Some(result.clone());
                }
            }
        }
        if let Some(result) = &previous_result {
            let _ = tx.unbounded_send(Some(result.clone()));
            if result.fully_matched {
                break;
            }
        }
    }
    let _ = hr.unbounded_send(count).ok();
    Ok(())
}

impl super::Hasher for Hasher {
    fn start_hashing(
        &self,
        job: Job,
        on_termination: OnTermination,
    ) -> (
        UnboundedReceiver<u64>,
        UnboundedReceiver<Option<super::utils::MatchResult>>,
    ) {
        let (tx, rx) = unbounded();
        let (metrics_tx, metrics_rx) = unbounded();
        for pro_que in self.pro_ques.iter() {
            let job = job.clone();
            let pro_que = pro_que.clone();
            let tx = tx.clone();
            let metrics_tx = metrics_tx.clone();
            let on_termination = on_termination.clone();
            std::thread::spawn(move || task(job, pro_que, on_termination, tx, metrics_tx));
        }
        (metrics_rx, rx)
    }
}
