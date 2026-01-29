use anchor_lang::prelude::*;

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

#[program]
pub mod loot_wheel_secure {
    use super::*;

    pub fn initialize_registry(
        ctx: Context<InitializeRegistry>, 
        commission_rate: u8, 
        treasury: Pubkey
    ) -> Result<()> {
        // SECURITY: Validate commission rate
        require!(
            commission_rate >= 5 && commission_rate <= 30,
            ErrorCode::InvalidCommissionRate
        );
        
        let registry = &mut ctx.accounts.registry;
        registry.authority = ctx.accounts.authority.key();
        registry.treasury = treasury;
        registry.wheel_count = 0;
        registry.commission_rate = commission_rate;
        registry.paused = false; // SECURITY: Emergency pause mechanism
        Ok(())
    }

    pub fn update_commission_rate(ctx: Context<UpdateRegistry>, new_rate: u8) -> Result<()> {
        // SECURITY: Bounded commission rate
        require!(
            new_rate >= 5 && new_rate <= 30,
            ErrorCode::InvalidCommissionRate
        );
        
        let registry = &mut ctx.accounts.registry;
        
        // SECURITY: Rate limit changes (once per day)
        let clock = Clock::get()?;
        require!(
            clock.unix_timestamp > registry.last_rate_change + 86400,
            ErrorCode::RateLimitExceeded
        );
        
        registry.commission_rate = new_rate;
        registry.last_rate_change = clock.unix_timestamp;
        
        // SECURITY: Emit event for transparency
        emit!(CommissionRateChanged {
            old_rate: ctx.accounts.registry.commission_rate,
            new_rate,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }

    pub fn pause_protocol(ctx: Context<UpdateRegistry>) -> Result<()> {
        // SECURITY: Emergency pause by authority only
        let registry = &mut ctx.accounts.registry;
        registry.paused = true;
        
        emit!(ProtocolPaused {
            authority: ctx.accounts.authority.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        
        Ok(())
    }

    pub fn initialize_wheel(
        ctx: Context<InitializeWheel>,
        wheel_id: u64,
        entry_fee: u64,
    ) -> Result<()> {
        // SECURITY: Validate entry fee
        require!(
            entry_fee >= 10_000_000 && entry_fee <= 10_000_000_000,
            ErrorCode::InvalidEntryFee
        ); // 0.01 SOL min, 10 SOL max
        
        // SECURITY: Check protocol not paused
        require!(!ctx.accounts.registry.paused, ErrorCode::ProtocolPaused);
        
        let wheel = &mut ctx.accounts.wheel;
        let registry = &mut ctx.accounts.registry;

        // SECURITY: Prevent wheel ID reuse
        require!(wheel.id == 0, ErrorCode::WheelAlreadyInitialized);

        wheel.id = wheel_id;
        wheel.parent = None;
        wheel.children = [None, None];
        wheel.participants = Vec::with_capacity(15);
        wheel.status = WheelStatus::Open;
        wheel.entry_fee = entry_fee;
        wheel.total_pool = 0;
        wheel.generation = 0;
        wheel.created_at = Clock::get()?.unix_timestamp;
        wheel.paid_out = false; // SECURITY: Prevent double payout!

        // SECURITY: Safe increment with overflow check
        registry.wheel_count = registry.wheel_count
            .checked_add(1)
            .ok_or(ErrorCode::Overflow)?;

        Ok(())
    }

    pub fn join_wheel(ctx: Context<JoinWheel>) -> Result<()> {
        let wheel = &mut ctx.accounts.wheel;
        let participant = &ctx.accounts.participant;
        let registry = &ctx.accounts.registry;
        
        // SECURITY: Check protocol not paused
        require!(!registry.paused, ErrorCode::ProtocolPaused);
        
        // SECURITY: Atomic state check and update
        require!(wheel.status == WheelStatus::Open, ErrorCode::WheelNotOpen);
        
        // SECURITY: Strict participant limit with immediate check
        let current_count = wheel.participants.len();
        require!(current_count < 15, ErrorCode::WheelFull);
        
        // SECURITY: Double-check no duplicate (O(n) but n=15 max)
        for p in wheel.participants.iter() {
            require!(p.pubkey != participant.key(), ErrorCode::AlreadyJoined);
        }
        
        // SECURITY: Validate entry fee matches
        let expected_fee = wheel.entry_fee;
        
        // Determine element based on current participant count
        let element = match current_count {
            0..=7 => Element::Earth,
            8..=11 => Element::Air,
            12..=13 => Element::Fire,
            14 => Element::Water,
            _ => return Err(ErrorCode::WheelFull.into()),
        };

        // SECURITY: Add participant BEFORE transfer (prevent reentrancy)
        wheel.participants.push(CompactParticipant {
            pubkey: participant.key(),
            element,
            joined_at: Clock::get()?.unix_timestamp as u32, // SECURITY: Track join time
        });

        // SECURITY: Update state BEFORE transfer
        wheel.total_pool = wheel.total_pool
            .checked_add(expected_fee)
            .ok_or(ErrorCode::Overflow)?;

        // If wheel is now full, mark it BEFORE transfer
        if wheel.participants.len() == 15 {
            wheel.status = WheelStatus::Full;
        }

        // SECURITY: Transfer AFTER all state updates (CEI pattern)
        let ix = anchor_lang::solana_program::system_instruction::transfer(
            &participant.key(),
            &registry.treasury,
            expected_fee,
        );
        anchor_lang::solana_program::program::invoke(
            &ix,
            &[
                participant.to_account_info(),
                ctx.accounts.treasury.to_account_info(),
            ],
        )?;

        // SECURITY: Emit event for tracking
        emit!(ParticipantJoined {
            wheel_id: wheel.id,
            participant: participant.key(),
            element,
            entry_fee: expected_fee,
            participant_count: wheel.participants.len() as u8,
        });

        Ok(())
    }

    pub fn split_wheel(ctx: Context<SplitWheel>, left_id: u64, right_id: u64) -> Result<()> {
        let parent_wheel = &mut ctx.accounts.parent_wheel;
        let left_wheel = &mut ctx.accounts.left_wheel;
        let right_wheel = &mut ctx.accounts.right_wheel;
        let registry = &ctx.accounts.registry;

        // SECURITY: Check protocol not paused
        require!(!registry.paused, ErrorCode::ProtocolPaused);
        
        // SECURITY: CRITICAL - Prevent double payout!
        require!(!parent_wheel.paid_out, ErrorCode::AlreadyPaidOut);
        
        // SECURITY: Validate wheel state
        require!(parent_wheel.status == WheelStatus::Full, ErrorCode::WheelNotFull);
        require!(parent_wheel.participants.len() == 15, ErrorCode::InvalidParticipantCount);
        
        // SECURITY: Validate water recipient matches actual water participant
        let water_participant = &parent_wheel.participants[14];
        require!(
            water_participant.element == Element::Water,
            ErrorCode::InvalidWaterParticipant
        );
        require!(
            ctx.accounts.water_recipient.key() == water_participant.pubkey,
            ErrorCode::InvalidRecipient
        );

        // SECURITY: Mark as paid IMMEDIATELY (prevent reentrancy)
        parent_wheel.paid_out = true;
        parent_wheel.status = WheelStatus::Split;

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

        // SECURITY: Validate treasury has sufficient balance
        let treasury_balance = ctx.accounts.treasury.lamports();
        require!(
            treasury_balance >= payout_amount,
            ErrorCode::InsufficientTreasuryBalance
        );

        // Initialize child wheels with validation
        initialize_child_wheel(
            left_wheel,
            left_id,
            parent_wheel,
            true, // is_left
        )?;
        
        initialize_child_wheel(
            right_wheel,
            right_id,
            parent_wheel,
            false, // is_left
        )?;

        // Update parent wheel children
        parent_wheel.children = [Some(left_wheel.key()), Some(right_wheel.key())];

        // SECURITY: Transfer AFTER all state updates
        **ctx.accounts.treasury.try_borrow_mut_lamports()? -= payout_amount;
        **ctx.accounts.water_recipient.try_borrow_mut_lamports()? += payout_amount;

        // SECURITY: Emit comprehensive event
        emit!(WheelSplit {
            parent_wheel_id: parent_wheel.id,
            left_wheel_id: left_id,
            right_wheel_id: right_id,
            water_winner: water_participant.pubkey,
            payout_amount,
            commission_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }
}

// SECURITY: Helper function to safely initialize child wheels
fn initialize_child_wheel(
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
    wheel.total_pool = 0;
    wheel.generation = parent.generation
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;
    wheel.created_at = Clock::get()?.unix_timestamp;
    wheel.paid_out = false;

    // SECURITY: Redistribute with validation
    if is_left {
        // Left wheel: Earth[0-3] -> Air, Air[8-9] -> Fire, Fire[12] -> Water
        add_participants_safely(wheel, parent, &[0, 1, 2, 3], Element::Air)?;
        add_participants_safely(wheel, parent, &[8, 9], Element::Fire)?;
        add_participants_safely(wheel, parent, &[12], Element::Water)?;
    } else {
        // Right wheel: Earth[4-7] -> Air, Air[10-11] -> Fire, Fire[13] -> Water
        add_participants_safely(wheel, parent, &[4, 5, 6, 7], Element::Air)?;
        add_participants_safely(wheel, parent, &[10, 11], Element::Fire)?;
        add_participants_safely(wheel, parent, &[13], Element::Water)?;
    }
    
    Ok(())
}

// SECURITY: Safe participant redistribution
fn add_participants_safely(
    wheel: &mut Account<Wheel>,
    parent: &Account<Wheel>,
    indices: &[usize],
    new_element: Element,
) -> Result<()> {
    for &i in indices {
        // SECURITY: Bounds check
        require!(i < parent.participants.len(), ErrorCode::InvalidIndex);
        
        let mut p = parent.participants[i].clone();
        p.element = new_element;
        
        // SECURITY: Prevent duplicates
        for existing in wheel.participants.iter() {
            require!(existing.pubkey != p.pubkey, ErrorCode::DuplicateParticipant);
        }
        
        wheel.participants.push(p);
    }
    Ok(())
}

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
        has_one = authority @ ErrorCode::UnauthorizedAuthority
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
    pub wheel: Account<'info, Wheel>,
    #[account(
        mut,
        seeds = [b"registry"],
        bump
    )]
    pub registry: Account<'info, Registry>,
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
    pub wheel: Account<'info, Wheel>,
    #[account(mut)]
    pub participant: Signer<'info>,
    #[account(
        seeds = [b"registry"],
        bump
    )]
    pub registry: Account<'info, Registry>,
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
pub struct SplitWheel<'info> {
    #[account(
        mut,
        constraint = parent_wheel.status == WheelStatus::Full @ ErrorCode::WheelNotFull,
        constraint = !parent_wheel.paid_out @ ErrorCode::AlreadyPaidOut
    )]
    pub parent_wheel: Account<'info, Wheel>,
    #[account(
        init,
        payer = authority,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", left_id.to_le_bytes().as_ref()],
        bump
    )]
    pub left_wheel: Account<'info, Wheel>,
    #[account(
        init,
        payer = authority,
        space = 8 + Wheel::INIT_SPACE,
        seeds = [b"wheel", right_id.to_le_bytes().as_ref()],
        bump
    )]
    pub right_wheel: Account<'info, Wheel>,
    #[account(
        seeds = [b"registry"],
        bump
    )]
    pub registry: Account<'info, Registry>,
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: Treasury wallet - payouts come from here
    #[account(
        mut,
        address = registry.treasury @ ErrorCode::InvalidTreasury
    )]
    pub treasury: UncheckedAccount<'info>,
    /// CHECK: Water participant receiving payout - validated in instruction
    #[account(mut)]
    pub water_recipient: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[account]
