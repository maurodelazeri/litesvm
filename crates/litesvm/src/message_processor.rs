// adapted from agave svm/src/message_processor.rs (agave 4.1.0, commit d3f1f55c65)
// with the per-program timings detail removed
use {
    solana_program_runtime::invoke_context::InvokeContext, solana_svm_timings::ExecuteTimings,
    solana_svm_transaction::svm_message::SVMMessage, solana_transaction_error::TransactionError,
};

/// Process a message.
/// This method calls each instruction in the message over the set of loaded accounts.
/// For each instruction it calls the program entrypoint method and verifies that the result of
/// the call does not violate the bank's accounting rules.
/// The accounts are committed back to the bank only if every instruction succeeds.
pub(crate) fn process_message<'ix_data>(
    message: &'ix_data impl SVMMessage,
    invoke_context: &mut InvokeContext<'_, 'ix_data>,
    execute_timings: &mut ExecuteTimings,
    accumulated_consumed_units: &mut u64,
) -> Result<(), TransactionError> {
    invoke_context
        .prepare_top_level_instructions(message)
        .map_err(|(ix_idx, err)| TransactionError::InstructionError(ix_idx, err))?;

    for (top_level_instruction_index, (program_id, instruction)) in
        message.program_instructions_iter().enumerate()
    {
        let mut compute_units_consumed = 0;
        let result = if invoke_context.is_precompile(program_id) {
            invoke_context.process_precompile(
                program_id,
                instruction.data,
                message.instructions_iter().map(|ix| ix.data),
            )
        } else {
            invoke_context.process_instruction(&mut compute_units_consumed, execute_timings)
        };

        *accumulated_consumed_units =
            accumulated_consumed_units.saturating_add(compute_units_consumed);

        result.map_err(|err| {
            TransactionError::InstructionError(top_level_instruction_index as u8, err)
        })?;
    }
    Ok(())
}
