// Copyright 2017 Parity Technologies (UK) Ltd.
// This file is part of Substrate Demo.

// Substrate Demo is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate Demo is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate Demo.  If not, see <http://www.gnu.org/licenses/>.

//! Council system: Handles the voting in and maintenance of council members.

use rstd::prelude::*;
use codec::KeyedVec;
use runtime_support::storage;
use demo_primitives::{Proposal, AccountId, Hash, BlockNumber};
use runtime::{staking, system, session};
use runtime::staking::Balance;

// no polynomial attack:
// all public operations should be constant time.
// all protected operations may be at most O(public operations)

// public operations:
// - express approval (ideally, express all approvals)
// - re-express approval (ideally, re-express all approvals)
// - clear all approvals
// - submit candidacy
// protected operations:
// - remove candidacy (remove all votes for a candidate)

// for an approval vote of C councilers:

// top K candidates are maintained between votes. all others are discarded.
// - candidate removed & bond returned when elected.
// - candidate removed & bond burned when discarded.

// at the point that the vote ends, all voters' balances are snapshotted.

// for B blocks following, there's a counting period whereby each of the candidates that believe
// they fall in the top K+C voted can present themselves. they get the total stake
// recorded (based on the snapshot); an ordered list is maintained. Noone may present themselves
// that, if elected, would result in being included twice on the council (important since existing
// councilers will will have their approval votes as it may be that they don't get removed), nor
// if existing presenters would mean they're not in the top K+C.

// following B blocks, the top C+K that presented themselves have their bond returned and the
// top C candidates are elected. the top C candidates have their bond returned.

// the top C candidates and all other candidates beyond the top C+K are cleared and the clearing
// mask is appended to mask list (ready to be applied to vote arrays).

// vote-clearing happens lazily, with previous write also storing the round and all subsequent
// rounds' operations applied at the next read/write time.

// vote buffer size increases as required.
// bond taken for initial approval, returned when clearing.

// vec<candidate> (list of addresses, each bonded, can contain "holes" of null addresses). Order important.
// vec<voter> (all voters. order unimportant - just need to be enumerable.)
// voter -> (last round, Vec<bool>)

type VoteIndex = u32;

// parameters
const CANDIDACY_BOND: &[u8] = b"cou:cbo";
const VOTING_BOND: &[u8] = b"cou:vbo";
const PRESENT_SLASH_PER_VOTER: &[u8] = b"cou:pss";
const CARRY_COUNT: &[u8] = b"cou:cco";
const PRESENTATION_DURATION: &[u8] = b"cou:pdu";
const VOTING_PERIOD: &[u8] = b"cou:per";
const TERM_DURATION: &[u8] = b"cou:trm";
const DESIRED_SEATS: &[u8] = b"cou:sts";

// permanent state (always relevant, changes only at the finalisation of voting)
const ACTIVE_COUNCIL: &[u8] = b"cou:act";
const VOTE_COUNT: &[u8] = b"cou:vco";

// persistent state (always relevant, changes constantly)
const APPROVALS_OF: &[u8] = b"cou:apr:";		// Vec<bool>
const REGISTER_INFO_OF: &[u8] = b"cou:reg:";	// Candidate -> (VoteIndex, u32)
const LAST_ACTIVE_OF: &[u8] = b"cou:lac:";		// Voter -> VoteIndex
const VOTERS: &[u8] = b"cou:vrs";				// Vec<AccountId>
const CANDIDATES: &[u8] = b"cou:can";			// Vec<AccountId>, has holes
const CANDIDATE_COUNT: &[u8] = b"cou:cnc";		// u32

// temporary state (only relevant during finalisation/presentation)
const NEXT_FINALISE: &[u8] = b"cou:nxt";
const SNAPSHOTED_STAKES: &[u8] = b"cou:sss";		// Vec<Balance>
const LEADERBOARD: &[u8] = b"cou:win";				// Vec<(Balance, AccountId)> ORDERED low -> high

/// How much should be locked up in order to submit one's candidacy.
pub fn candidacy_bond() -> Balance {
	storage::get(CANDIDACY_BOND)
		.expect("all core parameters of council module must be in place")
}

/// How much should be locked up in order to be able to submit votes.
pub fn voting_bond() -> Balance {
	storage::get(VOTING_BOND)
		.expect("all core parameters of council module must be in place")
}

/// How long to give each top candidate to present themselves after the vote ends.
pub fn presentation_duration() -> BlockNumber {
	storage::get(PRESENTATION_DURATION)
		.expect("all core parameters of council module must be in place")
}

/// How often (in blocks) to check for new votes.
pub fn voting_period() -> BlockNumber {
	storage::get(VOTING_PERIOD)
		.expect("all core parameters of council module must be in place")
}