#[derive(InitSpace)]
pub struct Registry {
    pub authority: Pubkey,
    pub treasury: Pubkey,
    pub wheel_count: u64,
    pub commission_rate: u8,
    pub paused: bool, // SECURITY: Emergency pause
    pub last_rate_change: i64, // SECURITY: Rate limit changes
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
    pub paid_out: bool, // SECURITY: Prevent double payout!
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct CompactParticipant {
    pub pubkey: Pubkey,      // 32 bytes
    pub element: Element,    // 1 byte
    pub joined_at: u32,      // 4 bytes (timestamp as u32 to save space)
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq, InitSpace)]
pub enum WheelStatus {
    Open,
    Full,
    Split,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum Element {
    Earth,
    Air,
    Fire,
    Water,
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
    pub water_winner: Pubkey,
    pub payout_amount: u64,
    pub commission_amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct CommissionRateChanged {
    pub old_rate: u8,
    pub new_rate: u8,
    pub timestamp: i64,
}

#[event]
pub struct ProtocolPaused {
    pub authority: Pubkey,
    pub timestamp: i64,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Wheel is not open for new participants")]
    WheelNotOpen,
    #[msg("Wheel is full")]
    WheelFull,
    #[msg("Wheel is not full yet")]
    WheelNotFull,
    #[msg("Participant has already joined this wheel")]
    AlreadyJoined,
    #[msg("Invalid participant count")]
    InvalidParticipantCount,
    #[msg("Commission rate must be between 5 and 30")]
    InvalidCommissionRate,
    #[msg("Invalid entry fee (0.01-10 SOL)")]
    InvalidEntryFee,
    #[msg("Wheel already initialized")]
    WheelAlreadyInitialized,
    #[msg("Already paid out")]
    AlreadyPaidOut,
    #[msg("Invalid water participant")]
    InvalidWaterParticipant,
    #[msg("Invalid recipient address")]
    InvalidRecipient,
    #[msg("Insufficient treasury balance")]
    InsufficientTreasuryBalance,
    #[msg("Overflow error")]
    Overflow,
    #[msg("Underflow error")]
    Underflow,
    #[msg("Invalid index")]
    InvalidIndex,
    #[msg("Duplicate participant")]
    DuplicateParticipant,
    #[msg("Unauthorized authority")]
    UnauthorizedAuthority,
    #[msg("Invalid treasury address")]
    InvalidTreasury,
    #[msg("Protocol is paused")]
    ProtocolPaused,
    #[msg("Rate limit exceeded")]
    RateLimitExceeded,
}







