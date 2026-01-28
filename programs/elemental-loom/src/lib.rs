use anchor_lang::prelude::*;

declare_id!("LW3BRgqg9QCZurj5yCMbNt6r3VUC9j1D5zMrHBUkFwJ");

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "Loot Wheel",
    project_url: "https://lootwheel.com",
    contacts: "email:lootwheel@test.com",
    policy: "https://lootwheel.com/security",
    preferred_languages: "en",
    source_code: "https://lootwheel.com"
}


/// Helper function to log payout information to transaction logs.
/// This information appears in Solana Explorer's "Program Logs" section.
/// 
/// Note: This uses msg!() instead of SPL Memo CPI to avoid balance tracking
/// issues when combined with direct lamport modifications.
fn log_payout_info(wheel_id: u64, payout_amount: u64, winner: &Pubkey) {
    let payout_sol = payout_amount as f64 / 1_000_000_000.0;
    msg!(
        "PAYOUT: Loot Wheel #{} - {:.4} SOL to {}",
        wheel_id,
        payout_sol,
        &winner.to_string()[..8]
    );
}

#[program]
pub mod loot_wheel_secure {
    use super::*;

    pub fn initialize_registry(
        ctx: Context<InitializeRegistry>,
        commission_wallet: Pubkey
    ) -> Result<()> {
        let registry = &mut ctx.accounts.registry;
        registry.authority = ctx.accounts.authority.key();
        registry.treasury = ctx.accounts.authority.key();
        registry.commission_wallet = commission_wallet;
        registry.wheel_count = 0;
        registry.commission_rate = 15; // 15% commission
        registry.paused = false;
        registry.last_rate_change = Clock::get()?.unix_timestamp;
        Ok(())
    }

    pub fn update_commission_rate(ctx: Context<UpdateRegistry>, new_rate: u8) -> Result<()> {
        // SECURITY: Bounded commission rate
        require!(new_rate <= 30, ErrorCode::InvalidCommissionRate);
        require!(new_rate >= 1, ErrorCode::InvalidCommissionRate);
        
        let registry = &mut ctx.accounts.registry;
        
        // SECURITY: Rate limit changes (once per week)
        let current_time = Clock::get()?.unix_timestamp;
        let time_since_last_change = current_time
            .checked_sub(registry.last_rate_change)
            .ok_or(ErrorCode::Underflow)?;
        
        // Allow immediate change if it's the first time (last_rate_change = 0)
        if registry.last_rate_change > 0 {
            require!(time_since_last_change >= 7 * 24 * 60 * 60, ErrorCode::RateLimitExceeded);
        }
        
        registry.commission_rate = new_rate;
        registry.last_rate_change = current_time;
        Ok(())
    }