/// How long each position is active for.
pub fn term_duration() -> BlockNumber {
	storage::get(TERM_DURATION)
		.expect("all core parameters of council module must be in place")
}

/// The punishment, per voter, if you provide an invalid presentation.
pub fn present_slash_per_voter() -> Balance {
	storage::get(PRESENT_SLASH_PER_VOTER)
		.expect("all core parameters of council module must be in place")
}

/// Number of accounts that should be sitting on the council.
pub fn desired_seats() -> u32 {
	storage::get(DESIRED_SEATS)
		.expect("all core parameters of council module must be in place")
}

/// How many runners-up should have their approvals persist until the next vote.
pub fn carry_count() -> u32 {
	storage::get(CARRY_COUNT)
		.expect("all core parameters of council module must be in place")
}

/// True if we're currently in a presentation period.
pub fn presentation_active() -> bool {
	storage::exists(NEXT_FINALISE)
}

/// The current council. When there's a vote going on, this should still be used for executive
/// matters.
pub fn active_council() -> Vec<(AccountId, BlockNumber)> {
	storage::get_or_default(ACTIVE_COUNCIL)
}

/// If `who` a candidate at the moment?
pub fn is_a_candidate(who: &AccountId) -> bool {
	storage::exists(&who.to_keyed_vec(REGISTER_INFO_OF))
}

/// Determine the block that a vote can happen on which is no less than `n`.
pub fn next_vote_from(n: BlockNumber) -> BlockNumber {
	let voting_period = voting_period();
	(n + voting_period - 1) / voting_period * voting_period
}

/// The block number on which the tally for the next election will happen. `None` only if the
/// desired seats of the council is zero.
pub fn next_tally() -> Option<BlockNumber> {
	let desired_seats = desired_seats();
	if desired_seats == 0 {
		None
	} else {
		let c = active_council();
		let (next_possible, count, coming) =
			if let Some((tally_end, comers, leavers)) = next_finalise() {
				// if there's a tally in progress, then next tally can begin immediately afterwards
				(tally_end, c.len() - leavers.len() + comers as usize, comers)
			} else {
				(system::block_number(), c.len(), 0)
			};
		if count < desired_seats as usize {
			Some(next_possible)
		} else {
			// next tally begins once enough council members expire to bring members below desired.
			if desired_seats <= coming {
				// the entire amount of desired seats is less than those new members - we'll have
				// to wait until they expire.
				Some(next_possible + term_duration())
			} else {
				Some(c[c.len() - (desired_seats - coming) as usize].1)
			}
		}.map(next_vote_from)
	}
}

/// The accounts holding the seats that will become free on the next tally.
pub fn next_finalise() -> Option<(BlockNumber, u32, Vec<AccountId>)> {
	storage::get(NEXT_FINALISE)
}

/// The total number of votes that have happened or are in progress.
pub fn vote_index() -> VoteIndex {
	storage::get_or_default(VOTE_COUNT)
}

/// The present candidate list.
pub fn candidates() -> Vec<AccountId> {
	storage::get_or_default(CANDIDATES)
}

/// The present voter list.
pub fn voters() -> Vec<AccountId> {
	storage::get_or_default(VOTERS)
}

/// The vote index and list slot that the candidate `who` was registered or `None` if they are not
/// currently registered.
pub fn candidate_reg_info(who: &AccountId) -> Option<(VoteIndex, u32)> {
	storage::get(&who.to_keyed_vec(REGISTER_INFO_OF))
}

/// The last cleared vote index that this voter was last active at.
pub fn voter_last_active(voter: &AccountId) -> Option<VoteIndex> {
	storage::get(&voter.to_keyed_vec(LAST_ACTIVE_OF))
}

/// The last cleared vote index that this voter was last active at.
pub fn approvals_of(voter: &AccountId) -> Vec<bool> {
	storage::get_or_default(&voter.to_keyed_vec(APPROVALS_OF))
}

/// Get the leaderboard if we;re in the presentation phase.
pub fn leaderboard() -> Option<Vec<(Balance, AccountId)>> {
	storage::get(LEADERBOARD)
}

pub mod public {
	use super::*;

	/// Set candidate approvals. Approval slots stay valid as long as candidates in those slots
	/// are registered.
	pub fn set_approvals(signed: &AccountId, votes: &Vec<bool>, index: VoteIndex) {
		assert!(!presentation_active());
		assert_eq!(index, vote_index());
		if !storage::exists(&signed.to_keyed_vec(LAST_ACTIVE_OF)) {
			// not yet a voter - deduct bond.
			let b = staking::balance(signed);
			assert!(b >= voting_bond());
			// TODO: this is no good as it precludes active stakers. check that when we allow
			// deductions of actively staked balances that things in the staking module don't
			// break.
//			assert!(staking::unlock_block(signed) == staking::LockStatus::Liquid);
			staking::internal::set_balance(signed, b - voting_bond());
			storage::put(VOTERS, &{
				let mut v: Vec<AccountId> = storage::get_or_default(VOTERS);
				v.push(signed.clone());
				v
			});
		}
		storage::put(&signed.to_keyed_vec(APPROVALS_OF), votes);
		storage::put(&signed.to_keyed_vec(LAST_ACTIVE_OF), &index);
	}

