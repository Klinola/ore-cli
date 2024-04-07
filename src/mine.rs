use std::{
    io::{stdout, Write},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Duration,
};

use ore::{self, BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION};
use solana_client::client_error::ClientErrorKind;
use solana_sdk::{
    keccak::{hashv, Hash as KeccakHash},
    signature::Signer,
};
use tokio::time::sleep;

use crate::{
    utils::{get_clock_account, get_proof, get_treasury},
    Miner,
};
use rand::seq::SliceRandom;
impl Miner {
    pub async fn mine(&self, threads: u64) {
        // Register, if needed.
        let signer = self.signer();
        self.register().await;

        let mut stdout = stdout();



        // Start mining loop
        loop {
            // Find a valid hash.
            let treasury = get_treasury(self.cluster.clone()).await;
            let proof = get_proof(self.cluster.clone(), signer.pubkey()).await;

            // Escape sequence that clears the screen and the scrollback buffer
            //stdout.write_all(b"\x1b[2J\x1b[3J\x1b[H").ok();
            stdout
                .write_all(format!("Searching for valid hash...\n").as_bytes())
                .ok();
            let (next_hash, nonce) =
                self.find_next_hash_par(proof.hash.into(), treasury.difficulty.into(), threads);
            stdout
                .write_all(format!("\nSubmitting hash for validation... \n").as_bytes())
                .ok();
            stdout.flush().ok();

            // Submit mine tx.
            // Use busses randomly so on each epoch, transactions don't pile on the same busses
            let mut bus_ids: Vec<usize> = (0..BUS_COUNT).collect();
            bus_ids.shuffle(&mut rand::thread_rng());

            let mut bus_ix = 0;
            let mut bus_id = bus_ids[bus_ix] as u8;

            let mut invalid_busses: Vec<u8> = vec![];
            let mut needs_reset = false;
            'submit: loop {
                // Find a valid bus.
                if invalid_busses.len().eq(&(BUS_COUNT as usize)) {
                    // All busses are drained. Wait until next epoch.
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    // All busses are empty, let;s start anew next time with fresh invalid_busses and bus_id
                    break 'submit;
                }
                if invalid_busses.contains(&bus_id) {
                    println!("Bus {} is empty... ", bus_id);
                    bus_ix += 1;
                    if bus_ix == BUS_COUNT {
                        std::thread::sleep(Duration::from_secs(1));
                        bus_ix = 0;
                    }
                    bus_id = bus_ids[bus_ix] as u8;
                }

                // Reset if epoch has ended
                let treasury = get_treasury(self.cluster.clone()).await;
                let clock = get_clock_account(self.cluster.clone()).await;
                let threshold = treasury.last_reset_at.saturating_add(EPOCH_DURATION);
                let mut attempts = 0;
                const MAX_ATTEMPTS: u8 = 10; // Maximum number of attempts before giving up
                if clock.unix_timestamp.ge(&threshold) || needs_reset {
                    let reset_ix = ore::instruction::reset(signer.pubkey());
                    loop {
                        let cloned_ix = reset_ix.clone();
                        match self.send_and_confirm(&[cloned_ix]).await {
                            Ok(_) => {
                                println!("Transaction confirmed");
                                break;
                            }
                            Err(e) => {
                                attempts += 1;
                                println!("Attempt {} failed: {:?}", attempts, e);
                                if attempts >= MAX_ATTEMPTS {
                                    panic!("Transaction failed after {} attempts: {:?}", MAX_ATTEMPTS, e);
                                }
                                // Exponential backoff or fixed delay could be considered here
                                sleep(Duration::from_secs(5)).await;
                            }
                        }
                    }
                    needs_reset = false;
                }

                // Submit request.
                let ix_mine = ore::instruction::mine(
                    signer.pubkey(),
                    BUS_ADDRESSES[bus_id as usize],
                    next_hash.into(),
                    nonce,
                );
                match self.send_and_confirm(&[ix_mine]).await {
                    Ok(sig) => {
                        stdout.write(format!("Success: {}", sig).as_bytes()).ok();
                        break;
                    }
                    Err(err) => match err.kind {
                        ClientErrorKind::Custom(msg) => {
                            if msg.contains("Bus insufficient") {
                                invalid_busses.push(bus_id);
                            } else if msg.contains("Needs reset") {
                                needs_reset = true;
                            } else if msg.contains("Hash invalid") {
                                break 'submit;
                            } else {
                                stdout
                                    .write_all(format!("\n{:?} \n", msg.to_string()).as_bytes())
                                    .ok();
                            }
                        }
                        _ => {
                            stdout
                                .write_all(format!("\nUnhandled error {:?} \n", err).as_bytes())
                                .ok();
                        }
                    },
                }
            }
        }
    }

    fn _find_next_hash(&self, hash: KeccakHash, difficulty: KeccakHash) -> (KeccakHash, u64) {
        let signer = self.signer();
        let mut next_hash: KeccakHash;
        let mut nonce = 0u64;
        loop {
            next_hash = hashv(&[
                hash.to_bytes().as_slice(),
                signer.pubkey().to_bytes().as_slice(),
                nonce.to_le_bytes().as_slice(),
            ]);
            if next_hash.le(&difficulty) {
                break;
            } else {
                println!("Invalid hash: {} Nonce: {:?}", next_hash.to_string(), nonce);
            }
            nonce += 1;
        }
        (next_hash, nonce)
    }

    fn find_next_hash_par(
        &self,
        hash: KeccakHash,
        difficulty: KeccakHash,
        threads: u64,
    ) -> (KeccakHash, u64) {
        let found_solution = Arc::new(AtomicBool::new(false));
        let solution = Arc::new(Mutex::<(KeccakHash, u64)>::new((
            KeccakHash::new_from_array([0; 32]),
            0,
        )));
        let signer = self.signer();
        let pubkey = signer.pubkey();
        let thread_handles: Vec<_> = (0..threads)
            .map(|i| {
                std::thread::spawn({
                    let found_solution = found_solution.clone();
                    let solution = solution.clone();
                    let mut stdout = stdout();
                    move || {
                        let n = u64::MAX.saturating_div(threads).saturating_mul(i);

                        let nonce_range = u64::MAX / threads + 1;
                        let start = n;
                        let end = start + nonce_range;
                        let mut nonce = start;

                        let mut next_hash: KeccakHash;
                        loop {
                            next_hash = hashv(&[
                                hash.to_bytes().as_slice(),
                                pubkey.to_bytes().as_slice(),
                                nonce.to_le_bytes().as_slice(),
                            ]);
                            if nonce % 10_000 == 0 {
                                if found_solution.load(std::sync::atomic::Ordering::Relaxed) {
                                    return;
                                }
                             }
                            if next_hash.le(&difficulty) {
                                stdout
                                    .write_all(format!("\rFound hash ✨{}", next_hash.to_string()).as_bytes())
                                    .ok();
                                found_solution.store(true, std::sync::atomic::Ordering::Relaxed);
                                let mut w_solution = solution.lock().expect("failed to lock mutex");
                                *w_solution = (next_hash, nonce);
                                return;
                            }
                            if nonce > end {
                                break;
                            }
                            nonce += 1;
                        }
                    }
                })
            })
            .collect();

        for thread_handle in thread_handles {
            thread_handle.join().unwrap();
        }

        let r_solution = solution.lock().expect("Failed to get lock");
        *r_solution
    }
}