    pub fn update_commission_wallet(
        ctx: Context<UpdateRegistry>, 
        new_commission_wallet: Pubkey
    ) -> Result<()> {
        let registry = &mut ctx.accounts.registry;
        
        // Emit event for transparency
        emit!(CommissionWalletUpdated {
            old_wallet: registry.commission_wallet,
            new_wallet: new_commission_wallet,
            updated_by: ctx.accounts.authority.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        
        registry.commission_wallet = new_commission_wallet;
        
        Ok(())
    }

    pub fn pause_protocol(ctx: Context<UpdateRegistry>, paused: bool) -> Result<()> {
        let registry = &mut ctx.accounts.registry;
        registry.paused = paused;
        Ok(())
    }

    pub fn initialize_wheel(
        ctx: Context<InitializeWheel>, 
        wheel_id: u64,
        tier: GameTier
    ) -> Result<()> {
        let wheel = &mut ctx.accounts.wheel;
        let registry = &mut ctx.accounts.registry;
        
        // SECURITY: Check protocol not paused
        require!(!registry.paused, ErrorCode::ProtocolPaused);
        
        // SECURITY: Validate wheel_id matches registry count
        require!(wheel_id == registry.wheel_count, ErrorCode::InvalidWheelId);
        
        // Map tier to entry fee
        let entry_fee = match tier {
            GameTier::EmberWolf => 10_000_000,        // 0.01 SOL
            GameTier::SkyWalker => 100_000_000,       // 0.1 SOL
            GameTier::Flamecaster => 1_000_000_000,   // 1 SOL
            GameTier::StormWarrior => 5_000_000_000,  // 5 SOL
            GameTier::TideWarlord => 10_000_000_000,  // 10 SOL
            GameTier::StoneTitan => 50_000_000_000,   // 50 SOL
            GameTier::VoidReaper => 100_000_000_000,  // 100 SOL
            GameTier::AstralEmperor => 500_000_000_000, // 500 SOL
            GameTier::CelestialOverlord => 1_000_000_000_000, // 1000 SOL
        };
        
        // SECURITY: Bounded initialization values
        wheel.id = wheel_id;
        wheel.parent = None;
        wheel.children = [None, None];
        wheel.participants = Vec::with_capacity(15);
        wheel.status = WheelStatus::Open;
        wheel.entry_fee = entry_fee;
        wheel.tier = tier;
        wheel.total_pool = 0;
        wheel.generation = 0;
        wheel.created_at = Clock::get()?.unix_timestamp;
        wheel.paid_out = false;
        
        // SECURITY: Update registry atomically
        registry.wheel_count = registry.wheel_count
            .checked_add(1)
            .ok_or(ErrorCode::Overflow)?;
        
        // SECURITY: Emit event for indexing
        emit!(WheelCreated {
            wheel_id,
            tier,
            entry_fee,
            timestamp: wheel.created_at,
        });
        
        Ok(())
    }

    // Standard join for participants 1-14
    pub fn join_wheel(ctx: Context<JoinWheel>) -> Result<()> {
        let wheel_key = ctx.accounts.wheel.key();
        let wheel = &mut ctx.accounts.wheel;
        let participant = &ctx.accounts.participant;
        let registry = &ctx.accounts.registry;
        
        // SECURITY: Check protocol not paused
        require!(!registry.paused, ErrorCode::ProtocolPaused);
        
        // SECURITY: Cannot join refunded wheel
        require!(wheel.status != WheelStatus::Refunded, ErrorCode::WheelRefunded);
        
        // SECURITY: Atomic state check and update
        require!(wheel.status == WheelStatus::Open, ErrorCode::WheelNotOpen);
        
        // SECURITY: Strict participant limit with immediate check
        let current_count = wheel.participants.len();
        require!(current_count < 14, ErrorCode::UseJoinAndSplitForFinalParticipant);
        
        // SECURITY: Validate entry fee matches
        let expected_fee = wheel.entry_fee;
        
        // Determine element based on current participant count
        let element = match current_count {
            0..=7 => Element::Earth,
            8..=11 => Element::Air,
            12..=13 => Element::Fire,
            _ => return Err(ErrorCode::WheelFull.into()),
        };

        // SECURITY: Add participant BEFORE transfer (prevent reentrancy)
        wheel.participants.push(CompactParticipant {
            pubkey: participant.key(),
            element,
        });

        // Update total pool before any transfers
        wheel.total_pool = wheel.total_pool
            .checked_add(expected_fee)
            .ok_or(ErrorCode::Overflow)?;

        // SECURITY: Transfer AFTER all state updates (CEI pattern)
        let ix = anchor_lang::solana_program::system_instruction::transfer(
            &participant.key(),
            &wheel_key,
            expected_fee,
        );
        
        let _ = wheel;
        
        anchor_lang::solana_program::program::invoke(
            &ix,
            &[
                participant.to_account_info(),
                ctx.accounts.wheel.to_account_info(),
            ],
        )?;
        
        // Get data for event
        let wheel_id = ctx.accounts.wheel.id;
        let participant_count = ctx.accounts.wheel.participants.len() as u8;
        
        // SECURITY: Emit event for tracking
        emit!(ParticipantJoined {
            wheel_id,
            participant: participant.key(),
            element,
            entry_fee: expected_fee,
            participant_count,
        });

        Ok(())
    }

    // Special join for the 15th participant that auto-splits
    // WINNER = FIRST JOINER (CENTER of wheel, index 0)
    pub fn join_and_split(
        ctx: Context<JoinAndSplit>, 
        left_id: u64, 
        right_id: u64
    ) -> Result<()> {
        let wheel_key = ctx.accounts.wheel.key();
        let wheel = &mut ctx.accounts.wheel;
        let participant = &ctx.accounts.participant;
        let registry = &mut ctx.accounts.registry;
        
        // SECURITY: Check protocol not paused
        require!(!registry.paused, ErrorCode::ProtocolPaused);
        
        // SECURITY: Cannot join refunded wheel
        require!(wheel.status != WheelStatus::Refunded, ErrorCode::WheelRefunded);
        
        // SECURITY: Validate child wheel IDs match current registry count
        require!(left_id == registry.wheel_count, ErrorCode::StaleWheelIds);
        require!(right_id == registry.wheel_count.checked_add(1).ok_or(ErrorCode::Overflow)?, ErrorCode::StaleWheelIds);
        
        // SECURITY: Atomic state check and update
        require!(wheel.status == WheelStatus::Open, ErrorCode::WheelNotOpen);
        
        // SECURITY: This instruction is ONLY for the 15th participant
        let current_count = wheel.participants.len();
        require!(current_count == 14, ErrorCode::NotFinalParticipant);
        
        // SECURITY: Validate entry fee matches
        let expected_fee = wheel.entry_fee;
        
        // The 15th participant gets the next element in the cycle
        let element = match current_count % 4 {
            0 => Element::Earth,
            1 => Element::Air,
            2 => Element::Fire,
            _ => Element::Water,
        };

        // SECURITY: Validate center_winner matches first participant (the winner!)
        let center_winner_pubkey = wheel.participants[0].pubkey;
        require!(
            ctx.accounts.center_winner.key() == center_winner_pubkey,
            ErrorCode::InvalidCenterWinner
        );

        // SECURITY: Add participant BEFORE transfer (prevent reentrancy)
        wheel.participants.push(CompactParticipant {
            pubkey: participant.key(),
            element,
        });

        // Update total pool before any transfers
        wheel.total_pool = wheel.total_pool
            .checked_add(expected_fee)
            .ok_or(ErrorCode::Overflow)?;

        // Mark wheel as full
        wheel.status = WheelStatus::Full;

        // SECURITY: Transfer AFTER all state updates (CEI pattern)
        let ix = anchor_lang::solana_program::system_instruction::transfer(
            &participant.key(),
            &wheel_key,
            expected_fee,
        );
        
        let _ = wheel;
        
        anchor_lang::solana_program::program::invoke(
            &ix,
            &[
                participant.to_account_info(),
                ctx.accounts.wheel.to_account_info(),
            ],
        )?;

        // Now perform the automatic split
        msg!("Auto-splitting wheel {} - 15th participant joined!", ctx.accounts.wheel.id);
        msg!("CENTER WINNER (first joiner): {}", center_winner_pubkey);
        
        // FAIRNESS FIX: Calculate child wheel rent cost BEFORE mutable borrows
        let child_wheel_rent = ctx.accounts.left_wheel.to_account_info().lamports();
        let total_child_rent = child_wheel_rent
            .checked_mul(2)
            .ok_or(ErrorCode::Overflow)?;
        
        let parent_wheel = &mut ctx.accounts.wheel;
        let left_wheel = &mut ctx.accounts.left_wheel;
        let right_wheel = &mut ctx.accounts.right_wheel;
        
        // SECURITY: CRITICAL - Prevent double payout!
        require!(!parent_wheel.paid_out, ErrorCode::AlreadyPaidOut);
        
        // Calculate distributable pool (total_pool minus child wheel rent)
        let total_pool = parent_wheel.total_pool;
        let distributable_pool = total_pool
            .checked_sub(total_child_rent)
            .ok_or(ErrorCode::Underflow)?;
        
        let commission_rate = registry.commission_rate as u64;
        
        // SECURITY: Safe math operations - calculate from distributable pool
        let commission_amount = distributable_pool
            .checked_mul(commission_rate)
            .ok_or(ErrorCode::Overflow)?
            .checked_div(100)
            .ok_or(ErrorCode::Overflow)?;
            
        let payout_amount = distributable_pool
            .checked_sub(commission_amount)
            .ok_or(ErrorCode::Underflow)?;

        // SECURITY: Validate wheel has sufficient balance
        let wheel_balance = parent_wheel.to_account_info().lamports();
        require!(
            wheel_balance >= total_pool,
            ErrorCode::InsufficientTreasuryBalance
        );

        // SECURITY: Mark as paid IMMEDIATELY (prevent reentrancy)
        parent_wheel.paid_out = true;
        parent_wheel.status = WheelStatus::Split;

        // Initialize child wheels with validation
        initialize_child_wheel_center_wins(
            left_wheel,
            left_id,
            parent_wheel,
            true,
        )?;
        
        initialize_child_wheel_center_wins(
            right_wheel,
            right_id,
            parent_wheel,
            false,
        )?;

        // Update parent wheel children
        parent_wheel.children = [Some(left_wheel.key()), Some(right_wheel.key())];

        // SECURITY: Transfer AFTER all state updates
        **parent_wheel.to_account_info().try_borrow_mut_lamports()? -= payout_amount;
        **ctx.accounts.center_winner.to_account_info().try_borrow_mut_lamports()? += payout_amount;
        
        // Transfer commission from wheel to commission_wallet
        **parent_wheel.to_account_info().try_borrow_mut_lamports()? -= commission_amount;
        **ctx.accounts.commission_wallet.to_account_info().try_borrow_mut_lamports()? += commission_amount;
        
        // FAIRNESS FIX: Reimburse the 15th participant for child wheel rent
        **parent_wheel.to_account_info().try_borrow_mut_lamports()? -= total_child_rent;
        **ctx.accounts.participant.to_account_info().try_borrow_mut_lamports()? += total_child_rent;
        
        msg!("Reimbursed 15th participant {} lamports for child wheel rent", total_child_rent);
        
        // Log payout info to transaction logs (visible on Solana Explorer)
        log_payout_info(parent_wheel.id, payout_amount, &center_winner_pubkey);

        // Update registry wheel count for the two new wheels
        registry.wheel_count = registry.wheel_count
            .checked_add(2)
            .ok_or(ErrorCode::Overflow)?;

        // Get data for join event before emitting
        let wheel_id = parent_wheel.id;
        
        // SECURITY: Emit join event first
        emit!(ParticipantJoined {
            wheel_id,
            participant: participant.key(),
            element,
            entry_fee: expected_fee,
            participant_count: 15,
        });

        // SECURITY: Emit comprehensive split event
        emit!(WheelAutoSplit {
            parent_wheel_id: parent_wheel.id,
            left_wheel_id: left_id,
            right_wheel_id: right_id,
            center_winner: center_winner_pubkey,
            payout_amount,
            commission_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    // Manual split_wheel - admin-only emergency function
    pub fn split_wheel(ctx: Context<SplitWheel>, left_id: u64, right_id: u64) -> Result<()> {
        let parent_wheel = &mut ctx.accounts.parent_wheel;
        let left_wheel = &mut ctx.accounts.left_wheel;
        let right_wheel = &mut ctx.accounts.right_wheel;
        let registry = &mut ctx.accounts.registry;

        // SECURITY: Check protocol not paused
        require!(!registry.paused, ErrorCode::ProtocolPaused);
        
        // SECURITY: Validate child wheel IDs match current registry count
        require!(left_id == registry.wheel_count, ErrorCode::StaleWheelIds);
        require!(right_id == registry.wheel_count.checked_add(1).ok_or(ErrorCode::Overflow)?, ErrorCode::StaleWheelIds);
        
        // SECURITY: CRITICAL - Prevent double payout!
        require!(!parent_wheel.paid_out, ErrorCode::AlreadyPaidOut);
        
        // SECURITY: Validate wheel state
        require!(parent_wheel.status == WheelStatus::Full, ErrorCode::WheelNotFull);
        require!(parent_wheel.participants.len() == 15, ErrorCode::InvalidParticipantCount);
        
        // CENTER WINS: Winner is first joiner (index 0)
        let center_winner_pubkey = parent_wheel.participants[0].pubkey;
        
        require!(
            ctx.accounts.center_winner.key() == center_winner_pubkey,
            ErrorCode::InvalidCenterWinner
        );

        // Calculate payout and commission
        let total_pool = parent_wheel.total_pool;
        let commission_rate = registry.commission_rate as u64;
        
        // SECURITY: Safe math operations
        let commission_amount = total_pool
            .checked_mul(commission_rate)
            .ok_or(ErrorCode::Overflow)?
            .checked_div(100)
            .ok_or(ErrorCode::Overflow)?;
            
        let payout_amount = total_pool
            .checked_sub(commission_amount)
            .ok_or(ErrorCode::Underflow)?;

        // SECURITY: Validate wheel has sufficient balance
        let wheel_balance = parent_wheel.to_account_info().lamports();
        require!(
            wheel_balance >= total_pool,
            ErrorCode::InsufficientTreasuryBalance
        );

        // SECURITY: Mark as paid IMMEDIATELY (prevent reentrancy)
        parent_wheel.paid_out = true;
        parent_wheel.status = WheelStatus::Split;

        // Initialize child wheels with center-wins redistribution
        initialize_child_wheel_center_wins(
            left_wheel,
            left_id,
            parent_wheel,
            true,
        )?;
        
        initialize_child_wheel_center_wins(
            right_wheel,
            right_id,
            parent_wheel,
            false,
        )?;

        // Update parent wheel children
        parent_wheel.children = [Some(left_wheel.key()), Some(right_wheel.key())];

        // SECURITY: Transfer AFTER all state updates
        **parent_wheel.to_account_info().try_borrow_mut_lamports()? -= payout_amount;
        **ctx.accounts.center_winner.try_borrow_mut_lamports()? += payout_amount;
        
        // Transfer commission from wheel to commission_wallet
        **parent_wheel.to_account_info().try_borrow_mut_lamports()? -= commission_amount;
        **ctx.accounts.commission_wallet.to_account_info().try_borrow_mut_lamports()? += commission_amount;
        
        // Log payout info to transaction logs (visible on Solana Explorer)
        log_payout_info(parent_wheel.id, payout_amount, &center_winner_pubkey);

        // Update registry wheel count for the two new wheels
        registry.wheel_count = registry.wheel_count
            .checked_add(2)
            .ok_or(ErrorCode::Overflow)?;

        // SECURITY: Emit comprehensive event
        emit!(WheelSplit {
            parent_wheel_id: parent_wheel.id,
            left_wheel_id: left_id,
            right_wheel_id: right_id,
            center_winner: center_winner_pubkey,
            payout_amount,
            commission_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    /// ADMIN ONLY: Refund all participants of a wheel
    pub fn refund_wheel(ctx: Context<RefundWheel>, gas_cost_per_refund: u64) -> Result<()> {
        let wheel = &mut ctx.accounts.wheel;
        let registry = &ctx.accounts.registry;
        let authority = &ctx.accounts.authority;
        
        // SECURITY: Only registry authority can refund
        require!(
            authority.key() == registry.authority,
            ErrorCode::Unauthorized
        );
        
        // SECURITY: Cannot refund already refunded wheel
        require!(wheel.status != WheelStatus::Refunded, ErrorCode::AlreadyRefunded);
        
        // SECURITY: Cannot refund wheel that was already paid out
        require!(!wheel.paid_out, ErrorCode::AlreadyPaidOut);
        
        // SECURITY: Validate gas cost is reasonable (max 10% of entry fee)
        let max_gas = wheel.entry_fee
            .checked_div(10)
            .ok_or(ErrorCode::Overflow)?;
        require!(gas_cost_per_refund <= max_gas, ErrorCode::GasCostTooHigh);
        
        let participant_count = wheel.participants.len();
        
        // SECURITY: Nothing to refund if no participants
        require!(participant_count > 0, ErrorCode::NoParticipantsToRefund);
        
        // Calculate refund per participant
        let refund_per_participant = wheel.entry_fee
            .checked_sub(gas_cost_per_refund)
            .ok_or(ErrorCode::Underflow)?;
        
        // SECURITY: Validate wheel has sufficient balance for all refunds
        let total_refund_needed = refund_per_participant
            .checked_mul(participant_count as u64)
            .ok_or(ErrorCode::Overflow)?;
        
        let wheel_balance = wheel.to_account_info().lamports();
        require!(
            wheel_balance >= total_refund_needed,
            ErrorCode::InsufficientBalanceForRefund
        );
        
        // SECURITY: Mark as refunded BEFORE transfers (prevent reentrancy)
        let old_status = wheel.status;
        wheel.status = WheelStatus::Refunded;
        
        // Get participant pubkeys for refund
        let participant_pubkeys: Vec<Pubkey> = wheel.participants
            .iter()
            .map(|p| p.pubkey)
            .collect();
        
        // SECURITY: Verify remaining accounts match participants exactly
        require!(
            ctx.remaining_accounts.len() == participant_count,
            ErrorCode::InvalidParticipantCount
        );
        
        // Track total refunded
        let mut total_refunded: u64 = 0;
        let mut refund_count: u32 = 0;
        
        // SECURITY: Process refunds - verify each account matches participant
        for (i, remaining_account) in ctx.remaining_accounts.iter().enumerate() {
            let expected_pubkey = participant_pubkeys[i];
            
            // SECURITY: Verify account matches participant at this index
            require!(
                remaining_account.key() == expected_pubkey,
                ErrorCode::InvalidRecipient
            );
            
            // SECURITY: Verify account is writable
            require!(remaining_account.is_writable, ErrorCode::AccountNotWritable);
            
            // Transfer refund from wheel to participant
            **wheel.to_account_info().try_borrow_mut_lamports()? -= refund_per_participant;
            **remaining_account.try_borrow_mut_lamports()? += refund_per_participant;
            
            total_refunded = total_refunded
                .checked_add(refund_per_participant)
                .ok_or(ErrorCode::Overflow)?;
            refund_count += 1;
        }
        
        // Calculate total gas deducted
        let total_gas_deducted = gas_cost_per_refund
            .checked_mul(participant_count as u64)
            .ok_or(ErrorCode::Overflow)?;
        
        // SECURITY: Emit comprehensive event for auditing
        emit!(WheelRefunded {
            wheel_id: wheel.id,
            participant_count: refund_count,
            refund_per_participant,
            total_refunded,
            gas_cost_per_refund,
            total_gas_deducted,
            previous_status: old_status,
            timestamp: Clock::get()?.unix_timestamp,
        });
        
        Ok(())
    }
}

// Center-wins redistribution logic
fn initialize_child_wheel_center_wins(
    wheel: &mut Account<Wheel>,
    wheel_id: u64,
    parent: &Account<Wheel>,
    is_left: bool,
) -> Result<()> {
    // SECURITY: Ensure wheel not already initialized
    require!(wheel.id == 0, ErrorCode::WheelAlreadyInitialized);
    
    wheel.id = wheel_id;
    wheel.parent = Some(parent.key());
    wheel.children = [None, None];
    wheel.participants = Vec::with_capacity(7);
    wheel.status = WheelStatus::Open;
    wheel.entry_fee = parent.entry_fee;
    wheel.tier = parent.tier;
    wheel.total_pool = 0;
    wheel.generation = parent.generation
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;
    wheel.created_at = Clock::get()?.unix_timestamp;
    wheel.paid_out = false;

    // CENTER-WINS REDISTRIBUTION:
    // Winner (index 0) is removed, remaining 14 participants are redistributed
    // Left wheel: indices 1-7 (7 participants)
    // Right wheel: indices 8-14 (7 participants)
    let start_idx = if is_left { 1 } else { 8 };
    let end_idx = if is_left { 8 } else { 15 };

    for i in start_idx..end_idx {
        if i < parent.participants.len() {
            wheel.participants.push(parent.participants[i]);
        }
    }
    
    // SECURITY: Status must be Open for new wheels
    require!(wheel.status == WheelStatus::Open, ErrorCode::InvalidWheelState);

    Ok(())
}

// ====== Account Structures ======

#[derive(Accounts)]
pub struct InitializeRegistry<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + Registry::INIT_SPACE,
        seeds = [b"registry"],
        bump
    )]
    pub registry: Account<'info, Registry>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateRegistry<'info> {
    #[account(
        mut,
        seeds = [b"registry"],
        bump,
        has_one = authority @ ErrorCode::Unauthorized
    )]
    pub registry: Account<'info, Registry>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(wheel_id: u64)]
pub struct InitializeWheel<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", wheel_id.to_le_bytes().as_ref()],
        bump
    )]
    pub wheel: Box<Account<'info, Wheel>>,
    #[account(
        mut,
        seeds = [b"registry"],
        bump
    )]
    pub registry: Box<Account<'info, Registry>>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct JoinWheel<'info> {
    #[account(
        mut,
        constraint = wheel.status == WheelStatus::Open @ ErrorCode::WheelNotOpen
    )]
    pub wheel: Box<Account<'info, Wheel>>,
    #[account(mut)]
    pub participant: Signer<'info>,
    #[account(
        seeds = [b"registry"],
        bump
    )]
    pub registry: Box<Account<'info, Registry>>,
    /// CHECK: Treasury wallet address from registry
    #[account(
        mut,
        address = registry.treasury @ ErrorCode::InvalidTreasury
    )]
    pub treasury: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(left_id: u64, right_id: u64)]