	/// Remove a voter. For it not to be a bond-consuming no-op, all approved candidate indices
	/// must now be either unregistered or registered to a candidate that registered the slot after
	/// the voter gave their last approval set.
	///
	/// May be called by anyone. Returns the voter deposit to `signed`.
	pub fn kill_inactive_voter(signed: &AccountId, signed_index: u32, who: &AccountId, who_index: u32, assumed_vote_index: VoteIndex) {
		assert!(!presentation_active());
		assert!(voter_last_active(signed).is_some());
		let last_active = voter_last_active(who).expect("target for inactivity cleanup must be active");
		assert!(assumed_vote_index == vote_index());
		assert!(last_active < assumed_vote_index);
		let voters = voters();
		let signed_index = signed_index as usize;
		let who_index = who_index as usize;
		assert!(signed_index < voters.len() && voters[signed_index] == *signed);
		assert!(who_index < voters.len() && voters[who_index] == *who);

		// will definitely kill one of signed or who now.

		let valid = !approvals_of(who).iter()
			.zip(candidates().iter())
			.any(|(&appr, addr)|
				appr &&
				*addr != AccountId::default() &&
				candidate_reg_info(addr)
					.expect("all items in candidates list are registered").0 <= last_active);

		remove_voter(
			if valid { who } else { signed },
			if valid { who_index } else { signed_index },
			voters
		);
		if valid {
			staking::internal::set_balance(signed, staking::balance(signed) + voting_bond());
		}
	}

	/// Remove a voter from the system. Will panic if voters()[index] != voter.
	fn remove_voter(voter: &AccountId, index: usize, mut voters: Vec<AccountId>) {
		storage::put(VOTERS, &{ voters.swap_remove(index); voters });
		storage::kill(&voter.to_keyed_vec(APPROVALS_OF));
		storage::kill(&voter.to_keyed_vec(LAST_ACTIVE_OF));
	}

	/// Remove a voter. All votes are cancelled and the voter deposit is returned.
	pub fn retract_voter(signed: &AccountId, index: u32) {
		assert!(!presentation_active());
		assert!(storage::exists(&signed.to_keyed_vec(LAST_ACTIVE_OF)));
		let voters = voters();
		let index = index as usize;
		assert!(index < voters.len() && voters[index] == *signed);
		remove_voter(signed, index, voters);
		staking::internal::set_balance(signed, staking::balance(signed) + voting_bond());
	}

	/// Submit oneself for candidacy.
	///
	/// Account must have enough transferrable funds in it to pay the bond.
	pub fn submit_candidacy(signed: &AccountId, slot: u32) {
		assert!(!is_a_candidate(signed));
		let b = staking::balance(signed);
		let candidacy_bond = candidacy_bond();
		assert!(b >= candidacy_bond);
		assert!(staking::unlock_block(signed) == staking::LockStatus::Liquid);

		let slot = slot as usize;
		let count = storage::get_or_default::<u32>(CANDIDATE_COUNT) as usize;
		let candidates: Vec<AccountId> = storage::get_or_default(CANDIDATES);
		assert!(
			(slot == count && count == candidates.len()) ||
			(slot < candidates.len() && candidates[slot] == AccountId::default())
		);

		staking::internal::set_balance(signed, b - candidacy_bond);

		let mut candidates = candidates;
		if slot == candidates.len() {
			candidates.push(signed.clone());
		} else {
			candidates[slot] = signed.clone();
		}
		storage::put(CANDIDATES, &candidates);
		storage::put(CANDIDATE_COUNT, &(count as u32 + 1));
		storage::put(&signed.to_keyed_vec(REGISTER_INFO_OF), &(vote_index(), slot));
	}

