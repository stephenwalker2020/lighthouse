use crate::metrics;
use crate::router::processor::FUTURE_SLOT_TOLERANCE;
use crate::sync::manager::SyncMessage;
use crate::sync::{BatchProcessResult, ChainId};
use beacon_chain::{BeaconChain, BeaconChainTypes, BlockError, ChainSegmentResult};
use eth2_libp2p::PeerId;
use slog::{debug, error, trace, warn};
use std::sync::Arc;
use tokio::sync::mpsc;
use types::{Epoch, EthSpec, Hash256, SignedBeaconBlock};

/// Id associated to a block processing request, either a batch or a single block.
#[derive(Clone, Debug, PartialEq)]
pub enum ProcessId {
    /// Processing Id of a range syncing batch.
    RangeBatchId(ChainId, Epoch),
    /// Processing Id of the parent lookup of a block.
    ParentLookup(PeerId, Hash256),
}

pub fn handle_chain_segment<T: BeaconChainTypes>(
    chain: Arc<BeaconChain<T>>,
    process_id: ProcessId,
    downloaded_blocks: Vec<SignedBeaconBlock<T::EthSpec>>,
    sync_send: mpsc::UnboundedSender<SyncMessage<T::EthSpec>>,
    log: slog::Logger,
) {
    match process_id {
        // this a request from the range sync
        ProcessId::RangeBatchId(chain_id, epoch) => {
            let start_slot = downloaded_blocks.first().map(|b| b.message.slot.as_u64());
            let end_slot = downloaded_blocks.last().map(|b| b.message.slot.as_u64());
            let sent_blocks = downloaded_blocks.len();

            let result = match process_blocks(chain, downloaded_blocks.iter(), &log) {
                (_, Ok(_)) => {
                    debug!(log, "Batch processed"; "batch_epoch" => epoch, "first_block_slot" => start_slot, "chain" => chain_id,
                        "last_block_slot" => end_slot, "processed_blocks" => sent_blocks, "service"=> "sync");
                    BatchProcessResult::Success(sent_blocks > 0)
                }
                (imported_blocks, Err(e)) => {
                    debug!(log, "Batch processing failed"; "batch_epoch" => epoch, "first_block_slot" => start_slot, "chain" => chain_id,
                        "last_block_slot" => end_slot, "error" => e, "imported_blocks" => imported_blocks, "service" => "sync");
                    BatchProcessResult::Failed(imported_blocks > 0)
                }
            };

            let msg = SyncMessage::BatchProcessed {
                chain_id,
                epoch,
                result,
            };
            sync_send.send(msg).unwrap_or_else(|_| {
                debug!(
                    log,
                    "Block processor could not inform range sync result. Likely shutting down."
                );
            });
        }
        // this is a parent lookup request from the sync manager
        ProcessId::ParentLookup(peer_id, chain_head) => {
            debug!(
                log, "Processing parent lookup";
                "last_peer_id" => format!("{}", peer_id),
                "blocks" => downloaded_blocks.len()
            );
            // parent blocks are ordered from highest slot to lowest, so we need to process in
            // reverse
            match process_blocks(chain, downloaded_blocks.iter().rev(), &log) {
                (_, Err(e)) => {
                    debug!(log, "Parent lookup failed"; "last_peer_id" => %peer_id, "error" => e);
                    sync_send
                        .send(SyncMessage::ParentLookupFailed{peer_id, chain_head})
                        .unwrap_or_else(|_| {
                            // on failure, inform to downvote the peer
                            debug!(
                                log,
                                "Block processor could not inform parent lookup result. Likely shutting down."
                            );
                        });
                }
                (_, Ok(_)) => {
                    debug!(log, "Parent lookup processed successfully");
                }
            }
        }
    }
}

/// Helper function to process blocks batches which only consumes the chain and blocks to process.
fn process_blocks<
    'a,
    T: BeaconChainTypes,
    I: Iterator<Item = &'a SignedBeaconBlock<T::EthSpec>>,