pub struct JoinAndSplit<'info> {
    #[account(
        mut,
        constraint = wheel.status == WheelStatus::Open @ ErrorCode::WheelNotOpen
    )]
    pub wheel: Box<Account<'info, Wheel>>,
    
    #[account(mut)]
    pub participant: Signer<'info>,
    
    #[account(
        mut,
        seeds = [b"registry"],
        bump
    )]
    pub registry: Box<Account<'info, Registry>>,
    
    /// CHECK: Treasury wallet address from registry
    #[account(
        mut,
        address = registry.treasury @ ErrorCode::InvalidTreasury
    )]
    pub treasury: UncheckedAccount<'info>,
    
    /// CHECK: Commission wallet address from registry
    #[account(
        mut,
        address = registry.commission_wallet @ ErrorCode::InvalidCommissionWallet
    )]
    pub commission_wallet: UncheckedAccount<'info>,
    
    /// CHECK: Center winner (first joiner) - receives payout
    #[account(mut)]
    pub center_winner: UncheckedAccount<'info>,
    
    #[account(
        init,
        payer = participant,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", left_id.to_le_bytes().as_ref()],
        bump
    )]
    pub left_wheel: Box<Account<'info, Wheel>>,
    
    #[account(
        init,
        payer = participant,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", right_id.to_le_bytes().as_ref()],
        bump
    )]
    pub right_wheel: Box<Account<'info, Wheel>>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(left_id: u64, right_id: u64)]