	/// Claim that `signed` is one of the top carry_count() + current_vote().1 candidates.
	/// Only works if the block number >= current_vote().0 and < current_vote().0 + presentation_duration()
	/// `signed` should have at least
	pub fn present(signed: &AccountId, candidate: &AccountId, total: Balance, index: VoteIndex) {
		assert_eq!(index, vote_index());
		assert!(presentation_active());
		let b = staking::balance(signed);
		let stakes: Vec<Balance> = storage::get_or_default(SNAPSHOTED_STAKES);
		let voters: Vec<AccountId> = storage::get_or_default(VOTERS);
		let bad_presentation_punishment = present_slash_per_voter() * voters.len() as Balance;
		assert!(b >= bad_presentation_punishment);

		let mut leaderboard = leaderboard().expect("leaderboard must exist while present phase active");
		assert!(total > leaderboard[0].0);

		let (registered_since, candidate_index): (VoteIndex, u32) =
			storage::get(&candidate.to_keyed_vec(REGISTER_INFO_OF)).expect("presented candidate must be current");
		let actual_total = voters.iter()
			.zip(stakes.iter())
			.filter_map(|(voter, stake)|
				match voter_last_active(voter) {
					Some(b) if b >= registered_since =>
						approvals_of(voter).get(candidate_index as usize)
							.and_then(|approved| if *approved { Some(stake) } else { None }),
					_ => None,
				})
			.sum();
		let dupe = leaderboard.iter().find(|&&(_, ref c)| c == candidate).is_some();
		if total == actual_total && !dupe {
			// insert into leaderboard
			leaderboard[0] = (total, candidate.clone());
			leaderboard.sort_by_key(|&(t, _)| t);
			storage::put(LEADERBOARD, &leaderboard);
		} else {
			staking::internal::set_balance(signed, b - bad_presentation_punishment);
		}
	}
}

pub mod privileged {
	use super::*;

	/// Set the desired member count; if lower than the current count, then seats will not be up
	/// election when they expire. If more, then a new vote will be started if one is not already
	/// in progress.
	pub fn set_desired_seats(count: u32) {
		storage::put(DESIRED_SEATS, &count);
	}

	/// Remove a particular member. A tally will happen instantly (if not already in a presentation
	/// period) to fill the seat if removal means that the desired members are not met.
	/// This is effective immediately.
	pub fn remove_member(who: &AccountId) {
		let new_council: Vec<(AccountId, BlockNumber)> = active_council()
			.into_iter()
			.filter(|i| i.0 != *who)
			.collect();
		storage::put(ACTIVE_COUNCIL, &new_council);
	}

	/// Set the presentation duration. If there is current a vote being presented for, will
	/// invoke `finalise_vote`.
	pub fn set_presentation_duration(count: BlockNumber) {
		storage::put(PRESENTATION_DURATION, &count);
	}

	/// Set the presentation duration. If there is current a vote being presented for, will
	/// invoke `finalise_vote`.
	pub fn set_term_duration(count: BlockNumber) {
		storage::put(TERM_DURATION, &count);
	}
}

pub mod internal {
	use super::*;
	use demo_primitives::Proposal;
	use dispatch::enact_proposal;

	/// Check there's nothing to do this block
	pub fn end_block() {
		let block_number = system::block_number();
		if block_number % voting_period() == 0 {
			if let Some(number) = next_tally() {
				if block_number == number {
					start_tally();
				}
			}
		}
		if let Some((number, _, _)) = next_finalise() {
			if block_number == number {
				finalise_tally();
			}
		}
	}
}

/// Close the voting, snapshot the staking and the number of seats that are actually up for grabs.
fn start_tally() {
	let active_council = active_council();
	let desired_seats = desired_seats() as usize;
	let number = system::block_number();
	let expiring = active_council.iter().take_while(|i| i.1 == number).cloned().collect::<Vec<_>>();
	if active_council.len() - expiring.len() < desired_seats {
		let empty_seats = desired_seats - (active_council.len() - expiring.len());
		storage::put(NEXT_FINALISE, &(number + presentation_duration(), empty_seats as u32, expiring));

		let voters: Vec<AccountId> = storage::get_or_default(VOTERS);
		let votes: Vec<Balance> = voters.iter().map(staking::balance).collect();
		storage::put(SNAPSHOTED_STAKES, &votes);

		// initialise leaderboard.
		let leaderboard_size = empty_seats + carry_count() as usize;
		storage::put(LEADERBOARD, &vec![(0 as Balance, AccountId::default()); leaderboard_size]);
	}
}

