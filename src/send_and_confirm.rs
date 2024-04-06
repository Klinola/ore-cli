use std::{
    io::{stdout, Write},
    time::Duration,
};

use solana_client::{
    client_error::{ClientError, ClientErrorKind, Result as ClientResult},
    nonblocking::rpc_client::RpcClient,
    rpc_config::{RpcSendTransactionConfig, RpcSimulateTransactionConfig},
};
use solana_program::instruction::{Instruction, InstructionError};
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    compute_budget::ComputeBudgetInstruction,
    signature::{Signature, Signer},
    transaction::{Transaction, TransactionError},
};
use solana_transaction_status::UiTransactionEncoding;
use tokio::time::sleep;
use rand::{thread_rng, Rng};


use crate::Miner;

const RPC_RETRIES: usize = 1;
const GATEWAY_RETRIES: usize = 10;
const CONFIRM_RETRIES: usize = 10;

impl Miner {
    pub async fn send_and_confirm(&self, ixs: &[Instruction]) -> ClientResult<Signature> {
        let mut stdout = stdout();
        let signer = self.signer();
        let client =
            RpcClient::new_with_commitment(self.cluster.clone(), CommitmentConfig::confirmed());

        // Build tx
        let mut attempts = 0;
        const MAX_ATTEMPTS: u8 = 3; // Maximum number of attempts before giving up
        let retry_delay = Duration::from_secs(5); // Delay between retries
        
        let (mut hash, mut slot) = loop {
            match client.get_latest_blockhash_with_commitment(CommitmentConfig::confirmed()).await {
                Ok(result) => break result,
                Err(e) => {
                    attempts += 1;
                    if attempts >= MAX_ATTEMPTS {
                        panic!("Failed to get latest blockhash after {} attempts: {:?}", MAX_ATTEMPTS, e);
                    }
                    eprintln!("Attempt {} failed, retrying in {:?}: {:?}", attempts, retry_delay, e);
                    sleep(retry_delay).await;
                }
            }
        };        

        let mut send_cfg = RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(CommitmentLevel::Confirmed),
            encoding: Some(UiTransactionEncoding::Base64),
            max_retries: Some(RPC_RETRIES),
            min_context_slot: Some(slot),
        };

        let mut tx = Transaction::new_with_payer(ixs, Some(&signer.pubkey()));
        tx.sign(&[&signer], hash);

        // Sim and prepend cu ixs
        let sim_res = client
            .simulate_transaction_with_config(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: false,
                    replace_recent_blockhash: false,
                    commitment: Some(CommitmentConfig::confirmed()),
                    encoding: Some(UiTransactionEncoding::Base64),
                    accounts: None,
                    min_context_slot: Some(slot),
                    inner_instructions: false,
                },
            )
            .await;
        if let Ok(sim_res) = sim_res {
            match sim_res.value.err {
                Some(err) => match err {
                    TransactionError::InstructionError(_, InstructionError::Custom(e)) => {
                        if e == 1 {
                            log::info!("Needs reset!");
                            return Err(ClientError {
                                request: None,
                                kind: ClientErrorKind::Custom("Needs reset".into()),
                            });
                        } else if e == 3 {
                            log::info!("Hash invalid!");
                            return Err(ClientError {
                                request: None,
                                kind: ClientErrorKind::Custom("Hash invalid".into()),
                            });
                        } else if e == 5 {
                            return Err(ClientError {
                                request: None,
                                kind: ClientErrorKind::Custom("Bus insufficient".into()),
                            });
                        } else {
                            return Err(ClientError {
                                request: None,
                                kind: ClientErrorKind::Custom("Sim failed".into()),
                            });
                        }
                    }
                    _ => {
                        return Err(ClientError {
                            request: None,
                            kind: ClientErrorKind::Custom("Sim failed".into()),
                        })
                    }
                },
                None => {
                    let cu_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(
                        sim_res.value.units_consumed.unwrap() as u32 + 1000,
                    );
                    let cu_price_ix =
                        ComputeBudgetInstruction::set_compute_unit_price(self.priority_fee);
                    let mut final_ixs = vec![];
                    final_ixs.extend_from_slice(&[cu_budget_ix, cu_price_ix]);
                    final_ixs.extend_from_slice(ixs);
                    tx = Transaction::new_with_payer(&final_ixs, Some(&signer.pubkey()));
                    tx.sign(&[&signer], hash);
                }
            }
        } else {
            return Err(ClientError {
                request: None,
                kind: ClientErrorKind::Custom("Failed simulation".into()),
            });
        };

        // Loop
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 20; 
        loop {
            println!("Attempt: {}", attempts + 1);
            match client.send_transaction_with_config(&tx, send_cfg).await {
                Ok(sig) => {
                    println!("👻Transaction sent successfully🎉: {}", sig);

                    let mut rng = thread_rng();
                    let wait_secs: u64 = rng.gen_range(0..=3);
                    println!("Cooling down for {} sec ✨", wait_secs);

                    sleep(Duration::from_secs(wait_secs)).await;
                },
                Err(err) => {
                    println!("Error sending transaction: {:?}", err);
                }
            }
        
            attempts += 1;
            if attempts >= MAX_ATTEMPTS {
                println!("Reached max attempts. Stopping.");
                break; 
            }
        
            let (hash, slot) = client.get_latest_blockhash_with_commitment(CommitmentConfig::confirmed()).await.unwrap();
            send_cfg = RpcSendTransactionConfig {
                skip_preflight: true,
                preflight_commitment: Some(CommitmentLevel::Confirmed),
                encoding: Some(UiTransactionEncoding::Base64),
                max_retries: Some(RPC_RETRIES),
                min_context_slot: Some(slot),
            };
            tx.sign(&[&signer], hash);
        }
    }
}