pub struct SplitWheel<'info> {
    #[account(
        mut,
        constraint = parent_wheel.status == WheelStatus::Full @ ErrorCode::WheelNotFull,
        constraint = !parent_wheel.paid_out @ ErrorCode::AlreadyPaidOut
    )]
    pub parent_wheel: Box<Account<'info, Wheel>>,
    #[account(
        init,
        payer = authority,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", left_id.to_le_bytes().as_ref()],
        bump
    )]
    pub left_wheel: Box<Account<'info, Wheel>>,
    #[account(
        init,
        payer = authority,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", right_id.to_le_bytes().as_ref()],
        bump
    )]
    pub right_wheel: Box<Account<'info, Wheel>>,
    #[account(
        mut,
        seeds = [b"registry"],
        bump
    )]
    pub registry: Box<Account<'info, Registry>>,
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        address = registry.treasury @ ErrorCode::InvalidTreasury
    )]
    pub treasury: Signer<'info>,
    /// CHECK: Commission wallet address from registry
    #[account(
        mut,
        address = registry.commission_wallet @ ErrorCode::InvalidCommissionWallet
    )]
    pub commission_wallet: UncheckedAccount<'info>,
    /// CHECK: Center winner (first joiner) receiving payout
    #[account(mut)]
    pub center_winner: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RefundWheel<'info> {
    #[account(mut)]
    pub wheel: Box<Account<'info, Wheel>>,
    #[account(
        seeds = [b"registry"],
        bump,
        constraint = authority.key() == registry.authority @ ErrorCode::Unauthorized
    )]
    pub registry: Box<Account<'info, Registry>>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[account]