/// Finalise the vote, removing each of the `removals` and inserting `seats` of the most approved
/// candidates in their place. If the total council members is less than the desired membership
/// a new vote is started.
/// Clears all presented candidates, returning the bond of the elected ones.
fn finalise_tally() {
	storage::kill(SNAPSHOTED_STAKES);
	let (_, coming, expiring): (BlockNumber, u32, Vec<AccountId>) = storage::take(NEXT_FINALISE)
		.expect("finalise can only be called after a tally is started.");
	let leaderboard: Vec<(Balance, AccountId)> = storage::take(LEADERBOARD).unwrap_or_default();
	let new_expiry = system::block_number() + term_duration();

	// return bond to winners.
	let candidacy_bond = candidacy_bond();
	for &(_, ref w) in leaderboard.iter()
		.rev()
		.take_while(|&&(b, _)| b != 0)
		.take(coming as usize)
	{
		staking::internal::set_balance(w, staking::balance(w) + candidacy_bond);
	}

	// set the new council.
	let new_council: Vec<_> = active_council()
		.into_iter()
		.skip(expiring.len())
		.chain(leaderboard.iter()
			.rev()
			.take_while(|&&(b, _)| b != 0)
			.take(coming as usize)
			.cloned()
			.map(|(_, a)| (a, new_expiry)))
		.collect();
	storage::put(ACTIVE_COUNCIL, &new_council);

	// clear all except runners-up from candidate list.
	let candidates: Vec<AccountId> = storage::get_or_default(CANDIDATES);
	let mut new_candidates = vec![AccountId::default(); candidates.len()];	// shrink later.
	let runners_up = leaderboard.into_iter()
		.rev()
		.take_while(|&(b, _)| b != 0)
		.skip(coming as usize)
		.map(|(_, a)| (a, candidate_reg_info(&a).expect("runner up must b registered").1));
	let mut count = 0u32;
	for (address, slot) in runners_up {
		new_candidates[slot as usize] = address;
		count += 1;
	}
	for (old, new) in candidates.iter().zip(new_candidates.iter()) {
		if old != new {
			// removed - kill it
			storage::kill(&old.to_keyed_vec(REGISTER_INFO_OF));
		}
	}
	// discard any superfluous slots.
	if let Some(last_index) = new_candidates.iter().rposition(|c| *c != AccountId::default()) {
		new_candidates.truncate(last_index + 1);
	}
	storage::put(CANDIDATES, &(new_candidates));
	storage::put(CANDIDATE_COUNT, &count);
	storage::put(VOTE_COUNT, &(vote_index() + 1));
}

#[cfg(test)]
mod tests {
	use super::*;
	use runtime_io::{with_externalities, twox_128, TestExternalities};
	use codec::{KeyedVec, Joiner};
	use keyring::Keyring;
	use environment::with_env;
	use demo_primitives::{AccountId, Proposal};
	use runtime::{staking, session, democracy};

	fn new_test_ext() -> TestExternalities {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let eve = Keyring::Eve.to_raw_public();
		let ferdie = Keyring::Ferdie.to_raw_public();
		let one = Keyring::One.to_raw_public();

		map![
			twox_128(b"ses:len").to_vec() => vec![].and(&1u64),
			twox_128(b"ses:val:len").to_vec() => vec![].and(&3u32),
			twox_128(&0u32.to_keyed_vec(b"ses:val:")).to_vec() => alice.to_vec(),
			twox_128(&1u32.to_keyed_vec(b"ses:val:")).to_vec() => bob.to_vec(),
			twox_128(&2u32.to_keyed_vec(b"ses:val:")).to_vec() => charlie.to_vec(),
			twox_128(b"sta:wil:len").to_vec() => vec![].and(&3u32),
			twox_128(&0u32.to_keyed_vec(b"sta:wil:")).to_vec() => alice.to_vec(),
			twox_128(&1u32.to_keyed_vec(b"sta:wil:")).to_vec() => bob.to_vec(),
			twox_128(&2u32.to_keyed_vec(b"sta:wil:")).to_vec() => charlie.to_vec(),
			twox_128(&alice.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&10u64),
			twox_128(&bob.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&20u64),
			twox_128(&charlie.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&30u64),
			twox_128(&dave.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&40u64),
			twox_128(&eve.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&50u64),
			twox_128(&ferdie.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&60u64),
			twox_128(&one.to_keyed_vec(b"sta:bal:")).to_vec() => vec![].and(&1u64),
			twox_128(b"sta:tot").to_vec() => vec![].and(&210u64),
			twox_128(b"sta:spe").to_vec() => vec![].and(&1u64),
			twox_128(b"sta:vac").to_vec() => vec![].and(&3u64),
			twox_128(b"sta:era").to_vec() => vec![].and(&1u64),

			twox_128(b"cou:cbo").to_vec() => vec![].and(&9u64), // CANDIDACY_BOND
			twox_128(b"cou:vbo").to_vec() => vec![].and(&3u64), // VOTING_BOND
			twox_128(b"cou:pss").to_vec() => vec![].and(&1u64), // PRESENT_SLASH_PER_VOTER
			twox_128(b"cou:cco").to_vec() => vec![].and(&2u64), // CARRY_COUNT
			twox_128(b"cou:pdu").to_vec() => vec![].and(&2u64), // PRESENTATION_DURATION
			twox_128(b"cou:per").to_vec() => vec![].and(&4u64), // VOTING_PERIOD
			twox_128(b"cou:trm").to_vec() => vec![].and(&5u64), // TERM_DURATION
			twox_128(b"cou:sts").to_vec() => vec![].and(&2u64)  // DESIRED_SEATS
		]
	}