>(
    chain: Arc<BeaconChain<T>>,
    downloaded_blocks: I,
    log: &slog::Logger,
) -> (usize, Result<(), String>) {
    let blocks = downloaded_blocks.cloned().collect::<Vec<_>>();
    match chain.process_chain_segment(blocks) {
        ChainSegmentResult::Successful { imported_blocks } => {
            metrics::inc_counter(&metrics::BEACON_PROCESSOR_CHAIN_SEGMENT_SUCCESS_TOTAL);
            if imported_blocks > 0 {
                // Batch completed successfully with at least one block, run fork choice.
                run_fork_choice(chain, log);
            }

            (imported_blocks, Ok(()))
        }
        ChainSegmentResult::Failed {
            imported_blocks,
            error,
        } => {
            metrics::inc_counter(&metrics::BEACON_PROCESSOR_CHAIN_SEGMENT_FAILED_TOTAL);
            let r = handle_failed_chain_segment(error, log);
            if imported_blocks > 0 {
                run_fork_choice(chain, log);
            }
            (imported_blocks, r)
        }
    }
}

/// Runs fork-choice on a given chain. This is used during block processing after one successful
/// block import.
fn run_fork_choice<T: BeaconChainTypes>(chain: Arc<BeaconChain<T>>, log: &slog::Logger) {
    match chain.fork_choice() {
        Ok(()) => trace!(
            log,
            "Fork choice success";
            "location" => "batch processing"
        ),
        Err(e) => error!(
            log,
            "Fork choice failed";
            "error" => ?e,
            "location" => "batch import error"
        ),
    }
}

/// Helper function to handle a `BlockError` from `process_chain_segment`
fn handle_failed_chain_segment<T: EthSpec>(
    error: BlockError<T>,
    log: &slog::Logger,
) -> Result<(), String> {
    match error {
        BlockError::ParentUnknown(block) => {
            // blocks should be sequential and all parents should exist

            Err(format!(
                "Block has an unknown parent: {}",
                block.parent_root()
            ))
        }
        BlockError::BlockIsAlreadyKnown => {
            // This can happen for many reasons. Head sync's can download multiples and parent
            // lookups can download blocks before range sync
            Ok(())
        }
        BlockError::FutureSlot {
            present_slot,
            block_slot,
        } => {
            if present_slot + FUTURE_SLOT_TOLERANCE >= block_slot {
                // The block is too far in the future, drop it.
                warn!(
                    log, "Block is ahead of our slot clock";
                    "msg" => "block for future slot rejected, check your time",
                    "present_slot" => present_slot,
                    "block_slot" => block_slot,
                    "FUTURE_SLOT_TOLERANCE" => FUTURE_SLOT_TOLERANCE,
                );
            } else {
                // The block is in the future, but not too far.
                debug!(
                    log, "Block is slightly ahead of our slot clock, ignoring.";
                    "present_slot" => present_slot,
                    "block_slot" => block_slot,
                    "FUTURE_SLOT_TOLERANCE" => FUTURE_SLOT_TOLERANCE,
                );
            }

            Err(format!(
                "Block with slot {} is higher than the current slot {}",
                block_slot, present_slot
            ))
        }
        BlockError::WouldRevertFinalizedSlot { .. } => {
            debug!( log, "Finalized or earlier block processed";);

            Ok(())
        }
        BlockError::GenesisBlock => {
            debug!(log, "Genesis block was processed");
            Ok(())
        }
        BlockError::BeaconChainError(e) => {
            warn!(
                log, "BlockProcessingFailure";
                "msg" => "unexpected condition in processing block.",
                "outcome" => ?e,
            );

            Err(format!("Internal error whilst processing block: {:?}", e))
        }
        other => {
            debug!(
                log, "Invalid block received";
                "msg" => "peer sent invalid block",
                "outcome" => %other,
            );

            Err(format!("Peer sent invalid block. Reason: {:?}", other))
        }
    }
}