#[derive(InitSpace)]
pub struct Registry {
    pub authority: Pubkey,
    pub treasury: Pubkey,
    pub commission_wallet: Pubkey,
    pub wheel_count: u64,
    pub commission_rate: u8,
    pub paused: bool,
    pub last_rate_change: i64,
}

#[account]
#[derive(InitSpace)]
pub struct Wheel {
    pub id: u64,
    pub parent: Option<Pubkey>,
    pub children: [Option<Pubkey>; 2],
    #[max_len(15)]
    pub participants: Vec<CompactParticipant>,
    pub status: WheelStatus,
    pub entry_fee: u64,
    pub total_pool: u64,
    pub generation: u32,
    pub created_at: i64,
    pub paid_out: bool,
    pub tier: GameTier,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct CompactParticipant {
    pub pubkey: Pubkey,
    pub element: Element,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, InitSpace)]
pub enum WheelStatus {
    Open,
    Full,
    Split,
    Refunded,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, InitSpace)]
pub enum Element {
    Earth,
    Air,
    Fire,
    Water,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, InitSpace)]
pub enum GameTier {
    EmberWolf,           // 0.01 SOL
    SkyWalker,           // 0.1 SOL
    Flamecaster,         // 1 SOL
    StormWarrior,        // 5 SOL
    TideWarlord,         // 10 SOL
    StoneTitan,          // 50 SOL
    VoidReaper,          // 100 SOL
    AstralEmperor,       // 500 SOL
    CelestialOverlord,   // 1000 SOL
}