	#[test]
	fn basic_environment_works() {
		let alice = Keyring::Alice.to_raw_public();
		let mut t = new_test_ext();
		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(next_vote_from(1), 4);
			assert_eq!(next_vote_from(4), 4);
			assert_eq!(next_vote_from(5), 8);
			assert_eq!(vote_index(), 0);
			assert_eq!(candidacy_bond(), 9);
			assert_eq!(voting_bond(), 3);
			assert_eq!(present_slash_per_voter(), 1);
			assert_eq!(presentation_duration(), 2);
			assert_eq!(voting_period(), 4);
			assert_eq!(term_duration(), 5);
			assert_eq!(desired_seats(), 2);
			assert_eq!(carry_count(), 2);

			assert_eq!(active_council(), vec![]);
			assert_eq!(next_tally(), Some(4));
			assert_eq!(presentation_active(), false);
			assert_eq!(next_finalise(), None);

			assert_eq!(candidates(), Vec::<AccountId>::new());
			assert_eq!(is_a_candidate(&alice), false);
			assert_eq!(candidate_reg_info(&alice), None);

			assert_eq!(voters(), Vec::<AccountId>::new());
			assert_eq!(voter_last_active(&alice), None);
			assert_eq!(approvals_of(&alice), vec![]);
		});
	}

	#[test]
	fn simple_candidate_submission_should_work() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(candidates(), Vec::<AccountId>::new());
			assert_eq!(candidate_reg_info(&alice), None);
			assert_eq!(candidate_reg_info(&bob), None);
			assert_eq!(is_a_candidate(&alice), false);
			assert_eq!(is_a_candidate(&bob), false);

			public::submit_candidacy(&alice, 0);
			assert_eq!(candidates(), vec![alice.clone()]);
			assert_eq!(candidate_reg_info(&alice), Some((0 as VoteIndex, 0u32)));
			assert_eq!(candidate_reg_info(&bob), None);
			assert_eq!(is_a_candidate(&alice), true);
			assert_eq!(is_a_candidate(&bob), false);

			public::submit_candidacy(&bob, 1);
			assert_eq!(candidates(), vec![alice.clone(), bob.clone()]);
			assert_eq!(candidate_reg_info(&alice), Some((0 as VoteIndex, 0u32)));
			assert_eq!(candidate_reg_info(&bob), Some((0 as VoteIndex, 1u32)));
			assert_eq!(is_a_candidate(&alice), true);
			assert_eq!(is_a_candidate(&bob), true);
		});
	}

	fn new_test_ext_with_candidate_holes() -> TestExternalities {
		let alice = Keyring::Alice.to_raw_public();
		let mut t = new_test_ext();
		t.insert(twox_128(CANDIDATES).to_vec(), vec![].and(&vec![AccountId::default(), AccountId::default(), alice.clone()]));
		t.insert(twox_128(CANDIDATE_COUNT).to_vec(), vec![].and(&1u32));
		t.insert(twox_128(&alice.to_keyed_vec(REGISTER_INFO_OF)).to_vec(), vec![].and(&(0 as VoteIndex, 2u32)));
		t
	}

	#[test]
	fn candidate_submission_using_free_slot_should_work() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let mut t = new_test_ext_with_candidate_holes();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(candidates(), vec![AccountId::default(), AccountId::default(), alice.clone()]);

			public::submit_candidacy(&bob, 1);
			assert_eq!(candidates(), vec![AccountId::default(), bob.clone(), alice.clone()]);

			public::submit_candidacy(&charlie, 0);
			assert_eq!(candidates(), vec![charlie.clone(), bob.clone(), alice.clone()]);
		});
	}

	#[test]
	fn candidate_submission_using_alternative_free_slot_should_work() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let mut t = new_test_ext_with_candidate_holes();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(candidates(), vec![AccountId::default(), AccountId::default(), alice.clone()]);

			public::submit_candidacy(&bob, 0);
			assert_eq!(candidates(), vec![bob.clone(), AccountId::default(), alice.clone()]);

			public::submit_candidacy(&charlie, 1);
			assert_eq!(candidates(), vec![bob.clone(), charlie.clone(), alice.clone()]);
		});
	}

	#[test]
	#[should_panic]
	fn candidate_submission_not_using_free_slot_should_panic() {
		let dave = Keyring::Dave.to_raw_public();
		let mut t = new_test_ext_with_candidate_holes();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			public::submit_candidacy(&dave, 3);
		});
	}

	#[test]
	#[should_panic]
	fn bad_candidate_slot_submission_should_panic() {
		let alice = Keyring::Alice.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(candidates(), Vec::<AccountId>::new());
			public::submit_candidacy(&alice, 1);
		});
	}

	#[test]
	#[should_panic]
	fn non_free_candidate_slot_submission_should_panic() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(candidates(), Vec::<AccountId>::new());
			public::submit_candidacy(&alice, 0);
			public::submit_candidacy(&bob, 0);
		});
	}

	#[test]
	#[should_panic]
	fn dupe_candidate_submission_should_panic() {
		let alice = Keyring::Alice.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			assert_eq!(candidates(), Vec::<AccountId>::new());
			public::submit_candidacy(&alice, 0);
			public::submit_candidacy(&alice, 1);
		});
	}

	#[test]
	fn voting_should_work() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let eve = Keyring::Eve.to_raw_public();
		let _ferdie = Keyring::Ferdie.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);

			public::submit_candidacy(&eve, 0);

			public::set_approvals(&alice, &vec![true], 0);
			public::set_approvals(&dave, &vec![true], 0);

			assert_eq!(approvals_of(&alice), vec![true]);
			assert_eq!(approvals_of(&dave), vec![true]);
			assert_eq!(voters(), vec![alice.clone(), dave.clone()]);

			public::submit_candidacy(&bob, 1);
			public::submit_candidacy(&charlie, 2);

			public::set_approvals(&bob, &vec![false, true, true], 0);
			public::set_approvals(&charlie, &vec![false, true, true], 0);

			assert_eq!(approvals_of(&alice), vec![true]);
			assert_eq!(approvals_of(&dave), vec![true]);
			assert_eq!(approvals_of(&bob), vec![false, true, true]);
			assert_eq!(approvals_of(&charlie), vec![false, true, true]);

			assert_eq!(voters(), vec![alice.clone(), dave.clone(), bob.clone(), charlie.clone()]);


		});
	}

	#[test]
	fn resubmitting_voting_should_work() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let eve = Keyring::Eve.to_raw_public();
		let _ferdie = Keyring::Ferdie.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);

			public::submit_candidacy(&eve, 0);
			public::set_approvals(&dave, &vec![true], 0);

			assert_eq!(approvals_of(&dave), vec![true]);

			public::submit_candidacy(&bob, 1);
			public::submit_candidacy(&charlie, 2);
			public::set_approvals(&dave, &vec![true, false, true], 0);

			assert_eq!(approvals_of(&dave), vec![true, false, true]);
		});
	}

	#[test]
	fn retracting_voter_should_work() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let eve = Keyring::Eve.to_raw_public();
		let _ferdie = Keyring::Ferdie.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);

			public::submit_candidacy(&eve, 0);
			public::submit_candidacy(&bob, 1);
			public::submit_candidacy(&charlie, 2);

			public::set_approvals(&alice, &vec![true], 0);
			public::set_approvals(&bob, &vec![false, true, true], 0);
			public::set_approvals(&charlie, &vec![false, true, true], 0);
			public::set_approvals(&dave, &vec![true, false, true], 0);

			assert_eq!(voters(), vec![alice.clone(), bob.clone(), charlie.clone(), dave.clone()]);
			assert_eq!(approvals_of(&alice), vec![true]);
			assert_eq!(approvals_of(&bob), vec![false, true, true]);
			assert_eq!(approvals_of(&charlie), vec![false, true, true]);
			assert_eq!(approvals_of(&dave), vec![true, false, true]);

			public::retract_voter(&alice, 0);

			assert_eq!(voters(), vec![dave.clone(), bob.clone(), charlie.clone()]);
			assert_eq!(approvals_of(&alice), Vec::<bool>::new());
			assert_eq!(approvals_of(&bob), vec![false, true, true]);
			assert_eq!(approvals_of(&charlie), vec![false, true, true]);
			assert_eq!(approvals_of(&dave), vec![true, false, true]);

			public::retract_voter(&bob, 1);

			assert_eq!(voters(), vec![dave.clone(), charlie.clone()]);
			assert_eq!(approvals_of(&alice), Vec::<bool>::new());
			assert_eq!(approvals_of(&bob), Vec::<bool>::new());
			assert_eq!(approvals_of(&charlie), vec![false, true, true]);
			assert_eq!(approvals_of(&dave), vec![true, false, true]);

			public::retract_voter(&charlie, 1);

			assert_eq!(voters(), vec![dave.clone()]);
			assert_eq!(approvals_of(&alice), Vec::<bool>::new());
			assert_eq!(approvals_of(&bob), Vec::<bool>::new());
			assert_eq!(approvals_of(&charlie), Vec::<bool>::new());
			assert_eq!(approvals_of(&dave), vec![true, false, true]);
		});
	}

	#[test]
	#[should_panic]
	fn invalid_retraction_index_should_panic() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			public::submit_candidacy(&charlie, 0);
			public::set_approvals(&alice, &vec![true], 0);
			public::set_approvals(&bob, &vec![true], 0);
			public::retract_voter(&alice, 1);
		});
	}

	#[test]
	#[should_panic]
	fn overflow_retraction_index_should_panic() {
		let alice = Keyring::Alice.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			public::submit_candidacy(&charlie, 0);
			public::set_approvals(&alice, &vec![true], 0);
			public::retract_voter(&alice, 1);
		});
	}

	#[test]
	#[should_panic]
	fn non_voter_retraction_should_panic() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 1);
			public::submit_candidacy(&charlie, 0);
			public::set_approvals(&alice, &vec![true], 0);
			public::retract_voter(&bob, 0);
		});
	}

	#[test]
	fn simple_tally_should_work() {
		let bob = Keyring::Bob.to_raw_public();
		let eve = Keyring::Eve.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 4);
			assert!(!presentation_active());

			public::submit_candidacy(&bob, 0);
			public::submit_candidacy(&eve, 1);
			public::set_approvals(&bob, &vec![true, false], 0);
			public::set_approvals(&eve, &vec![false, true], 0);
			internal::end_block();

			with_env(|e| e.block_number = 6);
			assert!(presentation_active());
			public::present(&dave, &bob, 8, 0);
			public::present(&dave, &eve, 38, 0);
			assert_eq!(leaderboard(), Some(vec![(0, AccountId::default()), (0, AccountId::default()), (8, bob.clone()), (38, eve.clone())]));

			internal::end_block();

			assert!(!presentation_active());
			assert_eq!(active_council(), vec![(eve.clone(), 11), (bob.clone(), 11)]);

			assert!(!is_a_candidate(&bob));
			assert!(!is_a_candidate(&eve));
			assert_eq!(vote_index(), 1);
			assert_eq!(voter_last_active(&bob), Some(0));
			assert_eq!(voter_last_active(&eve), Some(0));
		});
	}

	#[test]
	fn runners_up_should_be_kept() {
		let alice = Keyring::Alice.to_raw_public();
		let bob = Keyring::Bob.to_raw_public();
		let charlie = Keyring::Charlie.to_raw_public();
		let ferdie = Keyring::Ferdie.to_raw_public();
		let eve = Keyring::Eve.to_raw_public();
		let dave = Keyring::Dave.to_raw_public();
		let mut t = new_test_ext();

		with_externalities(&mut t, || {
			with_env(|e| e.block_number = 4);
			assert!(!presentation_active());

			public::submit_candidacy(&alice, 0);
			public::set_approvals(&ferdie, &vec![true], 0);
			public::submit_candidacy(&bob, 1);
			public::set_approvals(&bob, &vec![false, true], 0);
			public::submit_candidacy(&charlie, 2);
			public::set_approvals(&charlie, &vec![false, false, true], 0);
			public::submit_candidacy(&dave, 3);
			public::set_approvals(&dave, &vec![false, false, false, true], 0);
			public::submit_candidacy(&eve, 4);
			public::set_approvals(&eve, &vec![false, false, false, false, true], 0);

			internal::end_block();

			with_env(|e| e.block_number = 6);
			assert!(presentation_active());
			public::present(&dave, &alice, 57, 0);
			assert_eq!(leaderboard(), Some(vec![
				(0, AccountId::default()),
				(0, AccountId::default()),
				(0, AccountId::default()),
				(57, alice.clone())
			]));
			public::present(&dave, &charlie, 18, 0);
			public::present(&dave, &dave, 28, 0);
			public::present(&dave, &eve, 38, 0);
			assert_eq!(leaderboard(), Some(vec![
				(18, charlie.clone()),
				(28, dave.clone()),
				(38, eve.clone()),
				(57, alice.clone())
			]));

			internal::end_block();

			assert!(!presentation_active());
			assert_eq!(active_council(), vec![(alice.clone(), 11), (eve.clone(), 11)]);

			assert!(!is_a_candidate(&alice));
			assert!(!is_a_candidate(&eve));
			assert!(!is_a_candidate(&bob));
			assert!(is_a_candidate(&charlie));
			assert!(is_a_candidate(&dave));
			assert_eq!(vote_index(), 1);
			assert_eq!(voter_last_active(&bob), Some(0));
			assert_eq!(voter_last_active(&eve), Some(0));
			assert_eq!(candidate_reg_info(&charlie), Some((0, 2)));
			assert_eq!(candidate_reg_info(&dave), Some((0, 3)));
		});
	}
}