// ====== Events ======

#[event]
pub struct WheelCreated {
    pub wheel_id: u64,
    pub tier: GameTier,
    pub entry_fee: u64,
    pub timestamp: i64,
}

#[event]
pub struct ParticipantJoined {
    pub wheel_id: u64,
    pub participant: Pubkey,
    pub element: Element,
    pub entry_fee: u64,
    pub participant_count: u8,
}

#[event]
pub struct WheelSplit {
    pub parent_wheel_id: u64,
    pub left_wheel_id: u64,
    pub right_wheel_id: u64,
    pub center_winner: Pubkey,
    pub payout_amount: u64,
    pub commission_amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct WheelAutoSplit {
    pub parent_wheel_id: u64,
    pub left_wheel_id: u64,
    pub right_wheel_id: u64,
    pub center_winner: Pubkey,
    pub payout_amount: u64,
    pub commission_amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct WheelRefunded {
    pub wheel_id: u64,
    pub participant_count: u32,
    pub refund_per_participant: u64,
    pub total_refunded: u64,
    pub gas_cost_per_refund: u64,
    pub total_gas_deducted: u64,
    pub previous_status: WheelStatus,
    pub timestamp: i64,
}

#[event]
pub struct CommissionWalletUpdated {
    pub old_wallet: Pubkey,
    pub new_wallet: Pubkey,
    pub updated_by: Pubkey,
    pub timestamp: i64,
}

// ====== Errors ======

#[error_code]
pub enum ErrorCode {
    #[msg("Protocol is currently paused")]
    ProtocolPaused,
    #[msg("Invalid wheel ID - must match registry count")]
    InvalidWheelId,
    #[msg("Invalid commission rate - must be between 1% and 30%")]
    InvalidCommissionRate,
    #[msg("Rate limit exceeded - changes allowed once per week")]
    RateLimitExceeded,
    #[msg("Wheel is not open for participants")]
    WheelNotOpen,
    #[msg("Wheel is already full")]
    WheelFull,
    #[msg("Insufficient balance for transaction")]
    InsufficientBalance,
    #[msg("Overflow in calculation")]
    Overflow,
    #[msg("Underflow in calculation")]
    Underflow,
    #[msg("Unauthorized - Only registry authority can perform this action")]
    Unauthorized,
    #[msg("Wheel is not full - cannot split")]
    WheelNotFull,
    #[msg("Invalid participant count")]
    InvalidParticipantCount,
    #[msg("Invalid center winner - must match first participant")]
    InvalidCenterWinner,
    #[msg("Invalid recipient address")]
    InvalidRecipient,
    #[msg("Wheel already initialized")]
    WheelAlreadyInitialized,
    #[msg("Invalid wheel state")]
    InvalidWheelState,
    #[msg("Invalid treasury address")]
    InvalidTreasury,
    #[msg("Invalid commission wallet address")]
    InvalidCommissionWallet,
    #[msg("Insufficient treasury balance")]
    InsufficientTreasuryBalance,
    #[msg("Wheel already paid out")]
    AlreadyPaidOut,
    #[msg("Use join_and_split instruction for 15th participant")]
    UseJoinAndSplitForFinalParticipant,
    #[msg("This instruction is only for the 15th participant")]
    NotFinalParticipant,
    #[msg("Child wheel IDs are stale - another split occurred. Please retry with updated IDs")]
    StaleWheelIds,
    #[msg("Participant already in this wheel")]
    DuplicateParticipant,
    #[msg("Wheel has already been refunded")]
    AlreadyRefunded,
    #[msg("Gas cost per refund exceeds maximum (10% of entry fee)")]
    GasCostTooHigh,
    #[msg("No participants to refund")]
    NoParticipantsToRefund,
    #[msg("Insufficient balance in wheel for all refunds")]
    InsufficientBalanceForRefund,
    #[msg("Account is not writable")]
    AccountNotWritable,
    #[msg("Cannot join a refunded wheel")]
    WheelRefunded,
}
